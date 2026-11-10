//! kilit's answers against the lab's own measurements.
//!
//! The built-in result files are the evidence. These tests make sure that
//! kilit uses every measurement, places every measured statement in the
//! right case, and gives back exactly what the lab measured, on the lab's
//! own tables.

use std::fs;
use std::path::Path;

use kilit_core::{
    CASES, Condition, Engine, Impact, Knowledge, MYISAM_CASES, OLD_ALTER_TABLE_CASES,
    OLD_ALTER_TABLE_TWINS, Outcome, Prediction, ROW_FORMAT_CASES, RowFormat, ScenarioResult,
    Schema, Statement, TableState, Variant, Verdict, check, impacts, parse, read_pretend,
};

/// The tables every lab scenario starts from.
const LAB_TABLES: &str = include_str!("../../../scenarios/tables.sql");

/// The text of tables.sql as the lab creates the tables for a scenario:
/// with `orders` as MyISAM when the scenario says so, and only `orders`.
fn lab_tables(engine: Option<&str>) -> String {
    lab_tables_with(engine, "")
}

/// The same, with `options` (` ROW_FORMAT=COMPACT`, ` KEY_BLOCK_SIZE=8`)
/// right after `DEFAULT CHARSET=utf8mb4` in the statement of `orders`, where
/// the lab puts them.
fn lab_tables_with(engine: Option<&str>, options: &str) -> String {
    const CHARSET: &str = "DEFAULT CHARSET=utf8mb4";
    let at = LAB_TABLES.find("CREATE TABLE orders").unwrap();
    let (users, orders) = LAB_TABLES.split_at(at);
    let orders = match engine {
        None => orders.to_owned(),
        Some("MyISAM") => orders.replacen("ENGINE=InnoDB", "ENGINE=MyISAM", 1),
        Some(other) => panic!("the lab has no engine {other} for orders"),
    };
    let orders = orders.replacen(CHARSET, &format!("{CHARSET}{options}"), 1);
    format!("{users}{orders}")
}

/// The table options the lab gives `orders` for a scenario.
fn lab_options(scenario: &ScenarioResult) -> String {
    let mut options = String::new();
    if let Some(row_format) = &scenario.row_format {
        options.push_str(&format!(" ROW_FORMAT={row_format}"));
    }
    if let Some(size) = scenario.key_block_size {
        options.push_str(&format!(" KEY_BLOCK_SIZE={size}"));
    }
    options
}

/// The lab's tables as a scenario measured them: after its setup.
fn lab_schema(scenario: &ScenarioResult) -> Schema {
    let mut sql = lab_tables_with(scenario.engine.as_deref(), &lab_options(scenario));
    for statement in &scenario.setup {
        sql.push_str(statement);
        sql.push_str(";\n");
    }
    Schema::from_sql(&sql)
}

/// The scenario's statements as one SQL text, run as `variant`.
fn sql(scenario: &ScenarioResult, variant: Variant) -> String {
    let mut sql: String = scenario
        .session
        .iter()
        .map(|statement| format!("{statement};\n"))
        .collect();
    sql.push_str(&scenario.statement);
    sql.push_str(&variant.clause());
    sql
}

/// kilit's prediction for the scenario's ALTER, which comes last.
fn predict(
    scenario: &ScenarioResult,
    variant: Variant,
    server: &str,
    schema: &Schema,
) -> Prediction {
    let results = Knowledge::built_in().server(server).unwrap();
    let findings = check(&parse(&sql(scenario, variant)), schema, results);
    findings.last().unwrap().prediction.clone()
}

#[test]
fn every_result_file_is_built_in() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../matrix");
    for entry in fs::read_dir(dir).unwrap() {
        let name = entry.unwrap().file_name().into_string().unwrap();
        if let Some(server) = name.strip_suffix(".json") {
            assert!(
                Knowledge::built_in().server(server).is_some(),
                "matrix/{name} is not built into kilit-core: add it to BUILT_IN in knowledge.rs"
            );
        }
    }
}

/// Every case kilit answers from: InnoDB's, MyISAM's, the row formats' and
/// those of `old_alter_table`.
fn every_case() -> Vec<&'static kilit_core::Case> {
    CASES
        .into_iter()
        .chain(MYISAM_CASES)
        .chain(ROW_FORMAT_CASES)
        .chain(OLD_ALTER_TABLE_CASES)
        .collect()
}

#[test]
fn every_scenario_is_used() {
    for results in Knowledge::built_in().servers() {
        for scenario in results.scenarios() {
            let id = scenario.id.as_str();
            let used = every_case().into_iter().any(|case| {
                case.scenarios.contains(&id)
                    || case
                        .exceptions
                        .iter()
                        .any(|exception| exception.scenarios.contains(&id))
            });
            assert!(
                used,
                "{id} was measured on {} but no case uses it: add it to a case in check.rs",
                results.server.name()
            );
        }
    }
}

#[test]
fn the_scenarios_of_a_case_agree() {
    for results in Knowledge::built_in().servers() {
        for case in every_case() {
            // The case's own scenarios, then each exception's.
            let groups = std::iter::once(case.scenarios)
                .chain(case.exceptions.iter().map(|exception| exception.scenarios));
            for scenarios in groups {
                for variant in Variant::ALL {
                    let verdicts: Vec<_> = scenarios
                        .iter()
                        .map(|id| {
                            let scenario = results.scenario(id).unwrap_or_else(|| {
                                panic!(
                                    "{} uses {id}, which was not measured on {}",
                                    case.id,
                                    results.server.name()
                                )
                            });
                            scenario.verdict_for(variant)
                        })
                        .collect();
                    assert!(
                        verdicts.windows(2).all(|pair| pair[0] == pair[1]),
                        "the scenarios {scenarios:?} of {} disagree on {} as {variant}: \
                         {verdicts:?}",
                        case.id,
                        results.server.name()
                    );
                }
            }
        }
    }
}

#[test]
fn kilit_predicts_what_the_lab_measured() {
    for results in Knowledge::built_in().servers() {
        let server = results.server.name();
        for case in every_case() {
            for id in case.scenarios {
                let scenario = results.scenario(id).unwrap();
                let schema = lab_schema(scenario);
                // With old_alter_table on, a written ALGORITHM wins over the
                // setting, so kilit answers it with the answer measured
                // without the setting: the scenario's DYNAMIC twin. The
                // lab's own verdict for such a variant derives its writes
                // from the default algorithm, COPY.
                let twin = OLD_ALTER_TABLE_TWINS
                    .iter()
                    .find(|(old, _)| old == id)
                    .map(|(_, twin)| results.scenario(twin).unwrap());
                for variant in Variant::ALL {
                    let forced = variant.algorithm.is_some();
                    let (expected, source) = match twin {
                        Some(twin) if forced => {
                            assert_eq!(
                                observation(scenario, variant),
                                observation(twin, variant),
                                "{id} as {variant} on {server} should equal {}",
                                twin.id
                            );
                            (twin.verdict_for(variant), twin)
                        }
                        _ => (scenario.verdict_for(variant), scenario),
                    };
                    match predict(scenario, variant, &server, &schema) {
                        Prediction::Measured {
                            case: found,
                            verdict,
                            ..
                        } => {
                            if source.id == scenario.id {
                                assert_eq!(found, case.id, "{id} as {variant} on {server}");
                            }
                            assert_eq!(Some(verdict), expected, "{id} as {variant} on {server}");
                        }
                        other => panic!("{id} as {variant} on {server}: {other:?}"),
                    }
                }
            }
        }
    }
}

/// What the lab saw when it ran `scenario` as `variant`.
fn observation(scenario: &ScenarioResult, variant: Variant) -> Option<&Outcome> {
    scenario
        .observations
        .iter()
        .find(|observation| observation.variant == variant)
        .map(|observation| &observation.outcome)
}

#[test]
fn what_kilit_sees_in_the_tables_decides_the_answer() {
    for results in Knowledge::built_in().servers() {
        let server = results.server.name();
        for case in CASES {
            for exception in case.exceptions {
                // Rows no schema shows, and a name the table does not have
                // (dropping it is unknown on a known table, whatever the
                // lab measured): neither gives a measured answer here.
                if exception.conditions.contains(&Condition::Hidden)
                    || exception.conditions.contains(&Condition::NoSuchName)
                {
                    continue;
                }
                for id in exception.scenarios {
                    let scenario = results.scenario(id).unwrap();
                    let schema = lab_schema(scenario);
                    for variant in Variant::ALL {
                        match predict(scenario, variant, &server, &schema) {
                            Prediction::Measured {
                                case: found,
                                verdict,
                                evidence,
                                ..
                            } => {
                                assert_eq!(found, case.id, "{id} as {variant} on {server}");
                                assert_eq!(
                                    evidence, exception.scenarios,
                                    "{id} as {variant} on {server}"
                                );
                                assert_eq!(
                                    Some(verdict),
                                    scenario.verdict_for(variant),
                                    "{id} as {variant} on {server}"
                                );
                            }
                            other => panic!("{id} as {variant} on {server}: {other:?}"),
                        }
                    }
                }
            }
        }
    }
}

#[test]
fn what_kilit_cannot_see_shows_up_as_a_note() {
    for results in Knowledge::built_in().servers() {
        let server = results.server.name();
        for case in CASES {
            for exception in case.exceptions {
                // A note names the exception's first scenario.
                let first = exception.scenarios[0];
                for id in exception.scenarios {
                    let scenario = results.scenario(id).unwrap();
                    // No schema shows the rows: a note even with the tables.
                    // Otherwise a note when kilit does not know the table.
                    let schema = if exception.conditions.contains(&Condition::Hidden) {
                        lab_schema(scenario)
                    } else {
                        Schema::new()
                    };
                    match predict(scenario, Variant::SERVER_DEFAULT, &server, &schema) {
                        Prediction::Measured {
                            case: found, notes, ..
                        } => {
                            assert_eq!(found, case.id, "{id}");
                            assert!(
                                notes.iter().any(|note| note.contains(first)),
                                "{first} is missing from the notes of {id}: {notes:?}"
                            );
                        }
                        // Without the table a MODIFY has no answer at all.
                        Prediction::Unknown { reason }
                            if reason.starts_with("MODIFY and CHANGE depend on") => {}
                        other => panic!("{id} on {server}: {other:?}"),
                    }
                }
            }
        }
    }
}

/// What kilit says on MySQL 8.4.13 about each statement of `sql`, with the
/// tables of `schema`.
fn on_mysql_84(schema: &str, sql: &str) -> Vec<String> {
    statements_on_mysql_84(schema, &parse(sql))
}

/// What kilit says on MySQL 8.0.46 about each statement of `sql`, with the
/// tables of `schema`.
fn on_mysql_80(schema: &str, sql: &str) -> Vec<String> {
    statements_on("mysql-8.0.46", schema, &parse(sql))
}

/// The same on MySQL 8.4.13 for Laravel's `migrate --pretend` output.
fn pretend_on_mysql_84(schema: &str, output: &str) -> Vec<String> {
    let pretend = read_pretend(output).expect("the output of migrate --pretend");
    assert!(pretend.unreadable.is_empty(), "{:?}", pretend.unreadable);
    let statements: Vec<Statement> = pretend
        .migrations
        .into_iter()
        .flat_map(|migration| migration.statements)
        .collect();
    statements_on_mysql_84(schema, &statements)
}

/// The output of `migrate --pretend` for one migration that runs `sql`,
/// line for line as Laravel 13 prints it.
fn pretend(migration: &str, sql: &[&str]) -> String {
    let mut output = format!(
        "\n   INFO  Running migrations.  \n\n  {migration} {}  \n",
        ".".repeat(78 - migration.len())
    );
    for statement in sql {
        output.push_str(&format!("  \u{21c2} {statement}  \n"));
    }
    output
}

fn statements_on_mysql_84(schema: &str, statements: &[Statement]) -> Vec<String> {
    statements_on("mysql-8.4.13", schema, statements)
}

fn statements_on(server: &str, schema: &str, statements: &[Statement]) -> Vec<String> {
    let results = Knowledge::built_in().server(server).unwrap();
    check(statements, &Schema::from_sql(schema), results)
        .into_iter()
        .map(|finding| match finding.prediction {
            Prediction::Measured {
                verdict: Verdict::Fails { code, .. },
                ..
            } => format!("fails with error {code}"),
            Prediction::Measured { verdict, .. } => verdict.to_string(),
            Prediction::Unknown { .. } => "unknown".to_owned(),
            Prediction::NotChecked { .. } => "not checked".to_owned(),
        })
        .collect()
}

/// The tables of a small Laravel app, as `php artisan schema:dump` writes them.
const LARAVEL_TABLES: &str = include_str!("../examples/mysql-schema.sql");

// The facts below are what the README promises. If a new measurement
// changes one of them, the README has to change with it.

#[test]
fn a_constrained_foreign_id_copies_the_table() {
    // $table->foreignId('shop_id')->nullable()->constrained();
    assert_eq!(
        on_mysql_84(
            "",
            "alter table `orders` add `shop_id` bigint unsigned null;
             alter table `orders` add constraint `orders_shop_id_foreign` \
             foreign key (`shop_id`) references `shops` (`id`);"
        ),
        [
            "INSTANT, no rebuild, writes continue",
            "COPY, copies every row, writes wait"
        ]
    );
}

#[test]
fn turning_foreign_key_checks_off_avoids_the_copy() {
    // Schema::disableForeignKeyConstraints(); ... enableForeignKeyConstraints();
    assert_eq!(
        on_mysql_84(
            "",
            "SET FOREIGN_KEY_CHECKS=0;
             alter table `orders` add constraint `orders_shop_id_foreign` \
             foreign key (`shop_id`) references `shops` (`id`);
             SET FOREIGN_KEY_CHECKS=1;"
        ),
        [
            "not checked",
            "INPLACE, no rebuild, writes continue",
            "not checked"
        ]
    );
}

#[test]
fn a_fulltext_index_makes_writes_wait() {
    // $table->fullText('note');
    assert_eq!(
        on_mysql_84(
            "",
            "alter table `orders` add fulltext `orders_note_fulltext`(`note`)"
        ),
        ["INPLACE, rebuilds the table, writes wait"]
    );
}

#[test]
fn a_uuid_or_rand_default_fails_while_the_binary_log_is_on() {
    // $table->uuid('token')->default(new Expression('(UUID())'));
    assert_eq!(
        on_mysql_84(
            "",
            "alter table `orders` add `token` char(36) not null default (UUID());
             alter table `orders` add `weight` double not null default (RAND());"
        ),
        ["fails with error 1674", "fails with error 1674"]
    );
}

#[test]
fn the_other_measured_expression_defaults_copy_the_table() {
    // $table->date('due_on')->useCurrent(); and DB::raw() defaults.
    assert_eq!(
        on_mysql_84(
            "",
            "alter table `orders` add `due_on` date not null default (CURDATE());
             alter table `orders` add `placed_at` datetime not null default (NOW());
             alter table `orders` add `seen_at` timestamp null default (CURRENT_TIMESTAMP);
             alter table `orders` add `expires_on` date not null \
             default (CURRENT_DATE + INTERVAL 1 YEAR);
             alter table `orders` add `tags` json not null default (JSON_ARRAY());"
        ),
        ["COPY, copies every row, writes wait"; 5]
    );
}

#[test]
fn a_functional_index_makes_writes_wait() {
    // $table->rawIndex('(lower(email))', 'users_lower_email_index');
    assert_eq!(
        on_mysql_84(
            LARAVEL_TABLES,
            "alter table `users` add index `users_lower_email_index`((lower(email)))"
        ),
        ["INPLACE, no rebuild, writes wait"]
    );
}

#[test]
fn forcing_inplace_on_a_new_column_rebuilds_the_table() {
    assert_eq!(
        on_mysql_84(
            "",
            "alter table `orders` add `coupon` varchar(255) null, algorithm=inplace"
        ),
        ["INPLACE, rebuilds the table, writes continue"]
    );
}

#[test]
fn a_fulltext_index_turns_a_new_column_into_a_copy() {
    assert_eq!(
        on_mysql_84(
            LARAVEL_TABLES,
            "alter table `orders` add `coupon` varchar(255) null;
             alter table `orders` add fulltext `orders_note_fulltext`(`note`);
             alter table `orders` add `channel` varchar(255) null;"
        ),
        [
            "INSTANT, no rebuild, writes continue",
            "INPLACE, rebuilds the table, writes wait",
            "COPY, copies every row, writes wait"
        ]
    );
}

#[test]
fn a_second_fulltext_index_does_not_rebuild_the_table() {
    // $table->fullText('note'); then $table->fullText('legacy_code');
    assert_eq!(
        on_mysql_84(
            LARAVEL_TABLES,
            "alter table `orders` add fulltext `orders_note_fulltext`(`note`);
             alter table `orders` add fulltext `orders_legacy_code_fulltext`(`legacy_code`);"
        ),
        [
            "INPLACE, rebuilds the table, writes wait",
            "INPLACE, no rebuild, writes wait"
        ]
    );
}

#[test]
fn dropping_an_indexed_column_rebuilds_the_table() {
    // $table->dropColumn('status'); orders_status_index goes with it. With
    // a FULLTEXT index that stays on the table, MySQL copies it instead.
    assert_eq!(
        on_mysql_84(
            LARAVEL_TABLES,
            "alter table `orders` drop `status`;
             alter table `orders` add fulltext `orders_note_fulltext`(`note`);
             alter table `orders` add index `orders_amount_index`(`amount`);
             alter table `orders` drop `amount`;"
        ),
        [
            "INPLACE, rebuilds the table, writes continue",
            "INPLACE, rebuilds the table, writes wait",
            "INPLACE, no rebuild, writes continue",
            "COPY, copies every row, writes wait"
        ]
    );
}

#[test]
fn dropping_a_foreign_key_column_fails() {
    // $table->dropColumn('user_id'); without dropForeign first
    assert_eq!(
        pretend_on_mysql_84(
            LARAVEL_TABLES,
            &pretend(
                "2026_11_05_100000_drop_user_from_orders_table",
                &["alter table `orders` drop `user_id`"]
            )
        ),
        ["fails with error 1828"]
    );
}

#[test]
fn dropping_the_foreign_key_first_lets_its_column_and_index_go() {
    // $table->dropForeign(['user_id']); $table->dropColumn('user_id');
    // $table->dropConstrainedForeignId('user_id') writes the same two. The
    // index MySQL made for the key stays, and goes with its column.
    for migration in [
        "2026_11_05_100000_drop_user_from_orders_table",
        "2026_11_05_100100_drop_constrained_user_from_orders_table",
    ] {
        assert_eq!(
            pretend_on_mysql_84(
                LARAVEL_TABLES,
                &pretend(
                    migration,
                    &[
                        "alter table `orders` drop foreign key `orders_user_id_foreign`",
                        "alter table `orders` drop `user_id`"
                    ]
                )
            ),
            [
                "INPLACE, no rebuild, writes continue",
                "INPLACE, rebuilds the table, writes continue"
            ]
        );
    }
    // $table->dropForeign(['user_id']); $table->dropIndex(['user_id']);
    // after $table->foreignId('user_id')->index()->constrained(), whose
    // foreign key uses that index, so MySQL adds none of its own.
    let indexed = LARAVEL_TABLES.replace(
        "KEY `orders_user_id_foreign` (`user_id`)",
        "KEY `orders_user_id_index` (`user_id`)",
    );
    assert_eq!(
        pretend_on_mysql_84(
            &indexed,
            &pretend(
                "2026_11_05_100200_drop_user_index_from_orders_table",
                &[
                    "alter table `orders` drop foreign key `orders_user_id_foreign`",
                    "alter table `orders` drop index `orders_user_id_index`"
                ]
            )
        ),
        [
            "INPLACE, no rebuild, writes continue",
            "INPLACE, no rebuild, writes continue"
        ]
    );
}

#[test]
fn an_index_on_a_text_column_without_a_length_fails() {
    // $table->text('body')->index(); $table->text('summary')->unique();
    assert_eq!(
        pretend_on_mysql_84(
            LARAVEL_TABLES,
            &pretend(
                "2026_11_05_100300_add_body_to_orders_table",
                &[
                    "alter table `orders` add `body` text not null",
                    "alter table `orders` add `summary` text not null",
                    "alter table `orders` add index `orders_body_index`(`body`)",
                    "alter table `orders` add unique `orders_summary_unique`(`summary`)"
                ]
            )
        ),
        [
            "INSTANT, no rebuild, writes continue",
            "INSTANT, no rebuild, writes continue",
            "fails with error 1170",
            "fails with error 1170"
        ]
    );
    // Schema::create('tags', ...) with $table->id() and
    // $table->text('name')->unique(): the index comes in its own statement.
    assert_eq!(
        pretend_on_mysql_84(
            LARAVEL_TABLES,
            &pretend(
                "2026_11_05_100400_create_tags_table",
                &[
                    "create table `tags` (`id` bigint unsigned not null auto_increment primary \
                     key, `name` text not null) default character set utf8mb4 collate \
                     'utf8mb4_unicode_ci'",
                    "alter table `tags` add unique `tags_name_unique`(`name`)"
                ]
            )
        ),
        ["not checked", "fails with error 1170"]
    );
}

#[test]
fn dropping_the_index_of_a_foreign_key_that_stays_fails() {
    // $table->dropIndex('orders_user_id_foreign'), with the foreign key still
    // there. After it, the index and the key are both still there.
    assert_eq!(
        pretend_on_mysql_84(
            LARAVEL_TABLES,
            &pretend(
                "2026_11_05_100500_drop_user_index_from_orders_table",
                &[
                    "alter table `orders` drop index `orders_user_id_foreign`",
                    "alter table `orders` drop `user_id`"
                ]
            )
        ),
        ["fails with error 1553", "fails with error 1828"]
    );
    // $table->foreignId('shop_id')->constrained(), then the same.
    assert_eq!(
        pretend_on_mysql_84(
            LARAVEL_TABLES,
            &pretend(
                "2026_11_05_100600_add_shop_to_orders_table",
                &[
                    "alter table `orders` add `shop_id` bigint unsigned not null",
                    "alter table `orders` add constraint `orders_shop_id_foreign` foreign key \
                     (`shop_id`) references `shops` (`id`)",
                    "alter table `orders` drop index `orders_shop_id_foreign`"
                ]
            )
        ),
        [
            "INSTANT, no rebuild, writes continue",
            "COPY, copies every row, writes wait",
            "fails with error 1553"
        ]
    );
}

#[test]
fn dropping_the_foreign_key_first_lets_the_index_go() {
    // $table->dropForeign(['user_id']); $table->dropIndex('orders_user_id_foreign');
    assert_eq!(
        pretend_on_mysql_84(
            LARAVEL_TABLES,
            &pretend(
                "2026_11_05_100700_drop_user_key_from_orders_table",
                &[
                    "alter table `orders` drop foreign key `orders_user_id_foreign`",
                    "alter table `orders` drop index `orders_user_id_foreign`"
                ]
            )
        ),
        [
            "INPLACE, no rebuild, writes continue",
            "INPLACE, no rebuild, writes continue"
        ]
    );
}

#[test]
fn an_index_laravel_named_after_the_key_gets_no_answer_next_to_another() {
    // $table->index(['user_id', 'amount']), then dropIndex of either index.
    // The first may be gone already: MySQL replaces the index it made for a
    // key, and a dump cannot show whether it made this one.
    let add = "alter table `orders` add index `orders_user_id_amount_index`(`user_id`, `amount`)";
    for dropped in ["orders_user_id_foreign", "orders_user_id_amount_index"] {
        assert_eq!(
            pretend_on_mysql_84(
                LARAVEL_TABLES,
                &pretend(
                    "2026_11_05_100800_add_user_amount_index_to_orders_table",
                    &[add, &format!("alter table `orders` drop index `{dropped}`")]
                )
            ),
            ["INPLACE, no rebuild, writes continue", "unknown"],
            "{dropped}"
        );
    }
}

#[test]
fn a_generated_column_keeps_the_column_it_uses() {
    // $table->integer('double_amount')->virtualAs('amount * 2'), or ->storedAs,
    // then $table->dropColumn('amount') or $table->renameColumn('amount', 'total').
    for kind in ["virtual", "stored"] {
        for change in ["drop `amount`", "rename column `amount` to `total`"] {
            assert_eq!(
                pretend_on_mysql_84(
                    LARAVEL_TABLES,
                    &pretend(
                        "2026_11_05_100900_add_double_amount_to_orders_table",
                        &[
                            &format!(
                                "alter table `orders` add `double_amount` int as (amount * 2) \
                                 {kind}"
                            ),
                            &format!("alter table `orders` {change}"),
                            "alter table `orders` drop `double_amount`"
                        ]
                    )
                ),
                [
                    "unknown",
                    "fails with error 3108",
                    "INSTANT, no rebuild, writes continue"
                ],
                "{kind} {change}"
            );
        }
    }
}

#[test]
fn a_check_constraint_keeps_the_columns_it_uses() {
    // DB::statement('alter table orders add constraint ... check (...)'), then
    // $table->dropColumn('amount') or $table->renameColumn('amount', 'total').
    // Laravel has no call for a CHECK constraint.
    let add = "alter table `orders` add constraint `orders_amount_check` \
               check (`amount` + `legacy_flag` > 0)";
    for change in ["drop `amount`", "rename column `amount` to `total`"] {
        assert_eq!(
            pretend_on_mysql_84(
                LARAVEL_TABLES,
                &pretend(
                    "2026_11_05_101000_add_amount_check_to_orders_table",
                    &[
                        add,
                        &format!("alter table `orders` {change}"),
                        "alter table `orders` drop constraint `orders_amount_check`",
                        &format!("alter table `orders` {change}"),
                    ]
                )
            ),
            [
                "COPY, copies every row, writes wait",
                "fails with error 3959",
                "INSTANT, no rebuild, writes continue",
                "INSTANT, no rebuild, writes continue",
            ],
            "{change}"
        );
    }
}

#[test]
fn a_check_constraint_goes_with_its_only_column_but_not_with_a_new_name() {
    let add = "alter table `orders` add constraint `orders_amount_check` check (`amount` > 0)";
    assert_eq!(
        pretend_on_mysql_84(
            LARAVEL_TABLES,
            &pretend(
                "2026_11_05_101100_add_amount_check_to_orders_table",
                &[
                    add,
                    "alter table `orders` rename column `amount` to `total`",
                    "alter table `orders` drop `amount`",
                ]
            )
        ),
        [
            "COPY, copies every row, writes wait",
            "fails with error 3959",
            "INSTANT, no rebuild, writes continue",
        ]
    );
}

#[test]
fn a_schema_dump_shows_the_check_constraints_of_a_table() {
    // As `php artisan schema:dump` writes them.
    let schema = LARAVEL_TABLES.replacen(
        "CONSTRAINT `orders_user_id_foreign`",
        "CONSTRAINT `orders_chk_1` CHECK ((`amount` > `legacy_flag`)),\n  \
         CONSTRAINT `orders_user_id_foreign`",
        1,
    );
    assert_eq!(
        on_mysql_84(
            &schema,
            "alter table `orders` drop `amount`;
             alter table `orders` rename column `legacy_flag` to `flag`;
             alter table `orders` drop `note`;"
        ),
        [
            "fails with error 3959",
            "fails with error 3959",
            "INSTANT, no rebuild, writes continue",
        ]
    );
}

#[test]
fn a_varchar_that_crosses_255_bytes_copies_the_table() {
    // $table->string('note', 63)->nullable()->change(); then 64, in utf8mb4
    assert_eq!(
        on_mysql_84(
            LARAVEL_TABLES,
            "alter table `orders` modify `note` varchar(63) null;
             alter table `orders` modify `note` varchar(64) null;"
        ),
        [
            "INPLACE, no rebuild, writes continue",
            "COPY, copies every row, writes wait"
        ]
    );
}

#[test]
fn changing_the_type_of_a_foreign_key_column_fails() {
    // $table->unsignedInteger('user_id')->change();
    assert_eq!(
        on_mysql_84(
            LARAVEL_TABLES,
            "alter table `orders` modify `user_id` int unsigned not null"
        ),
        ["fails with error 3780"]
    );
}

#[test]
fn changing_a_column_without_repeating_its_default_is_two_changes() {
    // $table->string('status', 30)->change(); on Laravel 11 and later
    assert_eq!(
        on_mysql_84(
            LARAVEL_TABLES,
            "alter table `orders` modify `status` varchar(30) not null"
        ),
        ["unknown"]
    );
}

// A foreign key whose columns do not fit together, or whose parent has no
// usable index: MySQL 8.0.46 and 8.4.13 refuse it, and not always with the
// same error. Each group below is a group of lab scenarios, asserted on
// both servers.

const MYSQL_80: &str = "mysql-8.0.46";
const MYSQL_84: &str = "mysql-8.4.13";
const COPY: &str = "COPY, copies every row, writes wait";
const INSTANT: &str = "INSTANT, no rebuild, writes continue";
const INPLACE: &str = "INPLACE, no rebuild, writes continue";
const FAILS_3780: &str = "fails with error 3780";
const FAILS_1822: &str = "fails with error 1822";
const FAILS_6125: &str = "fails with error 6125";

const OC_REF: &str = "ALTER TABLE orders ADD COLUMN ref BIGINT UNSIGNED NULL";
const OC_NAME: &str = "ALTER TABLE orders ADD COLUMN user_name VARCHAR(100) NULL";
const UNIQUE_NAME: &str = "ALTER TABLE users ADD UNIQUE INDEX uq_users_name (name)";
const REF: &str = "ADD CONSTRAINT fk_orders_ref FOREIGN KEY (ref) REFERENCES users (id)";
const NAME: &str =
    "ADD CONSTRAINT fk_orders_user_name FOREIGN KEY (user_name) REFERENCES users (name)";
const CODE: &str =
    "ADD CONSTRAINT fk_orders_user_name FOREIGN KEY (user_name) REFERENCES users (code)";
const TWO: &str =
    "ADD CONSTRAINT fk_orders_two FOREIGN KEY (ref, user_name) REFERENCES users (id, name)";
const SELF: &str = "ADD CONSTRAINT fk_orders_parent FOREIGN KEY (parent_id) REFERENCES orders (id)";

/// The lab's tables after `setup`, as a scenario starts from them.
fn lab_after(setup: &[&str]) -> String {
    let mut sql = LAB_TABLES.to_owned();
    for statement in setup {
        sql.push_str(statement);
        sql.push_str(";\n");
    }
    sql
}

/// What kilit says about the last statement of `sql`, with the tables of
/// `schema`: on MySQL 8.0.46 first, then on 8.4.13.
fn on_both(schema: &str, sql: &str) -> [String; 2] {
    [
        on_mysql_80(schema, sql).pop().unwrap(),
        on_mysql_84(schema, sql).pop().unwrap(),
    ]
}

/// What kilit says about `ALTER TABLE orders <operation>`, in the lab's
/// tables after `setup`, with foreign_key_checks on.
fn lab_foreign_key(setup: &[&str], operation: &str) -> [String; 2] {
    on_both(
        &lab_after(setup),
        &format!("SET foreign_key_checks = 1;\nALTER TABLE orders {operation};"),
    )
}

/// The same answer on both servers.
fn assert_foreign_key(name: &str, setup: &[&str], operation: &str, answer: &str) {
    assert_eq!(
        lab_foreign_key(setup, operation),
        [answer, answer],
        "{name}"
    );
}

/// The answer of MySQL 8.0.46, then the answer of 8.4.13.
fn assert_foreign_key_per_server(
    name: &str,
    setup: &[&str],
    operation: &str,
    on_80: &str,
    on_84: &str,
) {
    assert_eq!(lab_foreign_key(setup, operation), [on_80, on_84], "{name}");
}

/// kilit's predictions for every statement of `sql` on `server`.
fn predictions_on(server: &str, schema: &str, sql: &str) -> Vec<Prediction> {
    let results = Knowledge::built_in().server(server).unwrap();
    check(&parse(sql), &Schema::from_sql(schema), results)
        .into_iter()
        .map(|finding| finding.prediction)
        .collect()
}

/// The notes of a measured prediction.
fn notes_of(prediction: &Prediction) -> &[String] {
    match prediction {
        Prediction::Measured { notes, .. } => notes,
        other => panic!("expected a measured answer, got {other:?}"),
    }
}

#[test]
fn a_foreign_key_between_integer_columns_of_other_types_fails_with_3780_on_both_servers() {
    // $table->integer('ref'); $table->foreign('ref')->references('id')->on('users');
    // against users.id BIGINT UNSIGNED, the lab's parent.
    for (name, column) in [
        ("int", "INT"),
        ("int unsigned", "INT UNSIGNED"),
        ("bigint", "BIGINT"),
        ("smallint unsigned", "SMALLINT UNSIGNED"),
        ("tinyint unsigned", "TINYINT UNSIGNED"),
        ("mediumint unsigned", "MEDIUMINT UNSIGNED"),
    ] {
        let add = format!("ALTER TABLE orders ADD COLUMN ref {column} NULL");
        assert_foreign_key(name, &[add.as_str()], REF, FAILS_3780);
    }
}

#[test]
fn a_bigint_unsigned_column_against_an_int_unsigned_parent_fails_with_3780_on_both_servers() {
    // $table->increments('code') on the parent, $table->foreignId('ref') on the child.
    assert_foreign_key(
        "parent int unsigned",
        &[
            "ALTER TABLE users ADD COLUMN code INT UNSIGNED NULL",
            "ALTER TABLE users ADD UNIQUE INDEX uq_users_code (code)",
            OC_REF,
        ],
        "ADD CONSTRAINT fk_orders_ref FOREIGN KEY (ref) REFERENCES users (code)",
        FAILS_3780,
    );
}

#[test]
fn a_foreign_key_from_a_plain_int_column_to_users_id_fails_with_3780_on_both_servers() {
    // The shape that started the step: orders.amount is INT, users.id is
    // BIGINT UNSIGNED.
    for checks in ["SET foreign_key_checks = 1;\n", ""] {
        let sql = format!(
            "{checks}ALTER TABLE orders ADD CONSTRAINT fk_orders_amount \
             FOREIGN KEY (amount) REFERENCES users (id)"
        );
        assert_eq!(
            on_both(LAB_TABLES, &sql),
            [FAILS_3780, FAILS_3780],
            "{checks:?}"
        );
    }
}

#[test]
fn a_foreign_key_between_texts_of_another_charset_or_collation_fails_with_3780_on_both_servers() {
    // $table->string('user_name') with its own charset or collation, against
    // users.name, utf8mb4 with its default collation.
    for (name, child) in [
        (
            "latin1",
            "ALTER TABLE orders ADD COLUMN user_name VARCHAR(100) CHARACTER SET latin1 NULL",
        ),
        (
            "utf8mb4_bin",
            "ALTER TABLE orders ADD COLUMN user_name VARCHAR(100) COLLATE utf8mb4_bin NULL",
        ),
    ] {
        assert_foreign_key(name, &[UNIQUE_NAME, child], NAME, FAILS_3780);
    }
}

#[test]
fn a_table_that_defaults_to_latin1_gives_its_columns_the_charset_that_mismatches() {
    // The child's own charset comes from the table when the column names none.
    let schema = "CREATE TABLE users (
            id BIGINT UNSIGNED AUTO_INCREMENT PRIMARY KEY,
            name VARCHAR(100) NOT NULL,
            UNIQUE KEY uq_users_name (name)
        ) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4;
        CREATE TABLE orders (
            id BIGINT UNSIGNED AUTO_INCREMENT PRIMARY KEY,
            user_name VARCHAR(100) NULL,
            other_name VARCHAR(100) CHARACTER SET utf8mb4 NULL
        ) ENGINE=InnoDB DEFAULT CHARSET=latin1;";
    let sql = format!("SET foreign_key_checks = 1;\nALTER TABLE orders {NAME};");
    assert_eq!(on_both(schema, &sql), [FAILS_3780, FAILS_3780]);
    // A column of its own charset keeps its answer next to the table's.
    let sql = "SET foreign_key_checks = 1;
        ALTER TABLE orders ADD CONSTRAINT fk_orders_other FOREIGN KEY (other_name) \
        REFERENCES users (name);";
    assert_eq!(on_both(schema, sql), [COPY, COPY]);
}

#[test]
fn a_foreign_key_to_a_column_without_an_index_fails_with_1822_on_mysql_80_and_6125_on_mysql_84() {
    for (name, setup, operation) in [
        ("no index on the parent", vec![OC_NAME], NAME),
        (
            "the column is second in the only index",
            vec![
                "ALTER TABLE users ADD INDEX idx_users_id_name (id, name)",
                OC_NAME,
            ],
            NAME,
        ),
        (
            "a shorter prefix index",
            vec![
                "ALTER TABLE users ADD INDEX idx_users_name_10 (name(10))",
                OC_NAME,
            ],
            NAME,
        ),
        (
            "a shorter unique prefix index",
            vec![
                "ALTER TABLE users ADD UNIQUE INDEX uq_users_name_10 (name(10))",
                OC_NAME,
            ],
            NAME,
        ),
    ] {
        assert_foreign_key_per_server(name, &setup, operation, FAILS_1822, FAILS_6125);
    }
}

#[test]
fn a_foreign_key_to_a_column_with_only_a_plain_or_longer_index_is_accepted_by_mysql_80_but_fails_with_6125_on_mysql_84()
 {
    for (name, index) in [
        (
            "plain index",
            "ALTER TABLE users ADD INDEX idx_users_name (name)",
        ),
        (
            "plain index, first column of two",
            "ALTER TABLE users ADD INDEX idx_users_name_id (name, id)",
        ),
        (
            "unique index, first column of two",
            "ALTER TABLE users ADD UNIQUE INDEX uq_users_name_id (name, id)",
        ),
        (
            "descending plain index",
            "ALTER TABLE users ADD INDEX idx_users_name_desc (name DESC)",
        ),
    ] {
        assert_foreign_key_per_server(name, &[index, OC_NAME], NAME, COPY, FAILS_6125);
    }
}

#[test]
fn a_foreign_key_to_a_primary_key_or_a_unique_index_of_exactly_its_columns_copies_the_table() {
    // Today's answer for the key that works, on both servers.
    for (name, setup, operation) in [
        ("bigint unsigned to the primary key", vec![OC_REF], REF),
        ("a unique index", vec![UNIQUE_NAME, OC_NAME], NAME),
        (
            "a descending unique index",
            vec![
                "ALTER TABLE users ADD UNIQUE INDEX uq_users_name_desc (name DESC)",
                OC_NAME,
            ],
            NAME,
        ),
        (
            "a unique index on a nullable parent column",
            vec![
                "ALTER TABLE users ADD COLUMN code VARCHAR(100) NULL",
                "ALTER TABLE users ADD UNIQUE INDEX uq_users_code (code)",
                OC_NAME,
            ],
            CODE,
        ),
        (
            "a unique index next to a plain one",
            vec![
                UNIQUE_NAME,
                "ALTER TABLE users ADD INDEX idx_users_name (name)",
                OC_NAME,
            ],
            NAME,
        ),
        (
            "a unique index next to a longer one",
            vec![
                UNIQUE_NAME,
                "ALTER TABLE users ADD INDEX idx_users_name_id (name, id)",
                OC_NAME,
            ],
            NAME,
        ),
        (
            "a unique index next to one that has the column second",
            vec![
                UNIQUE_NAME,
                "ALTER TABLE users ADD INDEX idx_users_id_name (id, name)",
                OC_NAME,
            ],
            NAME,
        ),
    ] {
        assert_foreign_key(name, &setup, operation, COPY);
    }
}

#[test]
fn text_columns_of_another_length_or_kind_with_the_same_charset_keep_the_copy_answer() {
    for (name, setup, operation) in [
        (
            "a shorter VARCHAR",
            vec![
                UNIQUE_NAME,
                "ALTER TABLE orders ADD COLUMN user_name VARCHAR(50) NULL",
            ],
            NAME,
        ),
        (
            "a longer VARCHAR",
            vec![
                UNIQUE_NAME,
                "ALTER TABLE orders ADD COLUMN user_name VARCHAR(200) NULL",
            ],
            NAME,
        ),
        (
            "a CHAR of the same length to a VARCHAR",
            vec![
                UNIQUE_NAME,
                "ALTER TABLE orders ADD COLUMN user_name CHAR(100) NULL",
            ],
            NAME,
        ),
        (
            "a shorter CHAR to a VARCHAR",
            vec![
                UNIQUE_NAME,
                "ALTER TABLE orders ADD COLUMN user_name CHAR(50) NULL",
            ],
            NAME,
        ),
        (
            "a VARCHAR to a CHAR",
            vec![
                "ALTER TABLE users ADD COLUMN code CHAR(100) NULL",
                "ALTER TABLE users ADD UNIQUE INDEX uq_users_code (code)",
                OC_NAME,
            ],
            CODE,
        ),
        (
            "a CHAR to a CHAR",
            vec![
                "ALTER TABLE users ADD COLUMN code CHAR(100) NULL",
                "ALTER TABLE users ADD UNIQUE INDEX uq_users_code (code)",
                "ALTER TABLE orders ADD COLUMN user_name CHAR(100) NULL",
            ],
            CODE,
        ),
    ] {
        assert_foreign_key(name, &setup, operation, COPY);
    }
}

#[test]
fn a_two_column_foreign_key_to_a_unique_index_of_exactly_its_columns_copies_the_table() {
    assert_foreign_key(
        "two columns",
        &[
            "ALTER TABLE users ADD UNIQUE INDEX uq_users_id_name (id, name)",
            OC_REF,
            OC_NAME,
        ],
        TWO,
        COPY,
    );
}

#[test]
fn a_self_referencing_key_on_a_bigint_to_the_primary_key_copies_the_table() {
    // $table->foreignId('parent_id')->nullable()->constrained('orders');
    assert_foreign_key(
        "orders.parent_id to orders.id",
        &["ALTER TABLE orders ADD COLUMN parent_id BIGINT UNSIGNED NULL"],
        SELF,
        COPY,
    );
}

#[test]
fn int_display_widths_letter_case_and_backticks_do_not_make_a_mismatch() {
    // mysqldump writes `bigint unsigned`, Laravel's older dumps `bigint(20) unsigned`.
    let users = "CREATE TABLE `users` (
            `id` bigint(20) unsigned NOT NULL AUTO_INCREMENT,
            `code` INT(11) NOT NULL,
            PRIMARY KEY (`id`),
            UNIQUE KEY `users_code_unique` (`code`)
        ) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4;";
    let orders = "CREATE TABLE `orders` (
            `id` bigint unsigned NOT NULL AUTO_INCREMENT,
            `ref` BIGINT UNSIGNED NULL,
            `code_ref` int(11) NULL,
            `wide_ref` int NULL,
            PRIMARY KEY (`id`)
        ) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4;";
    let schema = format!("{users}\n{orders}");
    for foreign_key in [
        "foreign key (`ref`) references `users` (`id`)",
        "FOREIGN KEY (REF) REFERENCES users (ID)",
        "foreign key (`code_ref`) references `users` (`code`)",
        "foreign key (`wide_ref`) references `users` (`code`)",
    ] {
        let sql = format!(
            "SET foreign_key_checks = 1;\nalter table `orders` add constraint `fk_x` {foreign_key}"
        );
        assert_eq!(on_both(&schema, &sql), [COPY, COPY], "{foreign_key}");
    }
    // int(11) is int: it still does not fit a BIGINT UNSIGNED.
    let sql = "SET foreign_key_checks = 1;
        alter table `orders` add constraint `fk_x` foreign key (`code_ref`) references `users` (`id`)";
    assert_eq!(on_both(&schema, sql), [FAILS_3780, FAILS_3780]);
}

#[test]
fn a_forced_algorithm_or_lock_gets_the_answer_of_exactly_that_variant() {
    // A mismatch fails the same way whatever the clause says.
    let mismatch = [OC_REF.replace("BIGINT UNSIGNED", "INT")];
    for clause in [
        ", ALGORITHM=INSTANT",
        ", ALGORITHM=INPLACE",
        ", ALGORITHM=INPLACE, LOCK=NONE",
        ", ALGORITHM=COPY",
        ", LOCK=NONE",
    ] {
        assert_foreign_key(
            clause,
            &[mismatch[0].as_str()],
            &format!("{REF}{clause}"),
            FAILS_3780,
        );
        // So does a parent without an index, with its error per server.
        assert_foreign_key_per_server(
            clause,
            &[OC_NAME],
            &format!("{NAME}{clause}"),
            FAILS_1822,
            FAILS_6125,
        );
        // A plain index: 8.4.13 refuses it in every variant.
        assert_eq!(
            lab_foreign_key(
                &["ALTER TABLE users ADD INDEX idx_users_name (name)", OC_NAME],
                &format!("{NAME}{clause}")
            )[1],
            FAILS_6125,
            "{clause}"
        );
    }
    // On 8.0.46 the plain index is accepted: INSTANT, INPLACE and LOCK=NONE
    // are refused with 1846, COPY is allowed.
    let plain = ["ALTER TABLE users ADD INDEX idx_users_name (name)", OC_NAME];
    for (clause, answer) in [
        (", ALGORITHM=INSTANT", "fails with error 1846"),
        (", ALGORITHM=INPLACE", "fails with error 1846"),
        (", ALGORITHM=INPLACE, LOCK=NONE", "fails with error 1846"),
        (", ALGORITHM=COPY", COPY),
        (", LOCK=NONE", "fails with error 1846"),
    ] {
        assert_eq!(
            lab_foreign_key(&plain, &format!("{NAME}{clause}"))[0],
            answer,
            "{clause}"
        );
    }
}

// What kilit cannot tell stays unknown, on both servers.

fn assert_unknown(name: &str, setup: &[&str], operation: &str) {
    assert_eq!(
        lab_foreign_key(setup, operation),
        ["unknown", "unknown"],
        "{name}"
    );
}

#[test]
fn a_difference_in_decimal_columns_is_unknown_and_identical_decimals_copy_the_table() {
    let parent = [
        "ALTER TABLE users ADD COLUMN price DECIMAL(10,2) NULL",
        "ALTER TABLE users ADD UNIQUE INDEX uq_users_price (price)",
    ];
    let price = "ADD CONSTRAINT fk_orders_ref FOREIGN KEY (ref) REFERENCES users (price)";
    for (name, child) in [
        (
            "another precision",
            "ALTER TABLE orders ADD COLUMN ref DECIMAL(12,2) NULL",
        ),
        (
            "unsigned",
            "ALTER TABLE orders ADD COLUMN ref DECIMAL(10,2) UNSIGNED NULL",
        ),
    ] {
        assert_unknown(name, &[parent[0], parent[1], child], price);
    }
    assert_foreign_key(
        "identical",
        &[
            parent[0],
            parent[1],
            "ALTER TABLE orders ADD COLUMN ref DECIMAL(10,2) NULL",
        ],
        price,
        COPY,
    );
}

#[test]
fn an_integer_pair_the_lab_did_not_measure_is_unknown() {
    // Against users.id BIGINT UNSIGNED: the signed small sizes.
    for column in ["SMALLINT", "TINYINT", "MEDIUMINT"] {
        let add = format!("ALTER TABLE orders ADD COLUMN ref {column} NULL");
        assert_unknown(column, &[add.as_str()], REF);
    }
    // Against an INT UNSIGNED parent: only BIGINT UNSIGNED was measured.
    let parent = [
        "ALTER TABLE users ADD COLUMN code INT UNSIGNED NULL",
        "ALTER TABLE users ADD UNIQUE INDEX uq_users_code (code)",
    ];
    let code = "ADD CONSTRAINT fk_orders_ref FOREIGN KEY (ref) REFERENCES users (code)";
    for column in ["INT", "BIGINT", "SMALLINT UNSIGNED"] {
        let add = format!("ALTER TABLE orders ADD COLUMN ref {column} NULL");
        assert_unknown(
            &format!("INT UNSIGNED parent, {column} child"),
            &[parent[0], parent[1], add.as_str()],
            code,
        );
    }
    // Identical types are not a difference.
    assert_foreign_key(
        "INT UNSIGNED to INT UNSIGNED",
        &[
            parent[0],
            parent[1],
            "ALTER TABLE orders ADD COLUMN ref INT UNSIGNED NULL",
        ],
        code,
        COPY,
    );
}

#[test]
fn a_collation_or_charset_pair_the_lab_did_not_measure_is_unknown() {
    for (name, setup) in [
        (
            "a child with utf8mb4_unicode_ci",
            vec![
                UNIQUE_NAME,
                "ALTER TABLE orders ADD COLUMN user_name VARCHAR(100) COLLATE utf8mb4_unicode_ci NULL",
            ],
        ),
        (
            "a CHAR child with another charset",
            vec![
                UNIQUE_NAME,
                "ALTER TABLE orders ADD COLUMN user_name CHAR(100) CHARACTER SET latin1 NULL",
            ],
        ),
        (
            "a latin1 parent and a utf8mb4 child",
            vec![
                "ALTER TABLE users MODIFY name VARCHAR(100) CHARACTER SET latin1 NOT NULL",
                UNIQUE_NAME,
                OC_NAME,
            ],
        ),
    ] {
        assert_unknown(name, &setup, NAME);
    }
}

#[test]
fn a_charset_kilit_cannot_work_out_is_unknown() {
    // Neither the column nor the table says which charset the parent uses,
    // and kilit does not guess it from the server's default.
    let schema = "CREATE TABLE users (
            id BIGINT UNSIGNED AUTO_INCREMENT PRIMARY KEY,
            name VARCHAR(100) NOT NULL,
            UNIQUE KEY uq_users_name (name)
        );
        CREATE TABLE orders (
            id BIGINT UNSIGNED AUTO_INCREMENT PRIMARY KEY,
            user_name VARCHAR(100) CHARACTER SET utf8mb4 NULL
        );";
    let sql = format!("SET foreign_key_checks = 1;\nALTER TABLE orders {NAME};");
    assert_eq!(on_both(schema, &sql), ["unknown", "unknown"]);
}

#[test]
fn a_fulltext_index_alone_does_not_tell_whether_the_key_works() {
    assert_unknown(
        "FULLTEXT only",
        &[
            "ALTER TABLE users ADD FULLTEXT INDEX ft_users_name (name)",
            OC_NAME,
        ],
        NAME,
    );
}

#[test]
fn a_prefix_index_as_long_as_the_column_does_not_tell_whether_the_key_works() {
    for (name, index) in [
        (
            "unique, as long as the column",
            "ALTER TABLE users ADD UNIQUE INDEX uq_users_name_100 (name(100))",
        ),
        (
            "plain, as long as the column",
            "ALTER TABLE users ADD INDEX idx_users_name_100 (name(100))",
        ),
        (
            "unique, longer than the column",
            "ALTER TABLE users ADD UNIQUE INDEX uq_users_name_150 (name(150))",
        ),
    ] {
        assert_unknown(name, &[index, OC_NAME], NAME);
    }
}

#[test]
fn a_composite_primary_key_that_starts_with_the_column_is_unknown() {
    assert_unknown(
        "primary key (id, name), key on id",
        &[
            "ALTER TABLE users DROP PRIMARY KEY, ADD PRIMARY KEY (id, name)",
            OC_REF,
        ],
        REF,
    );
}

#[test]
fn a_column_that_does_not_exist_or_a_missing_column_list_is_unknown() {
    let setup = [OC_REF];
    for (name, operation) in [
        (
            "no such child column",
            "ADD CONSTRAINT fk_orders_nope FOREIGN KEY (nope) REFERENCES users (id)",
        ),
        (
            "no such parent column",
            "ADD CONSTRAINT fk_orders_ref FOREIGN KEY (ref) REFERENCES users (nope)",
        ),
        (
            "no referenced column list",
            "ADD CONSTRAINT fk_orders_ref FOREIGN KEY (ref) REFERENCES users",
        ),
        (
            "two child columns, one referenced column",
            "ADD CONSTRAINT fk_orders_ref FOREIGN KEY (ref, user_id) REFERENCES users (id)",
        ),
    ] {
        assert_unknown(name, &setup, operation);
    }
}

#[test]
fn a_generated_column_on_either_side_is_unknown() {
    assert_unknown(
        "generated child column",
        &["ALTER TABLE orders ADD COLUMN ref BIGINT UNSIGNED AS (user_id + 1) STORED"],
        REF,
    );
    assert_unknown(
        "generated parent column",
        &[
            "ALTER TABLE users ADD COLUMN code BIGINT UNSIGNED AS (id + 1) STORED",
            "ALTER TABLE users ADD UNIQUE INDEX uq_users_code (code)",
            OC_REF,
        ],
        "ADD CONSTRAINT fk_orders_ref FOREIGN KEY (ref) REFERENCES users (code)",
    );
}

#[test]
fn a_parent_that_has_a_foreign_key_on_the_column_is_unknown() {
    // MySQL makes an index for that key; no schema shows it.
    assert_unknown(
        "the parent column is itself a foreign key",
        &[
            "ALTER TABLE users ADD COLUMN org_ref BIGINT UNSIGNED NULL",
            "ALTER TABLE users ADD CONSTRAINT fk_users_org FOREIGN KEY (org_ref) \
             REFERENCES orders (id)",
            OC_REF,
        ],
        "ADD CONSTRAINT fk_orders_ref FOREIGN KEY (ref) REFERENCES users (org_ref)",
    );
}

#[test]
fn a_type_mismatch_together_with_an_index_problem_is_unknown() {
    // Neither condition includes the other.
    let int_code = [
        "ALTER TABLE users ADD COLUMN code INT UNSIGNED NULL",
        OC_REF,
    ];
    let code = "ADD CONSTRAINT fk_orders_ref FOREIGN KEY (ref) REFERENCES users (code)";
    assert_unknown("INT UNSIGNED parent without an index", &int_code, code);
    let mut plain = int_code.to_vec();
    plain.insert(1, "ALTER TABLE users ADD INDEX idx_users_code (code)");
    assert_unknown("INT UNSIGNED parent with a plain index", &plain, code);
    let latin1 = "ALTER TABLE orders ADD COLUMN user_name VARCHAR(100) CHARACTER SET latin1 NULL";
    assert_unknown("latin1 child, no index", &[latin1], NAME);
    assert_unknown(
        "latin1 child, plain index",
        &["ALTER TABLE users ADD INDEX idx_users_name (name)", latin1],
        NAME,
    );
}

#[test]
fn text_and_blob_columns_are_unknown() {
    assert_unknown(
        "TEXT columns",
        &[
            "ALTER TABLE users ADD COLUMN bio TEXT NULL",
            "ALTER TABLE users ADD UNIQUE INDEX uq_users_bio (bio(50))",
            "ALTER TABLE orders ADD COLUMN user_bio TEXT NULL",
        ],
        "ADD CONSTRAINT fk_orders_bio FOREIGN KEY (user_bio) REFERENCES users (bio)",
    );
}

#[test]
fn a_self_reference_other_than_a_bigint_to_the_primary_key_is_unknown() {
    for (name, setup, operation) in [
        (
            "to a column that is not the primary key",
            vec!["ALTER TABLE orders ADD COLUMN parent_id BIGINT UNSIGNED NULL"],
            "ADD CONSTRAINT fk_orders_self FOREIGN KEY (parent_id) REFERENCES orders (user_id)",
        ),
        (
            "to a column without an index",
            vec!["ALTER TABLE orders ADD COLUMN ref INT NULL"],
            "ADD CONSTRAINT fk_orders_self FOREIGN KEY (ref) REFERENCES orders (amount)",
        ),
        (
            "of another integer type",
            vec!["ALTER TABLE orders ADD COLUMN ref INT NULL"],
            "ADD CONSTRAINT fk_orders_self FOREIGN KEY (ref) REFERENCES orders (id)",
        ),
        (
            "a column to itself",
            vec![],
            "ADD CONSTRAINT fk_orders_self FOREIGN KEY (id) REFERENCES orders (id)",
        ),
    ] {
        assert_unknown(name, &setup, operation);
    }
}

#[test]
fn a_two_column_key_other_than_a_unique_index_of_exactly_its_columns_is_unknown() {
    let columns = [OC_REF, OC_NAME];
    for (name, index, operation) in [
        (
            "a plain index on the same columns",
            "ALTER TABLE users ADD INDEX idx_users_id_name (id, name)",
            TWO,
        ),
        (
            "a longer unique index",
            "ALTER TABLE users ADD COLUMN extra INT NULL, \
             ADD UNIQUE INDEX uq_three (id, name, extra)",
            TWO,
        ),
        (
            "the same columns in the other order",
            "ALTER TABLE users ADD UNIQUE INDEX uq_users_name_id (name, id)",
            TWO,
        ),
        (
            "the key's columns swapped",
            "ALTER TABLE users ADD UNIQUE INDEX uq_users_id_name (id, name)",
            "ADD CONSTRAINT fk_orders_two FOREIGN KEY (user_name, ref) REFERENCES users (name, id)",
        ),
        (
            "no index at all on the pair",
            "ALTER TABLE users ADD INDEX idx_users_name (name)",
            TWO,
        ),
    ] {
        assert_unknown(name, &[index, columns[0], columns[1]], operation);
    }
    assert_unknown(
        "an integer pair that does not fit",
        &[
            "ALTER TABLE users ADD UNIQUE INDEX uq_users_id_name (id, name)",
            "ALTER TABLE orders ADD COLUMN ref INT NULL",
            OC_NAME,
        ],
        TWO,
    );
}

// restrict_fk_on_non_standard_key: MySQL 8.4.13 reads it, 8.0.46 does not
// know it, and the input may set it anywhere. kilit does not follow its
// value: the index rules become unknown.

const RESTRICT: [&str; 5] = [
    "SET restrict_fk_on_non_standard_key = OFF",
    "SET SESSION restrict_fk_on_non_standard_key = 0",
    "SET GLOBAL restrict_fk_on_non_standard_key = OFF",
    "SET restrict_fk_on_non_standard_key = ON",
    "SET GLOBAL restrict_fk_on_non_standard_key = 1",
];

#[test]
fn setting_restrict_fk_on_non_standard_key_makes_the_index_rules_unknown() {
    for set in RESTRICT {
        for (name, setup, operation) in [
            ("no index", vec![OC_NAME], NAME),
            (
                "a plain index",
                vec!["ALTER TABLE users ADD INDEX idx_users_name (name)", OC_NAME],
                NAME,
            ),
            (
                "a shorter prefix index",
                vec![
                    "ALTER TABLE users ADD INDEX idx_users_name_10 (name(10))",
                    OC_NAME,
                ],
                NAME,
            ),
        ] {
            let sql = format!("{set};\nALTER TABLE orders {operation};");
            assert_eq!(
                on_both(&lab_after(&setup), &sql),
                ["unknown", "unknown"],
                "{set}: {name}"
            );
        }
    }
}

#[test]
fn restrict_fk_on_non_standard_key_leaves_a_serving_unique_index_and_3780_alone() {
    for set in RESTRICT {
        // The primary key, and a unique index of exactly the column.
        for (name, setup, operation) in [
            ("the primary key", vec![OC_REF], REF),
            ("a unique index", vec![UNIQUE_NAME, OC_NAME], NAME),
        ] {
            let sql = format!("{set};\nALTER TABLE orders {operation};");
            assert_eq!(
                on_both(&lab_after(&setup), &sql),
                [COPY, COPY],
                "{set}: {name}"
            );
        }
        // A type mismatch is not an index question.
        let sql = format!("{set};\nALTER TABLE orders {REF};");
        assert_eq!(
            on_both(
                &lab_after(&["ALTER TABLE orders ADD COLUMN ref INT NULL"]),
                &sql
            ),
            [FAILS_3780, FAILS_3780],
            "{set}"
        );
    }
}

#[test]
fn restrict_fk_on_non_standard_key_makes_the_index_rules_unknown_even_when_a_table_is_not_known() {
    let sql = |set: &str| {
        format!(
            "{set};\nALTER TABLE orders ADD CONSTRAINT fk_orders_user \
             FOREIGN KEY (user_id) REFERENCES users (id);"
        )
    };
    let lost = |table: &str| format!("{LAB_TABLES}ALTER TABLE {table} AUTO_INCREMENT = 5;");
    let schemas = [
        ("neither table", String::new()),
        ("no parent", format!("{LAB_TABLES}DROP TABLE users;")),
        ("no child", format!("{LAB_TABLES}DROP TABLE orders;")),
        ("a lost parent", lost("users")),
        ("a lost child", lost("orders")),
    ];
    for set in RESTRICT {
        for (name, schema) in &schemas {
            assert_eq!(
                on_both(schema, &sql(set)),
                ["unknown", "unknown"],
                "{set}: {name}"
            );
        }
    }
    // Without the variable, the same statement copies the table.
    for (name, schema) in &schemas {
        let alter = "ALTER TABLE orders ADD CONSTRAINT fk_orders_user \
                     FOREIGN KEY (user_id) REFERENCES users (id);";
        assert_eq!(on_both(schema, alter), [COPY, COPY], "{name}");
    }
}

// foreign_key_checks = 0 skips the checks of the key, but a mismatch or a
// missing index that kilit can see, or anything unmeasured, makes it unknown.

#[test]
fn with_foreign_key_checks_off_a_key_that_fits_keeps_its_measured_answer() {
    let schema = lab_after(&[OC_REF, UNIQUE_NAME, OC_NAME]);
    for operation in [REF, NAME] {
        let sql = format!("SET foreign_key_checks = 0;\nALTER TABLE orders {operation};");
        assert_eq!(on_both(&schema, &sql), [INPLACE, INPLACE], "{operation}");
    }
}

#[test]
fn with_foreign_key_checks_off_a_visible_mismatch_or_index_problem_is_unknown() {
    for (name, setup, operation) in [
        (
            "a mismatch",
            vec!["ALTER TABLE orders ADD COLUMN ref INT NULL"],
            REF,
        ),
        ("no index", vec![OC_NAME], NAME),
        (
            "a shorter unique prefix",
            vec![
                "ALTER TABLE users ADD UNIQUE INDEX uq_users_name_10 (name(10))",
                OC_NAME,
            ],
            NAME,
        ),
        (
            "an unmeasured decimal pair",
            vec![
                "ALTER TABLE users ADD COLUMN price DECIMAL(10,2) NULL",
                "ALTER TABLE users ADD UNIQUE INDEX uq_users_price (price)",
                "ALTER TABLE orders ADD COLUMN ref DECIMAL(12,2) NULL",
            ],
            "ADD CONSTRAINT fk_orders_ref FOREIGN KEY (ref) REFERENCES users (price)",
        ),
    ] {
        let sql = format!("SET foreign_key_checks = 0;\nALTER TABLE orders {operation};");
        assert_eq!(
            on_both(&lab_after(&setup), &sql),
            ["unknown", "unknown"],
            "{name}"
        );
    }
    // A plain index is an index problem on 8.4.13.
    let sql = format!("SET foreign_key_checks = 0;\nALTER TABLE orders {NAME};");
    let plain = lab_after(&["ALTER TABLE users ADD INDEX idx_users_name (name)", OC_NAME]);
    assert_eq!(on_mysql_84(&plain, &sql).pop().unwrap(), "unknown");
}

#[test]
fn turning_foreign_key_checks_back_on_brings_the_failure_back() {
    let sql = format!(
        "SET foreign_key_checks = 0;\nSET foreign_key_checks = 1;\nALTER TABLE orders {REF};"
    );
    let schema = lab_after(&["ALTER TABLE orders ADD COLUMN ref INT NULL"]);
    assert_eq!(on_both(&schema, &sql), [FAILS_3780, FAILS_3780]);
}

#[test]
fn with_foreign_key_checks_off_a_table_kilit_does_not_know_keeps_todays_answer_and_notes() {
    let lost = |table: &str| format!("{LAB_TABLES}ALTER TABLE {table} AUTO_INCREMENT = 5;");
    let sql = "SET foreign_key_checks = 0;
        ALTER TABLE orders ADD CONSTRAINT fk_orders_user_name \
        FOREIGN KEY (user_name) REFERENCES users (name);";
    for (name, schema) in [
        ("neither table", String::new()),
        ("no parent", format!("{LAB_TABLES}DROP TABLE users;")),
        ("no child", format!("{LAB_TABLES}DROP TABLE orders;")),
        ("a lost parent", lost("users")),
        ("a lost child", lost("orders")),
    ] {
        for server in [MYSQL_80, MYSQL_84] {
            let predictions = predictions_on(server, &schema, sql);
            assert!(
                matches!(
                    &predictions[1],
                    Prediction::Measured {
                        verdict: Verdict::Runs { .. },
                        ..
                    }
                ),
                "{name} on {server}: {:?}",
                predictions[1]
            );
            // The one note it always had: rows that point nowhere are kept.
            // B1b: and one for what the tables would decide (a missing
            // column, SET NULL on a NOT NULL column, MyISAM), which kilit
            // cannot see when it does not know both tables.
            let notes = notes_of(&predictions[1]);
            assert_eq!(notes.len(), 2, "{name} on {server}: {notes:?}");
            assert!(
                notes
                    .iter()
                    .any(|note| note.starts_with("if a column of the key is missing")),
                "{name} on {server}: {notes:?}"
            );
        }
        assert_eq!(on_both(&schema, sql), [INPLACE, INPLACE], "{name}");
    }
}

// The three states of a table, for the child and for the parent: a failure
// needs both Known; otherwise the answer is today's, with a note for every
// exception kilit could not look at.

#[test]
fn a_foreign_key_is_never_a_failure_when_a_table_is_missing_or_lost() {
    let lost = |table: &str| format!("{LAB_TABLES}ALTER TABLE {table} AUTO_INCREMENT = 5;");
    let schemas = [
        ("neither table", String::new()),
        ("no parent", format!("{LAB_TABLES}DROP TABLE users;")),
        ("no child", format!("{LAB_TABLES}DROP TABLE orders;")),
        ("a lost parent", lost("users")),
        ("a lost child", lost("orders")),
    ];
    // amount is INT and users.id is BIGINT UNSIGNED (3780); users.name has
    // no index (1822 or 6125).
    for (shape, operation) in [
        (
            "a mismatch",
            "ADD CONSTRAINT fk_orders_amount FOREIGN KEY (amount) REFERENCES users (id)",
        ),
        (
            "no index",
            "ADD CONSTRAINT fk_orders_note FOREIGN KEY (note) REFERENCES users (name)",
        ),
    ] {
        let sql = format!("ALTER TABLE orders {operation};");
        // Both tables Known: the failure.
        let known = on_both(LAB_TABLES, &sql);
        if shape == "a mismatch" {
            assert_eq!(known, [FAILS_3780, FAILS_3780]);
        } else {
            assert_eq!(known, [FAILS_1822, FAILS_6125]);
        }
        for (state, schema) in &schemas {
            assert_eq!(on_both(schema, &sql), [COPY, COPY], "{shape}, {state}");
            for server in [MYSQL_80, MYSQL_84] {
                // Today's two notes, and one more for each of the five
                // exceptions kilit could not look at; B1b adds one for the
                // shapes the tables would decide (a missing column, SET NULL
                // on a NOT NULL column, MyISAM).
                let predictions = predictions_on(server, schema, &sql);
                assert_eq!(
                    notes_of(&predictions[0]).len(),
                    8,
                    "{shape}, {state} on {server}: {:?}",
                    predictions[0]
                );
            }
        }
    }
}

#[test]
fn a_foreign_key_that_fits_has_only_its_old_notes_when_both_tables_are_known() {
    // The notes: foreign_key_checks is assumed on, and rows that point nowhere.
    let sql = format!("ALTER TABLE orders {REF};");
    let schema = lab_after(&[OC_REF]);
    for server in [MYSQL_80, MYSQL_84] {
        let predictions = predictions_on(server, &schema, &sql);
        assert_eq!(notes_of(&predictions[0]).len(), 2, "{predictions:?}");
    }
}

// A measured failure does not change the tables.

#[test]
fn after_a_failed_foreign_key_dropping_its_column_gets_no_foreign_key_error() {
    // $table->integer('ref'); $table->foreign('ref')->references('id')->on('users');
    // fails with 3780 on both servers; no key was added, so the column goes.
    let sql = "ALTER TABLE orders ADD COLUMN ref INT NULL;
        ALTER TABLE orders ADD CONSTRAINT fk_orders_ref FOREIGN KEY (ref) REFERENCES users (id);
        ALTER TABLE orders DROP COLUMN ref;";
    let expected = [INSTANT, FAILS_3780, INSTANT];
    assert_eq!(on_mysql_80(LAB_TABLES, sql), expected);
    assert_eq!(on_mysql_84(LAB_TABLES, sql), expected);

    // The same after 1822 and 6125: the key is not there.
    let sql = "ALTER TABLE orders ADD COLUMN user_name VARCHAR(100) NULL;
        ALTER TABLE orders ADD CONSTRAINT fk_orders_user_name FOREIGN KEY (user_name) \
        REFERENCES users (name);
        ALTER TABLE orders DROP COLUMN user_name;";
    assert_eq!(on_mysql_80(LAB_TABLES, sql), [INSTANT, FAILS_1822, INSTANT]);
    assert_eq!(on_mysql_84(LAB_TABLES, sql), [INSTANT, FAILS_6125, INSTANT]);
}

#[test]
fn after_an_accepted_foreign_key_dropping_its_column_fails_with_1828() {
    let sql = "ALTER TABLE orders ADD COLUMN ref BIGINT UNSIGNED NULL;
        ALTER TABLE orders ADD CONSTRAINT fk_orders_ref FOREIGN KEY (ref) REFERENCES users (id);
        ALTER TABLE orders DROP COLUMN ref;";
    let expected = [INSTANT, COPY, "fails with error 1828"];
    assert_eq!(on_mysql_80(LAB_TABLES, sql), expected);
    assert_eq!(on_mysql_84(LAB_TABLES, sql), expected);
}

#[test]
fn a_plain_parent_index_gives_a_key_on_mysql_80_but_not_on_mysql_84() {
    let schema = lab_after(&["ALTER TABLE users ADD INDEX idx_users_name (name)"]);
    let sql = "ALTER TABLE orders ADD COLUMN user_name VARCHAR(100) NULL;
        ALTER TABLE orders ADD CONSTRAINT fk_orders_user_name FOREIGN KEY (user_name) \
        REFERENCES users (name);
        ALTER TABLE orders DROP COLUMN user_name;";
    // 8.0.46 accepted the key, so the column is in it; 8.4.13 refused it.
    assert_eq!(
        on_mysql_80(&schema, sql),
        [INSTANT, COPY, "fails with error 1828"]
    );
    assert_eq!(on_mysql_84(&schema, sql), [INSTANT, FAILS_6125, INSTANT]);
}

// Laravel 10 and 11: the same SQL as Laravel 12 (the framework code that
// writes it did not change), read from the source and not run.

const LARAVEL_USERS: &str = "create table `users` (`id` bigint unsigned not null auto_increment, \
    `name` varchar(255) collate utf8mb4_unicode_ci not null, primary key (`id`)) \
    engine=InnoDB default charset=utf8mb4 collate=utf8mb4_unicode_ci;\n";
const LARAVEL_ORDERS: &str = "create table `orders` (`id` bigint unsigned not null auto_increment, \
    primary key (`id`)) engine=InnoDB default charset=utf8mb4 collate=utf8mb4_unicode_ci;\n";
const LARAVEL_CHILDREN: &str = "create table `children` (`id` bigint unsigned not null auto_increment, \
    primary key (`id`)) engine=InnoDB default charset=utf8mb4 collate=utf8mb4_unicode_ci;\n";

/// What kilit says about Laravel's output for one migration that runs
/// `sql`, on the server named `server`.
fn laravel_on(server: &str, schema: &str, sql: &[&str]) -> Vec<String> {
    let output = pretend("2026_01_01_000001_change_the_tables", sql);
    let pretend = read_pretend(&output).expect("the output of migrate --pretend");
    assert!(pretend.unreadable.is_empty(), "{:?}", pretend.unreadable);
    let statements: Vec<Statement> = pretend
        .migrations
        .into_iter()
        .flat_map(|migration| migration.statements)
        .collect();
    statements_on(server, schema, &statements)
}

#[test]
fn laravel_10_and_11_foreign_keys_get_the_same_answers_as_laravel_12() {
    let users = LARAVEL_USERS.to_owned();
    let users_orders = format!("{LARAVEL_USERS}{LARAVEL_ORDERS}");
    let children = LARAVEL_CHILDREN.to_owned();
    let create_orders = |user_id: &str| {
        format!(
            "create table `orders` (`id` bigint unsigned not null auto_increment primary key, \
             `user_id` {user_id} not null) default character set utf8mb4 \
             collate 'utf8mb4_unicode_ci'"
        )
    };
    let fk_user_id = "alter table `orders` add constraint `orders_user_id_foreign` \
                      foreign key (`user_id`) references `users` (`id`)";
    let add_user_name = "alter table `orders` add `user_name` varchar(255) null";
    let fk_user_name = "alter table `orders` add constraint `orders_user_name_foreign` \
                        foreign key (`user_name`) references `users` (`name`)";
    // $table->integer('user_id'); $table->foreign('user_id')->references('id')->on('users');
    // in Schema::create, then in Schema::table.
    let create_int = create_orders("int");
    for server in [MYSQL_80, MYSQL_84] {
        assert_eq!(
            laravel_on(server, &users, &[create_int.as_str(), fk_user_id]),
            ["not checked", FAILS_3780],
            "create, {server}"
        );
        assert_eq!(
            laravel_on(
                server,
                &users_orders,
                &[
                    "alter table `orders` add `user_id` int not null",
                    fk_user_id
                ]
            ),
            [INSTANT, FAILS_3780],
            "table, {server}"
        );
        // $table->increments('id') on the parent, $table->foreignId('parent_id')->constrained()
        // on the child, in one migration or in two.
        assert_eq!(
            laravel_on(
                server,
                "",
                &[
                    "create table `parents` (`id` int unsigned not null auto_increment primary key) \
                     default character set utf8mb4 collate 'utf8mb4_unicode_ci'",
                    "create table `children` (`id` bigint unsigned not null auto_increment primary key, \
                     `parent_id` bigint unsigned not null) default character set utf8mb4 \
                     collate 'utf8mb4_unicode_ci'",
                    "alter table `children` add constraint `children_parent_id_foreign` \
                     foreign key (`parent_id`) references `parents` (`id`)"
                ]
            ),
            ["not checked", "not checked", FAILS_3780],
            "create parents and children, {server}"
        );
        assert_eq!(
            laravel_on(
                server,
                &children,
                &[
                    "create table `parents` (`id` int unsigned not null auto_increment primary key) \
                     default character set utf8mb4 collate 'utf8mb4_unicode_ci'",
                    "alter table `children` add `parent_id` bigint unsigned not null",
                    "alter table `children` add constraint `children_parent_id_foreign` \
                     foreign key (`parent_id`) references `parents` (`id`)"
                ]
            ),
            ["not checked", INSTANT, FAILS_3780],
            "parents, then a column on children, {server}"
        );
        // $table->foreignId('user_id')->constrained(): today's answer.
        let create_big = create_orders("bigint unsigned");
        assert_eq!(
            laravel_on(server, &users, &[create_big.as_str(), fk_user_id]),
            ["not checked", COPY],
            "foreignId in create, {server}"
        );
        assert_eq!(
            laravel_on(
                server,
                &users_orders,
                &[
                    "alter table `orders` add `user_id` bigint unsigned not null",
                    fk_user_id
                ]
            ),
            [INSTANT, COPY],
            "foreignId in table, {server}"
        );
    }

    // $table->string('user_name')->nullable(); $table->foreign('user_name')->references('name')->on('users');
    // with nothing on the parent, with ->index('name'), with ->unique('name').
    let index = "alter table `users` add index `users_name_index`(`name`)";
    let unique = "alter table `users` add unique `users_name_unique`(`name`)";
    assert_eq!(
        laravel_on(MYSQL_80, &users_orders, &[add_user_name, fk_user_name]),
        [INSTANT, FAILS_1822]
    );
    assert_eq!(
        laravel_on(MYSQL_84, &users_orders, &[add_user_name, fk_user_name]),
        [INSTANT, FAILS_6125]
    );
    assert_eq!(
        laravel_on(
            MYSQL_80,
            &users_orders,
            &[index, add_user_name, fk_user_name]
        ),
        [INPLACE, INSTANT, COPY]
    );
    assert_eq!(
        laravel_on(
            MYSQL_84,
            &users_orders,
            &[index, add_user_name, fk_user_name]
        ),
        [INPLACE, INSTANT, FAILS_6125]
    );
    for server in [MYSQL_80, MYSQL_84] {
        assert_eq!(
            laravel_on(
                server,
                &users_orders,
                &[unique, add_user_name, fk_user_name]
            ),
            [INPLACE, INSTANT, COPY],
            "{server}"
        );
    }
}

#[test]
fn indexes_are_found_by_column_name_whatever_their_spelling() {
    // A dump may write the column in capitals, with backticks, and a
    // descending part: none of it hides the index.
    let tables = |key: &str| {
        format!(
            "CREATE TABLE `users` (
               `id` bigint unsigned NOT NULL AUTO_INCREMENT,
               `name` varchar(100) NOT NULL,
               PRIMARY KEY (`id`),
               {key}
             ) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4;
             CREATE TABLE `orders` (
               `id` bigint unsigned NOT NULL AUTO_INCREMENT,
               `user_name` varchar(100) NULL,
               PRIMARY KEY (`id`)
             ) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4;"
        )
    };
    let sql = "SET foreign_key_checks = 1;
        ALTER TABLE `orders` ADD CONSTRAINT `fk_orders_user_name` \
        FOREIGN KEY (`USER_NAME`) REFERENCES `users` (`NAME`);";
    for key in [
        "UNIQUE KEY `uq` (`NAME`)",
        "UNIQUE KEY `uq` (`Name` DESC)",
        "UNIQUE KEY uq (name)",
    ] {
        assert_eq!(on_both(&tables(key), sql), [COPY, COPY], "{key}");
    }
    for key in ["KEY `idx` (`NAME`)", "KEY `idx` (`Name` DESC)"] {
        assert_eq!(on_both(&tables(key), sql), [COPY, FAILS_6125], "{key}");
    }
    assert_eq!(
        on_both(&tables("KEY `idx` (`id`, `NAME`)"), sql),
        [FAILS_1822, FAILS_6125]
    );
}

#[test]
fn a_foreign_key_without_a_name_gets_the_same_answers() {
    let int = "ALTER TABLE orders ADD COLUMN ref INT NULL";
    assert_foreign_key(
        "no name, a mismatch",
        &[int],
        "ADD FOREIGN KEY (ref) REFERENCES users (id)",
        FAILS_3780,
    );
    assert_foreign_key_per_server(
        "no name, no index",
        &[OC_NAME],
        "ADD FOREIGN KEY (user_name) REFERENCES users (name)",
        FAILS_1822,
        FAILS_6125,
    );
}

// Regression tests for what the review found.

/// The same answer on both servers for the last statement of `sql`.
fn assert_answer(name: &str, schema: &str, sql: &str, answer: &str) {
    assert_eq!(on_both(schema, sql), [answer, answer], "{name}");
}

const USERS_UNIQUE_NAME: &str = "CREATE TABLE users (
        id BIGINT UNSIGNED AUTO_INCREMENT PRIMARY KEY,
        name VARCHAR(100) NOT NULL,
        UNIQUE KEY u (name)
    ) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4;\n";

#[test]
fn every_self_reference_with_foreign_key_checks_off_is_unknown_on_both_servers() {
    // $table->foreignId('parent_id')->nullable()->constrained('orders'); inside
    // Schema::disableForeignKeyConstraints(): unknown, the matching key too.
    let matching = lab_after(&["ALTER TABLE orders ADD COLUMN parent_id BIGINT UNSIGNED NULL"]);
    let sql = format!("SET foreign_key_checks = 0;\nALTER TABLE orders {SELF};");
    assert_answer("matching", &matching, &sql, "unknown");
    // To a column that is not the primary key, or of another type.
    for (name, setup, operation) in [
        (
            "not the primary key",
            "ALTER TABLE orders ADD COLUMN parent_id BIGINT UNSIGNED NULL",
            "ADD CONSTRAINT fk_orders_self FOREIGN KEY (parent_id) REFERENCES orders (user_id)",
        ),
        (
            "another type",
            "ALTER TABLE orders ADD COLUMN parent_id INT NULL",
            SELF,
        ),
    ] {
        let sql = format!("SET foreign_key_checks = 0;\nALTER TABLE orders {operation};");
        assert_answer(name, &lab_after(&[setup]), &sql, "unknown");
    }
    // With the checks on, the matching key keeps its answer.
    let sql = format!("SET foreign_key_checks = 1;\nALTER TABLE orders {SELF};");
    assert_answer("matching, checks on", &matching, &sql, COPY);
}

#[test]
fn a_child_column_whose_index_would_be_too_long_is_unknown() {
    // The child's own index must fit 3072 bytes: VARCHAR(769) in utf8mb4 does not.
    let fk = "ALTER TABLE orders ADD CONSTRAINT f1 FOREIGN KEY (s) REFERENCES users (name);";
    let orders = |column: &str| {
        format!(
            "CREATE TABLE orders (id INT PRIMARY KEY, s {column} NULL) \
             ENGINE=InnoDB DEFAULT CHARSET=utf8mb4;"
        )
    };
    let schema = |column: &str| format!("{USERS_UNIQUE_NAME}{}", orders(column));
    assert_answer("VARCHAR(769)", &schema("VARCHAR(769)"), fk, "unknown");
    assert_answer("VARCHAR(768)", &schema("VARCHAR(768)"), fk, COPY);
    // The same on a child table created in the same migration.
    let new_table = "CREATE TABLE ch (id INT PRIMARY KEY, s VARCHAR(769) NULL) \
         ENGINE=InnoDB DEFAULT CHARSET=utf8mb4;
         ALTER TABLE ch ADD CONSTRAINT f1 FOREIGN KEY (s) REFERENCES users (name);";
    assert_answer("new table", USERS_UNIQUE_NAME, new_table, "unknown");
    // Two columns whose bytes together pass 3072.
    let schema = "CREATE TABLE p (a VARCHAR(500) NOT NULL, b VARCHAR(300) NOT NULL, \
                  UNIQUE KEY u (a, b)) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4;
        CREATE TABLE c (id INT PRIMARY KEY, a VARCHAR(500) NULL, b VARCHAR(300) NULL) \
        ENGINE=InnoDB DEFAULT CHARSET=utf8mb4;";
    let sql = "ALTER TABLE c ADD CONSTRAINT f1 FOREIGN KEY (a, b) REFERENCES p (a, b);";
    assert_answer("two columns", schema, sql, "unknown");
    // CHAR (four columns of 255 characters) and VARBINARY past the limit.
    let schema =
        "CREATE TABLE p (a CHAR(255) NOT NULL, b CHAR(255) NOT NULL, c CHAR(255) NOT NULL, \
                  d CHAR(255) NOT NULL, UNIQUE KEY u (a, b, c, d)) \
                  ENGINE=InnoDB DEFAULT CHARSET=utf8mb4;
        CREATE TABLE c (id INT PRIMARY KEY, a CHAR(255) NULL, b CHAR(255) NULL, \
        c CHAR(255) NULL, d CHAR(255) NULL) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4;";
    let sql = "ALTER TABLE c ADD CONSTRAINT f1 FOREIGN KEY (a, b, c, d) REFERENCES p (a, b, c, d);";
    assert_answer("CHAR", schema, sql, "unknown");
    let schema = "CREATE TABLE p (k VARBINARY(100) NOT NULL, UNIQUE KEY u (k));
        CREATE TABLE c (id INT PRIMARY KEY, k VARBINARY(3073) NULL);";
    let sql = "ALTER TABLE c ADD CONSTRAINT f1 FOREIGN KEY (k) REFERENCES p (k);";
    assert_answer("VARBINARY", schema, sql, "unknown");
}

#[test]
fn on_delete_or_on_update_set_null_on_a_not_null_child_is_unknown() {
    // MySQL refuses it with 1830 before it looks at anything else.
    let schema = "CREATE TABLE users (id BIGINT UNSIGNED AUTO_INCREMENT PRIMARY KEY, \
                  name VARCHAR(100) NOT NULL) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4;
        CREATE TABLE orders (id INT PRIMARY KEY, user_id BIGINT UNSIGNED NOT NULL, \
        amount INT NOT NULL, un VARCHAR(100) NOT NULL, ok BIGINT UNSIGNED NULL) \
        ENGINE=InnoDB DEFAULT CHARSET=utf8mb4;";
    for action in ["ON DELETE SET NULL", "ON UPDATE SET NULL"] {
        for (name, column, parent) in [
            ("a fitting key", "user_id", "id"),
            ("a type mismatch", "amount", "id"),
            ("no parent index", "un", "name"),
        ] {
            let sql = format!(
                "ALTER TABLE orders ADD CONSTRAINT f1 FOREIGN KEY ({column}) \
                 REFERENCES users ({parent}) {action};"
            );
            assert_answer(&format!("{action}, {name}"), schema, &sql, "unknown");
        }
        // A nullable child column has nothing to refuse.
        let sql = format!(
            "ALTER TABLE orders ADD CONSTRAINT f1 FOREIGN KEY (ok) REFERENCES users (id) {action};"
        );
        assert_answer(&format!("{action}, nullable"), schema, &sql, COPY);
    }
}

#[test]
fn a_parent_primary_key_on_a_prefix_is_unknown() {
    let orders = "CREATE TABLE orders (id INT PRIMARY KEY, user_name VARCHAR(100) NULL) \
                  ENGINE=InnoDB DEFAULT CHARSET=utf8mb4;";
    let sql =
        "ALTER TABLE orders ADD CONSTRAINT fk FOREIGN KEY (user_name) REFERENCES users (name);";
    for key in ["name(100)", "name(100), code", "name(10)"] {
        let schema = format!(
            "CREATE TABLE users (name VARCHAR(100) NOT NULL, code INT NOT NULL, \
             PRIMARY KEY ({key})) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4;\n{orders}"
        );
        assert_answer(key, &schema, sql, "unknown");
    }
}

#[test]
fn every_way_of_setting_restrict_fk_on_non_standard_key_makes_the_index_rules_unknown() {
    let no_index = [OC_NAME];
    let plain = ["ALTER TABLE users ADD INDEX idx_users_name (name)", OC_NAME];
    for set in [
        "SET restrict_fk_on_non_standard_key := OFF",
        "SET @@`restrict_fk_on_non_standard_key` = 0",
        "SET PERSIST restrict_fk_on_non_standard_key = 0",
        "SET PERSIST_ONLY restrict_fk_on_non_standard_key = 0",
        "SET @@persist.restrict_fk_on_non_standard_key = 0",
    ] {
        for (name, setup) in [("no index", &no_index[..]), ("plain index", &plain[..])] {
            let sql = format!("{set};\nALTER TABLE orders {NAME};");
            assert_answer(
                &format!("{set}: {name}"),
                &lab_after(setup),
                &sql,
                "unknown",
            );
        }
    }
}

#[test]
fn a_serial_column_hides_an_index_and_makes_the_answer_unknown() {
    // SERIAL is BIGINT UNSIGNED NOT NULL AUTO_INCREMENT UNIQUE.
    assert_unknown(
        "serial parent",
        &[
            "ALTER TABLE users ADD COLUMN k SERIAL",
            "ALTER TABLE orders ADD COLUMN ref BIGINT UNSIGNED NULL",
        ],
        "ADD CONSTRAINT fk_orders_ref FOREIGN KEY (ref) REFERENCES users (k)",
    );
    assert_unknown(
        "serial child, parent without an index",
        &[
            "ALTER TABLE users ADD COLUMN code BIGINT UNSIGNED NULL",
            "ALTER TABLE orders ADD COLUMN k SERIAL",
        ],
        "ADD CONSTRAINT fk_orders_k FOREIGN KEY (k) REFERENCES users (code)",
    );
}

#[test]
fn spatial_columns_and_a_very_long_varchar_child_are_unknown() {
    for kind in ["POINT", "GEOMETRY", "LINESTRING", "POLYGON"] {
        for index in [None, Some("ALTER TABLE users ADD INDEX idx_users_k (k)")] {
            let add_parent = format!("ALTER TABLE users ADD COLUMN k {kind} NULL");
            let add_child = format!("ALTER TABLE orders ADD COLUMN k {kind} NULL");
            let mut setup = vec![add_parent.as_str(), add_child.as_str()];
            setup.extend(index);
            assert_unknown(
                &format!("{kind}, index {index:?}"),
                &setup,
                "ADD CONSTRAINT fk_orders_k FOREIGN KEY (k) REFERENCES users (k)",
            );
        }
    }
    let wide = "ALTER TABLE orders ADD COLUMN user_name VARCHAR(1000) NULL";
    assert_unknown("VARCHAR(1000), no index", &[wide], NAME);
    assert_unknown(
        "VARCHAR(1000), plain index",
        &["ALTER TABLE users ADD INDEX idx_users_name (name)", wide],
        NAME,
    );
}

#[test]
fn a_parent_column_with_its_own_foreign_key_is_unknown_in_every_shape() {
    let tables = |profiles: &str| {
        format!(
            "CREATE TABLE users (id BIGINT UNSIGNED AUTO_INCREMENT PRIMARY KEY) \
             ENGINE=InnoDB DEFAULT CHARSET=utf8mb4;
             {profiles}
             CREATE TABLE orders (id INT PRIMARY KEY, p BIGINT UNSIGNED NULL) \
             ENGINE=InnoDB DEFAULT CHARSET=utf8mb4;"
        )
    };
    let add = "ALTER TABLE orders ADD CONSTRAINT f1 FOREIGN KEY (p) REFERENCES profiles (user_id);";
    let checks_off = format!("SET foreign_key_checks = 0;\n{add}");
    for (name, profiles) in [
        (
            "primary key and foreign key",
            "CREATE TABLE profiles (user_id BIGINT UNSIGNED NOT NULL PRIMARY KEY, \
             CONSTRAINT fp FOREIGN KEY (user_id) REFERENCES users (id)) \
             ENGINE=InnoDB DEFAULT CHARSET=utf8mb4;",
        ),
        (
            "unique and foreign key",
            "CREATE TABLE profiles (user_id BIGINT UNSIGNED NOT NULL, UNIQUE KEY (user_id), \
             CONSTRAINT fp FOREIGN KEY (user_id) REFERENCES users (id)) \
             ENGINE=InnoDB DEFAULT CHARSET=utf8mb4;",
        ),
        // MySQL's own index for the key is not in the schema.
        (
            "foreign key only",
            "CREATE TABLE profiles (user_id BIGINT UNSIGNED NOT NULL, \
             CONSTRAINT fp FOREIGN KEY (user_id) REFERENCES users (id)) \
             ENGINE=InnoDB DEFAULT CHARSET=utf8mb4;",
        ),
    ] {
        let schema = tables(profiles);
        assert_answer(name, &schema, add, "unknown");
        assert_answer(
            &format!("{name}, checks off"),
            &schema,
            &checks_off,
            "unknown",
        );
    }
}

#[test]
fn columns_with_options_kilit_does_not_follow_make_the_answer_unknown() {
    // INVISIBLE is an option kilit keeps but does not follow.
    let tables = |parent: &str, child: &str| {
        format!(
            "CREATE TABLE users (id BIGINT UNSIGNED AUTO_INCREMENT PRIMARY KEY, \
             code BIGINT UNSIGNED NOT NULL {parent}, UNIQUE KEY (code)) \
             ENGINE=InnoDB DEFAULT CHARSET=utf8mb4;
             CREATE TABLE orders (id INT PRIMARY KEY, ref BIGINT UNSIGNED NULL {child}) \
             ENGINE=InnoDB DEFAULT CHARSET=utf8mb4;"
        )
    };
    let by_id = "ALTER TABLE orders ADD CONSTRAINT f1 FOREIGN KEY (ref) REFERENCES users (id);";
    let by_code = "ALTER TABLE orders ADD CONSTRAINT f1 FOREIGN KEY (ref) REFERENCES users (code);";
    assert_answer("plain columns, by id", &tables("", ""), by_id, COPY);
    assert_answer("plain columns, by code", &tables("", ""), by_code, COPY);
    assert_answer("child", &tables("", "INVISIBLE"), by_id, "unknown");
    assert_answer("parent", &tables("INVISIBLE", ""), by_code, "unknown");
}

#[test]
fn json_columns_are_unknown_and_enum_columns_need_identical_definitions() {
    let json = [
        "ALTER TABLE users ADD COLUMN doc JSON NULL",
        "ALTER TABLE orders ADD COLUMN doc JSON NULL",
    ];
    assert_unknown(
        "JSON",
        &json,
        "ADD CONSTRAINT fk_orders_doc FOREIGN KEY (doc) REFERENCES users (doc)",
    );
    let enums = |child: &str| {
        [
            "ALTER TABLE users ADD COLUMN kind ENUM('a', 'b') NOT NULL".to_owned(),
            "ALTER TABLE users ADD UNIQUE INDEX uq_users_kind (kind)".to_owned(),
            format!("ALTER TABLE orders ADD COLUMN kind {child} NULL"),
        ]
    };
    let operation = "ADD CONSTRAINT fk_orders_kind FOREIGN KEY (kind) REFERENCES users (kind)";
    let same = enums("ENUM('a', 'b')");
    let same: Vec<&str> = same.iter().map(String::as_str).collect();
    assert_foreign_key("identical ENUM", &same, operation, COPY);
    let other = enums("ENUM('a', 'b', 'c')");
    let other: Vec<&str> = other.iter().map(String::as_str).collect();
    assert_unknown("another ENUM", &other, operation);
}

#[test]
fn texts_of_another_length_and_charset_or_a_parent_without_the_default_collation_are_unknown() {
    // latin1 VARCHAR(50) against utf8mb4 VARCHAR(100): the measured mismatch
    // is at the same length only.
    assert_unknown(
        "latin1 VARCHAR(50)",
        &[
            UNIQUE_NAME,
            "ALTER TABLE orders ADD COLUMN user_name VARCHAR(50) CHARACTER SET latin1 NULL",
        ],
        NAME,
    );
    // A parent that is not utf8mb4 with its default collation.
    for (name, parent_column) in [
        (
            "latin1 parent",
            "VARCHAR(100) CHARACTER SET latin1 NOT NULL",
        ),
        (
            "utf8mb4_bin parent",
            "VARCHAR(100) COLLATE utf8mb4_bin NOT NULL",
        ),
    ] {
        let modify = format!("ALTER TABLE users MODIFY name {parent_column}");
        assert_unknown(name, &[modify.as_str(), UNIQUE_NAME, OC_NAME], NAME);
    }
    // A latin1 child against a parent that is not utf8mb4 with its default
    // collation is not the measured shape either.
    let latin1 = "ALTER TABLE orders ADD COLUMN user_name VARCHAR(100) CHARACTER SET latin1 NULL";
    assert_unknown(
        "utf8mb4_bin parent, latin1 child",
        &[
            "ALTER TABLE users MODIFY name VARCHAR(100) COLLATE utf8mb4_bin NOT NULL",
            UNIQUE_NAME,
            latin1,
        ],
        NAME,
    );
    // CHAR with another charset, either side.
    assert_unknown(
        "CHAR child latin1",
        &[
            UNIQUE_NAME,
            "ALTER TABLE orders ADD COLUMN user_name CHAR(100) CHARACTER SET latin1 NULL",
        ],
        NAME,
    );
    assert_unknown(
        "CHAR parent latin1",
        &[
            "ALTER TABLE users ADD COLUMN code CHAR(100) CHARACTER SET latin1 NULL",
            "ALTER TABLE users ADD UNIQUE INDEX uq_users_code (code)",
            OC_NAME,
        ],
        CODE,
    );
}

#[test]
fn a_two_column_key_whose_columns_differ_only_in_collation_is_unknown() {
    assert_unknown(
        "same types, utf8mb4_bin on the second",
        &[
            "ALTER TABLE users ADD UNIQUE INDEX uq_users_id_name (id, name)",
            OC_REF,
            "ALTER TABLE orders ADD COLUMN user_name VARCHAR(100) COLLATE utf8mb4_bin NULL",
        ],
        TWO,
    );
}

#[test]
fn character_is_char() {
    let setup = |child: &str| {
        [
            UNIQUE_NAME.to_owned(),
            format!("ALTER TABLE orders ADD COLUMN user_name {child} NULL"),
        ]
    };
    let same = setup("CHARACTER(100)");
    let same: Vec<&str> = same.iter().map(String::as_str).collect();
    assert_foreign_key("CHARACTER(100)", &same, NAME, COPY);
    let other = setup("CHARACTER(100) CHARACTER SET latin1");
    let other: Vec<&str> = other.iter().map(String::as_str).collect();
    assert_unknown("CHARACTER(100) latin1", &other, NAME);
}

// Step 9a: the storage engine. kilit reads ENGINE from the schema and from
// CREATE TABLE in the input. A MyISAM table gets the answers the lab
// measured on MyISAM tables (the `myisam-*` scenarios, both servers gave the
// same result in every variant) and unknown for everything else; any other
// engine is unknown; InnoDB, and no ENGINE written, keep the InnoDB answers.

const FAILS_1071: &str = "fails with error 1071";
const FAILS_1845: &str = "fails with error 1845";

/// The lab's tables with `orders` as MyISAM, after `setup`.
fn myisam_lab_after(setup: &[&str]) -> String {
    let mut sql = lab_tables(Some("MyISAM"));
    for statement in setup {
        sql.push_str(statement);
        sql.push_str(";\n");
    }
    sql
}

/// What kilit says about `ALTER TABLE orders <operation>` on the lab's
/// tables with a MyISAM `orders`, after `setup`: on MySQL 8.0.46, then on
/// 8.4.13.
fn on_myisam_orders(setup: &[&str], operation: &str) -> [String; 2] {
    on_both(
        &myisam_lab_after(setup),
        &format!("ALTER TABLE orders {operation};"),
    )
}

/// The same answer on both servers.
fn assert_myisam(name: &str, setup: &[&str], operation: &str, answer: &str) {
    assert_eq!(
        on_myisam_orders(setup, operation),
        [answer, answer],
        "{name}"
    );
}

/// What kilit says about every statement of `sql` on both servers.
fn all_on_both(schema: &str, sql: &str) -> [Vec<String>; 2] {
    [on_mysql_80(schema, sql), on_mysql_84(schema, sql)]
}

const MYISAM_ADD_FK: &str =
    "ADD CONSTRAINT fk_orders_user FOREIGN KEY (user_id) REFERENCES users (id)";

#[test]
fn the_lab_makes_only_orders_a_myisam_table_for_a_myisam_scenario() {
    let myisam_text = lab_tables(Some("MyISAM"));
    assert_eq!(myisam_text.matches("ENGINE=MyISAM").count(), 1);
    assert_eq!(myisam_text.matches("ENGINE=InnoDB").count(), 1);

    let innodb = Schema::from_sql(&lab_tables(None));
    let myisam = Schema::from_sql(&myisam_text);
    let (TableState::Known(innodb_orders), TableState::Known(myisam_orders)) =
        (innodb.table("orders"), myisam.table("orders"))
    else {
        panic!("orders is not known");
    };
    assert_eq!(innodb_orders.engine, Some(Engine::InnoDb));
    assert_eq!(myisam_orders.engine, Some(Engine::MyIsam));
    assert_eq!(myisam_orders.columns, innodb_orders.columns);
    assert_eq!(myisam_orders.primary_key, innodb_orders.primary_key);
    assert_eq!(myisam_orders.charset, innodb_orders.charset);
    let (TableState::Known(innodb_users), TableState::Known(myisam_users)) =
        (innodb.table("users"), myisam.table("users"))
    else {
        panic!("users is not known");
    };
    assert_eq!(innodb_users, myisam_users);
    assert_eq!(myisam_users.engine, Some(Engine::InnoDb));
}

#[test]
fn a_myisam_scenario_of_the_lab_starts_from_a_myisam_orders() {
    for results in Knowledge::built_in().servers() {
        for scenario in results.scenarios() {
            let schema = lab_schema(scenario);
            let TableState::Known(orders) = schema.table("orders") else {
                panic!("{}: orders is not known", scenario.id);
            };
            let expected = match scenario.engine.as_deref() {
                Some("MyISAM") => Engine::MyIsam,
                None => Engine::InnoDb,
                Some(other) => panic!("{}: engine {other}", scenario.id),
            };
            assert_eq!(orders.engine, Some(expected), "{}", scenario.id);
            // The scenarios that are about MyISAM say so in their id.
            assert_eq!(
                scenario.engine.is_some(),
                scenario.id.starts_with("myisam-"),
                "{}",
                scenario.id
            );
            let TableState::Known(users) = schema.table("users") else {
                panic!("{}: users is not known", scenario.id);
            };
            assert_eq!(users.engine, Some(Engine::InnoDb), "{}", scenario.id);
        }
    }
}

#[test]
fn a_myisam_exception_that_kilit_cannot_see_shows_up_as_a_note() {
    for results in Knowledge::built_in().servers() {
        let server = results.server.name();
        for case in MYISAM_CASES {
            for exception in case.exceptions {
                if !exception.conditions.contains(&Condition::Hidden) {
                    continue;
                }
                let first = exception.scenarios[0];
                for id in exception.scenarios {
                    let scenario = results.scenario(id).unwrap();
                    let schema = lab_schema(scenario);
                    match predict(scenario, Variant::SERVER_DEFAULT, &server, &schema) {
                        Prediction::Measured {
                            case: found, notes, ..
                        } => {
                            assert_eq!(found, case.id, "{id}");
                            assert!(
                                notes.iter().any(|note| note.contains(first)),
                                "{first} is missing from the notes of {id}: {notes:?}"
                            );
                        }
                        other => panic!("{id} on {server}: {other:?}"),
                    }
                }
            }
        }
    }
}

#[test]
fn a_known_myisam_table_copies_for_the_changes_the_lab_measured() {
    // The myisam-* scenarios: COPY, writes wait.
    assert_myisam("add column", &[], "ADD COLUMN c INT NULL", COPY);
    assert_myisam(
        "add column not null default",
        &[],
        "ADD COLUMN c INT NOT NULL DEFAULT 0",
        COPY,
    );
    assert_myisam("drop column", &[], "DROP COLUMN note", COPY);
    assert_myisam("int to bigint", &[], "MODIFY amount BIGINT NOT NULL", COPY);
    assert_myisam("add index", &[], "ADD INDEX idx_amount (amount)", COPY);
    assert_myisam(
        "add unique index",
        &[],
        "ADD UNIQUE INDEX uq_amount (amount)",
        COPY,
    );
    assert_myisam(
        "add fulltext index",
        &[],
        "ADD FULLTEXT INDEX ft_note (note)",
        COPY,
    );
    assert_myisam(
        "drop index",
        &["ALTER TABLE orders ADD INDEX idx_amount (amount)"],
        "DROP INDEX idx_amount",
        COPY,
    );
}

#[test]
fn renaming_a_column_or_setting_a_default_on_a_known_myisam_table_is_instant() {
    assert_myisam("rename column", &[], "RENAME COLUMN note TO memo", INSTANT);
    assert_myisam(
        "set default",
        &[],
        "ALTER COLUMN note SET DEFAULT 'x'",
        INSTANT,
    );
}

#[test]
fn forcing_a_variant_the_myisam_table_cannot_do_fails_with_1845() {
    // ALGORITHM=INSTANT, ALGORITHM=INPLACE, ALGORITHM=INPLACE with LOCK=NONE
    // and LOCK=NONE all gave 1845 on both servers for these changes.
    let indexed = ["ALTER TABLE orders ADD INDEX idx_amount (amount)"];
    for (name, setup, operation) in [
        ("add column", &[][..], "ADD COLUMN c INT NULL"),
        ("drop column", &[][..], "DROP COLUMN note"),
        ("int to bigint", &[][..], "MODIFY amount BIGINT NOT NULL"),
        ("add index", &[][..], "ADD INDEX idx_amount (amount)"),
        ("add fulltext", &[][..], "ADD FULLTEXT INDEX ft_note (note)"),
        ("drop index", &indexed[..], "DROP INDEX idx_amount"),
        ("add foreign key", &[][..], MYISAM_ADD_FK),
    ] {
        for forced in [
            "ALGORITHM=INSTANT",
            "ALGORITHM=INPLACE",
            "ALGORITHM=INPLACE, LOCK=NONE",
            "LOCK=NONE",
        ] {
            assert_myisam(
                &format!("{name}, {forced}"),
                setup,
                &format!("{operation}, {forced}"),
                FAILS_1845,
            );
        }
    }
}

#[test]
fn an_index_on_a_myisam_table_is_checked_against_the_1000_byte_limit_in_utf8mb4() {
    let varchar =
        |length: u32| format!("ALTER TABLE orders ADD COLUMN code VARCHAR({length}) NULL");
    // 250 characters of utf8mb4 are 1000 bytes: accepted.
    let at_limit = varchar(250);
    assert_myisam(
        "index at the limit",
        &[at_limit.as_str()],
        "ADD INDEX idx_code (code)",
        COPY,
    );
    assert_myisam(
        "unique index at the limit",
        &[at_limit.as_str()],
        "ADD UNIQUE INDEX uq_code (code)",
        COPY,
    );
    // 251 characters are 1004 bytes: 1071.
    let over = varchar(251);
    assert_myisam(
        "index over the limit",
        &[over.as_str()],
        "ADD INDEX idx_code (code)",
        FAILS_1071,
    );
    assert_myisam(
        "unique index over the limit",
        &[over.as_str()],
        "ADD UNIQUE INDEX uq_code (code)",
        FAILS_1071,
    );
    // The lab's own email, VARCHAR(255): 1020 bytes.
    assert_myisam(
        "unique index on email",
        &[],
        "ADD UNIQUE INDEX uq_email (email)",
        FAILS_1071,
    );
    assert_myisam(
        "index on email",
        &[],
        "ADD INDEX idx_email (email)",
        FAILS_1071,
    );
}

#[test]
fn a_myisam_index_that_fails_with_1071_leaves_the_table_as_it_was() {
    // If the unique index stayed, dropping its column would be an
    // unmeasured shape: unknown. The table stays as it was: COPY.
    let sql = "ALTER TABLE orders ADD COLUMN code VARCHAR(251) NULL;
               ALTER TABLE orders ADD UNIQUE INDEX uq_code (code);
               ALTER TABLE orders DROP COLUMN code;";
    for answers in all_on_both(&myisam_lab_after(&[]), sql) {
        assert_eq!(answers, [COPY, FAILS_1071, COPY]);
    }
}

#[test]
fn a_myisam_key_in_another_character_set_or_of_another_kind_is_unknown() {
    let latin1 = "ALTER TABLE orders ADD COLUMN code VARCHAR(100) CHARACTER SET latin1 NULL";
    assert_myisam(
        "latin1 VARCHAR index",
        &[latin1],
        "ADD INDEX idx_code (code)",
        "unknown",
    );
    assert_myisam(
        "latin1 VARCHAR unique index",
        &[latin1],
        "ADD UNIQUE INDEX uq_code (code)",
        "unknown",
    );
    assert_myisam(
        "CHAR index",
        &["ALTER TABLE orders ADD COLUMN code CHAR(10) NULL"],
        "ADD INDEX idx_code (code)",
        "unknown",
    );
    assert_myisam(
        "prefix index",
        &[],
        "ADD INDEX idx_email (email(100))",
        "unknown",
    );
    assert_myisam(
        "two-column index",
        &[],
        "ADD INDEX idx_two (user_id, amount)",
        "unknown",
    );
    // A table that is not utf8mb4: the byte rule is measured for utf8mb4
    // only, whatever the column's own length.
    let schema = "CREATE TABLE t (id INT PRIMARY KEY, code VARCHAR(100) NULL) \
                  ENGINE=MyISAM DEFAULT CHARSET=latin1;";
    assert_answer(
        "latin1 table",
        schema,
        "ALTER TABLE t ADD INDEX idx_code (code);",
        "unknown",
    );
    // The column names its own character set: utf8mb4.
    let schema = "CREATE TABLE t (id INT PRIMARY KEY, \
                  code VARCHAR(250) CHARACTER SET utf8mb4 NULL) \
                  ENGINE=MyISAM DEFAULT CHARSET=latin1;";
    assert_answer(
        "utf8mb4 column in a latin1 table",
        schema,
        "ALTER TABLE t ADD INDEX idx_code (code);",
        COPY,
    );
}

#[test]
fn a_change_the_lab_did_not_measure_on_myisam_is_unknown() {
    assert_myisam(
        "lengthen a VARCHAR",
        &[],
        "MODIFY note VARCHAR(100) NULL",
        "unknown",
    );
    assert_myisam(
        "INT to SMALLINT",
        &[],
        "MODIFY amount SMALLINT NOT NULL",
        "unknown",
    );
    assert_myisam(
        "INT to BIGINT UNSIGNED",
        &[],
        "MODIFY amount BIGINT UNSIGNED NOT NULL",
        "unknown",
    );
    assert_myisam("drop primary key", &[], "DROP PRIMARY KEY", "unknown");
    assert_myisam(
        "add check",
        &[],
        "ADD CONSTRAINT chk_amount CHECK (amount > 0)",
        "unknown",
    );
    assert_myisam(
        "expression default",
        &[],
        "ADD COLUMN token CHAR(36) NOT NULL DEFAULT (UUID())",
        "unknown",
    );
    assert_myisam(
        "two-column foreign key",
        &[],
        "ADD CONSTRAINT fk_two FOREIGN KEY (user_id, amount) REFERENCES users (id, name)",
        "unknown",
    );
    assert_answer(
        "add primary key",
        "CREATE TABLE t (a INT NOT NULL, b INT) ENGINE=MyISAM DEFAULT CHARSET=utf8mb4;",
        "ALTER TABLE t ADD PRIMARY KEY (a);",
        "unknown",
    );
}

#[test]
fn a_myisam_table_with_a_fulltext_index_or_an_indexed_column_is_unknown_where_innodb_would_know() {
    // InnoDB: a FULLTEXT index turns a later ADD COLUMN into a COPY. The lab
    // measured nothing like it on MyISAM.
    assert_myisam(
        "ADD COLUMN after a FULLTEXT index",
        &["ALTER TABLE orders ADD FULLTEXT INDEX ft_note (note)"],
        "ADD COLUMN c INT NULL",
        "unknown",
    );
    // A column in a two-column index, or in a unique one.
    assert_myisam(
        "drop a column of a two-column index",
        &["ALTER TABLE orders ADD INDEX idx_two (user_id, amount)"],
        "DROP COLUMN amount",
        "unknown",
    );
    assert_myisam(
        "drop a column of a unique index",
        &["ALTER TABLE orders ADD UNIQUE INDEX uq_amount (amount)"],
        "DROP COLUMN amount",
        "unknown",
    );
    // A plain index on that column alone is the measured shape.
    assert_myisam(
        "drop a column of a plain index",
        &["ALTER TABLE orders ADD INDEX idx_amount (amount)"],
        "DROP COLUMN amount",
        COPY,
    );
}

#[test]
fn a_foreign_key_on_a_myisam_table_copies_and_never_fails_with_3780_or_a_missing_index() {
    // The MySQL servers accept it and keep no key: INT against BIGINT
    // UNSIGNED (3780 on InnoDB), a parent column without an index (1822 on
    // 8.0.46, 6125 on 8.4.13 on InnoDB).
    assert_myisam("matching types", &[], MYISAM_ADD_FK, COPY);
    assert_myisam(
        "INT against BIGINT UNSIGNED",
        &["ALTER TABLE orders ADD COLUMN ref INT NULL"],
        "ADD CONSTRAINT fk_orders_ref FOREIGN KEY (ref) REFERENCES users (id)",
        COPY,
    );
    assert_myisam(
        "parent column without an index",
        &["ALTER TABLE orders ADD COLUMN user_name VARCHAR(100) NULL"],
        "ADD CONSTRAINT fk_orders_user_name FOREIGN KEY (user_name) REFERENCES users (name)",
        COPY,
    );
    assert_myisam(
        "column that has an index already",
        &["ALTER TABLE orders ADD INDEX idx_user (user_id)"],
        MYISAM_ADD_FK,
        COPY,
    );
    // The same shapes on InnoDB, as the control.
    assert_foreign_key(
        "InnoDB, INT against BIGINT UNSIGNED",
        &["ALTER TABLE orders ADD COLUMN ref INT NULL"],
        "ADD CONSTRAINT fk_orders_ref FOREIGN KEY (ref) REFERENCES users (id)",
        FAILS_3780,
    );
}

#[test]
fn the_key_a_myisam_foreign_key_adds_over_1000_bytes_is_unknown() {
    // Measured by hand (1071); not a lab scenario, so kilit does not
    // answer it.
    assert_myisam(
        "foreign key on email",
        &[],
        "ADD CONSTRAINT fk_orders_email FOREIGN KEY (email) REFERENCES users (name)",
        "unknown",
    );
}

#[test]
fn after_a_foreign_key_on_a_myisam_table_dropping_its_column_copies_and_does_not_fail_with_1828() {
    let sql = format!(
        "ALTER TABLE orders {MYISAM_ADD_FK};
         ALTER TABLE orders DROP COLUMN user_id;"
    );
    for answers in all_on_both(&myisam_lab_after(&[]), &sql) {
        assert_eq!(answers, [COPY, COPY]);
    }
    // On InnoDB the same two statements fail with 1828.
    assert_eq!(
        on_both(
            &lab_after(&[]),
            &format!(
                "ALTER TABLE orders {MYISAM_ADD_FK};
                 ALTER TABLE orders DROP COLUMN user_id;"
            )
        ),
        ["fails with error 1828", "fails with error 1828"]
    );
}

#[test]
fn dropping_the_index_mysql_added_for_a_myisam_foreign_key_gets_the_drop_index_answer() {
    // A named constraint: the index has the constraint's name.
    let named = format!(
        "ALTER TABLE orders {MYISAM_ADD_FK};
         ALTER TABLE orders DROP INDEX fk_orders_user;"
    );
    // No name: the index has the column's name.
    let unnamed = "ALTER TABLE orders ADD FOREIGN KEY (user_id) REFERENCES users (id);
                   ALTER TABLE orders DROP INDEX user_id;";
    for sql in [named.as_str(), unnamed] {
        for answers in all_on_both(&myisam_lab_after(&[]), sql) {
            assert_eq!(answers, [COPY, COPY], "{sql}");
        }
    }
}

#[test]
fn dropping_a_foreign_key_of_a_myisam_table_copies_whatever_its_name() {
    // The lab measured DROP FOREIGN KEY with any name.
    assert_myisam("a name that was added", &[], "DROP FOREIGN KEY fk_x", COPY);
    let sql = format!(
        "ALTER TABLE orders {MYISAM_ADD_FK};
         ALTER TABLE orders DROP FOREIGN KEY fk_orders_user;"
    );
    for answers in all_on_both(&myisam_lab_after(&[]), &sql) {
        assert_eq!(answers, [COPY, COPY]);
    }
}

#[test]
fn a_foreign_key_clause_in_create_table_on_myisam_adds_an_index_and_no_key() {
    let tables = lab_after(&[]);
    // Named: the index has the constraint's name. Unnamed: the column's.
    let named = "CREATE TABLE child (id INT PRIMARY KEY, user_id BIGINT UNSIGNED NOT NULL, \
                 CONSTRAINT fk_child_user FOREIGN KEY (user_id) REFERENCES users (id)) \
                 ENGINE=MyISAM DEFAULT CHARSET=utf8mb4;";
    let unnamed = "CREATE TABLE child (id INT PRIMARY KEY, user_id BIGINT UNSIGNED NOT NULL, \
                   FOREIGN KEY (user_id) REFERENCES users (id)) \
                   ENGINE=MyISAM DEFAULT CHARSET=utf8mb4;";
    for (create, index) in [(named, "fk_child_user"), (unnamed, "user_id")] {
        let drop_index = format!("{create}\nALTER TABLE child DROP INDEX {index};");
        let drop_column = format!("{create}\nALTER TABLE child DROP COLUMN user_id;");
        for answers in all_on_both(&tables, &drop_index) {
            assert_eq!(answers, ["not checked", COPY], "{create}");
        }
        // The key is not kept: no 1828.
        for answers in all_on_both(&tables, &drop_column) {
            assert_eq!(answers, ["not checked", COPY], "{create}");
        }
    }
    // The control: the same table on InnoDB keeps the key.
    let innodb = named.replace("ENGINE=MyISAM", "ENGINE=InnoDB");
    for answers in all_on_both(
        &tables,
        &format!("{innodb}\nALTER TABLE child DROP COLUMN user_id;"),
    ) {
        assert_eq!(answers, ["not checked", "fails with error 1828"]);
    }
}

#[test]
fn a_column_level_references_on_myisam_is_ignored_and_adds_no_key() {
    let sql = "CREATE TABLE child (id INT PRIMARY KEY, \
               user_id BIGINT UNSIGNED NOT NULL REFERENCES users (id)) \
               ENGINE=MyISAM DEFAULT CHARSET=utf8mb4;
               ALTER TABLE child DROP COLUMN user_id;";
    for answers in all_on_both(&lab_after(&[]), sql) {
        assert_eq!(answers, ["not checked", COPY]);
    }
}

#[test]
fn a_myisam_foreign_key_that_kilit_cannot_place_loses_the_table() {
    // The name of the constraint is the name of an index already: kilit does
    // not know which index MySQL keeps, so the foreign key and what follows
    // are unknown.
    let sql = format!(
        "ALTER TABLE orders ADD INDEX fk_orders_user (amount);
         ALTER TABLE orders {MYISAM_ADD_FK};
         ALTER TABLE orders ADD COLUMN c INT NULL;"
    );
    for server in [MYSQL_80, MYSQL_84] {
        let predictions = predictions_on(server, &myisam_lab_after(&[]), &sql);
        assert_eq!(predictions.len(), 3, "{server}");
        for prediction in &predictions[1..] {
            match prediction {
                Prediction::Unknown { reason } => {
                    assert!(reason.contains("MyISAM"), "{server}: {reason}")
                }
                other => panic!("{server}: {other:?}"),
            }
        }
    }
}

#[test]
fn an_innodb_child_of_a_myisam_or_unknown_parent_is_unknown() {
    // MySQL refuses it with 1824 while foreign_key_checks is on.
    let myisam_parent = LAB_TABLES.replacen("ENGINE=InnoDB", "ENGINE=MyISAM", 1);
    let memory_parent = LAB_TABLES.replacen("ENGINE=InnoDB", "ENGINE=MEMORY", 1);
    for (name, schema) in [("MyISAM", &myisam_parent), ("MEMORY", &memory_parent)] {
        let matching = format!("ALTER TABLE orders {MYISAM_ADD_FK};");
        assert_answer(name, schema, &matching, "unknown");
        let mismatch = "ALTER TABLE orders ADD COLUMN ref INT NULL;
             ALTER TABLE orders ADD CONSTRAINT fk_orders_ref FOREIGN KEY (ref) REFERENCES users (id);";
        assert_answer(name, schema, mismatch, "unknown");
    }
    // An InnoDB parent keeps its answer.
    assert_answer(
        "InnoDB",
        LAB_TABLES,
        &format!("ALTER TABLE orders {MYISAM_ADD_FK};"),
        COPY,
    );
}

/// `orders` of the lab's tables with the engine `engine`.
fn lab_with_engine(engine: &str) -> String {
    let at = LAB_TABLES.find("CREATE TABLE orders").unwrap();
    let (users, orders) = LAB_TABLES.split_at(at);
    format!(
        "{users}{}",
        orders.replacen("ENGINE=InnoDB", &format!("ENGINE={engine}"), 1)
    )
}

#[test]
fn every_change_to_a_memory_archive_or_unknown_engine_table_is_unknown() {
    let operations = [
        "ADD COLUMN c INT NULL",
        "DROP COLUMN note",
        "RENAME COLUMN note TO memo",
        "ALTER COLUMN note SET DEFAULT 'x'",
        "MODIFY amount BIGINT NOT NULL",
        "ADD INDEX idx_amount (amount)",
        "ADD UNIQUE INDEX uq_amount (amount)",
        "ADD FULLTEXT INDEX ft_note (note)",
        "DROP INDEX idx_amount",
        MYISAM_ADD_FK,
        "DROP FOREIGN KEY fk_orders_user",
        "ADD COLUMN c INT NULL, ALGORITHM=INPLACE",
        "ADD COLUMN c INT NULL, ALGORITHM=INSTANT, LOCK=NONE",
    ];
    for engine in ["MEMORY", "ARCHIVE", "NoSuchEngine"] {
        let schema = lab_with_engine(engine);
        for operation in operations {
            let sql = format!("ALTER TABLE orders {operation};");
            for server in [MYSQL_80, MYSQL_84] {
                let predictions = predictions_on(server, &schema, &sql);
                match &predictions[..] {
                    [Prediction::Unknown { reason }] => assert!(
                        reason.contains(engine),
                        "{operation} on {engine}: the reason does not name the engine: {reason}"
                    ),
                    other => panic!("{operation} on {engine} on {server}: {other:?}"),
                }
            }
        }
    }
}

#[test]
fn the_engine_of_a_table_created_in_the_input_decides_its_answers() {
    let create = |options: &str| {
        format!(
            "CREATE TABLE t (id INT PRIMARY KEY, a INT) {options};
             ALTER TABLE t ADD COLUMN c INT NULL;"
        )
    };
    // ENGINE in the spellings mysqldump, Laravel and a hand-written
    // migration use. A repeated ENGINE: the last one counts.
    for options in [
        "ENGINE=MyISAM",
        "engine = MyISAM",
        "engine=myisam",
        "ENGINE=MYISAM",
        "ENGINE MyISAM",
        "ENGINE=`MyISAM`",
        "ENGINE=MyISAM, DEFAULT CHARSET=utf8mb4",
        "DEFAULT CHARSET=utf8mb4 ENGINE=MyISAM",
        "ENGINE=MyISAM AUTO_INCREMENT=3 DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_0900_ai_ci",
        "default character set utf8mb4 collate 'utf8mb4_unicode_ci' engine = MyISAM",
        "ENGINE=InnoDB ENGINE=MyISAM",
    ] {
        assert_answer(options, "", &create(options), COPY);
    }
    for options in [
        "",
        "ENGINE=InnoDB",
        "engine = InnoDB",
        "engine=innodb",
        "ENGINE `InnoDB`",
        "ENGINE=MyISAM ENGINE=InnoDB",
        "default character set utf8mb4 collate 'utf8mb4_unicode_ci' engine = InnoDB",
    ] {
        assert_answer(options, "", &create(options), INSTANT);
    }
    assert_answer(
        "the last ENGINE is MEMORY",
        "",
        &create("ENGINE=MyISAM ENGINE=MEMORY"),
        "unknown",
    );
    for options in ["ENGINE=MEMORY", "ENGINE=ARCHIVE", "engine = csv"] {
        assert_answer(options, "", &create(options), "unknown");
    }
}

#[test]
fn a_quoted_engine_name_that_the_parser_cannot_read_makes_the_table_unknown() {
    // MySQL accepts ENGINE='MyISAM'; kilit cannot read the CREATE TABLE and
    // must not give the InnoDB answer afterwards.
    let sql = |options: &str| {
        format!(
            "CREATE TABLE t (id INT PRIMARY KEY, a INT) {options};
             ALTER TABLE t ADD COLUMN c INT NULL;"
        )
    };
    assert_answer("single quotes", "", &sql("ENGINE='MyISAM'"), "unknown");
    assert_answer("lowercase", "", &sql("ENGINE = 'myisam'"), "unknown");
    assert_answer("double quotes", "", &sql("ENGINE=\"MyISAM\""), "unknown");
    // The reason names the engine the scan found.
    for server in [MYSQL_80, MYSQL_84] {
        for options in ["ENGINE='MyISAM'", "ENGINE=\"MyISAM\""] {
            match predictions_on(server, "", &sql(options)).last().unwrap() {
                Prediction::Unknown { reason } => {
                    assert!(reason.contains("MyISAM"), "{options}: {reason}")
                }
                other => panic!("{options} on {server}: {other:?}"),
            }
        }
    }
}

#[test]
fn a_table_copied_with_like_from_a_myisam_table_is_lost_and_unknown() {
    // The copy has the engine of the table after LIKE.
    let sql = "CREATE TABLE orders_copy LIKE orders;
               ALTER TABLE orders_copy ADD COLUMN c INT NULL;";
    for server in [MYSQL_80, MYSQL_84] {
        let predictions = predictions_on(server, &myisam_lab_after(&[]), sql);
        match predictions.last().unwrap() {
            Prediction::Unknown { reason } => {
                assert!(reason.contains("MyISAM"), "{server}: {reason}")
            }
            other => panic!("{server}: {other:?}"),
        }
    }
    // The control: a copy of an InnoDB table keeps today's answer.
    assert_answer("InnoDB copy", LAB_TABLES, sql, INSTANT);
}

#[test]
fn a_table_created_after_default_storage_engine_was_set_has_no_known_engine() {
    let create = "CREATE TABLE t (id INT PRIMARY KEY, a INT) DEFAULT CHARSET=utf8mb4;
                  ALTER TABLE t ADD COLUMN c INT NULL;";
    for set in [
        "SET default_storage_engine = MyISAM",
        "SET SESSION default_storage_engine = MyISAM",
        "SET GLOBAL default_storage_engine = MyISAM",
        "SET PERSIST default_storage_engine = MyISAM",
        "SET @@default_storage_engine = MyISAM",
        "SET @@global.default_storage_engine = MyISAM",
        "SET default_storage_engine := 'MyISAM'",
        "set DEFAULT_STORAGE_ENGINE = myisam",
    ] {
        assert_answer(set, "", &format!("{set};\n{create}"), "unknown");
    }
    // The engine the CREATE TABLE writes decides.
    let with = |engine: &str| {
        format!(
            "SET default_storage_engine = MyISAM;
             CREATE TABLE t (id INT PRIMARY KEY, a INT) ENGINE={engine};
             ALTER TABLE t ADD COLUMN c INT NULL;"
        )
    };
    assert_answer("ENGINE=InnoDB", "", &with("InnoDB"), INSTANT);
    assert_answer("ENGINE=MyISAM", "", &with("MyISAM"), COPY);
    // A table created before the setting is not affected by it.
    assert_answer(
        "created before",
        "",
        "CREATE TABLE t (id INT PRIMARY KEY, a INT);
         SET default_storage_engine = MyISAM;
         ALTER TABLE t ADD COLUMN c INT NULL;",
        INSTANT,
    );
}

#[test]
fn a_myisam_table_from_a_mysqldump_schema_is_known() {
    let schema = "CREATE TABLE `orders` (
  `id` bigint unsigned NOT NULL AUTO_INCREMENT,
  `user_id` bigint unsigned NOT NULL,
  `amount` int NOT NULL,
  `email` varchar(255) DEFAULT NULL,
  `note` varchar(50) DEFAULT NULL,
  PRIMARY KEY (`id`),
  KEY `idx_amount` (`amount`)
) ENGINE=MyISAM AUTO_INCREMENT=3 DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_0900_ai_ci;
";
    assert_eq!(
        Schema::from_sql(schema).tables().next().unwrap().engine,
        Some(Engine::MyIsam)
    );
    for (sql, answer) in [
        ("alter table `orders` add `coupon` varchar(255) null", COPY),
        ("alter table `orders` drop `note`", COPY),
        (
            "alter table `orders` rename column `note` to `memo`",
            INSTANT,
        ),
        ("alter table `orders` drop index `idx_amount`", COPY),
        (
            "alter table `orders` add index `orders_user_id_index`(`user_id`)",
            COPY,
        ),
        (
            "alter table `orders` add unique `orders_email_unique`(`email`)",
            FAILS_1071,
        ),
        (
            "alter table `orders` modify `amount` smallint not null",
            "unknown",
        ),
    ] {
        assert_answer(sql, schema, sql, answer);
    }
}

#[test]
fn a_new_myisam_table_follows_the_measurements_for_myisam() {
    let create = "CREATE TABLE t (id BIGINT UNSIGNED PRIMARY KEY, code VARCHAR(250) NOT NULL, \
                  big VARCHAR(255) NULL, amount INT NOT NULL) \
                  ENGINE=MyISAM DEFAULT CHARSET=utf8mb4;";
    for (operation, answer) in [
        // A measured success.
        ("ADD COLUMN c INT NULL", COPY),
        ("ADD INDEX idx_code (code)", COPY),
        ("RENAME COLUMN amount TO total", INSTANT),
        // A measured failure.
        ("ADD INDEX idx_big (big)", FAILS_1071),
        ("ADD UNIQUE INDEX uq_big (big)", FAILS_1071),
        // Not measured on MyISAM.
        ("MODIFY amount SMALLINT NOT NULL", "unknown"),
        ("ADD INDEX idx_two (code, amount)", "unknown"),
    ] {
        assert_answer(
            operation,
            "",
            &format!("{create}\nALTER TABLE t {operation};"),
            answer,
        );
    }
}

#[test]
fn a_lost_myisam_table_is_unknown_and_a_lost_innodb_table_keeps_its_answer() {
    // A change kilit cannot follow loses the table. (B1b: dropping a column
    // the table does not have no longer does: the table stays known.)
    let lose = "ALTER TABLE orders AUTO_INCREMENT = 5;";
    let then = "ALTER TABLE orders ADD COLUMN c INT NULL;";
    let myisam = all_on_both(&myisam_lab_after(&[]), &format!("{lose}\n{then}"));
    for answers in myisam {
        assert_eq!(answers.last().unwrap(), "unknown", "{answers:?}");
    }
    let innodb = all_on_both(&lab_after(&[]), &format!("{lose}\n{then}"));
    for answers in innodb {
        assert_eq!(answers.last().unwrap(), INSTANT, "{answers:?}");
    }
}

#[test]
fn changing_the_engine_with_alter_table_decides_what_follows() {
    let then = "ALTER TABLE orders ADD COLUMN c INT NULL;";
    for (engine, schema, answer) in [
        // To InnoDB: today's answer.
        ("ENGINE=InnoDB", lab_after(&[]), INSTANT),
        ("ENGINE InnoDB", lab_after(&[]), INSTANT),
        ("ENGINE=InnoDB", myisam_lab_after(&[]), INSTANT),
        ("ENGINE=MyISAM, ENGINE=InnoDB", lab_after(&[]), INSTANT),
        // To MyISAM (step 11b: the table stays known, with the new engine):
        // the later statement gets the MyISAM answer.
        ("ENGINE=MyISAM", lab_after(&[]), COPY),
        ("ENGINE MyISAM", lab_after(&[]), COPY),
        ("engine = myisam", lab_after(&[]), COPY),
        ("ENGINE=InnoDB, ENGINE=MyISAM", lab_after(&[]), COPY),
        // A quoted engine is not read, and somewhere else is not measured.
        ("ENGINE='MyISAM'", lab_after(&[]), "unknown"),
        ("ENGINE=MEMORY", lab_after(&[]), "unknown"),
    ] {
        let sql = format!("ALTER TABLE orders {engine};\n{then}");
        let answers = all_on_both(&schema, &sql);
        for answers in answers {
            assert_eq!(answers.len(), 2);
            // The statement itself is not something kilit can answer.
            assert_eq!(answers[0], "unknown", "{engine}");
            assert_eq!(answers[1], answer, "{engine}");
        }
    }
}

#[test]
fn a_change_of_engine_keeps_a_table_without_foreign_keys_known_and_loses_one_with_them() {
    // Step 11b: without a foreign key the table stays known, with the engine
    // the statement writes.
    let innodb = "CREATE TABLE orders (id INT PRIMARY KEY) ENGINE=InnoDB;
                  ALTER TABLE orders ENGINE=MyISAM;";
    assert!(matches!(
        Schema::from_sql(innodb).table("orders"),
        TableState::Known(table) if table.engine == Some(Engine::MyIsam)
    ));
    let myisam = "CREATE TABLE orders (id INT PRIMARY KEY) ENGINE=MyISAM;
                  ALTER TABLE orders ENGINE=InnoDB;";
    assert!(matches!(
        Schema::from_sql(myisam).table("orders"),
        TableState::Known(table) if table.engine == Some(Engine::InnoDb)
    ));
    // A table with a foreign key, or one that a foreign key points at, is
    // lost by a change to MyISAM.
    let keyed = "CREATE TABLE users (id INT PRIMARY KEY) ENGINE=InnoDB;
                 CREATE TABLE orders (id INT PRIMARY KEY, user_id INT, KEY (user_id),
                     CONSTRAINT fk_orders_user FOREIGN KEY (user_id) REFERENCES users (id))
                     ENGINE=InnoDB;";
    for table in ["orders", "users"] {
        let schema = Schema::from_sql(&format!("{keyed}ALTER TABLE {table} ENGINE=MyISAM;"));
        assert!(
            matches!(schema.table(table), TableState::Lost { .. }),
            "{table}"
        );
    }
}

#[test]
fn a_missing_table_gets_the_innodb_answer_whatever_its_engine_was() {
    let sql = "ALTER TABLE orders ADD COLUMN c INT NULL;";
    assert_answer("nothing known", "", sql, INSTANT);
    // A MyISAM table that was dropped is missing again.
    assert_answer(
        "dropped",
        "",
        "CREATE TABLE orders (id INT PRIMARY KEY) ENGINE=MyISAM;
         DROP TABLE orders;
         ALTER TABLE orders ADD COLUMN c INT NULL;",
        INSTANT,
    );
}

#[test]
fn innodb_tables_keep_their_answers_when_the_engine_is_read() {
    assert_foreign_key("add column", &[], "ADD COLUMN c INT NULL", INSTANT);
    assert_foreign_key("int to bigint", &[], "MODIFY amount BIGINT NOT NULL", COPY);
    assert_foreign_key(
        "INT against BIGINT UNSIGNED",
        &["ALTER TABLE orders ADD COLUMN ref INT NULL"],
        "ADD CONSTRAINT fk_orders_ref FOREIGN KEY (ref) REFERENCES users (id)",
        FAILS_3780,
    );
    assert_foreign_key("matching foreign key", &[], MYISAM_ADD_FK, COPY);
}

#[test]
fn setting_default_storage_engine_to_innodb_still_leaves_a_new_table_without_a_known_engine() {
    // Decision 1: any form of the setting, before a CREATE TABLE without ENGINE.
    assert_answer(
        "SET to InnoDB",
        "",
        "SET default_storage_engine = InnoDB;
         CREATE TABLE t (id INT PRIMARY KEY, a INT) DEFAULT CHARSET=utf8mb4;
         ALTER TABLE t ADD COLUMN c INT NULL;",
        "unknown",
    );
}

#[test]
fn a_schema_file_is_not_scanned_for_default_storage_engine_so_its_tables_get_the_innodb_answer() {
    // The limitation, as decided: mysqldump writes ENGINE on every table, so
    // no reader looks for the setting in the --schema text. Only the input
    // is scanned.
    let schema = "SET default_storage_engine = MyISAM;
                  CREATE TABLE t (id INT PRIMARY KEY, a INT) DEFAULT CHARSET=utf8mb4;";
    assert_answer(
        "schema file",
        schema,
        "ALTER TABLE t ADD COLUMN c INT NULL;",
        INSTANT,
    );
    // The same statements in the input are scanned.
    assert_answer(
        "input",
        "",
        &format!("{schema}\nALTER TABLE t ADD COLUMN c INT NULL;"),
        "unknown",
    );
}

// Regression tests for what the step 9a review found. The inputs and the
// server results are the review's proofs (p1 to p15).

/// What kilit says about every statement of `sql`, on both servers.
fn assert_every_answer(name: &str, schema: &str, sql: &str, expected: &[&str]) {
    for answers in all_on_both(schema, sql) {
        assert_eq!(answers, expected, "{name}");
    }
}

const REVIEW_USERS_AND_MYISAM_T: &str =
    "CREATE TABLE users (id BIGINT UNSIGNED NOT NULL PRIMARY KEY) ENGINE=InnoDB;
     CREATE TABLE t (id INT NOT NULL PRIMARY KEY, user_id BIGINT UNSIGNED NOT NULL, \
     amount INT NOT NULL) ENGINE=MyISAM DEFAULT CHARSET=utf8mb4;\n";

#[test]
fn a_myisam_foreign_key_that_names_its_own_index_is_unknown_and_loses_the_table() {
    // p2: the index MySQL adds takes the name after FOREIGN KEY, not the
    // column's, so DROP INDEX user_id must not get the measured answer.
    assert_every_answer(
        "ALTER TABLE, index name ix_u",
        "",
        &format!(
            "{REVIEW_USERS_AND_MYISAM_T}\
             ALTER TABLE t ADD FOREIGN KEY ix_u (user_id) REFERENCES users (id);
             ALTER TABLE t DROP INDEX user_id;"
        ),
        &["not checked", "not checked", "unknown", "unknown"],
    );
    // p3: the index name is taken already.
    assert_every_answer(
        "ALTER TABLE, index name taken",
        "",
        &format!(
            "{REVIEW_USERS_AND_MYISAM_T}\
             ALTER TABLE t ADD INDEX ix_u (amount);
             ALTER TABLE t ADD FOREIGN KEY ix_u (user_id) REFERENCES users (id);"
        ),
        &["not checked", "not checked", COPY, "unknown"],
    );
    // p4: the inline clause in CREATE TABLE.
    let sql = "CREATE TABLE users (id BIGINT UNSIGNED NOT NULL PRIMARY KEY) ENGINE=InnoDB;
        CREATE TABLE t (id INT NOT NULL PRIMARY KEY, user_id BIGINT UNSIGNED NOT NULL, \
        FOREIGN KEY ix_c (user_id) REFERENCES users (id)) ENGINE=MyISAM DEFAULT CHARSET=utf8mb4;
        ALTER TABLE t DROP INDEX user_id;";
    for answers in all_on_both("", sql) {
        assert_eq!(answers.last().unwrap(), "unknown", "{answers:?}");
    }
}

#[test]
fn a_myisam_foreign_key_with_a_constraint_name_and_no_index_name_keeps_the_copy_answer() {
    assert_every_answer(
        "ALTER TABLE, CONSTRAINT name only",
        "",
        &format!(
            "{REVIEW_USERS_AND_MYISAM_T}\
             ALTER TABLE t ADD CONSTRAINT fk_t_user FOREIGN KEY (user_id) REFERENCES users (id);
             ALTER TABLE t DROP INDEX fk_t_user;"
        ),
        &["not checked", "not checked", COPY, COPY],
    );
}

#[test]
fn a_descending_index_on_a_myisam_table_is_unknown_because_mysql_gives_1178() {
    // p1: a new table. p12: a table from the schema.
    let new_table = "CREATE TABLE t (id INT NOT NULL PRIMARY KEY, amount INT NOT NULL) \
                     ENGINE=MyISAM DEFAULT CHARSET=utf8mb4;\n";
    for operation in [
        "ADD INDEX i (amount DESC)",
        "ADD UNIQUE INDEX u (amount DESC)",
    ] {
        for answers in all_on_both("", &format!("{new_table}ALTER TABLE t {operation};")) {
            assert_eq!(answers.last().unwrap(), "unknown", "{operation}");
        }
        assert_myisam(operation, &[], operation, "unknown");
    }
    // The word DESC in a string is not a sort order.
    assert_myisam(
        "DESC in a COMMENT",
        &[],
        "ADD INDEX i (amount) COMMENT 'newest first, DESC'",
        COPY,
    );
}

#[test]
fn a_temporary_table_created_after_default_tmp_storage_engine_was_set_has_no_known_engine() {
    // p7.
    assert_answer(
        "default_tmp_storage_engine",
        "",
        "SET default_tmp_storage_engine = MyISAM;
         CREATE TEMPORARY TABLE tt (id INT, email VARCHAR(255)) DEFAULT CHARSET=utf8mb4;
         ALTER TABLE tt ADD INDEX ie (email);",
        "unknown",
    );
}

#[test]
fn create_table_if_not_exists_like_over_a_myisam_table_leaves_the_following_alter_unknown() {
    // p6: MySQL does nothing here, the table stays MyISAM.
    let schema = "CREATE TABLE `users` (
  `id` bigint unsigned NOT NULL AUTO_INCREMENT,
  `name` varchar(100) NOT NULL,
  PRIMARY KEY (`id`)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_0900_ai_ci;
CREATE TABLE `orders` (
  `id` bigint unsigned NOT NULL AUTO_INCREMENT,
  `user_id` bigint unsigned NOT NULL,
  `amount` int NOT NULL,
  `note` varchar(50) DEFAULT NULL,
  PRIMARY KEY (`id`)
) ENGINE=MyISAM DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_0900_ai_ci;
";
    assert_answer(
        "IF NOT EXISTS ... LIKE",
        schema,
        "CREATE TABLE IF NOT EXISTS orders LIKE users;
         ALTER TABLE orders ADD COLUMN c INT NULL;",
        "unknown",
    );
}

#[test]
fn the_word_engine_in_a_quoted_string_of_an_alter_is_not_read_as_an_engine() {
    // p8: the table is MyISAM after the first statement, whatever the
    // comment says (step 11b: it stays known, so the later statement gets
    // the MyISAM answer).
    let schema = "CREATE TABLE `orders` (
  `id` bigint unsigned NOT NULL AUTO_INCREMENT,
  `amount` int NOT NULL,
  PRIMARY KEY (`id`)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_0900_ai_ci;
";
    assert_answer(
        "ENGINE=MyISAM, COMMENT 'was engine InnoDB'",
        schema,
        "ALTER TABLE orders ENGINE=MyISAM, COMMENT 'was engine InnoDB';
         ALTER TABLE orders ADD COLUMN c INT NULL;",
        COPY,
    );
}

const SQL_MODE_SCHEMA: &str = "CREATE TABLE `orders` (
  `id` bigint unsigned NOT NULL AUTO_INCREMENT,
  `amount` int NOT NULL,
  `code` varchar(251) DEFAULT NULL,
  PRIMARY KEY (`id`)
) ENGINE=MyISAM DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_0900_ai_ci;
";

#[test]
fn a_myisam_key_over_1000_bytes_is_unknown_once_the_input_sets_sql_mode() {
    // p13 and p14: with sql_mode '' the server cuts the key to a 250-character
    // prefix and accepts it; with a strict mode it gives 1071. kilit does not
    // follow the mode, so any SET of it makes the answer unknown.
    for set in [
        "SET SESSION sql_mode = ''",
        "SET SESSION sql_mode = 'STRICT_TRANS_TABLES'",
        "SET sql_mode = ''",
    ] {
        for operation in [
            "ALTER TABLE orders ADD INDEX idx_code (code);",
            // p15
            "ALTER TABLE orders ADD UNIQUE INDEX uq_code (code);",
        ] {
            assert_answer(
                &format!("{set}; {operation}"),
                SQL_MODE_SCHEMA,
                &format!("{set};\n{operation}"),
                "unknown",
            );
        }
    }
}

#[test]
fn a_myisam_key_over_1000_bytes_without_a_sql_mode_in_the_input_fails_with_1071_and_says_it_assumes_strict_mode()
 {
    for operation in [
        "ALTER TABLE orders ADD INDEX idx_code (code);",
        "ALTER TABLE orders ADD UNIQUE INDEX uq_code (code);",
    ] {
        for server in [MYSQL_80, MYSQL_84] {
            match predictions_on(server, SQL_MODE_SCHEMA, operation)
                .pop()
                .unwrap()
            {
                Prediction::Measured {
                    verdict: Verdict::Fails { code, .. },
                    notes,
                    ..
                } => {
                    assert_eq!(code, 1071, "{operation} on {server}");
                    assert!(
                        notes.iter().any(|note| note.contains("strict SQL mode")),
                        "{operation} on {server}: {notes:?}"
                    );
                }
                other => panic!("{operation} on {server}: {other:?}"),
            }
        }
    }
}

#[test]
fn innobase_is_another_name_of_innodb_and_keeps_the_innodb_answer() {
    // p9: the servers run ADD COLUMN on it as INSTANT.
    assert_answer(
        "ENGINE=INNOBASE",
        "",
        "CREATE TABLE t (id INT NOT NULL PRIMARY KEY, note VARCHAR(50) NULL) \
         ENGINE=INNOBASE DEFAULT CHARSET=utf8mb4;
         ALTER TABLE t ADD COLUMN c INT NULL;",
        INSTANT,
    );
}

#[test]
fn the_reason_for_a_myisam_foreign_key_with_an_unknown_parent_starts_with_kilit_cannot_tell_if() {
    // Laravel's c1e and c2c without a schema file: users is not known.
    let create = "create table `orders` (`id` bigint unsigned not null auto_increment primary key, \
                  `user_id` bigint unsigned not null, `amount` int not null, \
                  `email` varchar(255) null, `note` varchar(50) null) \
                  default character set utf8mb4 collate 'utf8mb4_unicode_ci' engine = MyISAM";
    let foreign_key = "alter table `orders` add constraint `orders_user_id_foreign` \
                       foreign key (`user_id`) references `users` (`id`)";
    let output = pretend(
        "2026_01_01_000001_create_orders_table",
        &[create, foreign_key],
    );
    let statements: Vec<Statement> = read_pretend(&output)
        .unwrap()
        .migrations
        .into_iter()
        .flat_map(|migration| migration.statements)
        .collect();
    for server in [MYSQL_80, MYSQL_84] {
        let results = Knowledge::built_in().server(server).unwrap();
        let findings = check(&statements, &Schema::new(), results);
        match &findings.last().unwrap().prediction {
            Prediction::Unknown { reason } => {
                assert!(
                    reason.starts_with("kilit cannot tell if"),
                    "{server}: {reason}"
                );
                assert!(
                    !reason.contains("whether the change happens"),
                    "{server}: {reason}"
                );
            }
            other => panic!("{server}: {other:?}"),
        }
    }
}

// Step 9c: UUID_SHORT(), SYSDATE() and USER() as the default of a new
// column. The lab measured all three on both servers, NULL and NOT NULL, in
// all six variants: error 1674, "Statement is unsafe because it uses a system
// function that may return a different value on the replica", the answer
// UUID() and RAND() have had since step 4.

/// The three functions in the shapes the lab ran, with letter case and spaces
/// that do not matter inside the parentheses.
const BINLOG_UNSAFE_COLUMNS: [&str; 12] = [
    "ref BIGINT UNSIGNED NULL DEFAULT (UUID_SHORT())",
    "ref BIGINT UNSIGNED NOT NULL DEFAULT (UUID_SHORT())",
    "ref BIGINT UNSIGNED NOT NULL DEFAULT (uuid_short( ))",
    "ref BIGINT UNSIGNED NOT NULL DEFAULT (Uuid_Short())",
    "placed_at DATETIME NULL DEFAULT (SYSDATE())",
    "placed_at DATETIME NOT NULL DEFAULT (SYSDATE())",
    "placed_at DATETIME NOT NULL DEFAULT (sysdate( ))",
    "placed_at DATETIME NOT NULL DEFAULT (SysDate())",
    "created_by VARCHAR(288) NULL DEFAULT (USER())",
    "created_by VARCHAR(288) NOT NULL DEFAULT (USER())",
    "created_by VARCHAR(288) NOT NULL DEFAULT (user( ))",
    "created_by VARCHAR(288) NOT NULL DEFAULT (User())",
];

/// The reason kilit gives for the last statement of `sql` on `server`, when
/// it has no answer.
fn unknown_reason_on(server: &str, schema: &str, sql: &str) -> String {
    let results = Knowledge::built_in().server(server).unwrap();
    let findings = check(&parse(sql), &Schema::from_sql(schema), results);
    match &findings.last().unwrap().prediction {
        Prediction::Unknown { reason } => reason.clone(),
        other => panic!("{server}: {sql}: {other:?}"),
    }
}

#[test]
fn a_new_column_defaulting_to_uuid_short_sysdate_or_user_fails_with_1674_on_both_servers() {
    for column in BINLOG_UNSAFE_COLUMNS {
        assert_answer(
            column,
            LAB_TABLES,
            &format!("ALTER TABLE orders ADD COLUMN {column};"),
            "fails with error 1674",
        );
    }
}

#[test]
fn the_same_three_defaults_fail_without_a_schema_and_on_a_lost_innodb_table() {
    // The lab's table is not needed for the answer: the failure does not
    // depend on anything in the table. A lost table keeps its InnoDB answers.
    let lost = format!("{LAB_TABLES}ALTER TABLE orders AUTO_INCREMENT = 5;");
    for column in BINLOG_UNSAFE_COLUMNS {
        let sql = format!("ALTER TABLE orders ADD COLUMN {column};");
        assert_answer(column, "", &sql, "fails with error 1674");
        assert_answer(column, &lost, &sql, "fails with error 1674");
    }
}

#[test]
fn the_three_defaults_fail_on_a_table_created_earlier_in_the_same_input() {
    let sql = "CREATE TABLE tokens (id BIGINT UNSIGNED AUTO_INCREMENT PRIMARY KEY, \
               note VARCHAR(50) NULL) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4;
               ALTER TABLE tokens ADD COLUMN ref BIGINT UNSIGNED NOT NULL DEFAULT (UUID_SHORT());
               ALTER TABLE tokens ADD COLUMN placed_at DATETIME NULL DEFAULT (SYSDATE());
               ALTER TABLE tokens ADD COLUMN created_by VARCHAR(288) NULL DEFAULT (USER());";
    for answers in all_on_both("", sql) {
        assert_eq!(answers[1..], ["fails with error 1674"; 3], "{answers:?}");
    }
}

#[test]
fn forcing_an_algorithm_or_a_lock_on_the_three_defaults_still_fails_with_1674() {
    // The lab ran each of them as written, with ALGORITHM=INSTANT, INPLACE,
    // INPLACE with LOCK=NONE, COPY, and with LOCK=NONE: 1674 in every run.
    for column in [
        "ref BIGINT UNSIGNED NOT NULL DEFAULT (UUID_SHORT())",
        "placed_at DATETIME NULL DEFAULT (SYSDATE())",
        "created_by VARCHAR(288) NOT NULL DEFAULT (USER())",
    ] {
        for clause in [
            "ALGORITHM=INSTANT",
            "ALGORITHM=INPLACE",
            "ALGORITHM=INPLACE, LOCK=NONE",
            "ALGORITHM=COPY",
            "LOCK=NONE",
        ] {
            assert_answer(
                &format!("{column}, {clause}"),
                LAB_TABLES,
                &format!("ALTER TABLE orders ADD COLUMN {column}, {clause};"),
                "fails with error 1674",
            );
        }
    }
}

#[test]
fn the_three_defaults_on_a_myisam_table_are_unknown_as_uuid_is() {
    assert_myisam(
        "UUID()",
        &[],
        "ADD COLUMN token CHAR(36) NOT NULL DEFAULT (UUID())",
        "unknown",
    );
    for column in BINLOG_UNSAFE_COLUMNS {
        assert_myisam(column, &[], &format!("ADD COLUMN {column}"), "unknown");
    }
}

#[test]
fn a_failed_uuid_short_default_leaves_the_table_as_it_was() {
    // ref is not on the table after the failure, so a MODIFY of it is about
    // a column kilit cannot find. With the column really added, it can.
    let modify = "ALTER TABLE orders MODIFY ref BIGINT NULL;";
    let not_there = "orders has no column ref";
    for server in [MYSQL_80, MYSQL_84] {
        let results = Knowledge::built_in().server(server).unwrap();
        let last = |sql: &str| {
            check(&parse(sql), &Schema::from_sql(LAB_TABLES), results)
                .pop()
                .unwrap()
                .prediction
        };
        let added = last(&format!(
            "ALTER TABLE orders ADD COLUMN ref BIGINT UNSIGNED NULL;\n{modify}"
        ));
        assert!(
            !matches!(&added, Prediction::Unknown { reason } if reason.contains(not_there)),
            "{server}: {added:?}"
        );
        for default in ["(UUID_SHORT())", "(SYSDATE())", "(USER())"] {
            let sql = format!(
                "ALTER TABLE orders ADD COLUMN ref BIGINT UNSIGNED NULL DEFAULT {default};\n{modify}"
            );
            match last(&sql) {
                Prediction::Unknown { reason } => {
                    assert!(reason.contains(not_there), "{server}: {default}: {reason}")
                }
                other => panic!("{server}: {default}: {other:?}"),
            }
        }
    }
}

#[test]
fn the_binary_log_setting_decides_the_three_defaults_like_uuid() {
    let reason = "a statement before this one sets sql_log_bin = 0, and the lab has measured \
                  expression defaults only with the binary log on";
    for column in [
        "token CHAR(36) NOT NULL DEFAULT (UUID())",
        "ref BIGINT UNSIGNED NOT NULL DEFAULT (UUID_SHORT())",
        "placed_at DATETIME NULL DEFAULT (SYSDATE())",
        "created_by VARCHAR(288) NOT NULL DEFAULT (USER())",
    ] {
        let add = format!("ALTER TABLE orders ADD COLUMN {column};");
        // Off: the server accepts and copies the table, but the lab did not
        // measure the three with the binary log off: unknown, as for UUID().
        for server in [MYSQL_80, MYSQL_84] {
            assert_eq!(
                unknown_reason_on(server, LAB_TABLES, &format!("SET sql_log_bin = 0;\n{add}")),
                reason,
                "{server}: {column}"
            );
        }
        // On: the measured answer stays.
        assert_answer(
            column,
            LAB_TABLES,
            &format!("SET sql_log_bin = 1;\n{add}"),
            "fails with error 1674",
        );
        assert_answer(
            column,
            LAB_TABLES,
            &format!("SET sql_log_bin = 0;\nSET sql_log_bin = 1;\n{add}"),
            "fails with error 1674",
        );
    }
}

#[test]
fn modify_change_and_set_default_with_the_three_functions_stay_unknown() {
    for default in ["(UUID_SHORT())", "(SYSDATE())", "(USER())"] {
        for operation in [
            format!("MODIFY note VARCHAR(50) NULL DEFAULT {default}"),
            format!("CHANGE note remark VARCHAR(50) NULL DEFAULT {default}"),
            format!("ALTER COLUMN note SET DEFAULT {default}"),
        ] {
            assert_answer(
                &operation,
                LAB_TABLES,
                &format!("ALTER TABLE orders {operation};"),
                "unknown",
            );
        }
    }
}

#[test]
fn other_forms_of_the_three_functions_are_still_unknown() {
    // Only the measured shape has an answer: extra parentheses, arguments,
    // arithmetic, and the neighbours of USER().
    for default in [
        "((SYSDATE()))",
        "(SYSDATE(6))",
        "(UUID_SHORT() + 1)",
        "(CURRENT_USER())",
        "(SESSION_USER())",
        "(SYSTEM_USER())",
    ] {
        assert_answer(
            default,
            LAB_TABLES,
            &format!("ALTER TABLE orders ADD COLUMN c VARCHAR(100) NULL DEFAULT {default};"),
            "unknown",
        );
    }
}

#[test]
fn laravel_s_expression_default_with_uuid_short_sysdate_or_user_fails_with_1674() {
    // The form Laravel's grammar produces for
    // $table->unsignedBigInteger('ref')->default(new Expression('(UUID_SHORT())'));
    // an Expression is written as it is after `default`. The same for
    // '(SYSDATE())' on a dateTime and '(USER())' on a string.
    for statement in [
        "alter table `orders` add `ref` bigint unsigned not null default (UUID_SHORT())",
        "alter table `orders` add `placed_at` datetime not null default (SYSDATE())",
        "alter table `orders` add `created_by` varchar(255) not null default (USER())",
    ] {
        assert_eq!(
            pretend_on_mysql_84(
                LARAVEL_TABLES,
                &pretend(
                    "2026_11_05_100000_add_a_default_to_orders_table",
                    &[statement]
                )
            ),
            ["fails with error 1674"],
            "{statement}"
        );
    }
}

// Regression tests for what the step 9c review found.

#[test]
fn a_space_or_comment_between_a_function_name_and_its_parenthesis_is_unknown() {
    // Measured on both servers (manual/step9c/review/t1): `(SYSDATE ())`,
    // `( SYSDATE ( ) )`, `(SYSDATE /*x*/ ())` and `(NOW ())` fail with 3770,
    // because MySQL reads the name with a space as another function. A space
    // only inside the parentheses, `(sysdate( ))`, is 1674.
    let reason = "is written with a space or a comment where the lab measured none";
    for default in [
        "(SYSDATE ())",
        "( SYSDATE ( ) )",
        "(SYSDATE /*x*/ ())",
        "(NOW ())",
        "(USER ( ))",
        "(UUID_SHORT ( ))",
    ] {
        let sql = format!("ALTER TABLE orders ADD COLUMN c DATETIME NULL DEFAULT {default};");
        for server in [MYSQL_80, MYSQL_84] {
            let found = unknown_reason_on(server, LAB_TABLES, &sql);
            assert!(found.contains(reason), "{server}: {default}: {found}");
        }
    }
    // The shapes the lab measured keep their answers.
    assert_answer(
        "space inside only",
        LAB_TABLES,
        "ALTER TABLE orders ADD COLUMN c DATETIME NULL DEFAULT (sysdate( ));",
        "fails with error 1674",
    );
    assert_answer(
        "NOW without a space",
        LAB_TABLES,
        "ALTER TABLE orders ADD COLUMN c DATETIME NULL DEFAULT (NOW());",
        COPY,
    );
}

#[test]
fn an_earlier_statement_naming_binlog_format_makes_every_expression_default_unknown() {
    // Measured on both servers (manual/step9c/review/t2): after SET SESSION
    // binlog_format = 'STATEMENT' the server accepts UUID(), RAND(), USER()
    // and UUID_SHORT() defaults and copies the table.
    let reason = "names binlog_format";
    for set in [
        "SET SESSION binlog_format = 'STATEMENT'",
        "SET binlog_format = 'ROW'",
        "SET @@session.binlog_format = MIXED",
        "SET GLOBAL binlog_format = 'ROW'",
    ] {
        for default in [
            "token CHAR(36) NULL DEFAULT (UUID())",
            "weight DOUBLE NULL DEFAULT (RAND())",
            "ref BIGINT UNSIGNED NULL DEFAULT (UUID_SHORT())",
            "placed_at DATETIME NULL DEFAULT (SYSDATE())",
            "created_by VARCHAR(288) NULL DEFAULT (USER())",
            "placed_at DATETIME NULL DEFAULT (NOW())",
        ] {
            let sql = format!("{set};\nALTER TABLE orders ADD COLUMN {default};");
            for server in [MYSQL_80, MYSQL_84] {
                let found = unknown_reason_on(server, LAB_TABLES, &sql);
                assert!(
                    found.contains(reason),
                    "{server}: {set}: {default}: {found}"
                );
            }
        }
        // A literal default does not depend on it.
        assert_answer(
            "literal default",
            LAB_TABLES,
            &format!("{set};\nALTER TABLE orders ADD COLUMN c INT NULL DEFAULT 0;"),
            INSTANT,
        );
    }
}

// Step 9b: ROW_FORMAT, KEY_BLOCK_SIZE and old_alter_table.
//
// The expectations come from the lab's results for the compressed-*,
// compact-*, redundant-*, key-block-size-* and old-alter-table-* scenarios,
// and from the hand tests run on both servers for the spellings and SET
// forms the lab does not cover. Where a test relies on a decision of the
// step, the comment says so.

const INPLACE_REBUILD: &str = "INPLACE, rebuilds the table, writes continue";
const FAILS_1846: &str = "fails with error 1846";
const UNKNOWN: &str = "unknown";

const INDEX_ON_AMOUNT: &str = "ALTER TABLE orders ADD INDEX idx_orders_amount (amount)";
const ADD_COLUMN: &str = "ADD COLUMN coupon VARCHAR(255) NULL";

/// The eight changes the lab measured on every row format and under
/// old_alter_table: (name, setup, operation after `ALTER TABLE orders`).
const EIGHT: [(&str, &[&str], &str); 8] = [
    ("add column", &[], ADD_COLUMN),
    ("drop column", &[], "DROP COLUMN note"),
    ("rename column", &[], "RENAME COLUMN note TO memo"),
    ("set default", &[], "ALTER COLUMN amount SET DEFAULT 0"),
    ("add index", &[], "ADD INDEX idx_orders_amount (amount)"),
    (
        "add unique index",
        &[],
        "ADD UNIQUE INDEX uq_orders_email (email)",
    ),
    (
        "drop index",
        &[INDEX_ON_AMOUNT],
        "DROP INDEX idx_orders_amount",
    ),
    ("int to bigint", &[], "MODIFY COLUMN amount BIGINT NOT NULL"),
];

/// The lab's tables with `options` after the character set of `orders`,
/// where the lab puts them, then `setup`.
fn lab_with_options(options: &str, setup: &[&str]) -> String {
    let mut sql = lab_tables_with(None, options);
    for statement in setup {
        sql.push_str(statement);
        sql.push_str(";\n");
    }
    sql
}

/// `orders` as mysqldump writes it, with `options` after the collation and
/// `keys` (each starting with a comma and a new line) after the primary key.
fn dump_orders(options: &str, keys: &str) -> String {
    format!(
        "CREATE TABLE `users` (
  `id` bigint unsigned NOT NULL AUTO_INCREMENT,
  `name` varchar(100) NOT NULL,
  PRIMARY KEY (`id`)
) ENGINE=InnoDB AUTO_INCREMENT=3 DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_0900_ai_ci;

CREATE TABLE `orders` (
  `id` bigint unsigned NOT NULL AUTO_INCREMENT,
  `user_id` bigint unsigned NOT NULL,
  `amount` int NOT NULL,
  `email` varchar(255) DEFAULT NULL,
  `note` varchar(50) DEFAULT NULL,
  PRIMARY KEY (`id`){keys}
) ENGINE=InnoDB AUTO_INCREMENT=3 DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_0900_ai_ci{options};
"
    )
}

/// The same answer on both servers for `ALTER TABLE orders <operation>`.
fn assert_orders(name: &str, schema: &str, operation: &str, answer: &str) {
    assert_answer(
        name,
        schema,
        &format!("ALTER TABLE orders {operation};"),
        answer,
    );
}

/// The impact of every statement of `sql` on MySQL 8.4.13, with no tables
/// known.
fn impacts_of(sql: &str) -> Vec<Impact> {
    let statements = parse(sql);
    let results = Knowledge::built_in().server(MYSQL_84).unwrap();
    let findings = check(&statements, &Schema::new(), results);
    impacts(&statements, &findings)
}

const COMPRESSED: &str = " ROW_FORMAT=COMPRESSED";
const COMPACT_OPTION: &str = " ROW_FORMAT=COMPACT";
const REDUNDANT_OPTION: &str = " ROW_FORMAT=REDUNDANT";

/// Every way MySQL 8.0.46 and 8.4.13 accepted to make `orders` COMPRESSED
/// (hand tests, both servers): the row format alone, a key block size
/// alone, with ROW_FORMAT=DEFAULT, both, any letter case, no `=`, a comma
/// between options, and the last of two row formats.
const COMPRESSED_SPELLINGS: [&str; 13] = [
    " ROW_FORMAT=COMPRESSED",
    " KEY_BLOCK_SIZE=8",
    " ROW_FORMAT=DEFAULT KEY_BLOCK_SIZE=8",
    " ROW_FORMAT=COMPRESSED KEY_BLOCK_SIZE=8",
    " KEY_BLOCK_SIZE=8 ROW_FORMAT=COMPRESSED",
    " row_format=compressed",
    " Row_Format=Compressed Key_Block_Size=8",
    " ROW_FORMAT COMPRESSED",
    " KEY_BLOCK_SIZE 8",
    " ROW_FORMAT=COMPRESSED, KEY_BLOCK_SIZE=8",
    " ROW_FORMAT=COMPACT ROW_FORMAT=COMPRESSED",
    " ROW_FORMAT=DYNAMIC ROW_FORMAT=COMPRESSED",
    " COLLATE=utf8mb4_0900_ai_ci ROW_FORMAT=COMPRESSED KEY_BLOCK_SIZE=8",
];

#[test]
fn the_lab_writes_the_table_options_after_the_character_set_of_orders_only() {
    let text = lab_tables_with(None, " ROW_FORMAT=COMPACT KEY_BLOCK_SIZE=8");
    assert_eq!(text.matches("ROW_FORMAT=COMPACT").count(), 1);
    assert_eq!(text.matches("KEY_BLOCK_SIZE=8").count(), 1);
    let (users, orders) = text.split_at(text.find("CREATE TABLE orders").unwrap());
    assert!(!users.contains("ROW_FORMAT"), "{users}");
    assert!(
        orders.contains("DEFAULT CHARSET=utf8mb4 ROW_FORMAT=COMPACT KEY_BLOCK_SIZE=8;"),
        "{orders}"
    );
    assert_eq!(lab_tables_with(None, ""), LAB_TABLES);
}

#[test]
fn a_scenario_of_the_lab_starts_from_an_orders_with_the_row_format_it_asks_for() {
    for results in Knowledge::built_in().servers() {
        for scenario in results.scenarios() {
            let id = scenario.id.as_str();
            let schema = lab_schema(scenario);
            let TableState::Known(orders) = schema.table("orders") else {
                panic!("{id}: orders is not known");
            };
            let expected = match scenario.row_format.as_deref() {
                None => None,
                Some("COMPRESSED") => Some(RowFormat::Compressed),
                Some("COMPACT") => Some(RowFormat::Compact),
                Some("REDUNDANT") => Some(RowFormat::Redundant),
                Some(other) => panic!("{id}: row format {other}"),
            };
            assert_eq!(orders.row_format, expected, "{id}");
            assert_eq!(
                orders.key_block_size,
                scenario.key_block_size.map(|size| size.to_string()),
                "{id}"
            );
            // The scenarios about a row format say so in their id.
            let named = ["compressed-", "compact-", "redundant-", "key-block-size-"]
                .into_iter()
                .any(|prefix| id.starts_with(prefix));
            assert_eq!(
                named,
                scenario.row_format.is_some() || scenario.key_block_size.is_some(),
                "{id}"
            );
            // Only the table `orders` gets the options.
            let TableState::Known(users) = schema.table("users") else {
                panic!("{id}: users is not known");
            };
            assert_eq!(users.row_format, None, "{id}");
            assert_eq!(users.key_block_size, None, "{id}");
        }
    }
}

#[test]
fn an_old_alter_table_scenario_and_its_twin_ask_for_the_same_change() {
    for results in Knowledge::built_in().servers() {
        let server = results.server.name();
        let mut old_ids: Vec<&str> = OLD_ALTER_TABLE_CASES
            .iter()
            .flat_map(|case| case.scenarios.iter().copied())
            .collect();
        old_ids.sort_unstable();
        let mut twin_olds: Vec<&str> = OLD_ALTER_TABLE_TWINS.iter().map(|(old, _)| *old).collect();
        twin_olds.sort_unstable();
        assert_eq!(old_ids, twin_olds, "{server}");
        assert_eq!(old_ids.len(), 8, "{server}");
        for (old, twin) in OLD_ALTER_TABLE_TWINS {
            let old = results.scenario(old).unwrap();
            let twin = results.scenario(twin).unwrap();
            assert!(
                old.session
                    .iter()
                    .any(|statement| statement.contains("old_alter_table")),
                "{} sets no old_alter_table on {server}",
                old.id
            );
            assert!(twin.session.is_empty(), "{} on {server}", twin.id);
            assert_eq!(old.setup, twin.setup, "{} on {server}", old.id);
            assert_eq!(old.statement, twin.statement, "{} on {server}", old.id);
            assert_eq!(old.engine, None, "{} on {server}", old.id);
            assert_eq!(old.row_format, None, "{} on {server}", old.id);
            assert_eq!(twin.row_format, None, "{} on {server}", twin.id);
        }
    }
}

#[test]
fn old_alter_table_makes_the_lab_s_server_default_run_copy_for_all_eight_operations() {
    // Lab result: without a clause every one of the eight copies the table
    // and LOCK=NONE alone is refused with 1846; a forced ALGORITHM equals the
    // DYNAMIC twin's run.
    for results in Knowledge::built_in().servers() {
        for (old, twin) in OLD_ALTER_TABLE_TWINS {
            let old = results.scenario(old).unwrap();
            let twin = results.scenario(twin).unwrap();
            match &old.verdict {
                Verdict::Runs {
                    algorithm,
                    copied_rows,
                    writes,
                    ..
                } => {
                    assert_eq!(
                        algorithm.map(|algorithm| algorithm.to_string()),
                        Some("COPY".to_owned()),
                        "{}",
                        old.id
                    );
                    assert!(copied_rows.is_some_and(|rows| rows > 0), "{}", old.id);
                    assert_eq!(*writes, kilit_core::Writes::Blocked, "{}", old.id);
                }
                other => panic!("{}: {other:?}", old.id),
            }
            assert!(
                matches!(
                    old.verdict_for(Variant::LOCK_NONE),
                    Some(Verdict::Fails { code: 1846, .. })
                ),
                "{}",
                old.id
            );
            // INT to BIGINT copies the table with old_alter_table off as
            // well (the DYNAMIC int-to-bigint scenario: COPY, 1000 rows,
            // writes blocked), so its verdict equals its twin's. The other
            // seven differ from theirs.
            if old.id == "old-alter-table-int-to-bigint" {
                assert_eq!(old.verdict, twin.verdict, "{}", old.id);
            } else {
                assert_ne!(old.verdict, twin.verdict, "{}", old.id);
            }
        }
    }
}

#[test]
fn compressed_tables_in_every_spelling_rebuild_for_add_and_drop_column_and_refuse_instant() {
    for options in COMPRESSED_SPELLINGS {
        let lab = lab_with_options(options, &[]);
        for (name, operation, answer) in [
            ("add column", ADD_COLUMN.to_owned(), INPLACE_REBUILD),
            (
                "drop column",
                "DROP COLUMN note".to_owned(),
                INPLACE_REBUILD,
            ),
            (
                "add column, instant",
                format!("{ADD_COLUMN}, ALGORITHM=INSTANT"),
                FAILS_1845,
            ),
            (
                "drop column, instant",
                "DROP COLUMN note, ALGORITHM=INSTANT".to_owned(),
                FAILS_1845,
            ),
            (
                "add column, inplace",
                format!("{ADD_COLUMN}, ALGORITHM=INPLACE"),
                INPLACE_REBUILD,
            ),
        ] {
            assert_orders(&format!("{options}: {name}"), &lab, &operation, answer);
        }
    }
}

#[test]
fn compressed_tables_in_every_spelling_keep_the_dynamic_answers_for_the_other_six() {
    // compressed-rename-column to compressed-int-to-bigint, as written:
    // INSTANT, INSTANT, INPLACE, INPLACE, INPLACE, COPY.
    for options in COMPRESSED_SPELLINGS {
        for (name, setup, operation) in EIGHT {
            let answer = match name {
                "add column" | "drop column" => continue,
                "rename column" | "set default" => INSTANT,
                "add index" | "add unique index" | "drop index" => INPLACE,
                "int to bigint" => COPY,
                other => panic!("{other}"),
            };
            assert_orders(
                &format!("{options}: {name}"),
                &lab_with_options(options, setup),
                operation,
                answer,
            );
        }
    }
}

#[test]
fn a_compressed_table_from_a_mysqldump_schema_gets_the_compressed_answers() {
    // mysqldump writes the options after the collation, in this order.
    let schema = dump_orders(
        " ROW_FORMAT=COMPRESSED KEY_BLOCK_SIZE=8",
        ",\n  KEY `idx_orders_amount` (`amount`)",
    );
    for (sql, answer) in [
        (
            "alter table `orders` add `coupon` varchar(255) null",
            INPLACE_REBUILD,
        ),
        ("alter table `orders` drop `note`", INPLACE_REBUILD),
        (
            "alter table `orders` rename column `note` to `memo`",
            INSTANT,
        ),
        (
            "alter table `orders` add unique `orders_email_unique`(`email`)",
            INPLACE,
        ),
        (
            "alter table `orders` drop index `idx_orders_amount`",
            INPLACE,
        ),
        ("alter table `orders` modify `amount` bigint not null", COPY),
        (
            "alter table `orders` add `coupon` varchar(255) null, algorithm=instant",
            FAILS_1845,
        ),
    ] {
        assert_answer(sql, &schema, sql, answer);
    }
}

#[test]
fn a_compressed_table_keeps_its_row_format_through_the_changes_the_lab_measured() {
    // A rename does not change the row format: the add column after it
    // still rebuilds.
    assert_every_answer(
        "rename, then add column",
        &lab_with_options(COMPRESSED, &[]),
        "ALTER TABLE orders RENAME COLUMN note TO memo;
         ALTER TABLE orders ADD COLUMN coupon VARCHAR(255) NULL;",
        &[INSTANT, INPLACE_REBUILD],
    );
}

#[test]
fn compact_and_redundant_tables_give_the_dynamic_answers_for_the_eight_operations() {
    // compact-* and redundant-* scenarios; the unique index goes on a
    // 191-character column, the widest that fits 767 bytes.
    for options in [
        COMPACT_OPTION,
        REDUNDANT_OPTION,
        " row_format=compact",
        " ROW_FORMAT redundant",
    ] {
        for (name, setup, operation) in EIGHT {
            let (setup, operation): (Vec<&str>, &str) = if name == "add unique index" {
                (
                    vec!["ALTER TABLE orders ADD COLUMN code VARCHAR(191) NULL"],
                    "ADD UNIQUE INDEX uq_code (code)",
                )
            } else {
                (setup.to_vec(), operation)
            };
            let answer = match name {
                "add column" | "drop column" | "rename column" | "set default" => INSTANT,
                "add index" | "add unique index" | "drop index" => INPLACE,
                "int to bigint" => COPY,
                other => panic!("{other}"),
            };
            assert_orders(
                &format!("{options}: {name}"),
                &lab_with_options(options, &setup),
                operation,
                answer,
            );
        }
    }
}

#[test]
fn forcing_a_variant_on_a_compact_or_redundant_table_gives_the_lab_s_answer() {
    // compact-add-nullable-column: INSTANT, INPLACE rebuilds; add-index:
    // INSTANT is refused with 1845.
    for options in [COMPACT_OPTION, REDUNDANT_OPTION] {
        let lab = lab_with_options(options, &[]);
        for (name, operation, answer) in [
            (
                "add column, instant",
                format!("{ADD_COLUMN}, ALGORITHM=INSTANT"),
                INSTANT,
            ),
            (
                "drop column, instant",
                "DROP COLUMN note, ALGORITHM=INSTANT".to_owned(),
                INSTANT,
            ),
            (
                "add column, inplace",
                format!("{ADD_COLUMN}, ALGORITHM=INPLACE"),
                INPLACE_REBUILD,
            ),
            (
                "add index, instant",
                "ADD INDEX idx_orders_amount (amount), ALGORITHM=INSTANT".to_owned(),
                FAILS_1845,
            ),
            (
                "add index, inplace",
                "ADD INDEX idx_orders_amount (amount), ALGORITHM=INPLACE, LOCK=NONE".to_owned(),
                INPLACE,
            ),
            (
                "int to bigint, inplace",
                "MODIFY COLUMN amount BIGINT NOT NULL, ALGORITHM=INPLACE".to_owned(),
                FAILS_1846,
            ),
        ] {
            assert_orders(&format!("{options}: {name}"), &lab, &operation, answer);
        }
    }
}

#[test]
fn an_index_over_767_bytes_on_a_compact_or_redundant_table_fails_with_1071() {
    // compact-add-index-key-over-limit and the hand tests: 192 characters of
    // utf8mb4 are 768 bytes, 255 are 1020; all six variants gave 1071.
    for options in [COMPACT_OPTION, REDUNDANT_OPTION] {
        for length in [192, 255] {
            let column = format!("ALTER TABLE orders ADD COLUMN code VARCHAR({length}) NULL");
            let lab = lab_with_options(options, &[column.as_str()]);
            for operation in [
                "ADD INDEX idx_code (code)",
                "ADD UNIQUE INDEX uq_code (code)",
            ] {
                for clause in [
                    "",
                    ", ALGORITHM=INSTANT",
                    ", ALGORITHM=INPLACE",
                    ", ALGORITHM=INPLACE, LOCK=NONE",
                    ", ALGORITHM=COPY",
                    ", LOCK=NONE",
                ] {
                    assert_orders(
                        &format!("{options}: {length}: {operation}{clause}"),
                        &lab,
                        &format!("{operation}{clause}"),
                        FAILS_1071,
                    );
                }
            }
        }
        // The column the lab's tables have: email, 255 characters.
        let lab = lab_with_options(options, &[]);
        assert_orders(options, &lab, "ADD INDEX idx_email (email)", FAILS_1071);
        assert_orders(
            options,
            &lab,
            "ADD UNIQUE INDEX uq_orders_email (email)",
            FAILS_1071,
        );
    }
}

#[test]
fn an_index_of_191_characters_on_a_compact_or_redundant_table_gets_the_index_answer() {
    // compact-add-index-key-at-limit: 191 characters are 764 bytes.
    for options in [COMPACT_OPTION, REDUNDANT_OPTION] {
        let column = "ALTER TABLE orders ADD COLUMN code VARCHAR(191) NULL";
        let lab = lab_with_options(options, &[column]);
        assert_orders(options, &lab, "ADD INDEX idx_code (code)", INPLACE);
        assert_orders(options, &lab, "ADD UNIQUE INDEX uq_code (code)", INPLACE);
        assert_orders(
            options,
            &lab,
            "ADD INDEX idx_code (code), ALGORITHM=INSTANT",
            FAILS_1845,
        );
    }
}

#[test]
fn laravel_s_string_index_on_a_compact_table_fails_with_1071_for_255_characters() {
    // $table->string('email')->index(); and ->unique(): Laravel's string()
    // is varchar(255), which is 1020 bytes in utf8mb4.
    let schema = dump_orders(COMPACT_OPTION, "");
    for sql in [
        "alter table `orders` add index `orders_email_index`(`email`)",
        "alter table `orders` add unique `orders_email_unique`(`email`)",
    ] {
        assert_answer(sql, &schema, sql, FAILS_1071);
    }
    // The same tables with the server's default row format: accepted.
    let dynamic = dump_orders("", "");
    assert_answer(
        "dynamic",
        &dynamic,
        "alter table `orders` add index `orders_email_index`(`email`)",
        INPLACE,
    );
}

#[test]
fn a_key_that_fails_on_a_compact_table_leaves_the_table_as_it_was() {
    // A measured failure leaves the tables as they were: after it the table has no idx_code and no code index,
    // so the same name can be used again.
    let column = "ALTER TABLE orders ADD COLUMN code VARCHAR(192) NULL";
    for options in [COMPACT_OPTION, REDUNDANT_OPTION] {
        assert_every_answer(
            options,
            &lab_with_options(options, &[column]),
            "ALTER TABLE orders ADD INDEX idx_code (code);
             ALTER TABLE orders ADD INDEX idx_code (amount);
             ALTER TABLE orders ADD COLUMN coupon VARCHAR(255) NULL;",
            &[FAILS_1071, INPLACE, INSTANT],
        );
    }
}

#[test]
fn tables_without_a_row_format_or_with_the_default_one_keep_their_answers() {
    // Not written, DYNAMIC, DEFAULT and KEY_BLOCK_SIZE=0 all leave the table
    // DYNAMIC (hand tests): the answers of the lab's own scenarios.
    for options in [
        "",
        " ROW_FORMAT=DYNAMIC",
        " row_format=dynamic",
        " ROW_FORMAT=DEFAULT",
        " KEY_BLOCK_SIZE=0",
    ] {
        for (name, setup, operation) in EIGHT {
            let answer = match name {
                "add column" | "drop column" | "rename column" | "set default" => INSTANT,
                "add index" | "add unique index" | "drop index" => INPLACE,
                "int to bigint" => COPY,
                other => panic!("{other}"),
            };
            assert_orders(
                &format!("{options}: {name}"),
                &lab_with_options(options, setup),
                operation,
                answer,
            );
        }
        // 192 and 255 characters of utf8mb4 are fine on DYNAMIC.
        assert_orders(
            options,
            &lab_with_options(options, &[]),
            "ADD INDEX idx_email (email)",
            INPLACE,
        );
        assert_orders(
            options,
            &lab_with_options(
                options,
                &["ALTER TABLE orders ADD COLUMN code VARCHAR(192) NULL"],
            ),
            "ADD UNIQUE INDEX uq_code (code)",
            INPLACE,
        );
        assert_orders(
            options,
            &lab_with_options(options, &[]),
            "ADD COLUMN coupon VARCHAR(255) NULL, ALGORITHM=INSTANT",
            INSTANT,
        );
    }
}

#[test]
fn row_formats_and_block_sizes_the_lab_did_not_measure_make_every_alter_unknown() {
    // Decision 2 and the hand tests: KEY_BLOCK_SIZE 1, 2, 4 and 16 (not
    // measured), KEY_BLOCK_SIZE with another row format (MySQL: 1031 at
    // CREATE, 1478 at ALTER), FIXED and a misspelling (1031), and quoted
    // values (1064).
    for options in [
        " KEY_BLOCK_SIZE=1",
        " KEY_BLOCK_SIZE=2",
        " KEY_BLOCK_SIZE=4",
        " KEY_BLOCK_SIZE=16",
        " ROW_FORMAT=COMPRESSED KEY_BLOCK_SIZE=4",
        " ROW_FORMAT=DYNAMIC KEY_BLOCK_SIZE=8",
        " ROW_FORMAT=COMPACT KEY_BLOCK_SIZE=8",
        " ROW_FORMAT=REDUNDANT KEY_BLOCK_SIZE=8",
        " ROW_FORMAT=FIXED",
        " ROW_FORMAT=COMPRESED",
        " ROW_FORMAT='COMPRESSED'",
        " ROW_FORMAT=\"COMPRESSED\"",
        " ROW_FORMAT='DYNAMIC'",
        " KEY_BLOCK_SIZE='8'",
    ] {
        let lab = lab_with_options(options, &[INDEX_ON_AMOUNT]);
        // Even the changes that are INSTANT on DYNAMIC.
        for operation in [
            ADD_COLUMN,
            "RENAME COLUMN note TO memo",
            "ADD INDEX idx_orders_note (note)",
            "MODIFY COLUMN amount BIGINT NOT NULL",
        ] {
            assert_orders(&format!("{options}: {operation}"), &lab, operation, UNKNOWN);
        }
    }
}

#[test]
fn a_quoted_row_format_or_block_size_says_the_servers_do_not_accept_it() {
    // MySQL 8.0.46 and 8.4.13 give 1064 for both (hand tests).
    for options in [" ROW_FORMAT='COMPRESSED'", " KEY_BLOCK_SIZE='8'"] {
        let sql = format!("ALTER TABLE orders {ADD_COLUMN};");
        for server in [MYSQL_80, MYSQL_84] {
            let reason = unknown_reason_on(server, &lab_with_options(options, &[]), &sql);
            assert!(
                reason.contains("do not accept"),
                "{server}: {options}: {reason}"
            );
        }
    }
}

#[test]
fn a_myisam_table_with_a_row_format_or_block_size_is_unknown() {
    // Decision 6. Without the option the MyISAM answer is COPY.
    assert_orders(
        "no option",
        &lab_tables_with(Some("MyISAM"), ""),
        ADD_COLUMN,
        COPY,
    );
    for options in [
        " ROW_FORMAT=FIXED",
        " ROW_FORMAT=DYNAMIC",
        " ROW_FORMAT=DEFAULT",
        " ROW_FORMAT=COMPRESSED",
        " KEY_BLOCK_SIZE=8",
        " row_format=fixed",
    ] {
        let lab = lab_tables_with(Some("MyISAM"), options);
        assert_orders(options, &lab, ADD_COLUMN, UNKNOWN);
        assert_orders(options, &lab, "RENAME COLUMN note TO memo", UNKNOWN);
    }
}

#[test]
fn a_change_outside_the_eight_on_a_compressed_compact_or_redundant_table_is_unknown() {
    // Decision 2: only the measured operations get an answer, never a
    // DYNAMIC one.
    let add_foreign_key = MYISAM_ADD_FK;
    for options in [COMPRESSED, COMPACT_OPTION, REDUNDANT_OPTION] {
        for (name, setup, operation) in [
            ("add foreign key", &[][..], add_foreign_key),
            ("add fulltext", &[][..], "ADD FULLTEXT INDEX ft_note (note)"),
            (
                "lengthen a varchar",
                &[][..],
                "MODIFY COLUMN note VARCHAR(100) NULL",
            ),
            (
                "int to smallint",
                &[][..],
                "MODIFY COLUMN amount SMALLINT NOT NULL",
            ),
            (
                "change that renames",
                &[][..],
                "CHANGE COLUMN note memo VARCHAR(50) NULL",
            ),
            (
                "drop an indexed column",
                &[INDEX_ON_AMOUNT][..],
                "DROP COLUMN amount",
            ),
        ] {
            assert_orders(
                &format!("{options}: {name}"),
                &lab_with_options(options, setup),
                operation,
                UNKNOWN,
            );
        }
    }
}

#[test]
fn an_index_kilit_cannot_measure_the_key_bytes_of_on_a_compact_table_is_unknown() {
    for options in [COMPACT_OPTION, REDUNDANT_OPTION] {
        for (name, setup, operation) in [
            (
                "two columns",
                &[][..],
                "ADD INDEX idx_two (user_id, amount)",
            ),
            (
                "a prefix",
                &[][..],
                "ADD INDEX idx_email_prefix (email(100))",
            ),
            (
                "latin1 varchar",
                &["ALTER TABLE orders ADD COLUMN code VARCHAR(255) CHARACTER SET latin1 NULL"][..],
                "ADD INDEX idx_code (code)",
            ),
            (
                "text",
                &["ALTER TABLE orders ADD COLUMN body TEXT NULL"][..],
                "ADD INDEX idx_body (body)",
            ),
            (
                "text with a prefix",
                &["ALTER TABLE orders ADD COLUMN body TEXT NULL"][..],
                "ADD INDEX idx_body (body(100))",
            ),
        ] {
            assert_orders(
                &format!("{options}: {name}"),
                &lab_with_options(options, setup),
                operation,
                UNKNOWN,
            );
        }
    }
}

#[test]
fn adding_a_column_to_a_non_dynamic_table_with_a_fulltext_index_is_unknown() {
    // Decision 8: on DYNAMIC a FULLTEXT index changes the answer; nothing
    // measured the combination.
    let keys = ",\n  FULLTEXT KEY `ft_note` (`note`)";
    for options in [COMPRESSED, COMPACT_OPTION, REDUNDANT_OPTION] {
        assert_orders(options, &dump_orders(options, keys), ADD_COLUMN, UNKNOWN);
    }
    // The same table with the default row format: today's answer.
    assert_orders("dynamic", &dump_orders("", keys), ADD_COLUMN, COPY);
}

#[test]
fn naming_innodb_default_row_format_makes_tables_without_a_row_format_unknown() {
    // A rebuild takes the new default, so every table that
    // writes no ROW_FORMAT, old or new, known or not, has an unknown format.
    for set in [
        "SET GLOBAL innodb_default_row_format = COMPACT",
        "SET GLOBAL innodb_default_row_format = 'REDUNDANT'",
        "SET PERSIST innodb_default_row_format = COMPACT",
        "SET @@global.innodb_default_row_format = COMPACT",
        "set global INNODB_DEFAULT_ROW_FORMAT=compact",
        "SET GLOBAL innodb_default_row_format = DYNAMIC",
    ] {
        let alter = "ALTER TABLE orders ADD COLUMN coupon VARCHAR(255) NULL;";
        // A table of the schema.
        assert_answer(set, LAB_TABLES, &format!("{set};\n{alter}"), UNKNOWN);
        // A table that kilit does not know.
        assert_answer(set, "", &format!("{set};\n{alter}"), UNKNOWN);
        // A table created before and after the setting.
        let create = "CREATE TABLE orders (id BIGINT UNSIGNED PRIMARY KEY, \
                      amount INT NOT NULL) DEFAULT CHARSET=utf8mb4;";
        assert_answer(set, "", &format!("{create}\n{set};\n{alter}"), UNKNOWN);
        assert_answer(set, "", &format!("{set};\n{create}\n{alter}"), UNKNOWN);
        // A table that lost its definition.
        assert_answer(
            set,
            LAB_TABLES,
            &format!("ALTER TABLE orders AUTO_INCREMENT = 5;\n{set};\n{alter}"),
            UNKNOWN,
        );
        // What ran before the setting keeps its answer.
        let answers = on_mysql_84(LAB_TABLES, &format!("{alter}\n{set};\n{alter}"));
        assert_eq!(answers.first().map(String::as_str), Some(INSTANT), "{set}");
        assert_eq!(answers.last().map(String::as_str), Some(UNKNOWN), "{set}");
    }
}

#[test]
fn after_innodb_default_row_format_a_dynamic_table_keeps_all_its_answers_and_a_compact_one_all_but_its_indexes()
 {
    // Decision 5: a written ROW_FORMAT does not depend on the
    // default. kilit has no key-byte rule on DYNAMIC, so its index answer
    // stays; on COMPACT the index answers depend on the 767-byte limit and
    // are unknown.
    let set = "SET GLOBAL innodb_default_row_format = COMPACT";
    let seven = [
        (ADD_COLUMN, INSTANT),
        ("DROP COLUMN note", INSTANT),
        ("RENAME COLUMN note TO memo", INSTANT),
        ("ALTER COLUMN amount SET DEFAULT 0", INSTANT),
        ("MODIFY COLUMN amount BIGINT NOT NULL", COPY),
    ];
    let dynamic = lab_with_options(" ROW_FORMAT=DYNAMIC", &[]);
    let compact = lab_with_options(COMPACT_OPTION, &[]);
    for (operation, answer) in seven {
        for schema in [&dynamic, &compact] {
            assert_answer(
                operation,
                schema,
                &format!("{set};\nALTER TABLE orders {operation};"),
                answer,
            );
        }
    }
    for (operation, dynamic_answer) in [
        ("ADD INDEX idx_orders_amount (amount)", INPLACE),
        ("ADD UNIQUE INDEX uq_orders_email (email)", INPLACE),
        ("ADD INDEX idx_email (email)", INPLACE),
    ] {
        assert_answer(
            operation,
            &dynamic,
            &format!("{set};\nALTER TABLE orders {operation};"),
            dynamic_answer,
        );
        assert_answer(
            operation,
            &compact,
            &format!("{set};\nALTER TABLE orders {operation};"),
            UNKNOWN,
        );
    }
}

#[test]
fn naming_innodb_strict_mode_makes_tables_created_after_it_without_a_row_format_unknown() {
    // Decision 5. Tables created before the setting keep their answers.
    let create = "CREATE TABLE t (id BIGINT UNSIGNED PRIMARY KEY, amount INT NOT NULL, \
                  email VARCHAR(255) NULL) DEFAULT CHARSET=utf8mb4";
    for set in [
        "SET innodb_strict_mode = OFF",
        "SET SESSION innodb_strict_mode = 0",
        "SET GLOBAL innodb_strict_mode = OFF",
        "SET @@innodb_strict_mode = ON",
    ] {
        // Created after, no ROW_FORMAT: unknown, even for a rename.
        for operation in ["ADD COLUMN c INT NULL", "RENAME COLUMN amount TO total"] {
            assert_answer(
                set,
                "",
                &format!("{set};\n{create};\nALTER TABLE t {operation};"),
                UNKNOWN,
            );
        }
        // Created before: today's answers.
        assert_answer(
            set,
            "",
            &format!("{create};\n{set};\nALTER TABLE t ADD COLUMN c INT NULL;"),
            INSTANT,
        );
        assert_answer(
            set,
            LAB_TABLES,
            &format!("{set};\nALTER TABLE orders ADD COLUMN c INT NULL;"),
            INSTANT,
        );
        assert_answer(
            set,
            LAB_TABLES,
            &format!("{set};\nALTER TABLE orders ADD INDEX idx_orders_amount (amount);"),
            INPLACE,
        );
    }
}

#[test]
fn naming_innodb_strict_mode_makes_index_answers_on_other_row_formats_unknown() {
    // Decision 5: without strict mode MySQL cuts a too long key instead of
    // failing with 1071. The tables are created after the setting.
    let set = "SET SESSION innodb_strict_mode = OFF";
    for options in [COMPACT_OPTION, REDUNDANT_OPTION, COMPRESSED] {
        let create = format!(
            "CREATE TABLE t (id BIGINT UNSIGNED PRIMARY KEY, amount INT NOT NULL, \
             email VARCHAR(255) NULL) DEFAULT CHARSET=utf8mb4{options}"
        );
        for operation in [
            "ADD INDEX idx_amount (amount)",
            "ADD INDEX idx_email (email)",
            "ADD UNIQUE INDEX uq_email (email)",
        ] {
            assert_answer(
                &format!("{options}: {operation}"),
                "",
                &format!("{set};\n{create};\nALTER TABLE t {operation};"),
                UNKNOWN,
            );
        }
    }
    // The answers that depend on no key limit stay: COMPACT adds a column
    // instantly, COMPRESSED rebuilds.
    let compact = format!(
        "{set};\nCREATE TABLE t (id BIGINT UNSIGNED PRIMARY KEY, amount INT NOT NULL) \
         DEFAULT CHARSET=utf8mb4{COMPACT_OPTION};\nALTER TABLE t ADD COLUMN c INT NULL;"
    );
    assert_answer("compact", "", &compact, INSTANT);
    let compressed = format!(
        "{set};\nCREATE TABLE t (id BIGINT UNSIGNED PRIMARY KEY, amount INT NOT NULL) \
         DEFAULT CHARSET=utf8mb4{COMPRESSED};\nALTER TABLE t ADD COLUMN c INT NULL;"
    );
    assert_answer("compressed", "", &compressed, INPLACE_REBUILD);
}

#[test]
fn an_alter_that_writes_a_row_format_or_block_size_on_a_table_kilit_does_not_know_leaves_it_unknown()
 {
    // Decision 4 of step 9b, kept for a table kilit cannot see (step 11b):
    // the statement is read, but nothing says what the table was, so it
    // may have a row format other than DYNAMIC.
    let lost = format!("{LAB_TABLES}ALTER TABLE orders AUTO_INCREMENT = 5;");
    let then = "ALTER TABLE orders ADD COLUMN coupon VARCHAR(255) NULL;";
    for schema in [lost.as_str(), ""] {
        for option in [
            "ROW_FORMAT=COMPRESSED",
            "row_format=compressed",
            "KEY_BLOCK_SIZE=8",
        ] {
            let sql = format!("ALTER TABLE orders {option};\n{then}");
            assert_every_answer(option, schema, &sql, &[UNKNOWN, UNKNOWN]);
            for server in [MYSQL_80, MYSQL_84] {
                let reason = unknown_reason_on(server, schema, &sql);
                assert!(
                    reason.contains("may have a row format other than DYNAMIC"),
                    "{server}: {option}: {reason}"
                );
            }
        }
    }
}

#[test]
fn an_unreadable_alter_that_writes_neither_keeps_the_answer_of_a_dynamic_table() {
    let lose = "ALTER TABLE orders AUTO_INCREMENT = 5;";
    let then = "ALTER TABLE orders ADD COLUMN coupon VARCHAR(255) NULL;";
    assert_answer("dynamic", LAB_TABLES, &format!("{lose}\n{then}"), INSTANT);
    assert_answer(
        "dynamic written",
        &lab_with_options(" ROW_FORMAT=DYNAMIC", &[]),
        &format!("{lose}\n{then}"),
        INSTANT,
    );
}

#[test]
fn a_known_compressed_compact_or_redundant_table_lost_by_any_unreadable_statement_is_unknown() {
    let lose = "ALTER TABLE orders AUTO_INCREMENT = 5;";
    let then = "ALTER TABLE orders ADD COLUMN coupon VARCHAR(255) NULL;";
    for options in [
        COMPRESSED,
        COMPACT_OPTION,
        REDUNDANT_OPTION,
        " KEY_BLOCK_SIZE=8",
    ] {
        assert_answer(
            options,
            &lab_with_options(options, &[]),
            &format!("{lose}\n{then}"),
            UNKNOWN,
        );
    }
}

#[test]
fn a_new_compressed_table_is_never_waited_for_and_a_new_compact_table_can_still_fail() {
    // Decision 9: the new-table rule uses the measurement for the table's
    // row format.
    let create = |options: &str| {
        format!(
            "CREATE TABLE t (id BIGINT UNSIGNED PRIMARY KEY, amount INT NOT NULL, \
             code VARCHAR(255) NOT NULL) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4{options};\n"
        )
    };
    // The measured success on a new table: new_table.
    assert_eq!(
        impacts_of(&format!(
            "{}ALTER TABLE t ADD COLUMN c INT NULL;",
            create(COMPRESSED)
        )),
        [Impact::NotChecked, Impact::NewTable]
    );
    assert_eq!(
        impacts_of(&format!(
            "{}ALTER TABLE t ADD COLUMN c INT NULL;",
            create(COMPACT_OPTION)
        )),
        [Impact::NotChecked, Impact::NewTable]
    );
    // The measured failure on a new table: still fails.
    assert_eq!(
        impacts_of(&format!(
            "{}ALTER TABLE t ADD INDEX idx_code (code);",
            create(COMPACT_OPTION)
        )),
        [Impact::NotChecked, Impact::Fails]
    );
    assert_answer(
        "answer",
        "",
        &format!(
            "{}ALTER TABLE t ADD INDEX idx_code (code);",
            create(REDUNDANT_OPTION)
        ),
        FAILS_1071,
    );
    // What is not measured stays unknown there too; the default row format
    // keeps its answers.
    assert_eq!(
        impacts_of(&format!(
            "{}ALTER TABLE t MODIFY COLUMN amount SMALLINT NOT NULL;",
            create(COMPRESSED)
        )),
        [Impact::NotChecked, Impact::Unknown]
    );
    assert_eq!(
        impacts_of(&format!(
            "{}ALTER TABLE t ADD INDEX idx_code (code);",
            create("")
        )),
        [Impact::NotChecked, Impact::NewTable]
    );
}

/// The clauses an ALTER can end with besides the operation.
const NO_ALGORITHM: [&str; 2] = ["", ", ALGORITHM=DEFAULT"];
const LOCK_NONE_ONLY: [&str; 2] = [", LOCK=NONE", ", ALGORITHM=DEFAULT, LOCK=NONE"];
const FORCED: [&str; 6] = [
    ", ALGORITHM=INSTANT",
    ", ALGORITHM=INPLACE",
    ", ALGORITHM=INPLACE, LOCK=NONE",
    ", ALGORITHM=COPY",
    ", ALGORITHM=COPY, LOCK=NONE",
    ", ALGORITHM=INSTANT, LOCK=NONE",
];

/// Every way the hand tests turned old_alter_table on for the session.
const OLD_ALTER_TABLE_ON: [&str; 8] = [
    "SET SESSION old_alter_table = ON",
    "SET old_alter_table = 1",
    "SET @@old_alter_table = ON",
    "SET @@session.old_alter_table = TRUE",
    "SET old_alter_table = 'ON'",
    "SET old_alter_table = 'on'",
    "SET LOCAL old_alter_table = ON",
    "SET old_alter_table = ON, foreign_key_checks = 0",
];

const OLD_ALTER_TABLE_OFF: [&str; 5] = [
    "SET old_alter_table = OFF",
    "SET old_alter_table = 0",
    "SET old_alter_table = FALSE",
    "SET old_alter_table = 'OFF'",
    "SET @@session.old_alter_table = off",
];

#[test]
fn with_old_alter_table_on_the_eight_operations_copy_the_table_in_every_spelling() {
    // old-alter-table-* scenarios: as written and ALGORITHM=DEFAULT copy
    // the table and writes wait.
    for set in OLD_ALTER_TABLE_ON {
        for (name, setup, operation) in EIGHT {
            for clause in NO_ALGORITHM {
                assert_answer(
                    &format!("{set}: {name}{clause}"),
                    &lab_after(setup),
                    &format!("{set};\nALTER TABLE orders {operation}{clause};"),
                    COPY,
                );
            }
        }
    }
}

#[test]
fn with_old_alter_table_on_lock_none_alone_fails_with_1846() {
    // old-alter-table-*: LOCK=NONE with no ALGORITHM, and with
    // ALGORITHM=DEFAULT, gave 1846 "COPY algorithm requires a lock".
    for set in OLD_ALTER_TABLE_ON {
        for (name, setup, operation) in EIGHT {
            for clause in LOCK_NONE_ONLY {
                assert_answer(
                    &format!("{set}: {name}{clause}"),
                    &lab_after(setup),
                    &format!("{set};\nALTER TABLE orders {operation}{clause};"),
                    FAILS_1846,
                );
            }
        }
    }
}

#[test]
fn with_old_alter_table_on_a_written_algorithm_wins_and_gets_the_dynamic_answer() {
    // The manual: a written ALGORITHM other than DEFAULT takes precedence.
    // The forced runs of the old-alter-table scenarios equal their twins'.
    for (set, operation, answer) in [
        ("SET SESSION old_alter_table = ON", ADD_COLUMN, INSTANT),
        (
            "SET SESSION old_alter_table = ON",
            "ADD INDEX idx_orders_amount (amount)",
            INPLACE,
        ),
    ] {
        assert_orders(
            set,
            &lab_after(&[]),
            &format!("{operation}, ALGORITHM=INPLACE"),
            if operation == ADD_COLUMN {
                INPLACE_REBUILD
            } else {
                answer
            },
        );
        // Prefixed with the SET statement.
        assert_answer(
            set,
            &lab_after(&[]),
            &format!("{set};\nALTER TABLE orders {operation}, ALGORITHM=INPLACE;"),
            if operation == ADD_COLUMN {
                INPLACE_REBUILD
            } else {
                answer
            },
        );
    }
    let on = "SET SESSION old_alter_table = ON;\n";
    for (operation, answer) in [
        (
            "ADD COLUMN coupon VARCHAR(255) NULL, ALGORITHM=INSTANT",
            INSTANT,
        ),
        (
            "ADD COLUMN coupon VARCHAR(255) NULL, ALGORITHM=INPLACE",
            INPLACE_REBUILD,
        ),
        (
            "ADD INDEX idx_orders_amount (amount), ALGORITHM=INSTANT",
            FAILS_1845,
        ),
        (
            "ADD INDEX idx_orders_amount (amount), ALGORITHM=INPLACE",
            INPLACE,
        ),
        (
            "ADD INDEX idx_orders_amount (amount), ALGORITHM=INPLACE, LOCK=NONE",
            INPLACE,
        ),
        ("RENAME COLUMN note TO memo, ALGORITHM=INSTANT", INSTANT),
    ] {
        assert_answer(
            operation,
            &lab_after(&[]),
            &format!("{on}ALTER TABLE orders {operation};"),
            answer,
        );
    }
}

#[test]
fn with_old_alter_table_on_every_forced_algorithm_gives_the_answer_it_gives_with_it_off() {
    // Hand tests, both servers: for all eight operations the forced runs
    // under ON equal the runs under OFF.
    for set in OLD_ALTER_TABLE_ON {
        for (name, setup, operation) in EIGHT {
            for clause in FORCED {
                let alter = format!("ALTER TABLE orders {operation}{clause};");
                let schema = lab_after(setup);
                assert_eq!(
                    on_both(&schema, &format!("{set};\n{alter}")),
                    on_both(&schema, &alter),
                    "{set}: {name}{clause}"
                );
            }
        }
    }
}

#[test]
fn old_alter_table_stays_on_for_every_later_alter_until_it_is_turned_off() {
    let sql = "SET SESSION old_alter_table = ON;
               ALTER TABLE orders ADD COLUMN coupon VARCHAR(255) NULL;
               ALTER TABLE orders RENAME COLUMN note TO memo;";
    for answers in all_on_both(LAB_TABLES, sql) {
        assert_eq!(&answers[1..], [COPY, COPY], "{answers:?}");
    }
}

#[test]
fn turning_old_alter_table_off_again_gives_the_normal_answers() {
    for off in OLD_ALTER_TABLE_OFF {
        for (operation, answer) in [
            (ADD_COLUMN, INSTANT),
            ("RENAME COLUMN note TO memo", INSTANT),
            ("ADD INDEX idx_orders_amount (amount)", INPLACE),
            ("MODIFY COLUMN amount BIGINT NOT NULL", COPY),
        ] {
            // Turned on, then off.
            assert_answer(
                &format!("{off}: {operation}"),
                LAB_TABLES,
                &format!(
                    "SET SESSION old_alter_table = ON;\n{off};\nALTER TABLE orders {operation};"
                ),
                answer,
            );
            // Off from the start.
            assert_answer(
                &format!("{off}: {operation}"),
                LAB_TABLES,
                &format!("{off};\nALTER TABLE orders {operation};"),
                answer,
            );
        }
        // LOCK=NONE works again, too.
        assert_orders(
            off,
            LAB_TABLES,
            "ADD INDEX idx_orders_amount (amount), LOCK=NONE",
            INPLACE,
        );
    }
}

#[test]
fn old_alter_table_set_in_a_way_kilit_cannot_follow_makes_every_later_alter_unknown() {
    // Decision 1: GLOBAL, PERSIST, PERSIST_ONLY, DEFAULT, a value that is
    // neither on nor off, and a multi-assignment that names the global
    // variable. Hand tests: SET GLOBAL is not seen by the session, DEFAULT
    // takes the global value, = 2 and = YES give 1231.
    for set in [
        "SET GLOBAL old_alter_table = ON",
        "SET GLOBAL old_alter_table = OFF",
        "SET PERSIST old_alter_table = ON",
        "SET PERSIST_ONLY old_alter_table = ON",
        "SET @@global.old_alter_table = ON",
        "SET old_alter_table = DEFAULT",
        "SET old_alter_table = 2",
        "SET old_alter_table = YES",
        "SET foreign_key_checks = 1, @@global.old_alter_table = ON",
    ] {
        for (operation, clause) in [
            (ADD_COLUMN, ""),
            (ADD_COLUMN, ", ALGORITHM=DEFAULT"),
            (ADD_COLUMN, ", ALGORITHM=INSTANT"),
            (ADD_COLUMN, ", ALGORITHM=INPLACE"),
            (ADD_COLUMN, ", ALGORITHM=COPY"),
            (ADD_COLUMN, ", LOCK=NONE"),
            ("RENAME COLUMN note TO memo", ", ALGORITHM=INSTANT"),
            ("ADD INDEX idx_orders_amount (amount)", ""),
            ("MODIFY COLUMN amount BIGINT NOT NULL", ""),
        ] {
            let alter = format!("ALTER TABLE orders {operation}{clause};");
            assert_answer(
                &format!("{set}: {operation}{clause}"),
                LAB_TABLES,
                &format!("{set};\n{alter}"),
                UNKNOWN,
            );
            // Sticky: neither a later OFF nor a later ON brings it back.
            for later in ["SET old_alter_table = OFF", "SET old_alter_table = ON"] {
                assert_answer(
                    &format!("{set}, then {later}: {operation}{clause}"),
                    LAB_TABLES,
                    &format!("{set};\n{later};\n{alter}"),
                    UNKNOWN,
                );
            }
        }
    }
}

#[test]
fn what_ran_before_an_unreadable_old_alter_table_setting_keeps_its_answer() {
    assert_every_answer(
        "before",
        LAB_TABLES,
        "ALTER TABLE orders ADD COLUMN coupon VARCHAR(255) NULL;
         SET GLOBAL old_alter_table = ON;
         ALTER TABLE orders ADD COLUMN other VARCHAR(255) NULL;",
        &[INSTANT, "not checked", UNKNOWN],
    );
}

#[test]
fn with_old_alter_table_on_a_change_outside_the_eight_is_unknown_even_with_an_algorithm() {
    let set = "SET SESSION old_alter_table = ON";
    for (name, setup, operation) in [
        ("add foreign key", &[][..], MYISAM_ADD_FK),
        ("add fulltext", &[][..], "ADD FULLTEXT INDEX ft_note (note)"),
        (
            "lengthen a varchar",
            &[][..],
            "MODIFY COLUMN note VARCHAR(100) NULL",
        ),
        (
            "int to smallint",
            &[][..],
            "MODIFY COLUMN amount SMALLINT NOT NULL",
        ),
        (
            "change that renames",
            &[][..],
            "CHANGE COLUMN note memo VARCHAR(50) NULL",
        ),
        (
            "an expression default",
            &[][..],
            "ADD COLUMN token CHAR(36) NULL DEFAULT (UUID())",
        ),
        (
            "drop an indexed column",
            &[INDEX_ON_AMOUNT][..],
            "DROP COLUMN amount",
        ),
    ] {
        for clause in ["", ", ALGORITHM=INPLACE", ", ALGORITHM=INSTANT"] {
            assert_answer(
                &format!("{name}{clause}"),
                &lab_after(setup),
                &format!("{set};\nALTER TABLE orders {operation}{clause};"),
                UNKNOWN,
            );
        }
    }
}

#[test]
fn with_old_alter_table_on_a_table_kilit_cannot_vouch_for_is_unknown() {
    let set = "SET SESSION old_alter_table = ON;\n";
    let alter = format!("ALTER TABLE orders {ADD_COLUMN}");
    for clause in ["", ", ALGORITHM=INSTANT"] {
        let sql = format!("{set}{alter}{clause};");
        // MyISAM (decision 6), COMPRESSED, COMPACT and REDUNDANT (decision 8).
        for (name, schema) in [
            ("myisam", lab_tables_with(Some("MyISAM"), "")),
            ("compressed", lab_with_options(COMPRESSED, &[])),
            ("key block size", lab_with_options(" KEY_BLOCK_SIZE=8", &[])),
            ("compact", lab_with_options(COMPACT_OPTION, &[])),
            ("redundant", lab_with_options(REDUNDANT_OPTION, &[])),
            // Decision 8: a FULLTEXT index changes the DYNAMIC answer.
            (
                "fulltext",
                dump_orders("", ",\n  FULLTEXT KEY `ft_note` (`note`)"),
            ),
            // A table kilit has not seen.
            ("missing", String::new()),
        ] {
            assert_answer(&format!("{name}{clause}"), &schema, &sql, UNKNOWN);
        }
    }
}

#[test]
fn laravel_s_foreign_key_checks_lines_are_not_mistaken_for_old_alter_table() {
    // The set forms Laravel writes do not name old_alter_table.
    assert_eq!(
        on_mysql_84(
            "",
            "SET FOREIGN_KEY_CHECKS=0;
             alter table `orders` add `coupon` varchar(255) null;
             SET FOREIGN_KEY_CHECKS=1;"
        ),
        ["not checked", INSTANT, "not checked"]
    );
}

// Regression tests for what the step 9b review found. The server runs are
// the review's: the results below are what MySQL 8.0.46 and 8.4.13 did.

#[test]
fn a_global_keyword_in_front_of_a_list_carries_to_old_alter_table_and_makes_it_unknown() {
    // SET GLOBAL foreign_key_checks = 1, old_alter_table = ON: the GLOBAL
    // keyword applies to every assignment of the list, the session keeps
    // old_alter_table = OFF. kilit cannot tell which variable is which.
    for set in [
        "SET GLOBAL foreign_key_checks = 1, old_alter_table = ON",
        "SET PERSIST foreign_key_checks = 1, old_alter_table = ON",
        "SET PERSIST_ONLY foreign_key_checks = 1, old_alter_table = ON",
        "SET GLOBAL old_alter_table = ON, foreign_key_checks = 1",
    ] {
        assert_answer(
            set,
            LAB_TABLES,
            &format!("{set};\nALTER TABLE orders {ADD_COLUMN};"),
            UNKNOWN,
        );
    }
}

#[test]
fn a_string_value_the_server_refuses_for_old_alter_table_makes_every_later_alter_unknown() {
    // MySQL gives 1231 for '1', 'TRUE', '0' and 'FALSE' and keeps the old
    // value; only the words ON and OFF are accepted as strings.
    for value in ["'1'", "'TRUE'", "'true'", "'0'", "'FALSE'", "'false'"] {
        assert_answer(
            value,
            LAB_TABLES,
            &format!(
                "SET SESSION old_alter_table = ON;\nSET old_alter_table = {value};\n\
                 ALTER TABLE orders {ADD_COLUMN};"
            ),
            UNKNOWN,
        );
    }
    // 'OFF' in any letter case turns it off.
    for value in ["'OFF'", "'off'", "'Off'", "'oFF'"] {
        assert_answer(
            value,
            LAB_TABLES,
            &format!(
                "SET SESSION old_alter_table = ON;\nSET old_alter_table = {value};\n\
                 ALTER TABLE orders {ADD_COLUMN};"
            ),
            INSTANT,
        );
    }
}

#[test]
fn a_changed_sql_mode_makes_an_index_on_a_compact_table_unknown() {
    // MySQL shortens the key and gives warning 1071 instead of failing when
    // sql_mode is not strict.
    for options in [COMPACT_OPTION, REDUNDANT_OPTION] {
        let schema = lab_with_options(options, &[]);
        for operation in [
            "ADD INDEX idx_email (email)",
            "ADD UNIQUE INDEX uq_email (email)",
        ] {
            assert_answer(
                &format!("{options}: {operation}"),
                &schema,
                &format!("SET SESSION sql_mode = '';\nALTER TABLE orders {operation};"),
                UNKNOWN,
            );
        }
    }
}

#[test]
fn a_descending_index_on_a_compact_table_is_unknown() {
    for options in [COMPACT_OPTION, REDUNDANT_OPTION] {
        let schema = lab_with_options(options, &[]);
        for operation in [
            "ADD INDEX idx_email (email DESC)",
            "ADD INDEX idx_email (email(100) DESC)",
        ] {
            assert_answer(
                &format!("{options}: {operation}"),
                &schema,
                &format!("ALTER TABLE orders {operation};"),
                UNKNOWN,
            );
        }
    }
}

#[test]
fn a_1071_on_a_compact_table_has_no_note_about_duplicate_values() {
    // The key is too long whatever the data holds: the only note is the
    // assumption of a strict SQL mode.
    let schema = lab_with_options(COMPACT_OPTION, &[]);
    for operation in [
        "ALTER TABLE orders ADD UNIQUE INDEX uq_email (email);",
        "ALTER TABLE orders ADD INDEX idx_email (email);",
    ] {
        for server in [MYSQL_80, MYSQL_84] {
            match predictions_on(server, &schema, operation).pop().unwrap() {
                Prediction::Measured {
                    verdict: Verdict::Fails { code, .. },
                    notes,
                    ..
                } => {
                    assert_eq!(code, 1071, "{operation} on {server}");
                    assert_eq!(notes.len(), 1, "{operation} on {server}: {notes:?}");
                    assert!(
                        notes[0].contains("strict SQL mode"),
                        "{operation} on {server}: {notes:?}"
                    );
                }
                other => panic!("{operation} on {server}: {other:?}"),
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Step 11b: ALTER TABLE statements that only set table options, and how a SET
// reaches the session.
//
// The answers for COMMENT come from the lab's hand tests on both servers
// (manual/step11b, results.tsv) and the scenario set-table-comment: as written,
// ALGORITHM=INPLACE, INPLACE with LOCK=NONE and LOCK=NONE run INPLACE with no
// rebuild and writes continue; ALGORITHM=INSTANT fails with 1845; COPY
// rebuilds and writes wait. Laravel's `$table->comment('...')` writes
// `alter table `t` comment = '...'` as a statement of its own.
// ---------------------------------------------------------------------------

/// The first statement of `sql` has no answer: the reason on `server`.
fn first_reason_on(server: &str, schema: &str, sql: &str) -> String {
    let results = Knowledge::built_in().server(server).unwrap();
    let findings = check(&parse(sql), &Schema::from_sql(schema), results);
    match &findings[0].prediction {
        Prediction::Unknown { reason } => reason.clone(),
        other => panic!("{server}: {sql}: {other:?}"),
    }
}

/// Whether a measured answer carries the note about a FULLTEXT index kilit
/// could not rule out.
fn mentions_fulltext(prediction: &Prediction) -> bool {
    notes_of(prediction)
        .iter()
        .any(|note| note.contains("FULLTEXT"))
}

/// The lab's record of a forced COPY says the table was copied and does not
/// say whether writes wait.
const COPY_NOT_MEASURED: &str = "COPY, copies every row, writes not measured";
const ADD_C: &str = "ALTER TABLE orders ADD COLUMN c INT NULL;";
const NOTHING_MEASURED: &str = "has not been measured yet";

#[test]
fn the_lab_s_table_comment_scenario_records_what_the_hand_tests_found() {
    // Hand tests: ALTER TABLE orders COMMENT = 'x' on an InnoDB DYNAMIC table.
    for results in Knowledge::built_in().servers() {
        let server = results.server.name();
        let scenario = results
            .scenario("set-table-comment")
            .unwrap_or_else(|| panic!("set-table-comment was not measured on {server}"));
        for (variant, expected) in [
            (Variant::SERVER_DEFAULT, INPLACE),
            (Variant::forced(kilit_core::Algorithm::Instant), FAILS_1845),
            (Variant::forced(kilit_core::Algorithm::Inplace), INPLACE),
            (Variant::forced(kilit_core::Algorithm::Copy), COPY),
            (Variant::LOCK_NONE, INPLACE),
        ] {
            let verdict = scenario.verdict_for(variant).unwrap();
            let shown = match &verdict {
                Verdict::Fails { code, .. } => format!("fails with error {code}"),
                other => other.to_string(),
            };
            // The lab's own record of a COPY run does not say whether writes
            // wait; it says the table was copied.
            if expected == COPY {
                assert!(
                    shown.starts_with("COPY, copies every row"),
                    "{server} as {variant}: {shown}"
                );
            } else {
                assert_eq!(shown, expected, "{server} as {variant}");
            }
        }
    }
}

#[test]
fn a_table_comment_runs_inplace_with_no_rebuild_and_writes_continue_in_every_spelling() {
    // $table->comment('Orders placed by users');
    for option in [
        "COMMENT = 'x'",
        "COMMENT 'x'",
        "comment='x'",
        "Comment = 'x'",
        "COMMENT = ''",
        "COMMENT = 'Orders placed by users'",
        "COMMENT = 'It''s a note'",
        "COMMENT = 'one; two'",
    ] {
        assert_orders(option, LAB_TABLES, option, INPLACE);
    }
    // The form Laravel prints, with a backticked table.
    assert_answer(
        "laravel",
        LAB_TABLES,
        "alter table `orders` comment = 'Orders placed by users';",
        INPLACE,
    );
}

#[test]
fn a_table_comment_with_a_backslash_escaped_quote_is_one_string() {
    // MySQL reads \' inside a string as a quote (no ANSI mode).
    assert_orders(
        "backslash",
        LAB_TABLES,
        r"COMMENT = 'it\'s, ENGINE=MyISAM'",
        INPLACE,
    );
}

#[test]
fn forcing_a_variant_on_a_table_comment_gets_the_answer_of_exactly_that_variant() {
    for (clause, answer) in [
        (", ALGORITHM=INSTANT", FAILS_1845),
        (", ALGORITHM=INPLACE", INPLACE),
        (", ALGORITHM=INPLACE, LOCK=NONE", INPLACE),
        (", ALGORITHM=COPY", COPY_NOT_MEASURED),
        (", LOCK=NONE", INPLACE),
        (", ALGORITHM=DEFAULT", INPLACE),
        (", ALGORITHM=DEFAULT, LOCK=NONE", INPLACE),
        (", algorithm = inplace", INPLACE),
        (", LOCK=NONE, ALGORITHM=INPLACE", INPLACE),
    ] {
        for comment in ["COMMENT = 'x'", "COMMENT 'x'"] {
            assert_orders(
                &format!("{comment}{clause}"),
                LAB_TABLES,
                &format!("{comment}{clause}"),
                answer,
            );
        }
    }
}

#[test]
fn a_comment_string_holding_option_words_is_text_and_changes_nothing() {
    for comment in [
        "ROW_FORMAT=COMPACT, ENGINE=MyISAM",
        "KEY_BLOCK_SIZE=8",
        "x', ALGORITHM=INSTANT",
        "engine=MyISAM row_format=compressed",
    ] {
        let comment = comment.replace('\'', "''");
        let sql = format!("ALTER TABLE orders COMMENT = '{comment}';\n{ADD_C}");
        assert_every_answer(&comment, LAB_TABLES, &sql, &[INPLACE, INSTANT]);
    }
}

#[test]
fn a_table_comment_leaves_the_table_known_and_free_of_the_fulltext_note() {
    // Before step 11b the unreadable statement lost the table, and a later
    // ADD COLUMN carried the note about a FULLTEXT index kilit could not see.
    let sql = format!("ALTER TABLE orders COMMENT = 'x';\n{ADD_C}");
    for server in [MYSQL_80, MYSQL_84] {
        let predictions = predictions_on(server, LAB_TABLES, &sql);
        assert_eq!(predictions.len(), 2);
        assert!(!mentions_fulltext(&predictions[1]), "{server}");
        assert!(
            matches!(
                Schema::from_sql(&format!("{LAB_TABLES}ALTER TABLE orders COMMENT = 'x';"))
                    .table("orders"),
                TableState::Known(_)
            ),
            "{server}"
        );
    }
    // The contrast: a statement kilit cannot follow does lose it.
    let lost = format!("ALTER TABLE orders AUTO_INCREMENT = 5;\n{ADD_C}");
    for server in [MYSQL_80, MYSQL_84] {
        let predictions = predictions_on(server, LAB_TABLES, &lost);
        assert!(mentions_fulltext(&predictions[1]), "{server}");
    }
}

#[test]
fn a_known_table_with_a_fulltext_index_keeps_it_through_a_table_comment() {
    let schema = format!("{LAB_TABLES}ALTER TABLE orders ADD FULLTEXT INDEX ft_note (note);\n");
    // The FULLTEXT index is still seen: a new column copies the table.
    assert_answer(
        "comment, then add column",
        &schema,
        &format!("ALTER TABLE orders COMMENT = 'x';\n{ADD_C}"),
        COPY,
    );
}

#[test]
fn the_column_changes_after_a_table_comment_keep_their_answers() {
    for (operation, answer) in [
        ("DROP COLUMN note", INSTANT),
        ("RENAME COLUMN note TO memo", INSTANT),
        ("MODIFY COLUMN amount BIGINT NOT NULL", COPY),
        ("ADD INDEX idx_orders_amount (amount)", INPLACE),
    ] {
        assert_answer(
            operation,
            LAB_TABLES,
            &format!("ALTER TABLE orders COMMENT = 'x';\nALTER TABLE orders {operation};"),
            answer,
        );
    }
}

#[test]
fn a_table_comment_on_myisam_a_non_dynamic_row_format_or_old_alter_table_is_unknown() {
    // Decision 2: the lab measured only an InnoDB DYNAMIC table with
    // old_alter_table off.
    let comment = "ALTER TABLE orders COMMENT = 'x';";
    assert_answer("myisam", &myisam_lab_after(&[]), comment, UNKNOWN);
    for options in [
        COMPRESSED,
        COMPACT_OPTION,
        REDUNDANT_OPTION,
        " KEY_BLOCK_SIZE=8",
    ] {
        assert_answer(options, &lab_with_options(options, &[]), comment, UNKNOWN);
    }
    for set in OLD_ALTER_TABLE_ON {
        assert_answer(set, LAB_TABLES, &format!("{set};\n{comment}"), UNKNOWN);
    }
    // Turned off again, the measured answer is back.
    assert_answer(
        "off again",
        LAB_TABLES,
        &format!("SET old_alter_table = ON;\nSET old_alter_table = OFF;\n{comment}"),
        INPLACE,
    );
}

#[test]
fn a_table_comment_on_a_table_kilit_does_not_know_changes_nothing_about_it() {
    let lost = format!("{LAB_TABLES}ALTER TABLE orders AUTO_INCREMENT = 5;");
    let sql = format!("ALTER TABLE orders COMMENT = 'x';\n{ADD_C}");
    // A missing table stays missing: today's InnoDB answers.
    assert_every_answer("missing", "", &sql, &[INPLACE, INSTANT]);
    // A lost table stays lost: its later statement keeps the FULLTEXT note.
    assert_every_answer("lost", &lost, &sql, &[INPLACE, INSTANT]);
    for server in [MYSQL_80, MYSQL_84] {
        let predictions = predictions_on(server, &lost, &sql);
        assert!(mentions_fulltext(&predictions[1]), "{server}");
    }
}

#[test]
fn an_option_alter_that_names_the_engine_a_row_format_or_a_block_size_is_unknown_and_says_so() {
    for (options, names) in [
        ("ENGINE=InnoDB", "ENGINE"),
        ("ENGINE InnoDB", "ENGINE"),
        ("engine = innodb", "ENGINE"),
        ("ENGINE=MyISAM", "ENGINE"),
        ("ROW_FORMAT=DYNAMIC", "ROW_FORMAT"),
        ("row_format = compressed", "ROW_FORMAT"),
        ("KEY_BLOCK_SIZE=8", "KEY_BLOCK_SIZE"),
        ("KEY_BLOCK_SIZE 0", "KEY_BLOCK_SIZE"),
        ("COMMENT = 'x', ENGINE=InnoDB", "ENGINE"),
        ("ENGINE=InnoDB COMMENT = 'x'", "ENGINE"),
        (
            "ROW_FORMAT=COMPRESSED KEY_BLOCK_SIZE=8",
            "ROW_FORMAT and KEY_BLOCK_SIZE",
        ),
        (
            "KEY_BLOCK_SIZE=8, ROW_FORMAT=COMPRESSED",
            "ROW_FORMAT and KEY_BLOCK_SIZE",
        ),
        ("ENGINE=InnoDB, ROW_FORMAT=DYNAMIC", "ENGINE and ROW_FORMAT"),
        ("ROW_FORMAT=DYNAMIC, ENGINE=InnoDB", "ENGINE and ROW_FORMAT"),
        (
            "KEY_BLOCK_SIZE=0, ENGINE=InnoDB",
            "ENGINE and KEY_BLOCK_SIZE",
        ),
        (
            "KEY_BLOCK_SIZE=8 COMMENT='x' ROW_FORMAT=COMPRESSED ENGINE=InnoDB",
            "ENGINE and ROW_FORMAT and KEY_BLOCK_SIZE",
        ),
        ("ENGINE=InnoDB, ALGORITHM=COPY", "ENGINE"),
        ("ROW_FORMAT=DYNAMIC, LOCK=NONE", "ROW_FORMAT"),
    ] {
        let sql = format!("ALTER TABLE orders {options};");
        for server in [MYSQL_80, MYSQL_84] {
            assert_eq!(
                first_reason_on(server, LAB_TABLES, &sql),
                format!("changing a table's {names} {NOTHING_MEASURED}"),
                "{server}: {options}"
            );
        }
    }
}

#[test]
fn the_backticked_laravel_form_of_an_engine_change_is_unknown_too() {
    // DB::statement('alter table `orders` engine = InnoDB')
    assert_answer(
        "backticks",
        LAB_TABLES,
        "alter table `orders` engine = InnoDB;",
        UNKNOWN,
    );
}

#[test]
fn after_a_row_format_alter_the_later_statements_get_the_answers_of_that_row_format() {
    let then = format!("ALTER TABLE orders {ADD_COLUMN};");
    // Every way the hand tests made the table COMPRESSED, written in an ALTER.
    for option in COMPRESSED_SPELLINGS.iter().take(12) {
        let sql = format!("ALTER TABLE orders {};\n{then}", option.trim());
        assert_every_answer(option, LAB_TABLES, &sql, &[UNKNOWN, INPLACE_REBUILD]);
    }
    // The comment next to it does not matter.
    for options in [
        "ROW_FORMAT=COMPRESSED, COMMENT='x'",
        "COMMENT='x' ROW_FORMAT=COMPRESSED",
        "COMMENT 'x', ROW_FORMAT COMPRESSED",
    ] {
        let sql = format!("ALTER TABLE orders {options};\n{then}");
        assert_every_answer(options, LAB_TABLES, &sql, &[UNKNOWN, INPLACE_REBUILD]);
    }
    // The last one wins.
    for (options, answer) in [
        ("ROW_FORMAT=COMPRESSED, ROW_FORMAT=DYNAMIC", INSTANT),
        ("ROW_FORMAT=DYNAMIC, ROW_FORMAT=COMPRESSED", INPLACE_REBUILD),
        ("ROW_FORMAT=COMPACT", INSTANT),
        ("ROW_FORMAT=REDUNDANT", INSTANT),
        ("ROW_FORMAT=DYNAMIC", INSTANT),
        ("ROW_FORMAT=DEFAULT", INSTANT),
        ("KEY_BLOCK_SIZE=0", INSTANT),
    ] {
        let sql = format!("ALTER TABLE orders {options};\n{then}");
        assert_every_answer(options, LAB_TABLES, &sql, &[UNKNOWN, answer]);
    }
}

#[test]
fn a_compressed_table_made_by_an_alter_refuses_instant_like_one_created_so() {
    let sql = format!(
        "ALTER TABLE orders KEY_BLOCK_SIZE=8;\nALTER TABLE orders {ADD_COLUMN}, ALGORITHM=INSTANT;"
    );
    assert_every_answer("key block size", LAB_TABLES, &sql, &[UNKNOWN, FAILS_1845]);
}

#[test]
fn a_row_format_or_block_size_the_lab_did_not_measure_leaves_the_later_statements_unknown() {
    let then = format!("ALTER TABLE orders {ADD_COLUMN};");
    for options in [
        "ROW_FORMAT=FIXED",
        "KEY_BLOCK_SIZE=4",
        "KEY_BLOCK_SIZE=1",
        "KEY_BLOCK_SIZE=2",
        "KEY_BLOCK_SIZE=16",
        "ROW_FORMAT=COMPRESSED KEY_BLOCK_SIZE=4",
    ] {
        let sql = format!("ALTER TABLE orders {options};\n{then}");
        assert_every_answer(options, LAB_TABLES, &sql, &[UNKNOWN, UNKNOWN]);
    }
    // ROW_FORMAT=DYNAMIC against KEY_BLOCK_SIZE=8: MySQL refuses it (1478).
    let sql = format!("ALTER TABLE orders ROW_FORMAT=DYNAMIC;\n{then}");
    assert_every_answer(
        "dynamic on a block size of 8",
        &lab_with_options(" KEY_BLOCK_SIZE=8", &[]),
        &sql,
        &[UNKNOWN, UNKNOWN],
    );
}

#[test]
fn a_dump_with_a_compressed_orders_returns_to_dynamic_with_an_alter() {
    let sql = format!("ALTER TABLE orders ROW_FORMAT=DYNAMIC;\nALTER TABLE orders {ADD_COLUMN};");
    assert_every_answer(
        "compressed to dynamic",
        &lab_with_options(COMPRESSED, &[]),
        &sql,
        &[UNKNOWN, INSTANT],
    );
}

#[test]
fn after_an_engine_alter_the_later_statements_get_the_answers_of_that_engine() {
    // To MyISAM and back, with no foreign key anywhere near the table.
    for (options, schema, answer) in [
        ("ENGINE=MyISAM", lab_after(&[]), COPY),
        ("engine = MyISAM", lab_after(&[]), COPY),
        ("ENGINE MyISAM", lab_after(&[]), COPY),
        ("ENGINE=InnoDB, ENGINE=MyISAM", lab_after(&[]), COPY),
        ("COMMENT='x', ENGINE=MyISAM", lab_after(&[]), COPY),
        ("ENGINE=InnoDB", myisam_lab_after(&[]), INSTANT),
        (
            "ENGINE=MyISAM, ENGINE=InnoDB",
            myisam_lab_after(&[]),
            INSTANT,
        ),
    ] {
        let sql = format!("ALTER TABLE orders {options};\n{ADD_C}");
        assert_every_answer(options, &schema, &sql, &[UNKNOWN, answer]);
    }
    // Other engines are not measured.
    for options in ["ENGINE=MEMORY", "ENGINE=ARCHIVE", "ENGINE=CSV"] {
        let sql = format!("ALTER TABLE orders {options};\n{ADD_C}");
        assert_every_answer(options, LAB_TABLES, &sql, &[UNKNOWN, UNKNOWN]);
    }
}

#[test]
fn a_changed_engine_shows_in_every_later_change_of_the_table() {
    let sql = "ALTER TABLE orders ENGINE=MyISAM;
        ALTER TABLE orders DROP COLUMN note;
        ALTER TABLE orders RENAME COLUMN amount TO total;
        ALTER TABLE orders ADD INDEX idx_orders_total (total);";
    assert_every_answer("myisam", LAB_TABLES, sql, &[UNKNOWN, COPY, INSTANT, COPY]);
}

#[test]
fn a_change_to_myisam_of_a_table_with_a_foreign_key_or_a_parent_loses_the_table() {
    let child = lab_after(&[&format!("ALTER TABLE orders {MYISAM_ADD_FK}")]);
    for (table, options) in [
        ("orders", "ENGINE=MyISAM"),
        ("users", "ENGINE=MyISAM"),
        ("orders", "ENGINE=MEMORY"),
        ("users", "ENGINE=MEMORY"),
    ] {
        let sql =
            format!("ALTER TABLE {table} {options};\nALTER TABLE {table} ADD COLUMN c INT NULL;");
        assert_every_answer(
            &format!("{table} {options}"),
            &child,
            &sql,
            &[UNKNOWN, UNKNOWN],
        );
    }
    // Still InnoDB: nothing changes.
    for table in ["orders", "users"] {
        let sql = format!(
            "ALTER TABLE {table} ENGINE=InnoDB;\nALTER TABLE {table} ADD COLUMN c INT NULL;"
        );
        assert_every_answer(table, &child, &sql, &[UNKNOWN, INSTANT]);
    }
}

#[test]
fn an_engine_or_row_format_alter_on_a_table_kilit_does_not_know_leaves_it_as_before() {
    let lost = format!("{LAB_TABLES}ALTER TABLE orders AUTO_INCREMENT = 5;");
    for schema in ["", lost.as_str()] {
        // The engine written is the engine the table has.
        assert_every_answer(
            "myisam",
            schema,
            &format!("ALTER TABLE orders ENGINE=MyISAM;\n{ADD_C}"),
            &[UNKNOWN, UNKNOWN],
        );
        assert_every_answer(
            "innodb",
            schema,
            &format!("ALTER TABLE orders ENGINE=InnoDB;\n{ADD_C}"),
            &[UNKNOWN, INSTANT],
        );
        // A comment next to the engine changes nothing else.
        assert_every_answer(
            "myisam and comment",
            schema,
            &format!("ALTER TABLE orders COMMENT = 'x', ENGINE=MyISAM;\n{ADD_C}"),
            &[UNKNOWN, UNKNOWN],
        );
    }
}

/// A statement kilit still cannot read, and whether it loses `orders`.
const UNREADABLE_OPTION_ALTERS: [(&str, bool); 16] = [
    ("ALTER TABLE orders COMMENT='x' ALGORITHM=INSTANT", true),
    ("ALTER TABLE orders ENGINE='InnoDB'", false),
    ("ALTER TABLE orders ROW_FORMAT='DYNAMIC'", false),
    ("ALTER TABLE orders KEY_BLOCK_SIZE='8'", false),
    ("ALTER TABLE orders COMMENT='x', AUTO_INCREMENT=5", true),
    (
        "ALTER TABLE orders COMMENT='x', ADD COLUMN y INT NULL",
        true,
    ),
    ("ALTER TABLE orders COMMENT=_utf8mb4'x'", true),
    ("ALTER TABLE orders COMMENT='a' 'b'", true),
    ("ALTER TABLE orders COMMENT = x", true),
    ("ALTER TABLE orders , COMMENT='x'", true),
    ("ALTER TABLE orders COMMENT='x',", true),
    ("ALTER TABLE orders COMMENT='x',, ALGORITHM=INPLACE", true),
    ("ALTER TABLE orders /*!80000 COMMENT='x' */", true),
    (
        "ALTER TABLE orders COMMENT='x' /*!80000 , ALGORITHM=INPLACE */",
        true,
    ),
    ("ALTER TABLE orders COMMENT='x', COMMENT", true),
    ("ALTER TABLE \"orders\" COMMENT='x'", false),
];

#[test]
fn an_option_alter_kilit_still_cannot_read_is_unknown_and_loses_the_table_as_before() {
    for (statement, loses) in UNREADABLE_OPTION_ALTERS {
        let sql = format!("{statement};\n{ADD_C}");
        for server in [MYSQL_80, MYSQL_84] {
            let reason = first_reason_on(server, LAB_TABLES, &sql);
            assert!(
                reason.starts_with("kilit could not read this statement"),
                "{server}: {statement}: {reason}"
            );
            if loses {
                let predictions = predictions_on(server, LAB_TABLES, &sql);
                assert!(
                    mentions_fulltext(&predictions[1]),
                    "{server}: {statement}: {:?}",
                    predictions[1]
                );
            }
        }
    }
}

#[test]
fn a_quoted_option_value_in_an_alter_is_not_read_as_the_value_it_names() {
    // ENGINE='MyISAM' is accepted by MySQL but kilit does not read it: the
    // table must not get the InnoDB answer afterwards, as for CREATE TABLE.
    assert_every_answer(
        "quoted myisam",
        LAB_TABLES,
        &format!("ALTER TABLE orders ENGINE='MyISAM';\n{ADD_C}"),
        &[UNKNOWN, UNKNOWN],
    );
}

#[test]
fn option_alters_are_followed_by_the_statements_after_a_failed_one() {
    // K11: a measured failure leaves the tables as they were.
    let sql = format!(
        "ALTER TABLE orders COMMENT = 'x', ALGORITHM=INSTANT;\nALTER TABLE orders {ADD_COLUMN}, ALGORITHM=INSTANT;"
    );
    assert_every_answer("failed comment", LAB_TABLES, &sql, &[FAILS_1845, INSTANT]);
}

#[test]
fn a_table_option_alter_from_a_mysqldump_schema_keeps_the_table_known() {
    // `orders` as mysqldump writes it, with no row format.
    let schema = dump_orders("", "");
    let sql = format!("ALTER TABLE `orders` COMMENT = 'x';\nALTER TABLE `orders` {ADD_COLUMN};");
    assert_every_answer("dump", &schema, &sql, &[INPLACE, INSTANT]);
    let compressed =
        format!("ALTER TABLE `orders` ROW_FORMAT=COMPRESSED;\nALTER TABLE `orders` {ADD_COLUMN};");
    assert_every_answer(
        "dump, compressed",
        &schema,
        &compressed,
        &[UNKNOWN, INPLACE_REBUILD],
    );
}

// SET scope. A GLOBAL, PERSIST or PERSIST_ONLY keyword applies to the later
// assignments of the same SET that have no scope of their own; `@@global.`,
// `@@session.` and `@@` apply to their own variable only. Hand tests of step
// 11b: `@@global.x = 1, y = 0` sets y for the session, so does
// `GLOBAL a = 1, @@y = 0`.

const CHECKS_ON_PLAIN: &str =
    "assumes foreign_key_checks is ON, the MySQL default: nothing before this statement sets it";
const CHECKS_ON_GLOBAL_CHANGED: &str = "assumes foreign_key_checks is ON in this session, the \
     MySQL default: a statement before this one changes its global value, which does not change \
     this session";

/// A child column that does not match its parent: the key fails with 3780
/// when the checks are on, and is unknown when they are off.
fn mismatching_key_schema() -> String {
    lab_after(&["ALTER TABLE orders ADD COLUMN ref INT NULL"])
}

#[test]
fn a_global_keyword_carries_to_the_later_assignments_so_the_session_checks_stay_on() {
    let schema = mismatching_key_schema();
    for (set, note) in [
        (
            "SET GLOBAL innodb_lock_wait_timeout = 50, foreign_key_checks = 0",
            CHECKS_ON_GLOBAL_CHANGED,
        ),
        (
            "SET PERSIST innodb_lock_wait_timeout = 50, foreign_key_checks = 0",
            CHECKS_ON_GLOBAL_CHANGED,
        ),
        (
            "SET GLOBAL foreign_key_checks = 0, innodb_lock_wait_timeout = 50",
            CHECKS_ON_GLOBAL_CHANGED,
        ),
        (
            "SET @@global.innodb_lock_wait_timeout = 50, @@global.foreign_key_checks = 0",
            CHECKS_ON_GLOBAL_CHANGED,
        ),
        (
            "SET PERSIST_ONLY innodb_lock_wait_timeout = 50, foreign_key_checks = 0",
            CHECKS_ON_PLAIN,
        ),
    ] {
        let sql = format!("{set};\nALTER TABLE orders {REF};");
        for server in [MYSQL_80, MYSQL_84] {
            let predictions = predictions_on(server, &schema, &sql);
            match predictions.last().unwrap() {
                Prediction::Measured {
                    verdict: Verdict::Fails { code, .. },
                    notes,
                    ..
                } => {
                    assert_eq!(*code, 3780, "{server}: {set}");
                    assert!(
                        notes.iter().any(|n| n == note),
                        "{server}: {set}: {notes:?}"
                    );
                    let other = if note == CHECKS_ON_PLAIN {
                        CHECKS_ON_GLOBAL_CHANGED
                    } else {
                        CHECKS_ON_PLAIN
                    };
                    assert!(
                        !notes.iter().any(|n| n == other),
                        "{server}: {set}: {notes:?}"
                    );
                }
                other => panic!("{server}: {set}: {other:?}"),
            }
        }
    }
}

#[test]
fn a_scope_of_its_own_or_a_session_keyword_turns_the_session_checks_off() {
    let schema = mismatching_key_schema();
    for set in [
        "SET @@global.innodb_lock_wait_timeout = 50, foreign_key_checks = 0",
        "SET GLOBAL innodb_lock_wait_timeout = 50, @@foreign_key_checks = 0",
        "SET GLOBAL innodb_lock_wait_timeout = 50, SESSION foreign_key_checks = 0",
        "SET GLOBAL innodb_lock_wait_timeout = 50, @@session.foreign_key_checks = 0",
        "SET SESSION innodb_lock_wait_timeout = 50, foreign_key_checks = 0",
        "SET LOCAL innodb_lock_wait_timeout = 50, foreign_key_checks = 0",
        "SET innodb_lock_wait_timeout = 50, foreign_key_checks = 0",
    ] {
        // Checks off and a mismatch kilit can see: unknown, as for a plain
        // SET foreign_key_checks = 0.
        assert_answer(
            set,
            &schema,
            &format!("{set};\nALTER TABLE orders {REF};"),
            UNKNOWN,
        );
    }
}

#[test]
fn a_global_set_of_two_variables_leaves_the_checks_on_for_dropping_a_foreign_key_column() {
    let schema = lab_after(&[&format!("ALTER TABLE orders {MYISAM_ADD_FK}")]);
    let drop = "ALTER TABLE orders DROP COLUMN user_id;";
    assert_answer("plain", &schema, drop, FAILS_1828);
    assert_answer(
        "global carries",
        &schema,
        &format!("SET GLOBAL innodb_lock_wait_timeout = 50, foreign_key_checks = 0;\n{drop}"),
        FAILS_1828,
    );
    // With the checks off for the session, the column goes.
    assert_ne!(
        on_both(
            &schema,
            &format!("SET @@global.innodb_lock_wait_timeout = 50, foreign_key_checks = 0;\n{drop}")
        ),
        [FAILS_1828, FAILS_1828]
    );
}

const FAILS_1828: &str = "fails with error 1828";
const FAILS_1674: &str = "fails with error 1674";
const UUID_DEFAULT: &str =
    "ALTER TABLE orders ADD COLUMN token CHAR(36) NOT NULL DEFAULT (UUID());";

#[test]
fn a_global_keyword_carries_to_sql_log_bin_and_a_scope_of_its_own_does_not() {
    // sql_log_bin = 1 or ON keeps the measured answer; a GLOBAL scope makes
    // it unknown, as GLOBAL sql_log_bin alone does.
    assert_answer(
        "global alone",
        LAB_TABLES,
        &format!("SET GLOBAL sql_log_bin = 1;\n{UUID_DEFAULT}"),
        UNKNOWN,
    );
    for set in [
        "SET GLOBAL innodb_lock_wait_timeout = 50, sql_log_bin = 1",
        "SET PERSIST innodb_lock_wait_timeout = 50, sql_log_bin = 1",
        "SET PERSIST_ONLY innodb_lock_wait_timeout = 50, sql_log_bin = 1",
    ] {
        assert_answer(set, LAB_TABLES, &format!("{set};\n{UUID_DEFAULT}"), UNKNOWN);
    }
    for set in [
        "SET @@global.innodb_lock_wait_timeout = 50, sql_log_bin = 1",
        "SET GLOBAL innodb_lock_wait_timeout = 50, @@sql_log_bin = 1",
        "SET GLOBAL innodb_lock_wait_timeout = 50, SESSION sql_log_bin = 1",
    ] {
        assert_answer(
            set,
            LAB_TABLES,
            &format!("{set};\n{UUID_DEFAULT}"),
            FAILS_1674,
        );
    }
    // The session scope turning it off is as before.
    assert_answer(
        "session off",
        LAB_TABLES,
        &format!("SET @@global.innodb_lock_wait_timeout = 50, sql_log_bin = 0;\n{UUID_DEFAULT}"),
        UNKNOWN,
    );
}

#[test]
fn a_global_keyword_carries_to_old_alter_table_in_every_position_and_a_scope_of_its_own_does_not() {
    let add = format!("ALTER TABLE orders {ADD_COLUMN};");
    for set in [
        "SET GLOBAL innodb_lock_wait_timeout = 50, old_alter_table = ON",
        "SET PERSIST innodb_lock_wait_timeout = 50, old_alter_table = ON",
        "SET GLOBAL old_alter_table = ON, innodb_lock_wait_timeout = 50",
    ] {
        assert_answer(set, LAB_TABLES, &format!("{set};\n{add}"), UNKNOWN);
    }
    // The session's old_alter_table is ON: the table is copied.
    for set in [
        "SET @@global.innodb_lock_wait_timeout = 50, old_alter_table = ON",
        "SET GLOBAL innodb_lock_wait_timeout = 50, @@old_alter_table = ON",
        "SET GLOBAL innodb_lock_wait_timeout = 50, SESSION old_alter_table = ON",
    ] {
        assert_answer(set, LAB_TABLES, &format!("{set};\n{add}"), COPY);
    }
}

// foreign_key_checks = DEFAULT. Hand tests of step 11a's review: the session
// ends with the checks off on both servers (the inverse of the global value);
// the lab has not measured it, so every answer that depends on the checks is
// unknown until a readable session SET of the variable.

const SESSION_DEFAULTS: [&str; 9] = [
    "SET foreign_key_checks = DEFAULT",
    "SET SESSION foreign_key_checks = DEFAULT",
    "SET LOCAL foreign_key_checks = DEFAULT",
    "SET @@foreign_key_checks = DEFAULT",
    "SET @@session.foreign_key_checks = DEFAULT",
    "SET @@local.foreign_key_checks = DEFAULT",
    "set Foreign_Key_Checks = default",
    "SET innodb_lock_wait_timeout = 50, foreign_key_checks = DEFAULT",
    "SET GLOBAL innodb_lock_wait_timeout = 50, SESSION foreign_key_checks = DEFAULT",
];

fn sets_default_reason(line: usize) -> String {
    format!("line {line} sets foreign_key_checks to DEFAULT, which the lab has not measured")
}

#[test]
fn a_session_default_of_foreign_key_checks_makes_a_new_key_unknown_with_the_line_of_the_set() {
    // A key that fails with the checks on, and one that fits.
    let failing = mismatching_key_schema();
    let fitting = lab_after(&[OC_REF]);
    for set in SESSION_DEFAULTS {
        for (name, schema) in [("mismatch", &failing), ("fit", &fitting)] {
            let sql = format!("{set};\nALTER TABLE orders {REF};");
            assert_answer(&format!("{set}: {name}"), schema, &sql, UNKNOWN);
            for server in [MYSQL_80, MYSQL_84] {
                assert_eq!(
                    unknown_reason_on(server, schema, &sql),
                    sets_default_reason(1),
                    "{server}: {set}: {name}"
                );
            }
        }
    }
    // The line of the SET, not of the key.
    let sql = format!(
        "SET foreign_key_checks = 1;\nSET foreign_key_checks = 1;\nSET foreign_key_checks = DEFAULT;\nSELECT 1;\nALTER TABLE orders {REF};"
    );
    for server in [MYSQL_80, MYSQL_84] {
        assert_eq!(
            unknown_reason_on(server, &failing, &sql),
            sets_default_reason(3),
            "{server}"
        );
    }
}

#[test]
fn a_later_readable_session_set_of_foreign_key_checks_brings_the_measured_answers_back() {
    let failing = mismatching_key_schema();
    for set in [
        "SET foreign_key_checks = 1",
        "SET SESSION foreign_key_checks = ON",
    ] {
        let sql = format!("SET foreign_key_checks = DEFAULT;\n{set};\nALTER TABLE orders {REF};");
        assert_answer(set, &failing, &sql, FAILS_3780);
    }
    // Off is off again: a mismatch kilit can see is unknown, as before.
    let sql = format!(
        "SET foreign_key_checks = DEFAULT;\nSET foreign_key_checks = 0;\nALTER TABLE orders {REF};"
    );
    assert_answer("off", &failing, &sql, UNKNOWN);
}

#[test]
fn a_default_of_foreign_key_checks_does_not_touch_the_statements_that_do_not_depend_on_it() {
    let sql = format!("SET foreign_key_checks = DEFAULT;\nALTER TABLE orders {ADD_COLUMN};");
    assert_every_answer("add column", LAB_TABLES, &sql, &["not checked", INSTANT]);
}

#[test]
fn a_global_default_of_foreign_key_checks_changes_only_the_global_value() {
    let failing = mismatching_key_schema();
    let sql = format!("SET GLOBAL foreign_key_checks = DEFAULT;\nALTER TABLE orders {REF};");
    assert_answer("global default", &failing, &sql, FAILS_3780);
    for server in [MYSQL_80, MYSQL_84] {
        let predictions = predictions_on(server, &failing, &sql);
        assert!(
            notes_of(predictions.last().unwrap())
                .iter()
                .any(|note| note == CHECKS_ON_GLOBAL_CHANGED),
            "{server}"
        );
    }
}

/// `orders` with a key block of its own: `idx_orders_user` serves the key
/// `fk_orders_user`, as mysqldump writes them.
const KEYED_ORDERS: &str = "CREATE TABLE `users` (
  `id` bigint unsigned NOT NULL AUTO_INCREMENT,
  PRIMARY KEY (`id`)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4;
CREATE TABLE `orders` (
  `id` bigint unsigned NOT NULL AUTO_INCREMENT,
  `user_id` bigint unsigned NOT NULL,
  PRIMARY KEY (`id`),
  KEY `idx_orders_user` (`user_id`),
  CONSTRAINT `fk_orders_user` FOREIGN KEY (`user_id`) REFERENCES `users` (`id`)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4;
";

#[test]
fn a_session_default_of_foreign_key_checks_makes_dropping_a_key_column_or_its_index_unknown() {
    let drop_column = "ALTER TABLE orders DROP COLUMN user_id;";
    let drop_index = "ALTER TABLE orders DROP INDEX idx_orders_user;";
    // As before: with the checks on, both fail.
    assert_answer("column", KEYED_ORDERS, drop_column, FAILS_1828);
    assert_answer("index", KEYED_ORDERS, drop_index, FAILS_1553);
    for set in SESSION_DEFAULTS {
        for (statement, shape) in [
            (
                drop_column,
                format!(
                    "user_id is in the foreign key fk_orders_user, and {}",
                    sets_default_reason(1)
                ),
            ),
            (
                drop_index,
                format!(
                    "idx_orders_user serves the foreign key fk_orders_user, and {}",
                    sets_default_reason(1)
                ),
            ),
        ] {
            let sql = format!("{set};\n{statement}");
            assert_answer(&format!("{set}: {statement}"), KEYED_ORDERS, &sql, UNKNOWN);
            for server in [MYSQL_80, MYSQL_84] {
                assert_eq!(
                    unknown_reason_on(server, KEYED_ORDERS, &sql),
                    shape,
                    "{server}: {set}: {statement}"
                );
            }
        }
    }
    // A readable session SET brings them back.
    for statement in [drop_column, drop_index] {
        let answer = if statement == drop_column {
            FAILS_1828
        } else {
            FAILS_1553
        };
        assert_answer(
            statement,
            KEYED_ORDERS,
            &format!("SET foreign_key_checks = DEFAULT;\nSET foreign_key_checks = 1;\n{statement}"),
            answer,
        );
    }
}

const FAILS_1553: &str = "fails with error 1553";

#[test]
fn changing_the_type_of_a_foreign_key_column_after_a_default_of_the_checks_is_unknown() {
    // The 1828 shape for MODIFY: the column is in a key and the checks matter.
    let sql = "SET foreign_key_checks = DEFAULT;
               ALTER TABLE orders MODIFY COLUMN user_id INT NOT NULL;";
    assert_answer("modify", KEYED_ORDERS, sql, UNKNOWN);
    assert_eq!(
        unknown_reason_on(MYSQL_84, KEYED_ORDERS, sql),
        format!(
            "user_id is in the foreign key fk_orders_user, and {}",
            sets_default_reason(1)
        )
    );
}

// Regression tests for what the step 11b review found. The server runs are
// the review's hand tests (both servers, session only): MySQL 1628 for a
// comment over 2048 characters, 1228 for a GLOBAL sql_log_bin, 1193 for an
// unknown variable (and, on 8.0.46, restrict_fk_on_non_standard_key), 1071 for
// a key too long for the new engine or row format, 1231 for a quoted DEFAULT.

#[test]
fn a_table_comment_over_2048_characters_is_unknown_and_2048_keeps_the_measured_answer() {
    let reason = "a table comment longer than 2048 characters has not been measured";
    for server in [MYSQL_80, MYSQL_84] {
        assert_eq!(
            unknown_reason_on(
                server,
                LAB_TABLES,
                &format!("ALTER TABLE orders COMMENT = '{}';", "a".repeat(2049))
            ),
            reason,
            "{server}"
        );
        assert_eq!(
            unknown_reason_on(
                server,
                LAB_TABLES,
                &format!("ALTER TABLE orders COMMENT '{}';", "\u{e9}".repeat(2049))
            ),
            reason,
            "{server}"
        );
    }
    // Characters count, not bytes: 2048 two-byte characters are accepted.
    for comment in ["a".repeat(2048), "\u{e9}".repeat(2048)] {
        assert_orders(
            "2048 characters",
            LAB_TABLES,
            &format!("COMMENT = '{comment}'"),
            INPLACE,
        );
    }
}

#[test]
fn a_set_that_mysql_refuses_as_a_whole_leaves_old_alter_table_unknown() {
    // 1228 for a GLOBAL sql_log_bin: the whole statement fails, so
    // old_alter_table may or may not have changed.
    let add = format!("ALTER TABLE orders {ADD_COLUMN};");
    for set in [
        "SET SESSION old_alter_table = ON, GLOBAL foreign_key_checks = 0, sql_log_bin = 0",
        "SET old_alter_table = ON, GLOBAL sql_log_bin = 1",
    ] {
        let sql = format!("{set};\n{add}");
        assert_answer(set, LAB_TABLES, &sql, UNKNOWN);
        for server in [MYSQL_80, MYSQL_84] {
            let reason = unknown_reason_on(server, LAB_TABLES, &sql);
            assert!(
                reason.starts_with("line 1 sets old_alter_table"),
                "{server}: {set}: {reason}"
            );
        }
    }
}

#[test]
fn a_set_that_mysql_refuses_as_a_whole_leaves_foreign_key_checks_unknown() {
    let schema = mismatching_key_schema();
    let reason = "line 1 sets foreign_key_checks in a SET that MySQL may refuse as a whole, \
                  and the lab has not measured it";
    for set in [
        "SET foreign_key_checks = 0, GLOBAL sql_log_bin = 1",
        "SET foreign_key_checks = 0, restrict_fk_on_non_standard_key = OFF",
    ] {
        let sql = format!("{set};\nALTER TABLE orders {REF};");
        // Not the checks-off answer, and not the checks-on failure.
        for server in [MYSQL_80, MYSQL_84] {
            assert_eq!(
                unknown_reason_on(server, &schema, &sql),
                reason,
                "{server}: {set}"
            );
        }
    }
}

/// The review's `t1`: utf8mb4 VARCHAR(255) columns, one of them indexed.
const T1_TABLE: &str = "CREATE TABLE t1 (
  id BIGINT UNSIGNED AUTO_INCREMENT PRIMARY KEY,
  email VARCHAR(255) NULL,
  note VARCHAR(255) NULL,
  KEY by_email (email)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4;
";

#[test]
fn a_change_of_engine_that_mysql_may_refuse_for_the_keys_loses_the_table() {
    // ENGINE=MyISAM on InnoDB t1: 1071, a key over 1000 bytes; the table
    // stays InnoDB on the server.
    for server in [MYSQL_80, MYSQL_84] {
        let reason = unknown_reason_on(
            server,
            T1_TABLE,
            &format!("ALTER TABLE t1 ENGINE=MyISAM;\n{ADD_C_T1}"),
        );
        assert!(
            reason.starts_with("kilit cannot tell the engine of t1"),
            "{server}: {reason}"
        );
    }
    // A table with a FULLTEXT index is lost too.
    let fulltext = lab_after(&["ALTER TABLE orders ADD FULLTEXT INDEX ft_note (note)"]);
    assert_every_answer(
        "fulltext",
        &fulltext,
        &format!("ALTER TABLE orders ENGINE=MyISAM;\n{ADD_C}"),
        &[UNKNOWN, UNKNOWN],
    );
    // MyISAM to InnoDB with an AUTO_INCREMENT column that is not first in any
    // key: 1075.
    let auto = "CREATE TABLE m (a INT NOT NULL, id INT NOT NULL AUTO_INCREMENT, KEY (a, id))
                ENGINE=MyISAM;";
    assert_every_answer(
        "auto_increment second in its key",
        auto,
        "ALTER TABLE m ENGINE=InnoDB;\nALTER TABLE m ADD COLUMN c INT NULL;",
        &[UNKNOWN, UNKNOWN],
    );
}

const ADD_C_T1: &str = "ALTER TABLE t1 ADD COLUMN c INT NULL;";

#[test]
fn a_change_to_compact_or_redundant_that_mysql_may_refuse_for_the_keys_loses_the_table() {
    // ROW_FORMAT=COMPACT on t1: 1071 (767 bytes); the table stays DYNAMIC
    // and ADD INDEX by_note then works, so kilit must not say 1071.
    for options in [
        "ROW_FORMAT=COMPACT",
        "ROW_FORMAT=REDUNDANT",
        "row_format = compact",
    ] {
        let sql = format!("ALTER TABLE t1 {options};\nALTER TABLE t1 ADD INDEX by_note (note);");
        assert_every_answer(options, T1_TABLE, &sql, &[UNKNOWN, UNKNOWN]);
    }
    let fulltext = lab_after(&["ALTER TABLE orders ADD FULLTEXT INDEX ft_note (note)"]);
    assert_every_answer(
        "fulltext",
        &fulltext,
        &format!("ALTER TABLE orders ROW_FORMAT=COMPACT;\n{ADD_C}"),
        &[UNKNOWN, UNKNOWN],
    );
}

#[test]
fn keys_of_integers_keep_the_new_engine_or_row_format_and_other_changes_are_followed() {
    let keyed = lab_after(&["ALTER TABLE orders ADD INDEX idx_orders_amount (amount)"]);
    assert_every_answer(
        "myisam, integer key",
        &keyed,
        &format!("ALTER TABLE orders ENGINE=MyISAM;\n{ADD_C}"),
        &[UNKNOWN, COPY],
    );
    assert_every_answer(
        "compact, integer key",
        &keyed,
        &format!("ALTER TABLE orders ROW_FORMAT=COMPACT;\n{ADD_C}"),
        &[UNKNOWN, INSTANT],
    );
    // On t1, with its string key, the changes that cannot break a key.
    for (options, answer) in [
        ("ROW_FORMAT=DYNAMIC", INSTANT),
        ("ROW_FORMAT=DEFAULT", INSTANT),
        ("ROW_FORMAT=COMPRESSED", INPLACE_REBUILD),
        ("KEY_BLOCK_SIZE=8", INPLACE_REBUILD),
    ] {
        let sql = format!("ALTER TABLE t1 {options};\nALTER TABLE t1 {ADD_COLUMN};");
        assert_every_answer(options, T1_TABLE, &sql, &[UNKNOWN, answer]);
    }
}

#[test]
fn a_foreign_key_checks_assignment_kilit_cannot_read_makes_a_new_key_unknown() {
    let schema = mismatching_key_schema();
    let reason = "line 1 sets foreign_key_checks in a form kilit cannot read";
    // All three turn the checks off or to their default on the server.
    for set in [
        "SET foreign_key_checks := DEFAULT",
        "SET @@`foreign_key_checks` = DEFAULT",
        "SET NAMES utf8mb4, foreign_key_checks = 0",
    ] {
        let sql = format!("{set};\nALTER TABLE orders {REF};");
        for server in [MYSQL_80, MYSQL_84] {
            assert_eq!(
                unknown_reason_on(server, &schema, &sql),
                reason,
                "{server}: {set}"
            );
        }
    }
    // The header mysqldump writes is still read: the checks are off, and a
    // key that fits gets the checks-off answer, INPLACE.
    let fitting = lab_after(&[OC_REF]);
    assert_answer(
        "mysqldump header",
        &fitting,
        &format!(
            "SET @OLD_FOREIGN_KEY_CHECKS=@@FOREIGN_KEY_CHECKS, FOREIGN_KEY_CHECKS=0;\n\
             ALTER TABLE orders {REF};"
        ),
        INPLACE,
    );
}

#[test]
fn a_quoted_default_of_foreign_key_checks_changes_nothing() {
    // MySQL gives 1231 and keeps the value: the checks stay on.
    let sql = format!("SET foreign_key_checks = 'DEFAULT';\nALTER TABLE orders {REF};");
    assert_answer("mismatch", &mismatching_key_schema(), &sql, FAILS_3780);
    assert_answer("fit", &lab_after(&[OC_REF]), &sql, COPY);
    for server in [MYSQL_80, MYSQL_84] {
        let predictions = predictions_on(server, &mismatching_key_schema(), &sql);
        assert!(
            notes_of(predictions.last().unwrap())
                .iter()
                .any(|note| note == CHECKS_ON_PLAIN),
            "{server}"
        );
    }
}

#[test]
fn a_comment_between_set_and_global_still_makes_the_value_global() {
    let schema = mismatching_key_schema();
    let sql = format!("SET/**/GLOBAL foreign_key_checks = 0;\nALTER TABLE orders {REF};");
    assert_answer("comment", &schema, &sql, FAILS_3780);
    for server in [MYSQL_80, MYSQL_84] {
        let predictions = predictions_on(server, &schema, &sql);
        assert!(
            notes_of(predictions.last().unwrap())
                .iter()
                .any(|note| note == CHECKS_ON_GLOBAL_CHANGED),
            "{server}"
        );
    }
}

#[test]
fn the_reason_for_a_table_of_unknown_engine_says_a_statement_names_an_engine_it_cannot_follow() {
    let sql = "SET default_storage_engine = MyISAM;
               CREATE TABLE t (id INT PRIMARY KEY);
               ALTER TABLE t ADD COLUMN c INT NULL;";
    for server in [MYSQL_80, MYSQL_84] {
        assert_eq!(
            unknown_reason_on(server, "", sql),
            "kilit cannot tell the engine of t: the input sets default_storage_engine or \
             default_tmp_storage_engine before creating it, or a statement names an engine \
             for it that kilit cannot follow",
            "{server}"
        );
    }
}

// Step 12: the measured servers and the second look at the input.

/// What kilit shows of an answer, apart from its notes: the impact, and the
/// error number or the algorithm, the rebuild, the copied rows and the writes.
/// An answer kilit cannot give is compared by its kind only.
fn shown_answer(impact: Impact, prediction: &Prediction) -> String {
    match prediction {
        Prediction::Measured {
            verdict: Verdict::Fails { code, .. },
            ..
        } => format!("{impact:?}: fails with error {code}"),
        Prediction::Measured {
            verdict:
                Verdict::Runs {
                    algorithm,
                    rebuilds_table,
                    copied_rows,
                    writes,
                    ..
                },
            ..
        } => format!(
            "{impact:?}: {algorithm:?}, rebuilds_table {rebuilds_table}, copies rows {}, \
             writes {writes:?}",
            copied_rows.is_some_and(|rows| rows > 0)
        ),
        Prediction::Unknown { .. } => format!("{impact:?}: unknown"),
        Prediction::NotChecked { .. } => format!("{impact:?}: not checked"),
    }
}

/// What each statement of the scenario's run shows, on `server`, and whether
/// any of the answers depends on the server.
fn shown_on(
    server: &str,
    scenario: &ScenarioResult,
    variant: Variant,
    schema: &Schema,
) -> (Vec<String>, bool) {
    let results = Knowledge::built_in().server(server).unwrap();
    let statements = parse(&sql(scenario, variant));
    let findings = check(&statements, schema, results);
    let shown = impacts(&statements, &findings)
        .into_iter()
        .zip(&findings)
        .map(|(impact, finding)| shown_answer(impact, &finding.prediction))
        .collect();
    let dependent = findings.iter().any(|finding| finding.server_dependent);
    (shown, dependent)
}

#[test]
fn when_no_answer_of_a_run_depends_on_the_server_every_other_server_answers_the_same() {
    let knowledge = Knowledge::built_in();
    assert!(knowledge.servers().len() >= 2);
    let (mut dependent_runs, mut independent_runs) = (0, 0);
    for results in knowledge.servers() {
        let server = results.server.name();
        for scenario in results.scenarios() {
            let schema = lab_schema(scenario);
            for variant in Variant::ALL {
                let (shown, dependent) = shown_on(&server, scenario, variant, &schema);
                if dependent {
                    dependent_runs += 1;
                    continue;
                }
                independent_runs += 1;
                for other in knowledge.servers() {
                    let other = other.server.name();
                    if other == server {
                        continue;
                    }
                    let (shown_there, _) = shown_on(&other, scenario, variant, &schema);
                    assert_eq!(
                        shown, shown_there,
                        "{} as {variant}: no answer depends on the server on {server}, yet \
                         {other} answers differently",
                        scenario.id
                    );
                }
            }
        }
    }
    // The check is not empty: some runs depend on the server (the foreign
    // keys to a parent column without a unique index), most do not.
    assert!(dependent_runs > 0, "no run depends on the server");
    assert!(independent_runs > dependent_runs, "{independent_runs}");
}

// Step 13a: forms MySQL 8.0.46 and 8.4.13 reject, which kilit used to answer.
// Every refused statement below was run by hand on both servers (error 1064,
// 1067, 1294 or 1391 on each; manual/step13a.sql), and so was every twin that
// must keep its answer. A refused statement changes no table.

/// What every refusal of this step starts with. The rest of the reason is the
/// lead's wording.
const NOT_ACCEPTED: &str = "MySQL 8.0.46 and 8.4.13 do not accept";

/// The tables of the hand tests: users, orders (with an index, a foreign key
/// and a DATETIME), d (every column type, a DATE and a TIME), k, k2 and kt.
const S13A_TABLES: &str = "\
CREATE TABLE users (id BIGINT UNSIGNED AUTO_INCREMENT PRIMARY KEY, name VARCHAR(100) NOT NULL);
CREATE TABLE orders (id BIGINT UNSIGNED AUTO_INCREMENT PRIMARY KEY, user_id BIGINT UNSIGNED NOT NULL, amount INT NOT NULL, email VARCHAR(255) NULL, note VARCHAR(50) NULL, seen DATETIME NULL, KEY ix (amount), CONSTRAINT fk_x FOREIGN KEY (user_id) REFERENCES users (id));
CREATE TABLE d (id INT PRIMARY KEY, amount INT NOT NULL, price DECIMAL(5,2) NOT NULL, ratio DOUBLE NOT NULL, note VARCHAR(50) NULL, seen DATETIME NULL, flag TINYINT(1) NOT NULL, bits BIT(2) NOT NULL, raw VARBINARY(4) NOT NULL, day DATE NULL, t TIME NULL);
CREATE TABLE k (id INT PRIMARY KEY, email VARCHAR(255) NULL, note VARCHAR(50) NULL);
CREATE TABLE k2 (email VARCHAR(255) NOT NULL, note VARCHAR(50) NOT NULL);
CREATE TABLE kt (id INT PRIMARY KEY, body TEXT NULL);
";

/// Every table name a statement of this step may touch or create.
const S13A_NAMES: [&str; 22] = [
    "users", "orders", "d", "k", "k2", "kt", "orders2", "tq", "tq1", "tq2", "tq3", "tq4", "tp0",
    "tp2", "tp3", "g1", "g16", "g17", "g18", "g19", "g20", "g21",
];

/// How `statement` (one statement) fails to be refused: on a server it is not
/// unknown with the reason of this step, or it changes a table for kilit.
fn refusal_problems(statement: &str) -> Vec<String> {
    let mut problems = Vec::new();
    for server in [MYSQL_80, MYSQL_84] {
        match predictions_on(server, S13A_TABLES, statement).last() {
            Some(Prediction::Unknown { reason }) if reason.starts_with(NOT_ACCEPTED) => {}
            other => problems.push(format!("{server}: {statement}\n    got {other:?}")),
        }
    }
    let before = Schema::from_sql(S13A_TABLES);
    let after = Schema::from_sql(&format!("{S13A_TABLES}{statement}"));
    for name in S13A_NAMES {
        if before.table(name) != after.table(name) {
            problems.push(format!("{statement}\n    changed the table {name}"));
        }
    }
    problems
}

/// Every statement is refused on both servers and leaves every table as it was.
fn assert_all_refused(statements: &[&str]) {
    let problems: Vec<String> = statements
        .iter()
        .flat_map(|statement| refusal_problems(statement))
        .collect();
    assert!(problems.is_empty(), "\n{}", problems.join("\n"));
}

/// The same for statements made of a pattern.
fn assert_all_refused_owned(statements: &[String]) {
    let borrowed: Vec<&str> = statements.iter().map(String::as_str).collect();
    assert_all_refused(&borrowed);
}

/// The statement is not refused for the reason of this step, on either server.
fn assert_not_refused(statements: &[&str]) {
    let mut problems = Vec::new();
    for statement in statements {
        for server in [MYSQL_80, MYSQL_84] {
            if let Some(Prediction::Unknown { reason }) =
                predictions_on(server, S13A_TABLES, statement).last()
                && reason.starts_with(NOT_ACCEPTED)
            {
                problems.push(format!("{server}: {statement}\n    refused: {reason}"));
            }
        }
    }
    assert!(problems.is_empty(), "\n{}", problems.join("\n"));
}

/// kilit's answer to each statement, one at a time against the hand-test
/// tables, on both servers: (statement, expected answer).
fn assert_answers_on_both(statements: &[(&str, &str)]) {
    let mut problems = Vec::new();
    for (statement, expected) in statements {
        for server in [MYSQL_80, MYSQL_84] {
            let got = statements_on(server, S13A_TABLES, &parse(statement));
            if got.last().map(String::as_str) != Some(*expected) {
                problems.push(format!(
                    "{server}: {statement}\n    expected {expected}, got {got:?}"
                ));
            }
        }
    }
    assert!(problems.is_empty(), "\n{}", problems.join("\n"));
}

fn impacts_on(server: &str, schema: &str, sql: &str) -> Vec<Impact> {
    let statements = parse(sql);
    let results = Knowledge::built_in().server(server).unwrap();
    let findings = check(&statements, &Schema::from_sql(schema), results);
    impacts(&statements, &findings)
}

#[test]
fn a_table_name_in_single_quotes_is_refused_wherever_a_table_goes() {
    // A table name in single quotes is error 1064 at ALTER TABLE, CREATE INDEX
    // ON, DROP INDEX ON, CREATE TABLE, REFERENCES, RENAME TO and RENAME TABLE.
    assert_all_refused(&[
        "ALTER TABLE 'orders' ADD COLUMN b1 INT NULL",
        "CREATE INDEX ix_b1 ON 'orders' (amount)",
        "DROP INDEX ix ON 'orders'",
        "CREATE TABLE 'tq' (id INT PRIMARY KEY)",
        "ALTER TABLE orders ADD CONSTRAINT fk_b1 FOREIGN KEY (user_id) REFERENCES 'users' (id)",
        "ALTER TABLE orders RENAME TO 'orders2'",
        "ALTER TABLE orders RENAME AS 'orders2'",
        "RENAME TABLE orders TO 'orders2'",
        "RENAME TABLE 'orders' TO orders2",
    ]);
}

#[test]
fn a_column_name_in_single_quotes_is_refused_wherever_a_column_goes() {
    assert_all_refused(&[
        "ALTER TABLE orders ADD COLUMN 'b2' INT NULL",
        "ALTER TABLE orders ADD COLUMN b3 INT NULL AFTER 'note'",
        "ALTER TABLE orders MODIFY 'note' VARCHAR(60) NULL",
        "ALTER TABLE orders MODIFY COLUMN 'note' VARCHAR(60) NULL",
        "ALTER TABLE orders CHANGE 'note' memo VARCHAR(60) NULL",
        "ALTER TABLE orders CHANGE note 'memo' VARCHAR(60) NULL",
        "ALTER TABLE orders RENAME COLUMN 'note' TO memo",
        "ALTER TABLE orders RENAME COLUMN note TO 'memo'",
        "ALTER TABLE orders ALTER COLUMN 'amount' SET DEFAULT 0",
        "ALTER TABLE orders ALTER 'amount' DROP DEFAULT",
        "ALTER TABLE orders DROP COLUMN 'note'",
        "ALTER TABLE orders DROP 'note'",
        "ALTER TABLE orders ADD CONSTRAINT fk_b2 FOREIGN KEY ('user_id') REFERENCES users (id)",
        "ALTER TABLE orders ADD CONSTRAINT fk_b3 FOREIGN KEY (user_id) REFERENCES users ('id')",
        "ALTER TABLE orders ADD COLUMN b4 INT NULL, ADD INDEX ix_b4 (b4), \
         ALTER COLUMN 'b4' SET DEFAULT 1",
    ]);
}

#[test]
fn an_index_name_in_single_quotes_is_refused_wherever_an_index_goes() {
    assert_all_refused(&[
        "ALTER TABLE orders ADD INDEX 'ix_b2' (amount)",
        "ALTER TABLE orders ADD KEY 'ix_b3' (amount)",
        "ALTER TABLE orders ADD UNIQUE 'uq_b1' (email)",
        "ALTER TABLE orders ADD UNIQUE INDEX 'uq_b2' (email)",
        "ALTER TABLE orders ADD FULLTEXT INDEX 'ft_b1' (note)",
        "CREATE INDEX 'ix_b4' ON orders (amount)",
        "ALTER TABLE orders DROP INDEX 'ix'",
        "DROP INDEX 'ix' ON orders",
        "ALTER TABLE orders ADD CONSTRAINT fk_b4 FOREIGN KEY 'ix_b6' (user_id) REFERENCES users (id)",
    ]);
}

#[test]
fn a_key_part_in_single_quotes_is_refused_in_every_kind_of_index() {
    assert_all_refused(&[
        "ALTER TABLE orders ADD INDEX ix_b7 ('amount')",
        "ALTER TABLE orders ADD UNIQUE INDEX uq_b3 ('email')",
        "ALTER TABLE k2 ADD PRIMARY KEY ('email')",
        "CREATE INDEX ix_b8 ON orders ('amount')",
    ]);
}

#[test]
fn a_constraint_name_in_single_quotes_is_refused_wherever_a_constraint_goes() {
    // $table->check('amount > 0', 'ck') in a hand-written migration, and the
    // drops by name.
    assert_all_refused(&[
        "ALTER TABLE orders ADD CONSTRAINT 'ck_b1' CHECK (amount > 0)",
        "ALTER TABLE orders ADD CONSTRAINT 'uq_b4' UNIQUE (email)",
        "ALTER TABLE orders ADD CONSTRAINT 'fk_b5' FOREIGN KEY (user_id) REFERENCES users (id)",
        "ALTER TABLE k2 ADD CONSTRAINT 'pk_b1' PRIMARY KEY (email)",
        "ALTER TABLE orders ADD COLUMN b5 INT NULL CONSTRAINT 'ck_b2' CHECK (b5 > 0)",
        "ALTER TABLE orders DROP FOREIGN KEY 'fk_x'",
        "ALTER TABLE orders DROP CONSTRAINT 'fk_x'",
    ]);
}

#[test]
fn a_name_in_single_quotes_inside_create_table_is_refused_and_creates_no_table() {
    assert_all_refused(&[
        "CREATE TABLE tq1 (id INT PRIMARY KEY, KEY 'k' (id))",
        "CREATE TABLE tq2 (id INT PRIMARY KEY, KEY k ('id'))",
        "CREATE TABLE tq3 (id INT PRIMARY KEY, CONSTRAINT 'c' CHECK (id > 0))",
        "CREATE TABLE tq4 (id INT PRIMARY KEY, n INT NULL CONSTRAINT 'c' CHECK (n > 0))",
        "CREATE TABLE g17 (id INT PRIMARY KEY, user_id BIGINT UNSIGNED NOT NULL, \
         FOREIGN KEY (user_id) REFERENCES 'users' (id))",
        "CREATE TABLE g18 (id INT PRIMARY KEY, user_id BIGINT UNSIGNED NOT NULL, \
         FOREIGN KEY ('user_id') REFERENCES users (id))",
        "CREATE TABLE g19 (id INT PRIMARY KEY, user_id BIGINT UNSIGNED NOT NULL, \
         FOREIGN KEY (user_id) REFERENCES users ('id'))",
        "CREATE TABLE g20 (id INT PRIMARY KEY, user_id BIGINT UNSIGNED NOT NULL, \
         CONSTRAINT 'fk' FOREIGN KEY (user_id) REFERENCES users (id))",
        "CREATE TABLE g21 (id INT, PRIMARY KEY ('id'))",
    ]);
}

#[test]
fn text_mysql_takes_in_single_quotes_keeps_its_answer() {
    // COMMENT, COLLATE, CHARACTER SET, DEFAULT, ENUM and SET values are no
    // names: all accepted by both servers.
    assert_answers_on_both(&[
        (
            "ALTER TABLE orders ADD COLUMN bw1 INT NULL COMMENT 'x'",
            INSTANT,
        ),
        (
            "ALTER TABLE orders ADD INDEX ix_bw1 (amount) COMMENT 'x'",
            INPLACE,
        ),
        ("ALTER TABLE orders COMMENT 'x'", INPLACE),
        ("ALTER TABLE orders COMMENT = 'x'", INPLACE),
        (
            "ALTER TABLE orders ADD COLUMN bw2 VARCHAR(10) COLLATE 'utf8mb4_bin' NULL",
            INSTANT,
        ),
        (
            "ALTER TABLE orders ADD COLUMN bw3 VARCHAR(10) CHARACTER SET 'utf8mb4' NULL",
            INSTANT,
        ),
        (
            "ALTER TABLE orders ADD COLUMN bw4 VARCHAR(10) CHARACTER SET 'utf8mb4' \
             COLLATE 'utf8mb4_bin' NULL",
            INSTANT,
        ),
        (
            "ALTER TABLE orders ADD COLUMN bw5 VARCHAR(10) NOT NULL DEFAULT 'x'",
            INSTANT,
        ),
        (
            "ALTER TABLE orders ADD COLUMN bw6 ENUM('a', 'b') NULL",
            INSTANT,
        ),
        (
            "ALTER TABLE orders ADD COLUMN bw7 SET('a', 'b') NULL",
            INSTANT,
        ),
        // Laravel's collate 'utf8mb4_bin' and a doubled quote in a comment.
        (
            "alter table `orders` add `bw8` varchar(255) collate 'utf8mb4_bin' not null",
            INSTANT,
        ),
        (
            "alter table `orders` add `bw9` varchar(255) not null comment 'it''s'",
            INSTANT,
        ),
    ]);
    // Laravel's create table, and a create table with the same text in a
    // column, an index and the table.
    assert_answers_on_both(&[
        (
            "create table `tw1` (`id` bigint unsigned not null auto_increment primary key, \
             `name` varchar(255) not null) default character set utf8mb4 \
             collate 'utf8mb4_unicode_ci'",
            "not checked",
        ),
        (
            "CREATE TABLE tw2 (id INT PRIMARY KEY, s VARCHAR(10) CHARACTER SET 'utf8mb4' \
             COLLATE 'utf8mb4_bin' NULL COMMENT 'x', KEY k (s) COMMENT 'x') COMMENT = 'x'",
            "not checked",
        ),
    ]);
}

#[test]
fn a_key_part_that_is_not_a_column_a_prefix_or_an_expression_in_parentheses_is_refused() {
    // Error 1064 on both servers for each.
    assert_all_refused(&[
        "ALTER TABLE orders ADD INDEX c1 (orders.amount)",
        "ALTER TABLE orders ADD INDEX c2 (1)",
        "ALTER TABLE orders ADD INDEX c3 (-amount)",
        "ALTER TABLE orders ADD INDEX c4 (amount IS NULL)",
        "ALTER TABLE orders ADD INDEX c5 (amount + 0)",
        "ALTER TABLE orders ADD INDEX c6 (note COLLATE utf8mb4_bin)",
        "ALTER TABLE orders ADD INDEX c7 (note->'$.a')",
        "ALTER TABLE orders ADD INDEX c8 (CAST(note AS CHAR(5)))",
        "ALTER TABLE orders ADD INDEX c9 (`orders`.`amount`)",
        "ALTER TABLE k ADD INDEX k22 (id + 1)",
    ]);
}

#[test]
fn a_prefix_length_of_zero_or_above_2147483647_is_refused_in_every_kind_of_index() {
    // 0 is error 1391; 2147483648 and more (4294967295, 4294967296, 25 digits,
    // 2147483648 padded with zeros) are error 1064. 2147483647 is not refused.
    assert_all_refused(&[
        "ALTER TABLE k ADD INDEX p0 (note(0))",
        "ALTER TABLE k ADD INDEX p3 (note(2147483648))",
        "ALTER TABLE k ADD INDEX p4 (note(4294967295))",
        "ALTER TABLE k ADD INDEX k21 (note(4294967296))",
        "ALTER TABLE k ADD INDEX p7 (note(1000000000000000000000000))",
        "ALTER TABLE k ADD INDEX p10 (note(0000000000000002147483648))",
        "ALTER TABLE k ADD INDEX p11 (note(00))",
        "ALTER TABLE k ADD INDEX p15 (note(18446744073709551615))",
        "ALTER TABLE k ADD INDEX p16 (note(18446744073709551616))",
        "ALTER TABLE kt ADD INDEX q0 (body(0))",
        "ALTER TABLE kt ADD INDEX q4 (body(2147483648))",
        "ALTER TABLE kt ADD INDEX q5 (body(4294967295))",
        "ALTER TABLE k ADD UNIQUE INDEX pu0 (note(0))",
        "ALTER TABLE k ADD UNIQUE INDEX pu2 (note(2147483648))",
        "ALTER TABLE k ADD UNIQUE INDEX pu3 (note(4294967296))",
        "ALTER TABLE k2 ADD PRIMARY KEY (note(0))",
        "ALTER TABLE k2 ADD PRIMARY KEY (note(2147483648))",
        "ALTER TABLE k2 ADD PRIMARY KEY (note(4294967296))",
        "CREATE INDEX pc0 ON k (note(0))",
        "CREATE INDEX pc2 ON k (note(2147483648))",
        "CREATE INDEX pc3 ON k (note(4294967296))",
        "ALTER TABLE orders ADD FULLTEXT INDEX pf0 (note(0))",
        "CREATE TABLE tp0 (id INT PRIMARY KEY, note VARCHAR(50) NULL, KEY k (note(0)))",
        "CREATE TABLE tp2 (id INT PRIMARY KEY, note VARCHAR(50) NULL, KEY k (note(2147483648)))",
        "CREATE TABLE tp3 (id INT PRIMARY KEY, note VARCHAR(50) NULL, KEY k (note(4294967296)))",
    ]);
}

#[test]
fn key_parts_mysql_accepts_are_not_refused() {
    // Accepted twins: an expression in its own parentheses, prefixes of 1, of
    // 2147483647 (MySQL then fails for another reason, which kilit does not
    // claim to know) and with leading zeros, a descending prefix, a prefix in
    // capitals, with a space and in backticks.
    assert_not_refused(&[
        "ALTER TABLE orders ADD INDEX c11 ((amount + 0))",
        "ALTER TABLE orders ADD INDEX c12 ((-amount))",
        "ALTER TABLE orders ADD INDEX c13 ((amount IS NULL))",
        "ALTER TABLE orders ADD INDEX c14 ((note COLLATE utf8mb4_bin))",
        "ALTER TABLE orders ADD INDEX c15 ((CAST(note AS CHAR(5))))",
        "ALTER TABLE orders ADD INDEX c16 ((1))",
        "ALTER TABLE k ADD INDEX p1 (note(1))",
        "ALTER TABLE k ADD INDEX p2 (note(2147483647))",
        "ALTER TABLE k ADD INDEX p9 (note(0000000000000002147483647))",
        "CREATE TABLE tp1 (id INT PRIMARY KEY, note VARCHAR(50) NULL, KEY k (note(2147483647)))",
    ]);
    assert_answers_on_both(&[
        ("ALTER TABLE k ADD INDEX p6 (note(0000000000010))", INPLACE),
        (
            "ALTER TABLE k ADD INDEX p8 (note(0000000000000000000000010))",
            INPLACE,
        ),
        ("ALTER TABLE k ADD INDEX w1 (note(10) DESC)", INPLACE),
        ("ALTER TABLE k ADD INDEX w2 (NOTE(10))", INPLACE),
        ("ALTER TABLE k ADD INDEX w3 (note (10))", INPLACE),
        ("ALTER TABLE k ADD INDEX w4 (`note`(10))", INPLACE),
        (
            "ALTER TABLE k ADD INDEX w6 (note(10) ASC, email(20) DESC)",
            INPLACE,
        ),
        // The functional index kilit measured: LOWER() of a VARCHAR column.
        (
            "ALTER TABLE k ADD INDEX w5 ((lower(email)))",
            "INPLACE, no rebuild, writes wait",
        ),
        (
            "alter table `k` add index `w7`((lower(`email`)))",
            "INPLACE, no rebuild, writes wait",
        ),
    ]);
}

#[test]
fn on_update_with_anything_but_the_current_timestamp_functions_is_refused() {
    // Error 1064 on both servers for each, on a DATETIME column.
    let forms = [
        "CURRENT_DATE",
        "CURDATE()",
        "UTC_TIMESTAMP()",
        "UTC_TIMESTAMP",
        "SYSDATE()",
        "(NOW())",
        "NULL",
        "1",
        "CURRENT_TIMESTAMP + 0",
        "NOW",
        "'2020-01-01 00:00:00'",
        "(CURRENT_TIMESTAMP)",
    ];
    let statements: Vec<String> = forms
        .iter()
        .map(|form| format!("ALTER TABLE d ADD COLUMN u3 DATETIME NULL ON UPDATE {form}"))
        .collect();
    assert_all_refused_owned(&statements);
    // The same on a DATE column, which the nine forms of step 5b measured.
    assert_all_refused(&["ALTER TABLE d ADD COLUMN u2 DATE NULL ON UPDATE CURRENT_DATE"]);
}

#[test]
fn a_current_timestamp_function_on_another_column_type_is_refused() {
    // DEFAULT is error 1067, ON UPDATE error 1294, for each function on DATE,
    // TIME, INT, VARCHAR and TEXT. YEAR is measured too; kilit leaves it as it
    // was (see below).
    let mut statements = Vec::new();
    for column in ["DATE", "TIME", "INT", "VARCHAR(20)", "TEXT"] {
        for function in ["CURRENT_TIMESTAMP", "NOW()", "LOCALTIME", "LOCALTIMESTAMP"] {
            for clause in ["DEFAULT", "ON UPDATE"] {
                statements.push(format!(
                    "ALTER TABLE d ADD COLUMN e1 {column} NULL {clause} {function}"
                ));
            }
        }
    }
    assert_eq!(statements.len(), 40);
    assert_all_refused_owned(&statements);
}

#[test]
fn a_current_timestamp_function_of_another_precision_than_its_column_is_refused() {
    // Measured pairs of (column, function) that MySQL rejects: the precision
    // of a bare name or () is 0; 7 is above the maximum of 6.
    let pairs = [
        ("DATETIME(3)", "CURRENT_TIMESTAMP()"),
        ("DATETIME(3)", "NOW()"),
        ("DATETIME(3)", "LOCALTIME()"),
        ("DATETIME(3)", "LOCALTIMESTAMP()"),
        ("DATETIME", "CURRENT_TIMESTAMP(3)"),
        ("DATETIME", "NOW(3)"),
        ("DATETIME", "LOCALTIME(3)"),
        ("DATETIME", "LOCALTIMESTAMP(3)"),
        ("DATETIME(3)", "CURRENT_TIMESTAMP(6)"),
        ("DATETIME(3)", "NOW(6)"),
        ("DATETIME(6)", "CURRENT_TIMESTAMP(3)"),
        ("DATETIME(6)", "LOCALTIMESTAMP(3)"),
        ("DATETIME(6)", "CURRENT_TIMESTAMP"),
        ("DATETIME(6)", "NOW()"),
        ("DATETIME(6)", "LOCALTIME"),
        ("DATETIME(6)", "LOCALTIMESTAMP"),
        ("DATETIME(6)", "CURRENT_TIMESTAMP(7)"),
        ("DATETIME(6)", "NOW(7)"),
        ("TIMESTAMP(3)", "CURRENT_TIMESTAMP()"),
        ("TIMESTAMP(3)", "NOW()"),
        ("TIMESTAMP", "CURRENT_TIMESTAMP(3)"),
        ("TIMESTAMP", "LOCALTIMESTAMP(3)"),
        ("TIMESTAMP(3)", "CURRENT_TIMESTAMP(6)"),
        ("TIMESTAMP(6)", "CURRENT_TIMESTAMP(3)"),
        ("TIMESTAMP(6)", "CURRENT_TIMESTAMP"),
        ("TIMESTAMP(6)", "NOW()"),
        ("TIMESTAMP(6)", "CURRENT_TIMESTAMP(7)"),
        ("TIMESTAMP(6)", "LOCALTIME(7)"),
    ];
    let mut statements = Vec::new();
    for (column, function) in pairs {
        for clause in ["DEFAULT", "ON UPDATE"] {
            statements.push(format!(
                "ALTER TABLE d ADD COLUMN e1 {column} NULL {clause} {function}"
            ));
        }
    }
    // Both clauses in one definition, one of them not matching.
    statements.extend([
        "ALTER TABLE d ADD COLUMN e900 DATETIME(6) NULL DEFAULT CURRENT_TIMESTAMP(6) \
         ON UPDATE CURRENT_TIMESTAMP"
            .to_owned(),
        "ALTER TABLE d ADD COLUMN e901 DATETIME(6) NULL DEFAULT CURRENT_TIMESTAMP \
         ON UPDATE CURRENT_TIMESTAMP(6)"
            .to_owned(),
        "ALTER TABLE d ADD COLUMN e902 TIMESTAMP(3) NOT NULL DEFAULT CURRENT_TIMESTAMP(3) \
         ON UPDATE CURRENT_TIMESTAMP(6)"
            .to_owned(),
        // The servers' own wording for a DATETIME(6) column, as in the nine forms.
        "ALTER TABLE d ADD COLUMN c24 DATETIME(6) NULL DEFAULT CURRENT_TIMESTAMP".to_owned(),
        "ALTER TABLE d ADD COLUMN c25 DATETIME NULL DEFAULT CURRENT_TIMESTAMP(7)".to_owned(),
    ]);
    assert_all_refused_owned(&statements);
}

#[test]
fn a_current_timestamp_function_in_modify_and_change_follows_the_same_rule() {
    assert_all_refused(&[
        "ALTER TABLE d MODIFY seen DATETIME(6) NULL DEFAULT CURRENT_TIMESTAMP",
        "ALTER TABLE d MODIFY seen DATE NULL ON UPDATE CURRENT_TIMESTAMP",
        "ALTER TABLE d MODIFY day DATE NULL DEFAULT NOW()",
        "ALTER TABLE d CHANGE seen seen2 DATETIME(3) NULL DEFAULT CURRENT_TIMESTAMP(6)",
        "ALTER TABLE d CHANGE seen seen2 DATE NULL ON UPDATE LOCALTIMESTAMP",
        "ALTER TABLE d CHANGE day day DATE NULL DEFAULT CURRENT_TIMESTAMP",
    ]);
}

#[test]
fn a_current_timestamp_function_that_matches_its_column_keeps_the_instant_answer() {
    // Accepted by both servers (e49 to e256, u15 to u23 of the hand tests):
    // no precision, () and (0) are the same; 06 is 6; TIMESTAMP too.
    let accepted = [
        "DATETIME NULL DEFAULT CURRENT_TIMESTAMP()",
        "DATETIME NULL ON UPDATE CURRENT_TIMESTAMP()",
        "DATETIME NULL DEFAULT NOW()",
        "DATETIME NULL DEFAULT LOCALTIME()",
        "DATETIME NULL DEFAULT LOCALTIMESTAMP()",
        "DATETIME(0) NULL DEFAULT CURRENT_TIMESTAMP",
        "DATETIME(0) NULL ON UPDATE NOW()",
        "DATETIME NULL DEFAULT CURRENT_TIMESTAMP(0)",
        "DATETIME NULL ON UPDATE LOCALTIME(0)",
        "DATETIME(3) NULL DEFAULT CURRENT_TIMESTAMP(3)",
        "DATETIME(3) NULL ON UPDATE NOW(3)",
        "DATETIME(6) NULL DEFAULT NOW(6)",
        "DATETIME(6) NULL ON UPDATE LOCALTIMESTAMP(6)",
        "DATETIME(06) NULL DEFAULT CURRENT_TIMESTAMP(6)",
        "DATETIME(6) NULL DEFAULT CURRENT_TIMESTAMP(06)",
        "DATETIME NULL ON UPDATE NOW()",
        "DATETIME NULL ON UPDATE LOCALTIME",
        "DATETIME(6) NULL ON UPDATE now(6)",
        "TIMESTAMP NULL DEFAULT CURRENT_TIMESTAMP()",
        "TIMESTAMP NULL ON UPDATE CURRENT_TIMESTAMP",
        "TIMESTAMP(0) NULL DEFAULT NOW()",
        "TIMESTAMP(3) NULL DEFAULT CURRENT_TIMESTAMP(3)",
        "TIMESTAMP(6) NULL ON UPDATE LOCALTIME(6)",
        "TIMESTAMP(06) NULL DEFAULT CURRENT_TIMESTAMP(6)",
        "TIMESTAMP(3) NOT NULL DEFAULT CURRENT_TIMESTAMP(3) ON UPDATE CURRENT_TIMESTAMP(3)",
        // $table->timestamp('t', 3)->useCurrent()->useCurrentOnUpdate()
        "TIMESTAMP(3) NOT NULL DEFAULT CURRENT_TIMESTAMP(3) on update CURRENT_TIMESTAMP(3)",
    ];
    let expected: Vec<(String, &str)> = accepted
        .iter()
        .map(|column| (format!("ALTER TABLE d ADD COLUMN e1 {column}"), INSTANT))
        .collect();
    let borrowed: Vec<(&str, &str)> = expected
        .iter()
        .map(|(statement, answer)| (statement.as_str(), *answer))
        .collect();
    assert_answers_on_both(&borrowed);
}

#[test]
fn a_year_column_with_a_current_timestamp_function_is_left_as_it_was() {
    // MySQL gives 1067 and 1294; step 13a does not take YEAR (a decision), so
    // the answer stays the add-column one and is not the refusal.
    let statements = [
        "ALTER TABLE d ADD COLUMN e17 YEAR NULL DEFAULT CURRENT_TIMESTAMP",
        "ALTER TABLE d ADD COLUMN e18 YEAR NULL ON UPDATE CURRENT_TIMESTAMP",
        "ALTER TABLE d ADD COLUMN e19 YEAR NULL DEFAULT NOW()",
    ];
    assert_not_refused(&statements);
    assert_answers_on_both(&[
        (statements[0], INSTANT),
        (statements[1], INSTANT),
        (statements[2], INSTANT),
    ]);
}

#[test]
fn modify_and_change_with_a_matching_current_timestamp_function_are_not_refused() {
    assert_not_refused(&[
        "ALTER TABLE d MODIFY seen DATETIME(6) NULL DEFAULT CURRENT_TIMESTAMP(6) \
         ON UPDATE CURRENT_TIMESTAMP(6)",
        "ALTER TABLE d CHANGE seen seen DATETIME NULL DEFAULT CURRENT_TIMESTAMP \
         ON UPDATE CURRENT_TIMESTAMP",
    ]);
}

#[test]
fn set_default_with_a_date_or_time_literal_in_parentheses_is_refused() {
    // Error 1067 for each, on a DATE, a TIME, a DATETIME and an INT column.
    let mut statements = Vec::new();
    for column in ["day", "t", "seen", "amount"] {
        for literal in [
            "(DATE '2020-01-01')",
            "(TIME '10:00:00')",
            "(TIMESTAMP '2020-01-01 00:00:00')",
            "((DATE '2020-01-01'))",
        ] {
            statements.push(format!(
                "ALTER TABLE d ALTER COLUMN {column} SET DEFAULT {literal}"
            ));
        }
    }
    assert_eq!(statements.len(), 16);
    assert_all_refused_owned(&statements);
}

#[test]
fn the_same_literal_in_a_column_definition_and_other_set_defaults_keep_their_answers() {
    // MODIFY, CHANGE and ADD COLUMN with DEFAULT (DATE '...') work on both
    // servers, and so do the SET DEFAULTs below; kilit has measured none of
    // them, so each stays unknown for that reason and is not the refusal.
    let statements = [
        "ALTER TABLE d MODIFY day DATE NULL DEFAULT (DATE '2020-01-01')",
        "ALTER TABLE d CHANGE day day DATE NULL DEFAULT (DATE '2020-01-02')",
        "ALTER TABLE d ADD COLUMN f1 DATE NULL DEFAULT (DATE '2020-01-01')",
        "ALTER TABLE d ALTER COLUMN day SET DEFAULT (DATE '2020-01-01' + INTERVAL 1 DAY)",
        "ALTER TABLE d ALTER COLUMN day SET DEFAULT (CURRENT_DATE)",
        "ALTER TABLE d ALTER COLUMN day SET DEFAULT ('2020-01-01')",
    ];
    assert_not_refused(&statements);
    for statement in statements {
        for server in [MYSQL_80, MYSQL_84] {
            let reason = unknown_reason_on(server, S13A_TABLES, statement);
            assert!(
                reason.contains(NOTHING_MEASURED),
                "{server}: {statement}: {reason}"
            );
        }
    }
    assert_answers_on_both(&[(
        "CREATE TABLE tf1 (id INT PRIMARY KEY, day DATE NULL DEFAULT (DATE '2020-01-01'), \
         t TIME NULL DEFAULT (TIME '10:00:00'), \
         s DATETIME NULL DEFAULT (TIMESTAMP '2020-01-01 00:00:00'))",
        "not checked",
    )]);
}

#[test]
fn create_table_is_refused_for_every_default_and_key_part_alter_table_refuses() {
    assert_all_refused(&[
        // Defaults without parentheses (step 5b).
        "CREATE TABLE g1 (id INT PRIMARY KEY, day DATE NULL DEFAULT CURRENT_DATE)",
        "CREATE TABLE g1 (id INT PRIMARY KEY, n INT NULL DEFAULT 1+1)",
        "CREATE TABLE g1 (id INT PRIMARY KEY, u CHAR(36) NULL DEFAULT UUID())",
        // Key parts without parentheses, and a prefix of 0.
        "CREATE TABLE g1 (id INT PRIMARY KEY, n VARCHAR(20) NULL, KEY (lower(n)))",
        "CREATE TABLE g1 (id INT PRIMARY KEY, KEY (id + 1))",
        "CREATE TABLE g1 (id INT PRIMARY KEY, n VARCHAR(20) NULL, UNIQUE KEY (lower(n)))",
        "CREATE TABLE g1 (id INT, PRIMARY KEY (id + 1))",
        "CREATE TABLE g1 (id INT PRIMARY KEY, KEY (g1.id))",
        "CREATE TABLE g1 (id INT PRIMARY KEY, n VARCHAR(20) NULL, KEY (n(0)))",
        "CREATE TABLE g1 (id INT PRIMARY KEY, n VARCHAR(20) NULL, KEY (n(4294967296)))",
        // ON UPDATE, CURRENT_TIMESTAMP on another type or precision.
        "CREATE TABLE g1 (id INT PRIMARY KEY, day DATE NULL ON UPDATE CURRENT_DATE)",
        "CREATE TABLE g1 (id INT PRIMARY KEY, day DATE NULL DEFAULT CURRENT_TIMESTAMP)",
        "CREATE TABLE g1 (id INT PRIMARY KEY, s DATETIME(6) NULL DEFAULT CURRENT_TIMESTAMP)",
        "CREATE TABLE g1 (id INT PRIMARY KEY, day DATE NULL ON UPDATE CURRENT_TIMESTAMP)",
        "CREATE TABLE g1 (id INT PRIMARY KEY, s DATETIME(3) NULL ON UPDATE CURRENT_TIMESTAMP(6))",
        "CREATE TABLE g1 (id INT PRIMARY KEY, s DATETIME NULL ON UPDATE UTC_TIMESTAMP())",
        "CREATE TABLE g1 (id INT PRIMARY KEY, s DATETIME NULL DEFAULT CURRENT_TIMESTAMP(7))",
        // NO INHERIT.
        "CREATE TABLE g1 (id INT PRIMARY KEY, n INT NULL CHECK (n > 0) NO INHERIT)",
        "CREATE TABLE g1 (id INT PRIMARY KEY, n INT NULL, CONSTRAINT c30 CHECK (n > 0) NO INHERIT)",
    ]);
}

#[test]
fn create_table_with_a_double_quoted_name_is_refused_for_ansi_quotes_not_for_the_servers() {
    // Error 1064 without ANSI_QUOTES. The reason is the ANSI_QUOTES one: with
    // that sql_mode the same text is accepted.
    for statement in [
        "CREATE TABLE \"g11\" (id INT PRIMARY KEY)",
        "CREATE TABLE g1 (id INT PRIMARY KEY, KEY \"k\" (id))",
        "CREATE TABLE g1 (id INT PRIMARY KEY, KEY k (\"id\"))",
        "CREATE TABLE g1 (id INT PRIMARY KEY, CONSTRAINT \"c\" CHECK (id > 0))",
    ] {
        for server in [MYSQL_80, MYSQL_84] {
            let reason = unknown_reason_on(server, S13A_TABLES, statement);
            assert!(
                reason.contains("ANSI_QUOTES"),
                "{server}: {statement}: {reason}"
            );
            assert!(
                !reason.starts_with(NOT_ACCEPTED),
                "{server}: {statement}: {reason}"
            );
        }
        let after = Schema::from_sql(&format!("{S13A_TABLES}{statement}"));
        assert!(
            matches!(after.table("g1"), TableState::Missing),
            "{statement}"
        );
    }
}

#[test]
fn laravel_s_create_table_shapes_are_not_refused() {
    // Schema::create with timestamps, useCurrent, useCurrentOnUpdate, a
    // default expression, a collation and a comment, unique and foreign keys.
    assert_answers_on_both(&[
        (
            "create table `gl1` (`id` bigint unsigned not null auto_increment primary key, \
             `user_id` bigint unsigned not null, `title` varchar(255) not null, `body` text null, \
             `created_at` timestamp null, `updated_at` timestamp null) default character set \
             utf8mb4 collate 'utf8mb4_unicode_ci'",
            "not checked",
        ),
        (
            "create table `gl2` (`id` bigint unsigned not null auto_increment primary key, \
             `t` timestamp(3) not null default CURRENT_TIMESTAMP(3) on update CURRENT_TIMESTAMP(3), \
             `d` date not null default (CURDATE()), \
             `s` varchar(255) collate 'utf8mb4_bin' not null comment 'x', \
             `u` timestamp not null default CURRENT_TIMESTAMP, \
             `v` datetime(6) null default CURRENT_TIMESTAMP(6)) default character set utf8mb4 \
             collate 'utf8mb4_unicode_ci' engine = InnoDB",
            "not checked",
        ),
        (
            "create table `gl3` (`id` bigint unsigned not null auto_increment primary key, \
             `email` varchar(255) not null, `n` json null, `price` decimal(8, 2) not null \
             default '0', `status` enum('a', 'b') not null default 'a', \
             unique key `gl3_email_unique` (`email`)) default character set utf8mb4 \
             collate 'utf8mb4_unicode_ci'",
            "not checked",
        ),
        (
            "create table `gl4` (`id` bigint unsigned not null auto_increment primary key, \
             `user_id` bigint unsigned not null, index `gl4_user_id_index`(`user_id`), \
             constraint `gl4_user_id_foreign` foreign key (`user_id`) references `users` (`id`)) \
             default character set utf8mb4 collate 'utf8mb4_unicode_ci'",
            "not checked",
        ),
    ]);
}

#[test]
fn a_table_dump_with_mysqldump_forms_stays_known() {
    // The forms mysqldump and SHOW CREATE TABLE write: backticks, capitals,
    // _utf8mb4'..' strings, a doubled quote in a comment, ((..)) defaults,
    // expression keys, descending prefixes and automatic names. None of them
    // may be refused as a table, or the table would be gone.
    let dump = "CREATE TABLE `things` (
  `id` bigint unsigned NOT NULL AUTO_INCREMENT,
  `kind` varchar(20) COLLATE utf8mb4_unicode_ci NOT NULL DEFAULT _utf8mb4'new',
  `created_at` timestamp NOT NULL DEFAULT CURRENT_TIMESTAMP,
  `updated_at` timestamp(3) NULL DEFAULT CURRENT_TIMESTAMP(3) ON UPDATE CURRENT_TIMESTAMP(3),
  `note` varchar(50) COLLATE utf8mb4_unicode_ci DEFAULT NULL COMMENT 'it''s',
  `day` date DEFAULT (curdate()),
  `n` int DEFAULT ((1 + 1)),
  PRIMARY KEY (`id`),
  KEY `things_kind_index` (`kind`),
  KEY `fn` ((lower(`kind`))),
  KEY `pfx` (`note`(10) DESC),
  CONSTRAINT `things_chk_1` CHECK ((`n` > 0))
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_unicode_ci COMMENT='it''s a table';
";
    let schema = Schema::from_sql(dump);
    assert!(matches!(schema.table("things"), TableState::Known(_)));
    for server in [MYSQL_80, MYSQL_84] {
        let predictions =
            predictions_on(server, dump, "ALTER TABLE things ADD COLUMN zz INT NULL;");
        assert_eq!(predictions.len(), 1);
        assert!(!mentions_fulltext(&predictions[0]), "{server}");
    }
    for server in [MYSQL_80, MYSQL_84] {
        assert_eq!(
            statements_on(
                server,
                dump,
                &parse("ALTER TABLE things ADD COLUMN zz INT NULL;")
            ),
            [INSTANT],
            "{server}"
        );
    }
}

#[test]
fn constraint_and_index_drops_with_words_mysql_rejects_are_refused() {
    // Error 1064 on both servers: IF EXISTS after DROP CONSTRAINT, CASCADE and
    // RESTRICT after a drop (but not after DROP COLUMN), NO INHERIT.
    assert_all_refused(&[
        "ALTER TABLE orders DROP CONSTRAINT IF EXISTS ck_h",
        "ALTER TABLE orders DROP CONSTRAINT ck_h CASCADE",
        "ALTER TABLE orders DROP CONSTRAINT ck_h RESTRICT",
        "ALTER TABLE orders DROP CONSTRAINT fk_x CASCADE",
        "ALTER TABLE orders ADD CONSTRAINT ck_h2 CHECK (amount > 0) NO INHERIT",
        "ALTER TABLE orders ADD COLUMN h1 INT NULL CHECK (h1 > 0) NO INHERIT",
        "ALTER TABLE orders DROP FOREIGN KEY fk_x CASCADE",
        "ALTER TABLE orders DROP FOREIGN KEY fk_x RESTRICT",
        "ALTER TABLE orders DROP INDEX ix CASCADE",
        "ALTER TABLE orders DROP INDEX ix RESTRICT",
        "ALTER TABLE orders DROP KEY ix CASCADE",
        "ALTER TABLE k DROP PRIMARY KEY CASCADE",
        "ALTER TABLE k DROP PRIMARY KEY RESTRICT",
    ]);
}

#[test]
fn dropping_an_index_with_a_trailing_cascade_in_its_own_statement_is_refused() {
    // DROP INDEX ix ON orders CASCADE is error 1064 on both servers too
    // (hand test, not in the list of rules).
    assert_all_refused(&["DROP INDEX ix ON orders CASCADE"]);
}

#[test]
fn drop_column_with_cascade_or_restrict_keeps_the_measured_drop_column_answer() {
    // MySQL accepts the word and ignores it (SHOW CREATE TABLE: the column is
    // gone).
    assert_answers_on_both(&[
        ("ALTER TABLE orders DROP COLUMN note CASCADE", INSTANT),
        ("ALTER TABLE orders DROP COLUMN email RESTRICT", INSTANT),
        ("ALTER TABLE orders DROP seen CASCADE", INSTANT),
    ]);
}

#[test]
fn a_refused_alter_leaves_the_table_known_so_the_next_add_column_has_no_fulltext_note() {
    // A table kilit lost carries the note about a FULLTEXT index it cannot
    // see; a table it still knows does not. After a refusal the table is as
    // before, whatever family the refused statement belongs to.
    for refused in [
        "ALTER TABLE orders ADD COLUMN 'coupon' INT NULL",
        "ALTER TABLE orders ADD INDEX ix_amount ('amount')",
        "ALTER TABLE orders ADD INDEX ix_b4 (note(4294967296))",
        "ALTER TABLE orders ADD CONSTRAINT 'ck' CHECK (amount > 0)",
        "ALTER TABLE orders ADD COLUMN u2 DATETIME NULL ON UPDATE CURRENT_DATE",
        "ALTER TABLE orders ADD COLUMN c24 DATETIME(6) NULL DEFAULT CURRENT_TIMESTAMP",
        "ALTER TABLE orders ADD COLUMN q INT NULL CHECK (q > 0) NO INHERIT",
        "ALTER TABLE orders DROP CONSTRAINT IF EXISTS ck_h",
        "ALTER TABLE orders DROP CONSTRAINT ck_h CASCADE",
        "ALTER TABLE orders DROP FOREIGN KEY fk_x CASCADE",
        "ALTER TABLE orders DROP INDEX ix CASCADE",
    ] {
        let sql = format!("{refused};\nALTER TABLE orders ADD COLUMN zz INT NULL;");
        for server in [MYSQL_80, MYSQL_84] {
            let predictions = predictions_on(server, S13A_TABLES, &sql);
            assert_eq!(predictions.len(), 2, "{refused}");
            assert!(
                matches!(&predictions[0], Prediction::Unknown { reason } if reason.starts_with(NOT_ACCEPTED)),
                "{server}: {refused}: {:?}",
                predictions[0]
            );
            assert!(!mentions_fulltext(&predictions[1]), "{server}: {refused}");
            assert_eq!(
                statements_on(server, S13A_TABLES, &parse(&sql))[1],
                INSTANT,
                "{server}: {refused}"
            );
        }
        assert!(
            matches!(
                Schema::from_sql(&format!("{S13A_TABLES}{refused};")).table("orders"),
                TableState::Known(_)
            ),
            "{refused}"
        );
    }
}

#[test]
fn a_refused_create_table_creates_no_table_for_the_statements_after_it() {
    // MySQL never creates the table, so the next ALTER is an ALTER of a table
    // kilit does not know: no "created earlier" answer, not a new table.
    let accepted = "CREATE TABLE tq (id INT PRIMARY KEY);\nALTER TABLE tq ADD COLUMN c INT NULL;";
    for server in [MYSQL_80, MYSQL_84] {
        assert_eq!(
            impacts_on(server, S13A_TABLES, accepted),
            [Impact::NotChecked, Impact::NewTable],
            "{server}: control"
        );
    }
    for refused in [
        "CREATE TABLE tq (id INT PRIMARY KEY, KEY k (id + 1))",
        "CREATE TABLE tq (id INT PRIMARY KEY, day DATE NULL DEFAULT CURRENT_DATE)",
        "CREATE TABLE tq (id INT PRIMARY KEY, s DATETIME(6) NULL DEFAULT CURRENT_TIMESTAMP)",
        "CREATE TABLE tq (id INT PRIMARY KEY, n INT NULL CHECK (n > 0) NO INHERIT)",
        "CREATE TABLE 'tq' (id INT PRIMARY KEY)",
        "CREATE TABLE tq (id INT PRIMARY KEY, KEY 'k' (id))",
        "CREATE TABLE \"tq\" (id INT PRIMARY KEY)",
    ] {
        let sql = format!("{refused};\nALTER TABLE tq ADD COLUMN c INT NULL;");
        for server in [MYSQL_80, MYSQL_84] {
            assert_eq!(
                impacts_on(server, S13A_TABLES, &sql),
                [Impact::Unknown, Impact::WritesContinue],
                "{server}: {refused}"
            );
            assert!(
                matches!(Schema::from_sql(&sql).table("tq"), TableState::Missing),
                "{refused}"
            );
        }
    }
}

#[test]
fn a_refused_statement_is_unknown_in_the_impact_on_both_servers() {
    for server in [MYSQL_80, MYSQL_84] {
        for refused in [
            "ALTER TABLE orders ADD CONSTRAINT 'ck' CHECK (amount > 0)",
            "CREATE TABLE tq (id INT PRIMARY KEY, day DATE NULL DEFAULT CURRENT_DATE)",
            "ALTER TABLE d ALTER COLUMN day SET DEFAULT (DATE '2020-01-01')",
        ] {
            assert_eq!(
                impacts_on(server, S13A_TABLES, refused),
                [Impact::Unknown],
                "{server}: {refused}"
            );
        }
    }
}

// Step 13a, review findings: key parts that look like a function call but are
// a column name with a prefix length, or a column name that looks like a
// number. Hand tests on MySQL 8.0.46 and 8.4.13 (same result on both):
// manual/step13a/review/h1.sql, h2.sql and h3.sql.

/// The table of the review hand tests: the names are words MySQL reads as a
/// function in some places, or as a number.
const KW_TABLE: &str = "\
CREATE TABLE kw (id INT PRIMARY KEY, ceil VARCHAR(20), floor VARCHAR(20), substr VARCHAR(20), trim VARCHAR(20), `2e` VARCHAR(20), `1e` VARCHAR(20));
";

/// The statement, one at a time against `kw`, is not refused on either server
/// and gets the measured add-index answer (INPLACE, no rebuild, writes
/// continue); the unique form has the same verdict.
fn assert_accepted_as_an_index(statements: &[&str]) {
    let mut problems = Vec::new();
    for statement in statements {
        for server in [MYSQL_80, MYSQL_84] {
            let got = statements_on(server, KW_TABLE, &parse(statement));
            if got.last().map(String::as_str) != Some(INPLACE) {
                problems.push(format!(
                    "{server}: {statement}\n    expected {INPLACE}, got {got:?}"
                ));
            }
            if let Some(Prediction::Unknown { reason }) =
                predictions_on(server, KW_TABLE, statement).last()
                && reason.contains("do not accept")
            {
                problems.push(format!("{server}: {statement}\n    refused: {reason}"));
            }
        }
    }
    assert!(problems.is_empty(), "\n{}", problems.join("\n"));
}

#[test]
fn a_column_named_ceil_or_floor_with_a_prefix_length_is_an_index_mysql_accepts() {
    // ALTER TABLE kw ADD INDEX (ceil(5)) and (floor(5)): accepted; SHOW CREATE
    // TABLE shows KEY ceil (ceil(5)).
    assert_accepted_as_an_index(&[
        "ALTER TABLE kw ADD INDEX (ceil(5))",
        "ALTER TABLE kw ADD INDEX (floor(5))",
        "ALTER TABLE kw ADD INDEX (ceil ( 5 ))",
        "ALTER TABLE kw ADD UNIQUE (floor(5))",
        "CREATE INDEX i7 ON kw (ceil(4))",
    ]);
}

#[test]
fn a_column_named_substr_or_trim_with_a_space_before_the_prefix_length_is_accepted() {
    // ALTER TABLE kw ADD INDEX (substr (5)) and (trim (5)): accepted.
    assert_accepted_as_an_index(&[
        "ALTER TABLE kw ADD INDEX (substr (5))",
        "ALTER TABLE kw ADD INDEX (trim (5))",
    ]);
}

#[test]
fn a_create_table_with_prefix_keys_on_ceil_floor_and_trim_is_not_refused() {
    // CREATE TABLE t3 (id INT PRIMARY KEY, ceil VARCHAR(20), floor VARCHAR(20),
    // trim VARCHAR(20), KEY (ceil(5)), KEY (floor(3)), KEY (trim (4))): accepted.
    let create = "CREATE TABLE t3 (id INT PRIMARY KEY, ceil VARCHAR(20), floor VARCHAR(20), \
                  trim VARCHAR(20), KEY (ceil(5)), KEY (floor(3)), KEY (trim (4)))";
    let sql = format!("{create};\nALTER TABLE t3 ADD COLUMN c INT NULL;");
    for server in [MYSQL_80, MYSQL_84] {
        match predictions_on(server, KW_TABLE, create).last() {
            Some(Prediction::Unknown { reason }) if reason.contains("do not accept") => {
                panic!("{server}: refused: {reason}")
            }
            _ => {}
        }
        assert_eq!(
            impacts_on(server, KW_TABLE, &sql),
            [Impact::NotChecked, Impact::NewTable],
            "{server}"
        );
    }
    assert!(matches!(
        Schema::from_sql(&sql).table("t3"),
        TableState::Known(_)
    ));
}

#[test]
fn a_table_with_a_ceil_prefix_key_is_created_earlier_in_the_input() {
    // The shape Laravel writes for $table->string('ceil', 100) with an index
    // named posts_ceil_index, here with a prefix length: accepted by MySQL, so
    // the table exists for the ALTER after it (not Missing, not unknown).
    let create = "CREATE TABLE posts (id BIGINT UNSIGNED AUTO_INCREMENT PRIMARY KEY, \
                  ceil VARCHAR(100) NOT NULL, KEY posts_ceil_index (ceil(10)))";
    let sql = format!("{create};\nALTER TABLE posts ADD COLUMN c INT NULL;");
    for server in [MYSQL_80, MYSQL_84] {
        assert_eq!(
            impacts_on(server, KW_TABLE, &sql),
            [Impact::NotChecked, Impact::NewTable],
            "{server}"
        );
    }
    assert!(matches!(
        Schema::from_sql(&sql).table("posts"),
        TableState::Known(_)
    ));
}

#[test]
fn a_schema_file_with_a_ceil_prefix_key_makes_the_table_known() {
    // The same CREATE TABLE in the --schema file: the table is known, so the
    // ALTER after it is answered from the table (an added column is INSTANT).
    let schema = "CREATE TABLE posts (id BIGINT UNSIGNED AUTO_INCREMENT PRIMARY KEY, \
                  ceil VARCHAR(100) NOT NULL, KEY posts_ceil_index (ceil(10)));\n";
    assert!(matches!(
        Schema::from_sql(schema).table("posts"),
        TableState::Known(_)
    ));
    for server in [MYSQL_80, MYSQL_84] {
        assert_eq!(
            statements_on(
                server,
                schema,
                &parse("ALTER TABLE posts ADD COLUMN c INT NULL")
            ),
            [INSTANT],
            "{server}"
        );
    }
}

#[test]
fn a_column_named_2e_or_1e_is_an_index_column_mysql_accepts() {
    // ALTER TABLE kw ADD INDEX (2e) and ADD INDEX (1e DESC): accepted; the
    // column names look like numbers in scientific form.
    assert_accepted_as_an_index(&[
        "ALTER TABLE kw ADD INDEX (2e)",
        "ALTER TABLE kw ADD INDEX (1e DESC)",
    ]);
}

#[test]
fn a_number_as_a_key_part_is_refused_on_both_servers() {
    // ALTER TABLE kw ADD INDEX (9), (1e5), (0x1): error 1064 on both servers,
    // whatever columns the table has.
    for statement in [
        "ALTER TABLE kw ADD INDEX (9)",
        "ALTER TABLE kw ADD INDEX (1e5)",
        "ALTER TABLE kw ADD INDEX (0x1)",
    ] {
        for server in [MYSQL_80, MYSQL_84] {
            match predictions_on(server, KW_TABLE, statement).last() {
                Some(Prediction::Unknown { reason }) if reason.starts_with(NOT_ACCEPTED) => {}
                other => panic!("{server}: {statement}\n    got {other:?}"),
            }
        }
    }
}

// Step 13b: DROP CHECK, ALTER CHECK and ALTER CONSTRAINT [NOT] ENFORCED, and
// ADD CONSTRAINT ... CHECK (...) NOT ENFORCED. Laravel has no call for any of
// them; they come as DB::statement('alter table ...') or in a schema dump.
// The answers are the lab's (drop-check, alter-check-*, alter-constraint-*,
// add-check-constraint-not-enforced; both servers agree on every scenario),
// the refusals are hand tests on MySQL 8.0.46 and 8.4.13 (same result on
// both): the 1064 forms, error 3821 for DROP CHECK and ALTER CHECK of a name
// that is no CHECK, 3941 and 3940 for ALTER CONSTRAINT of one.

/// A mysqldump of `orders`: a foreign key, a unique and a plain index, an
/// enforced CHECK on one column (chk_amount), an enforced CHECK on two columns
/// (chk_two) and a NOT ENFORCED CHECK (chk_off).
const S13B_SCHEMA: &str = "\
CREATE TABLE `users` (
  `id` bigint unsigned NOT NULL AUTO_INCREMENT,
  `name` varchar(100) NOT NULL,
  PRIMARY KEY (`id`)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_0900_ai_ci;
CREATE TABLE `orders` (
  `id` bigint unsigned NOT NULL AUTO_INCREMENT,
  `user_id` bigint unsigned NOT NULL,
  `amount` int NOT NULL,
  `qty` int NOT NULL,
  `price` int NOT NULL,
  `stock` int NOT NULL,
  `email` varchar(255) DEFAULT NULL,
  `note` varchar(50) DEFAULT NULL,
  PRIMARY KEY (`id`),
  UNIQUE KEY `uq_email` (`email`),
  KEY `ix_note` (`note`),
  KEY `fk_orders_user` (`user_id`),
  CONSTRAINT `fk_orders_user` FOREIGN KEY (`user_id`) REFERENCES `users` (`id`),
  CONSTRAINT `chk_amount` CHECK ((`amount` > 0)),
  CONSTRAINT `chk_two` CHECK ((`qty` > `price`)),
  CONSTRAINT `chk_off` CHECK ((`stock` > 0)) /*!80016 NOT ENFORCED */
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_0900_ai_ci;
";

/// An `orders` whose CHECK names need backticks.
const S13B_NAMED: &str = "\
CREATE TABLE `orders` (
  `id` bigint NOT NULL,
  `amount` int NOT NULL,
  PRIMARY KEY (`id`),
  CONSTRAINT `ck amount` CHECK ((`amount` > 0)),
  CONSTRAINT `ck``q` CHECK ((`amount` > 1)),
  CONSTRAINT `check` CHECK ((`amount` > 3)) /*!80016 NOT ENFORCED */
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4;
";

const ADD_COUPON: &str = "ALTER TABLE orders ADD COLUMN coupon INT NULL";

/// The measured answer of the last statement: (case, verdict text, notes).
fn measured_13b(server: &str, schema: &str, sql: &str) -> (&'static str, String, Vec<String>) {
    match predictions_on(server, schema, sql).pop().unwrap() {
        Prediction::Measured {
            case,
            verdict,
            notes,
            ..
        } => (case, verdict.to_string(), notes),
        other => panic!("{server}: {sql}\n    expected a measured answer, got {other:?}"),
    }
}

/// kilit's answer to each statement of `sql` against `schema` is `expected`,
/// on both servers.
fn assert_13b(schema: &str, sql: &str, expected: &[&str]) {
    for server in [MYSQL_80, MYSQL_84] {
        assert_eq!(
            statements_on(server, schema, &parse(sql)),
            expected,
            "{server}: {sql}"
        );
    }
}

/// The last statement of `sql` is unknown with a reason that starts with
/// `NOT_ACCEPTED` or contains `part`, on both servers. Returns the reason.
fn assert_unknown_13b(schema: &str, sql: &str, part: &str) -> String {
    let mut reason = String::new();
    for server in [MYSQL_80, MYSQL_84] {
        match predictions_on(server, schema, sql).pop() {
            Some(Prediction::Unknown { reason: found }) => {
                assert!(
                    found.contains(part),
                    "{server}: {sql}\n    reason {found:?} lacks {part:?}"
                );
                reason = found;
            }
            other => panic!("{server}: {sql}\n    expected unknown, got {other:?}"),
        }
    }
    reason
}

#[test]
fn dropping_a_check_with_drop_check_is_instant_and_writes_continue() {
    // DROP CHECK chk_amount (one column), chk_two (two columns) and chk_off
    // (NOT ENFORCED): INSTANT on both servers, the case of DROP CONSTRAINT.
    for name in ["chk_amount", "chk_two", "chk_off"] {
        for form in ["DROP CHECK", "DROP CONSTRAINT"] {
            for server in [MYSQL_80, MYSQL_84] {
                let sql = format!("ALTER TABLE orders {form} {name}");
                let (case, verdict, _) = measured_13b(server, S13B_SCHEMA, &sql);
                assert_eq!(case, "drop-check-constraint", "{server}: {sql}");
                assert_eq!(verdict, INSTANT, "{server}: {sql}");
            }
        }
    }
}

#[test]
fn changing_whether_a_check_is_enforced_is_instant_and_writes_continue() {
    // alter-check-not-enforced, alter-check-not-enforced-again,
    // alter-check-enforced-again and the same three with ALTER CONSTRAINT.
    for form in ["ALTER CHECK", "ALTER CONSTRAINT"] {
        for (name, state) in [
            ("chk_amount", "NOT ENFORCED"),
            ("chk_two", "NOT ENFORCED"),
            ("chk_off", "NOT ENFORCED"),
            ("chk_amount", "ENFORCED"),
            ("chk_two", "ENFORCED"),
        ] {
            for server in [MYSQL_80, MYSQL_84] {
                let sql = format!("ALTER TABLE orders {form} {name} {state}");
                let (case, verdict, notes) = measured_13b(server, S13B_SCHEMA, &sql);
                assert_eq!(case, "alter-check-constraint", "{server}: {sql}");
                assert_eq!(verdict, INSTANT, "{server}: {sql}");
                assert!(
                    !notes.iter().any(|note| note.contains("3819")),
                    "{server}: {sql}\n    {notes:?}"
                );
            }
        }
    }
}

#[test]
fn enforcing_a_not_enforced_check_copies_the_table_and_notes_the_rows_that_break_it() {
    // alter-check-enforced and alter-constraint-enforced: COPY on both
    // servers; the rows-break-it scenarios fail with 3819, which no schema
    // shows, so it is a note, as for ADD CHECK.
    for form in ["ALTER CHECK", "ALTER CONSTRAINT"] {
        for server in [MYSQL_80, MYSQL_84] {
            let sql = format!("ALTER TABLE orders {form} chk_off ENFORCED");
            let (case, verdict, notes) = measured_13b(server, S13B_SCHEMA, &sql);
            assert_eq!(case, "enforce-check-constraint", "{server}: {sql}");
            assert_eq!(verdict, COPY, "{server}: {sql}");
            assert!(
                notes
                    .iter()
                    .any(|note| note.starts_with("if a row breaks the constraint")
                        && note.contains("fails with error 3819")),
                "{server}: {sql}\n    {notes:?}"
            );
        }
    }
}

#[test]
fn forcing_an_algorithm_or_lock_on_enforcing_a_check_gets_the_answer_of_that_variant() {
    // alter-check-enforced and alter-constraint-enforced, six variants: the
    // as-written run and ALGORITHM=COPY copy the table; INSTANT, INPLACE and
    // INPLACE with LOCK=NONE fail with 1845; LOCK=NONE alone with 1846.
    for form in ["ALTER CHECK", "ALTER CONSTRAINT"] {
        let base = format!("ALTER TABLE orders {form} chk_off ENFORCED");
        for (clause, answer) in [
            ("", COPY),
            (", ALGORITHM=COPY", COPY),
            (", ALGORITHM=INSTANT", "fails with error 1845"),
            (", ALGORITHM=INPLACE", "fails with error 1845"),
            (", ALGORITHM=INPLACE, LOCK=NONE", "fails with error 1845"),
            (", LOCK=NONE", "fails with error 1846"),
        ] {
            assert_13b(S13B_SCHEMA, &format!("{base}{clause}"), &[answer]);
        }
    }
}

#[test]
fn forcing_an_algorithm_or_lock_on_the_instant_check_changes_keeps_the_lab_s_variants() {
    // drop-check, alter-check-not-enforced, add-check-constraint-not-enforced
    // and the others: accepted in every variant without a rebuild, except
    // ALGORITHM=COPY, which copies the table.
    for base in [
        "ALTER TABLE orders DROP CHECK chk_amount",
        "ALTER TABLE orders ALTER CHECK chk_amount NOT ENFORCED",
        "ALTER TABLE orders ALTER CONSTRAINT chk_off NOT ENFORCED",
        "ALTER TABLE orders ADD CONSTRAINT chk_new CHECK (amount >= 0) NOT ENFORCED",
    ] {
        for server in [MYSQL_80, MYSQL_84] {
            for (clause, starts) in [
                (", ALGORITHM=INSTANT", "INSTANT"),
                (", ALGORITHM=INPLACE", "INPLACE"),
                (", ALGORITHM=INPLACE, LOCK=NONE", "INPLACE"),
                (", LOCK=NONE", ""),
            ] {
                let sql = format!("{base}{clause}");
                let (_, verdict, _) = measured_13b(server, S13B_SCHEMA, &sql);
                assert!(
                    verdict.starts_with(starts)
                        && verdict.ends_with(", no rebuild, writes continue"),
                    "{server}: {sql}\n    got {verdict}"
                );
            }
            // A forced COPY run has no write measurement in the lab.
            let sql = format!("{base}, ALGORITHM=COPY");
            let (_, verdict, _) = measured_13b(server, S13B_SCHEMA, &sql);
            assert_eq!(verdict, COPY_NOT_MEASURED, "{server}: {sql}");
        }
    }
}

#[test]
fn the_enforced_state_of_a_check_is_followed_from_statement_to_statement() {
    let enforce =
        |name: &str, state: &str| format!("ALTER TABLE orders ALTER CHECK {name} {state};");
    // Not enforced, then enforced again: the second copies the table.
    assert_13b(
        S13B_SCHEMA,
        &format!(
            "{}{}",
            enforce("chk_amount", "NOT ENFORCED"),
            enforce("chk_amount", "ENFORCED")
        ),
        &[INSTANT, COPY],
    );
    // Enforced twice on an enforced CHECK: both INSTANT.
    assert_13b(
        S13B_SCHEMA,
        &format!(
            "{}{}",
            enforce("chk_amount", "ENFORCED"),
            enforce("chk_amount", "ENFORCED")
        ),
        &[INSTANT, INSTANT],
    );
    // The CHECK of the dump that is NOT ENFORCED: enforcing copies the table
    // once, then it is enforced.
    assert_13b(
        S13B_SCHEMA,
        &format!(
            "{}{}{}{}",
            enforce("chk_off", "ENFORCED"),
            enforce("chk_off", "ENFORCED"),
            enforce("chk_off", "NOT ENFORCED"),
            enforce("chk_off", "ENFORCED")
        ),
        &[COPY, INSTANT, INSTANT, COPY],
    );
    // ALTER CONSTRAINT and ALTER CHECK set the same state.
    assert_13b(
        S13B_SCHEMA,
        "ALTER TABLE orders ALTER CONSTRAINT chk_amount NOT ENFORCED;
         ALTER TABLE orders ALTER CHECK chk_amount ENFORCED;
         ALTER TABLE orders ALTER CHECK chk_off ENFORCED;
         ALTER TABLE orders ALTER CONSTRAINT chk_off ENFORCED;",
        &[INSTANT, COPY, COPY, INSTANT],
    );
    // Changing the state keeps the CHECK: it can still be dropped.
    assert_13b(
        S13B_SCHEMA,
        "ALTER TABLE orders ALTER CHECK chk_amount NOT ENFORCED;
         ALTER TABLE orders DROP CHECK chk_amount;",
        &[INSTANT, INSTANT],
    );
}

#[test]
fn a_failed_enforcing_leaves_the_check_not_enforced() {
    // ALGORITHM=INSTANT and LOCK=NONE fail on enforcing (1845, 1846), so the
    // CHECK stays NOT ENFORCED and the next enforcing copies the table.
    assert_13b(
        S13B_SCHEMA,
        "ALTER TABLE orders ALTER CHECK chk_off ENFORCED, ALGORITHM=INSTANT;
         ALTER TABLE orders ALTER CHECK chk_off ENFORCED, LOCK=NONE;
         ALTER TABLE orders ALTER CHECK chk_off ENFORCED;
         ALTER TABLE orders ALTER CHECK chk_off ENFORCED;",
        &[
            "fails with error 1845",
            "fails with error 1846",
            COPY,
            INSTANT,
        ],
    );
}

#[test]
fn a_check_that_is_not_enforced_still_keeps_the_columns_it_uses() {
    // Hand test: after ALTER CHECK chk_two NOT ENFORCED, DROP COLUMN of its
    // column fails with 3959 still; after DROP CHECK it does not.
    assert_13b(
        S13B_SCHEMA,
        "ALTER TABLE orders ALTER CHECK chk_two NOT ENFORCED;
         ALTER TABLE orders DROP COLUMN qty;
         ALTER TABLE orders DROP CHECK chk_two;
         ALTER TABLE orders DROP COLUMN qty;",
        &[INSTANT, "fails with error 3959", INSTANT, INSTANT],
    );
}

#[test]
fn dropping_a_check_with_drop_check_frees_the_column_it_used_with_another() {
    // chk_two uses qty and price: DROP COLUMN qty fails with 3959 until the
    // CHECK is gone. The failed DROP COLUMN changes nothing.
    for form in ["DROP CHECK", "DROP CONSTRAINT"] {
        assert_13b(
            S13B_SCHEMA,
            &format!(
                "ALTER TABLE orders DROP COLUMN qty;
                 ALTER TABLE orders {form} chk_two;
                 ALTER TABLE orders DROP COLUMN qty;
                 ALTER TABLE orders DROP COLUMN price;"
            ),
            &["fails with error 3959", INSTANT, INSTANT, INSTANT],
        );
    }
}

#[test]
fn after_a_check_clause_the_table_is_still_known() {
    // Before step 13b the statement was unreadable and the table lost, and
    // the ADD COLUMN after it carried the note about a FULLTEXT index.
    for statement in [
        "ALTER TABLE orders DROP CHECK chk_amount",
        "ALTER TABLE orders DROP CHECK chk_two",
        "ALTER TABLE orders DROP CHECK chk_off",
        "ALTER TABLE orders ALTER CHECK chk_amount NOT ENFORCED",
        "ALTER TABLE orders ALTER CHECK chk_off ENFORCED",
        "ALTER TABLE orders ALTER CONSTRAINT chk_amount NOT ENFORCED",
        "ALTER TABLE orders ALTER CONSTRAINT chk_off ENFORCED",
        "ALTER TABLE orders ADD CONSTRAINT chk_new CHECK (amount > 10) NOT ENFORCED",
    ] {
        let sql = format!("{statement};\n{ADD_COUPON}");
        for server in [MYSQL_80, MYSQL_84] {
            let predictions = predictions_on(server, S13B_SCHEMA, &sql);
            assert_eq!(predictions.len(), 2, "{server}: {sql}");
            assert!(
                !mentions_fulltext(&predictions[1]),
                "{server}: {sql}\n    {:?}",
                predictions[1]
            );
        }
        assert_13b(S13B_SCHEMA, &sql, &[statements_after(statement), INSTANT]);
    }
}

/// The answer the first statement of `after_a_check_clause_the_table_is_still_known`
/// gets, by its operation.
fn statements_after(statement: &str) -> &'static str {
    if statement.contains("ENFORCED") && !statement.contains("NOT ENFORCED") {
        COPY
    } else {
        INSTANT
    }
}

#[test]
fn adding_a_not_enforced_check_is_instant_and_has_no_note_about_rows() {
    // add-check-constraint-not-enforced and its rows-break-it twin: INSTANT
    // on both servers, whatever the rows hold.
    for sql in [
        "ALTER TABLE orders ADD CONSTRAINT chk_new CHECK (amount >= 0) NOT ENFORCED",
        "ALTER TABLE orders ADD CONSTRAINT chk_new CHECK (amount > 10) NOT ENFORCED",
        "ALTER TABLE orders ADD CONSTRAINT chk_new CHECK (qty > price) NOT ENFORCED",
        "alter table `orders` add constraint `chk_new` check (`amount` > 0) not enforced",
    ] {
        for server in [MYSQL_80, MYSQL_84] {
            let (case, verdict, notes) = measured_13b(server, S13B_SCHEMA, sql);
            assert_eq!(case, "add-not-enforced-check-constraint", "{server}: {sql}");
            assert_eq!(verdict, INSTANT, "{server}: {sql}");
            assert!(
                !notes.iter().any(|note| note.contains("3819")),
                "{server}: {sql}\n    {notes:?}"
            );
        }
    }
}

#[test]
fn adding_a_check_without_not_enforced_keeps_its_copy_answer_and_its_note() {
    // The nearest existing answer: add-check-constraint.
    for sql in [
        "ALTER TABLE orders ADD CONSTRAINT chk_new CHECK (amount >= 0)",
        "ALTER TABLE orders ADD CONSTRAINT chk_new CHECK (amount >= 0) ENFORCED",
    ] {
        for server in [MYSQL_80, MYSQL_84] {
            let (case, verdict, notes) = measured_13b(server, S13B_SCHEMA, sql);
            assert_eq!(case, "add-check-constraint", "{server}: {sql}");
            assert_eq!(verdict, COPY, "{server}: {sql}");
            assert!(
                notes
                    .iter()
                    .any(|note| note.contains("fails with error 3819")),
                "{server}: {sql}\n    {notes:?}"
            );
        }
    }
}

#[test]
fn a_not_enforced_check_that_add_check_would_not_answer_stays_unknown() {
    // Everything ADD CHECK keeps unknown stays unknown with NOT ENFORCED: an
    // expression that is not plain, a name the table already has, a column
    // the table does not have.
    for shape in [
        "CONSTRAINT chk_new CHECK (LOWER(note) = 'a')",
        "CONSTRAINT chk_new CHECK (nosuch > 0)",
        "CONSTRAINT chk_amount CHECK (amount > 5)",
    ] {
        for state in ["", " NOT ENFORCED"] {
            let sql = format!("ALTER TABLE orders ADD {shape}{state}");
            assert_unknown_13b(S13B_SCHEMA, &sql, "");
        }
    }
}

#[test]
fn a_check_added_not_enforced_is_followed_as_not_enforced() {
    assert_13b(
        S13B_SCHEMA,
        "ALTER TABLE orders ADD CONSTRAINT chk_new CHECK (amount > 10) NOT ENFORCED;
         ALTER TABLE orders ALTER CHECK chk_new ENFORCED;
         ALTER TABLE orders ALTER CHECK chk_new ENFORCED;",
        &[INSTANT, COPY, INSTANT],
    );
    // An enforced one is enforced already.
    assert_13b(
        S13B_SCHEMA,
        "ALTER TABLE orders ADD CONSTRAINT chk_new CHECK (amount > 10);
         ALTER TABLE orders ALTER CHECK chk_new ENFORCED;
         ALTER TABLE orders DROP CHECK chk_new;",
        &[COPY, INSTANT, INSTANT],
    );
}

#[test]
fn clauses_mysql_rejects_are_unknown_and_leave_the_table_known_and_unchanged() {
    // Each one is error 1064 on both servers (hand tests), whatever the CHECK
    // names. The table stays as it was: ADD COLUMN gets the answer of a known
    // table, and the CHECK named in the statement can still be dropped.
    for statement in [
        "ALTER TABLE orders DROP CHECK",
        "ALTER TABLE orders DROP CHECK, ALGORITHM=INSTANT",
        "ALTER TABLE orders DROP CHECK, ALGORITHM=INPLACE, LOCK=NONE",
        "ALTER TABLE orders DROP CHECK IF EXISTS chk_amount",
        "ALTER TABLE orders DROP CHECK IF EXISTS nosuch",
        "ALTER TABLE orders ALTER CHECK chk_amount",
        "ALTER TABLE orders ALTER CHECK chk_amount, ALGORITHM=INSTANT",
        "ALTER TABLE orders ALTER CHECK ENFORCED",
        "ALTER TABLE orders ALTER CHECK NOT ENFORCED",
        "ALTER TABLE orders ALTER CHECK",
        "ALTER TABLE orders ALTER CONSTRAINT chk_amount",
        "ALTER TABLE orders ALTER CONSTRAINT ENFORCED",
        "ALTER TABLE orders ALTER CONSTRAINT",
        "ALTER TABLE orders ALTER CHECK IF EXISTS chk_amount ENFORCED",
        "ALTER TABLE orders DROP CHECK chk_amount CASCADE",
        "ALTER TABLE orders DROP CHECK chk_amount RESTRICT",
        "ALTER TABLE orders ALTER CHECK chk_amount NOT ENFORCED CASCADE",
        "ALTER TABLE orders ALTER CONSTRAINT chk_amount NOT ENFORCED RESTRICT",
        "ALTER TABLE orders DROP CHECK chk_amount,",
        "ALTER TABLE orders ALTER CHECK chk_amount ENFORCED ENFORCED",
        "ALTER TABLE orders ALTER CHECK chk_amount NOT NOT ENFORCED",
        "ALTER TABLE orders DROP CHECK (chk_amount)",
        "ALTER TABLE orders DROP CHECK PRIMARY",
        "ALTER TABLE orders ALTER CONSTRAINT PRIMARY ENFORCED",
        "ALTER TABLE orders DROP CHECK 'chk_amount'",
        "ALTER TABLE orders ALTER CHECK 'chk_amount' ENFORCED",
        "ALTER TABLE orders ALTER CHECK 'chk_amount' NOT ENFORCED",
        "ALTER TABLE orders ALTER CONSTRAINT 'chk_amount' NOT ENFORCED",
        "alter table `orders` drop check",
        "alter table `orders` drop check if exists `chk_amount`",
    ] {
        let sql = format!(
            "{statement};\n{ADD_COUPON};\nALTER TABLE orders DROP CHECK chk_amount;\n\
             ALTER TABLE orders DROP COLUMN qty"
        );
        for server in [MYSQL_80, MYSQL_84] {
            let predictions = predictions_on(server, S13B_SCHEMA, &sql);
            assert_eq!(predictions.len(), 4, "{server}: {sql}");
            match &predictions[0] {
                Prediction::Unknown { reason } if reason.starts_with(NOT_ACCEPTED) => {}
                other => panic!("{server}: {statement}\n    got {other:?}"),
            }
            assert!(
                !mentions_fulltext(&predictions[1]),
                "{server}: {statement}: the table was lost"
            );
        }
        // Not a column drop, and nothing else changes: the rest as for a
        // table that nothing happened to.
        assert_13b(
            S13B_SCHEMA,
            &sql,
            &["unknown", INSTANT, INSTANT, "fails with error 3959"],
        );
    }
}

#[test]
fn drop_check_without_a_name_is_not_a_column_drop() {
    // sqlparser reads `DROP CHECK` as dropping a column named CHECK, which
    // would be "INSTANT, writes continue, drop-column" (hand test: error 1064).
    for statement in [
        "ALTER TABLE orders DROP CHECK",
        "ALTER TABLE orders DROP CHECK, ALGORITHM=INSTANT",
    ] {
        let reason = assert_unknown_13b(S13B_SCHEMA, statement, NOT_ACCEPTED);
        assert!(reason.starts_with(NOT_ACCEPTED), "{reason}");
    }
}

#[test]
fn a_double_quoted_check_name_is_unknown_because_of_ansi_quotes_and_leaves_the_table_known() {
    // Error 1064 by default (hand test): a double-quoted word is a string.
    // Under ANSI_QUOTES it would be a name; kilit does not know the sql_mode.
    for statement in [
        "ALTER TABLE orders DROP CHECK \"chk_amount\"",
        "ALTER TABLE orders ALTER CHECK \"chk_amount\" NOT ENFORCED",
        "ALTER TABLE orders ALTER CONSTRAINT \"chk_amount\" NOT ENFORCED",
    ] {
        assert_unknown_13b(S13B_SCHEMA, statement, "ANSI_QUOTES");
        assert_13b(
            S13B_SCHEMA,
            &format!("{statement};\n{ADD_COUPON};\nALTER TABLE orders DROP CHECK chk_amount"),
            &["unknown", INSTANT, INSTANT],
        );
        for server in [MYSQL_80, MYSQL_84] {
            let predictions =
                predictions_on(server, S13B_SCHEMA, &format!("{statement};\n{ADD_COUPON}"));
            assert!(!mentions_fulltext(&predictions[1]), "{server}: {statement}");
        }
    }
}

#[test]
fn a_name_that_is_no_check_of_the_table_is_unknown_and_the_table_unchanged() {
    // A foreign key, a unique index, PRIMARY or a plain index: error 3821
    // (DROP CHECK, ALTER CHECK) or 3941 / 3940 (ALTER CONSTRAINT) on both
    // servers. kilit gives no error number; the table stays as it was, so the
    // foreign key still fails the drop of its column (1828) and the CHECK is
    // still there.
    let names = ["fk_orders_user", "uq_email", "`PRIMARY`", "ix_note"];
    let mut forms: Vec<(String, &str)> = Vec::new();
    for name in names {
        forms.push((format!("DROP CHECK {name}"), "do not accept DROP CHECK"));
        for state in ["ENFORCED", "NOT ENFORCED"] {
            forms.push((
                format!("ALTER CHECK {name} {state}"),
                "do not accept ALTER CHECK",
            ));
            forms.push((
                format!("ALTER CONSTRAINT {name} {state}"),
                "do not accept ALTER CONSTRAINT",
            ));
        }
    }
    for (clause, part) in forms {
        let statement = format!("ALTER TABLE orders {clause}");
        let reason = assert_unknown_13b(S13B_SCHEMA, &statement, part);
        assert!(
            reason.contains("MySQL 8.0.46 and 8.4.13 do not accept"),
            "{statement}: {reason}"
        );
        assert_13b(
            S13B_SCHEMA,
            &format!(
                "{statement};
                 ALTER TABLE orders DROP COLUMN user_id;
                 {ADD_COUPON};
                 ALTER TABLE orders DROP CHECK chk_amount;"
            ),
            &["unknown", "fails with error 1828", INSTANT, INSTANT],
        );
        for server in [MYSQL_80, MYSQL_84] {
            let predictions =
                predictions_on(server, S13B_SCHEMA, &format!("{statement};\n{ADD_COUPON}"));
            assert!(!mentions_fulltext(&predictions[1]), "{server}: {statement}");
        }
    }
}

#[test]
fn a_name_the_table_has_no_constraint_or_index_of_is_unknown_and_the_table_is_lost() {
    // Error 3821 / 3940 on both servers (hand tests). kilit loses the table
    // as it does for DROP CONSTRAINT of an unknown name, so the ADD COLUMN
    // after it gets the notes of a table kilit does not know.
    for statement in [
        "ALTER TABLE orders DROP CHECK nosuch",
        "ALTER TABLE orders DROP CHECK note",
        "ALTER TABLE orders ALTER CHECK nosuch NOT ENFORCED",
        "ALTER TABLE orders ALTER CHECK nosuch ENFORCED",
        "ALTER TABLE orders ALTER CONSTRAINT nosuch ENFORCED",
        "ALTER TABLE orders ALTER CONSTRAINT nosuch NOT ENFORCED",
    ] {
        assert_unknown_13b(S13B_SCHEMA, statement, "");
        for server in [MYSQL_80, MYSQL_84] {
            let predictions =
                predictions_on(server, S13B_SCHEMA, &format!("{statement};\n{ADD_COUPON}"));
            assert!(mentions_fulltext(&predictions[1]), "{server}: {statement}");
        }
    }
}

#[test]
fn a_check_dropped_twice_is_unknown_the_second_time_and_loses_the_table() {
    // Hand test: DROP CHECK of a name the table no longer has is error 3821.
    let sql = "ALTER TABLE orders DROP CHECK chk_amount;
               ALTER TABLE orders DROP CHECK chk_amount;";
    assert_13b(S13B_SCHEMA, sql, &[INSTANT, "unknown"]);
    for server in [MYSQL_80, MYSQL_84] {
        let predictions = predictions_on(server, S13B_SCHEMA, &format!("{sql}\n{ADD_COUPON};"));
        assert!(mentions_fulltext(&predictions[2]), "{server}");
    }
}

#[test]
fn a_check_and_a_foreign_key_with_the_same_name_make_the_clause_unknown_and_lose_the_table() {
    let schema = "\
CREATE TABLE users (id BIGINT UNSIGNED AUTO_INCREMENT PRIMARY KEY, name VARCHAR(100) NOT NULL);
CREATE TABLE orders (id BIGINT UNSIGNED AUTO_INCREMENT PRIMARY KEY, user_id BIGINT UNSIGNED NOT NULL, \
amount INT NOT NULL, KEY dup (user_id), CONSTRAINT dup FOREIGN KEY (user_id) REFERENCES users (id), \
CONSTRAINT dup CHECK (amount > 0));
";
    for statement in [
        "ALTER TABLE orders DROP CHECK dup",
        "ALTER TABLE orders ALTER CHECK dup NOT ENFORCED",
        "ALTER TABLE orders ALTER CONSTRAINT dup ENFORCED",
    ] {
        assert_unknown_13b(schema, statement, "");
        for server in [MYSQL_80, MYSQL_84] {
            let predictions =
                predictions_on(server, schema, &format!("{statement};\n{ADD_COUPON};"));
            assert!(mentions_fulltext(&predictions[1]), "{server}: {statement}");
        }
    }
}

#[test]
fn a_check_clause_on_a_lost_or_missing_table_is_unknown_and_never_a_failure() {
    // Lost: an earlier clause on an unknown name took the table. Missing: the
    // table is in no schema and no statement.
    let lost = "ALTER TABLE orders DROP CHECK nosuch;\n";
    for clause in [
        "DROP CHECK chk_amount",
        "ALTER CHECK chk_amount NOT ENFORCED",
        "ALTER CHECK chk_off ENFORCED",
        "ALTER CONSTRAINT chk_off ENFORCED",
        "ALTER CONSTRAINT fk_orders_user ENFORCED",
    ] {
        assert_13b(
            S13B_SCHEMA,
            &format!("{lost}ALTER TABLE orders {clause}"),
            &["unknown", "unknown"],
        );
        assert_13b(
            S13B_SCHEMA,
            &format!("ALTER TABLE ghosts {clause}"),
            &["unknown"],
        );
        assert_13b("", &format!("ALTER TABLE orders {clause}"), &["unknown"]);
    }
}

#[test]
fn several_check_clauses_in_one_statement_are_unknown_and_each_check_leaves_the_schema() {
    // Hand test: DROP CHECK a, DROP CHECK b is accepted and drops both.
    let reason = assert_unknown_13b(
        S13B_SCHEMA,
        "ALTER TABLE orders DROP CHECK chk_amount, DROP CHECK chk_two",
        "2 changes in one statement",
    );
    assert!(!reason.is_empty());
    assert_13b(
        S13B_SCHEMA,
        "ALTER TABLE orders DROP CHECK chk_amount, DROP CHECK chk_two;
         ALTER TABLE orders DROP COLUMN qty;",
        &["unknown", INSTANT],
    );
    // A CHECK clause next to a table option: unknown, the table stays known.
    let sql = "ALTER TABLE orders DROP CHECK chk_amount, COMMENT = 'x';";
    assert_unknown_13b(S13B_SCHEMA, sql, "");
    for server in [MYSQL_80, MYSQL_84] {
        let predictions = predictions_on(server, S13B_SCHEMA, &format!("{sql}\n{ADD_COUPON};"));
        assert!(!mentions_fulltext(&predictions[1]), "{server}");
    }
}

#[test]
fn the_check_clauses_are_read_in_every_spelling_mysql_accepts() {
    // Hand tests on both servers: backticks, lower and mixed case, the name in
    // another letter case, the schema before the table, a comment between the
    // words, the options before and after.
    for (statement, answer) in [
        ("ALTER TABLE `orders` DROP CHECK `chk_amount`", INSTANT),
        ("alter table orders drop check chk_amount", INSTANT),
        ("Alter Table orders Drop Check chk_amount", INSTANT),
        ("ALTER TABLE orders DROP CHECK CHK_AMOUNT", INSTANT),
        ("ALTER TABLE orders DROP /* x */ CHECK chk_amount", INSTANT),
        (
            "ALTER TABLE orders DROP CHECK chk_amount, ALGORITHM=INSTANT",
            INSTANT,
        ),
        (
            "ALTER TABLE orders ALTER CHECK `chk_amount` NOT ENFORCED",
            INSTANT,
        ),
        ("alter table orders alter check chk_off enforced", COPY),
        (
            "alter table `orders` alter constraint `chk_off` not enforced",
            INSTANT,
        ),
        ("Alter Table orders Alter Constraint CHK_OFF Enforced", COPY),
        (
            "ALTER TABLE orders ALTER /* x */ CHECK chk_off ENFORCED",
            COPY,
        ),
        (
            "ALTER TABLE orders ALGORITHM = INSTANT, ALTER CHECK chk_amount NOT ENFORCED",
            INSTANT,
        ),
        (
            "ALTER TABLE orders LOCK=DEFAULT, ALGORITHM=DEFAULT, ALTER /* x */ CHECK chk_off ENFORCED",
            COPY,
        ),
    ] {
        assert_13b(S13B_SCHEMA, statement, &[answer]);
    }
}

#[test]
fn a_schema_qualified_table_is_read_but_not_known_so_the_check_clause_is_unknown() {
    // Schema-qualified names are a limitation older than this step: shop.orders
    // is not the --schema table orders. The clause is read (it is not "could
    // not read") and the table is unknown, as for DROP CONSTRAINT.
    for statement in [
        "ALTER TABLE shop.orders DROP CHECK chk_amount",
        "ALTER TABLE `shop`.`orders` DROP CHECK `chk_amount`",
        "ALTER TABLE shop.orders ALTER CHECK chk_off ENFORCED",
        "ALTER TABLE shop.orders ALTER CONSTRAINT chk_off NOT ENFORCED",
    ] {
        let reason = assert_unknown_13b(S13B_SCHEMA, statement, "kilit does not know this table");
        assert!(!reason.contains("could not read"), "{statement}: {reason}");
    }
    assert_unknown_13b(
        S13B_SCHEMA,
        "ALTER TABLE shop.orders DROP CHECK chk_amount",
        "kilit does not know this table, so it cannot tell whether chk_amount is one of its CHECK constraints",
    );
}

#[test]
fn a_check_name_with_a_space_or_a_doubled_backtick_is_read_whole() {
    for (statement, answer) in [
        ("ALTER TABLE orders DROP CHECK `ck amount`", INSTANT),
        (
            "ALTER TABLE orders ALTER CHECK `ck amount` NOT ENFORCED",
            INSTANT,
        ),
        (
            "ALTER TABLE orders ALTER CONSTRAINT `ck amount` ENFORCED",
            INSTANT,
        ),
        ("ALTER TABLE orders DROP CHECK `ck``q`", INSTANT),
        (
            "ALTER TABLE orders ALTER CHECK `ck``q` NOT ENFORCED",
            INSTANT,
        ),
        // `check` is a reserved word as a name; backticked it is a name.
        ("ALTER TABLE orders DROP CHECK `check`", INSTANT),
        ("ALTER TABLE orders ALTER CHECK `check` ENFORCED", COPY),
    ] {
        assert_13b(S13B_NAMED, statement, &[answer]);
    }
    // The name is the whole backticked text: `ck` alone is no CHECK.
    assert_unknown_13b(S13B_NAMED, "ALTER TABLE orders DROP CHECK `ck`", "");
}

#[test]
fn a_check_named_after_a_longer_word_with_check_in_it_is_read_as_that_name() {
    let schema = "CREATE TABLE orders (id INT PRIMARY KEY, amount INT NOT NULL, \
                  CONSTRAINT my_check_1 CHECK (amount > 0), CONSTRAINT checked CHECK (amount > 1));";
    assert_13b(
        schema,
        "ALTER TABLE orders DROP CHECK my_check_1;
         ALTER TABLE orders ALTER CHECK checked NOT ENFORCED;
         ALTER TABLE orders DROP CHECK checked;",
        &[INSTANT, INSTANT, INSTANT],
    );
}

#[test]
fn an_unnamed_check_has_the_name_mysql_gave_it_and_a_renamed_table_the_new_one() {
    // Hand test: the first unnamed CHECK of orders is orders_chk_1. After
    // RENAME TABLE orders TO orders2 it is orders2_chk_1, and orders_chk_1 is
    // error 3821.
    let add = "ALTER TABLE orders ADD CHECK (amount > 10);\n";
    for server in [MYSQL_80, MYSQL_84] {
        let sql = format!("{add}ALTER TABLE orders DROP CHECK orders_chk_1");
        assert_eq!(
            statements_on(server, S13B_SCHEMA, &parse(&sql)),
            [COPY, INSTANT],
            "{server}"
        );
        let sql = format!(
            "{add}RENAME TABLE orders TO orders2;\nALTER TABLE orders2 DROP CHECK orders2_chk_1"
        );
        assert_eq!(
            statements_on(server, S13B_SCHEMA, &parse(&sql))
                .last()
                .map(String::as_str),
            Some(INSTANT),
            "{server}"
        );
        let sql = format!(
            "{add}RENAME TABLE orders TO orders2;\nALTER TABLE orders2 DROP CHECK orders_chk_1"
        );
        assert_unknown_13b(S13B_SCHEMA, &sql, "");
        let sql = format!(
            "{add}RENAME TABLE orders TO orders2;\nALTER TABLE orders2 DROP CHECK orders_chk_1;\n\
             ALTER TABLE orders2 ADD COLUMN coupon INT NULL;"
        );
        assert!(
            mentions_fulltext(predictions_on(server, S13B_SCHEMA, &sql).last().unwrap()),
            "{server}: the table should be lost"
        );
    }
    // A CHECK written in a column definition has the same kind of name.
    assert_eq!(
        on_mysql_84(
            "",
            "CREATE TABLE t (a INT CHECK (a > 0), b INT);\nALTER TABLE t ALTER CHECK t_chk_1 NOT ENFORCED;\n\
             ALTER TABLE t DROP CHECK t_chk_1;"
        )[1..],
        [INSTANT, INSTANT]
    );
}

#[test]
fn a_check_clause_on_a_table_created_earlier_in_the_input_is_a_new_table() {
    // alter table on a table this input creates: nothing waits for it.
    let create = "CREATE TABLE gifts (id INT PRIMARY KEY, amount INT NOT NULL, \
                  CONSTRAINT chk_gift CHECK (amount > 0) NOT ENFORCED);\n";
    for server in [MYSQL_80, MYSQL_84] {
        for clause in [
            "ALTER CHECK chk_gift ENFORCED",
            "ALTER CONSTRAINT chk_gift ENFORCED",
            "ALTER CHECK chk_gift NOT ENFORCED",
            "DROP CHECK chk_gift",
        ] {
            assert_eq!(
                impacts_on(server, "", &format!("{create}ALTER TABLE gifts {clause}")),
                [Impact::NotChecked, Impact::NewTable],
                "{server}: {clause}"
            );
        }
    }
}

#[test]
fn laravel_raw_check_statements_get_the_lab_answers() {
    // DB::statement('alter table `orders` add constraint ... check (...)'),
    // then the same with `alter check` and `drop check`. Laravel has no call
    // for a CHECK constraint; --pretend prints a raw statement as written.
    assert_eq!(
        pretend_on_mysql_84(
            LARAVEL_TABLES,
            &pretend(
                "2026_11_09_101000_manage_amount_check_on_orders_table",
                &[
                    "alter table `orders` add constraint `orders_amount_check` check (`amount` > 0) not enforced",
                    "alter table `orders` alter check `orders_amount_check` enforced",
                    "alter table `orders` alter check `orders_amount_check` not enforced",
                    "alter table `orders` drop check `orders_amount_check`",
                    "alter table `orders` add `coupon` varchar(255) null",
                ]
            )
        ),
        [INSTANT, COPY, INSTANT, INSTANT, INSTANT,]
    );
}

#[test]
fn an_unquoted_reserved_word_as_a_check_name_is_unreadable_but_in_backticks_it_is_answered() {
    // By hand on both servers: DROP CHECK check, ALTER CHECK key ENFORCED and
    // DROP CHECK select are error 1064 even when the table has a CHECK with
    // that name (created in backticks).
    let schema = "\
CREATE TABLE `rw` (
  `id` int NOT NULL,
  `a` int NOT NULL,
  PRIMARY KEY (`id`),
  CONSTRAINT `check` CHECK ((`a` > 0)),
  CONSTRAINT `key` CHECK ((`a` > 1)),
  CONSTRAINT `select` CHECK ((`a` > 2))
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4;
";
    for statement in [
        "ALTER TABLE rw DROP CHECK check",
        "ALTER TABLE rw DROP CHECK key",
        "ALTER TABLE rw DROP CHECK select",
        "ALTER TABLE rw ALTER CHECK key ENFORCED",
        "ALTER TABLE rw ALTER CHECK check NOT ENFORCED",
        "ALTER TABLE rw ALTER CONSTRAINT select ENFORCED",
        "ALTER TABLE rw ALTER CONSTRAINT select NOT ENFORCED",
    ] {
        for server in [MYSQL_80, MYSQL_84] {
            match predictions_on(server, schema, statement).last() {
                Some(Prediction::Unknown { reason })
                    if reason.starts_with("kilit could not read this statement") => {}
                other => panic!("{server}: {statement}\n    got {other:?}"),
            }
        }
    }
    for (statement, case, answer) in [
        (
            "ALTER TABLE rw DROP CHECK `check`",
            "drop-check-constraint",
            INSTANT,
        ),
        (
            "ALTER TABLE rw DROP CHECK `select`",
            "drop-check-constraint",
            INSTANT,
        ),
        (
            "ALTER TABLE rw ALTER CHECK `key` NOT ENFORCED",
            "alter-check-constraint",
            INSTANT,
        ),
        (
            "ALTER TABLE rw ALTER CONSTRAINT `select` ENFORCED",
            "alter-check-constraint",
            INSTANT,
        ),
    ] {
        for server in [MYSQL_80, MYSQL_84] {
            let (found, verdict, _) = measured_13b(server, schema, statement);
            assert_eq!(found, case, "{server}: {statement}");
            assert_eq!(verdict, answer, "{server}: {statement}");
        }
    }
}

// Step 14: table names that differ only in letter case. With
// lower_case_table_names = 0 (the default on Linux) `orders` and `ORDERS` are
// two tables, with 1 (Windows) or 2 (macOS) they are one. MySQL 8.0.46 and
// 8.4.13 were run by hand with 0 and with 1 (hand tests of step 14). The
// statement that names a table in another case is unknown, since kilit does
// not know the setting. What follows depends on what the hand tests found:
//   A) the statement fails with 0 (1146, 1051, 1824) and the migration stops
//      there, so kilit goes on as with 1 or 2: the known table stays known
//      and the statement is applied to it;
//   B) it fails with 1 (1050: CREATE TABLE ORDERS next to orders, RENAME
//      orders TO ORDERS), so kilit goes on as with 0: ORDERS is a table of
//      its own;
//   B') a foreign key with the checks off is accepted by both settings: the
//      child keeps the key;
//   C) everything else leaves different tables under the two settings: the
//      tables the statement may change are no longer trusted, and the new
//      name stays ambiguous next to the table it matches.
// Column, index, foreign key and CHECK names are compared without case on
// every setting, and kilit keeps doing so.

const FULLTEXT_KEY: &str = ",\n  FULLTEXT KEY `ft_note` (`note`)";

/// Laravel's two tables as mysqldump writes them, and a table with a
/// non-ASCII name. `users_key` and `orders_key` end the definitions of
/// `users` and `orders`.
fn case_tables(users_key: &str, orders_key: &str) -> String {
    format!(
        "CREATE TABLE `users` (
  `id` bigint unsigned NOT NULL AUTO_INCREMENT,
  `name` varchar(100) NOT NULL,
  PRIMARY KEY (`id`){users_key}
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_0900_ai_ci;
CREATE TABLE `orders` (
  `id` bigint unsigned NOT NULL AUTO_INCREMENT,
  `user_id` bigint unsigned NOT NULL,
  `amount` int NOT NULL,
  `email` varchar(255) DEFAULT NULL,
  `note` varchar(50) DEFAULT NULL,
  PRIMARY KEY (`id`),
  KEY `ix` (`amount`),
  KEY `fk_x` (`user_id`),
  CONSTRAINT `fk_x` FOREIGN KEY (`user_id`) REFERENCES `users` (`id`),
  CONSTRAINT `ck_a` CHECK ((`amount` >= 0)){orders_key}
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_0900_ai_ci;
CREATE TABLE `çizelge` (
  `id` int NOT NULL,
  `n` int DEFAULT NULL,
  PRIMARY KEY (`id`)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_0900_ai_ci;
"
    )
}

/// `users` and `orders` without a FULLTEXT index.
fn case_plain() -> String {
    case_tables("", "")
}

/// `users` and `orders` with a FULLTEXT index each: a new column on either
/// copies the table while kilit knows it, and is INSTANT where it does not.
fn case_fulltext() -> String {
    case_tables(",\n  FULLTEXT KEY `ft_name` (`name`)", FULLTEXT_KEY)
}

/// What MySQL 8.0.46 and 8.4.13 do, as the reason names it.
const CASE_REFUSE: &str = "refuse the statement when no table has that exact name";
const CASE_SKIP: &str = "skip it with a note when no table has that exact name";
const CASE_NEW_NAME: &str = "take it as a new name";
const CASE_FOREIGN_KEY: &str =
    "refuse the foreign key when foreign_key_checks is on and no table has that exact name";

/// The reason kilit gives for a name in another letter case.
fn case_sentence(known: &str, written: &str, effect: &str) -> String {
    format!(
        "kilit knows this table as {known}. With lower_case_table_names = 0, the default on \
         Linux, {written} is another table, and MySQL 8.0.46 and 8.4.13 {effect}; with 1 or 2, \
         the defaults on Windows and macOS, it is the same table."
    )
}

/// The sentence a reason ends with when kilit goes on as with 1 or 2 (A).
fn case_ending_a(known: &str, written: &str) -> String {
    format!(
        "With 0 the statement fails and the migration stops there, so kilit goes on as with 1 or \
         2, where {written} is {known}."
    )
}

/// The sentence a reason ends with when kilit goes on as with 0 (B).
fn case_ending_b(written: &str) -> String {
    format!(
        "With 1 or 2 the statement fails and the migration stops there, so kilit goes on as with \
         0, where {written} is a table of its own."
    )
}

/// The sentence of a foreign key with the checks off (B').
const CASE_ENDING_CHECKS_OFF: &str =
    "With the checks off both settings add the foreign key, so kilit goes on with it as written.";

/// The sentence of a foreign key when the checks may be off (B').
const CASE_ENDING_CHECKS_MAYBE_OFF: &str = "The checks may be off, and then both settings add the foreign key, so kilit goes on with \
     it as written.";

/// The sentence of a statement that fails under 1 or 2 too (A, a measured
/// failure).
fn case_ending_a_failure(known: &str, written: &str) -> String {
    format!(
        "With 0 the statement fails, and with 1 or 2, where {written} is {known}, the lab's \
         answer for it is a failure too, so the migration stops here and kilit leaves the \
         tables as they are."
    )
}

/// The sentence of every other case (C).
const CASE_ENDING_LOST: &str = "kilit cannot tell which setting the migration goes on with, so it \
     stops trusting the tables the statement may change.";

/// What kilit says about each statement of `sql`: the same on both servers.
fn case_answers(schema: &str, sql: &str) -> Vec<String> {
    let on_80 = on_mysql_80(schema, sql);
    let on_84 = on_mysql_84(schema, sql);
    assert_eq!(on_80, on_84, "{sql}");
    on_84
}

/// What kilit says about the last statement of `sql`: the same on both
/// servers.
fn case_last_answer(schema: &str, sql: &str) -> String {
    case_answers(schema, sql).pop().unwrap()
}

/// The reason for the last statement of `sql`, which has no answer: the same
/// on both servers.
fn case_reason(schema: &str, sql: &str) -> String {
    let reason = unknown_reason_on(MYSQL_84, schema, sql);
    assert_eq!(reason, unknown_reason_on(MYSQL_80, schema, sql), "{sql}");
    reason
}

/// Asserts that the last statement of `sql` is unknown for a name in
/// another letter case: the reason says lower_case_table_names = 0, names
/// the table kilit knows and the name written.
fn assert_case_unknown(schema: &str, sql: &str, known: &str, written: &str) -> String {
    let reason = case_reason(schema, sql);
    assert!(
        reason.contains("lower_case_table_names = 0"),
        "{sql}\n    {reason}"
    );
    assert!(
        reason.contains(&format!("kilit knows this table as {known}.")),
        "{sql}\n    {reason}"
    );
    assert!(
        reason.contains(&format!("{written} is another table")),
        "{sql}\n    {reason}"
    );
    reason
}

#[test]
fn a_table_name_written_as_kilit_knows_it_keeps_its_answers() {
    let plain = case_plain();
    for (sql, answer) in [
        ("ALTER TABLE orders ADD COLUMN x INT NULL", INSTANT),
        ("ALTER TABLE `orders` ADD COLUMN x INT NULL", INSTANT),
        ("ALTER TABLE orders DROP COLUMN user_id", FAILS_1828),
        ("CREATE INDEX i3 ON orders (note)", INPLACE),
        ("DROP INDEX ix ON orders", INPLACE),
        ("ALTER TABLE `çizelge` ADD COLUMN x INT NULL", INSTANT),
        ("ALTER TABLE users ADD COLUMN x INT NULL", INSTANT),
    ] {
        assert_eq!(case_last_answer(&plain, sql), answer, "{sql}");
    }
    assert_eq!(
        case_last_answer(&case_fulltext(), "ALTER TABLE orders ADD COLUMN x INT NULL"),
        COPY
    );
    // Not measured today, and not because of the name.
    assert_eq!(case_last_answer(&plain, "DROP TABLE orders"), "not checked");
    for sql in ["RENAME TABLE orders TO o2", "TRUNCATE TABLE orders"] {
        let reason = case_reason(&plain, sql);
        assert!(reason.contains("has not been measured yet"), "{reason}");
    }
}

#[test]
fn altering_a_table_in_another_letter_case_is_unknown_where_the_exact_name_copies() {
    // The old answer was orders': COPY, writes wait.
    let schema = case_fulltext();
    assert_eq!(
        case_last_answer(&schema, "ALTER TABLE orders ADD COLUMN x INT NULL"),
        COPY
    );
    for written in ["ORDERS", "Orders", "oRDERS", "`ORDERS`", "`Orders`"] {
        let sql = format!("ALTER TABLE {written} ADD COLUMN x INT NULL");
        assert_eq!(case_last_answer(&schema, &sql), UNKNOWN, "{sql}");
        assert_case_unknown(&schema, &sql, "orders", written.trim_matches('`'));
    }
}

#[test]
fn the_reason_for_a_name_in_another_letter_case_says_what_each_setting_does() {
    let reason = case_reason(&case_plain(), "ALTER TABLE ORDERS ADD COLUMN x INT NULL");
    assert_eq!(
        reason,
        format!(
            "{} {}",
            case_sentence("orders", "ORDERS", CASE_REFUSE),
            case_ending_a("orders", "ORDERS")
        )
    );
    assert_eq!(
        reason,
        "kilit knows this table as orders. With lower_case_table_names = 0, the default on \
         Linux, ORDERS is another table, and MySQL 8.0.46 and 8.4.13 refuse the statement \
         when no table has that exact name; with 1 or 2, the defaults on Windows and macOS, \
         it is the same table. With 0 the statement fails and the migration stops there, so \
         kilit goes on as with 1 or 2, where ORDERS is orders."
    );
}

#[test]
fn the_reason_of_a_statement_that_fails_with_1_ends_with_the_sentence_for_going_on_as_with_0() {
    // CREATE TABLE ORDERS next to orders is 1050 with 1 or 2 (hand test 9).
    let reason = case_reason(&case_plain(), "CREATE TABLE ORDERS (id INT PRIMARY KEY)");
    assert_eq!(
        reason,
        "kilit knows this table as orders. With lower_case_table_names = 0, the default on \
         Linux, ORDERS is another table, and MySQL 8.0.46 and 8.4.13 take it as a new name; \
         with 1 or 2, the defaults on Windows and macOS, it is the same table. With 1 or 2 \
         the statement fails and the migration stops there, so kilit goes on as with 0, \
         where ORDERS is a table of its own."
    );
}

#[test]
fn the_reason_of_a_foreign_key_with_the_checks_off_ends_with_the_sentence_for_the_key_as_written() {
    // Hand test 10: both settings accept it with foreign_key_checks = 0.
    let sql = "SET foreign_key_checks = 0;
        ALTER TABLE orders ADD CONSTRAINT fk_u2 FOREIGN KEY (user_id) REFERENCES USERS (id)";
    let reason = case_reason(&case_plain(), sql);
    assert_eq!(
        reason,
        format!(
            "{} {CASE_ENDING_CHECKS_OFF}",
            case_sentence("users", "USERS", CASE_FOREIGN_KEY)
        )
    );
    assert_eq!(
        reason,
        "kilit knows this table as users. With lower_case_table_names = 0, the default on \
         Linux, USERS is another table, and MySQL 8.0.46 and 8.4.13 refuse the foreign key \
         when foreign_key_checks is on and no table has that exact name; with 1 or 2, the \
         defaults on Windows and macOS, it is the same table. With the checks off both \
         settings add the foreign key, so kilit goes on with it as written."
    );
}

#[test]
fn the_reason_of_a_statement_whose_two_settings_part_ways_ends_with_the_sentence_for_not_trusting()
{
    // DROP TABLE IF EXISTS ORDERS: a note with 0, the drop of orders with 1.
    let reason = case_reason(&case_plain(), "DROP TABLE IF EXISTS ORDERS");
    assert_eq!(
        reason,
        "kilit knows this table as orders. With lower_case_table_names = 0, the default on \
         Linux, ORDERS is another table, and MySQL 8.0.46 and 8.4.13 skip it with a note \
         when no table has that exact name; with 1 or 2, the defaults on Windows and macOS, \
         it is the same table. kilit cannot tell which setting the migration goes on with, \
         so it stops trusting the tables the statement may change."
    );
}

#[test]
fn a_name_that_matches_no_table_in_any_case_gets_the_answer_it_got_before() {
    // Missing: a note, never a failure, and the letter case changes nothing.
    let schema = case_plain();
    for table in ["ghosts", "GHOSTS", "Ghosts", "orders_old", "ORDERS_OLD"] {
        let sql = format!("ALTER TABLE {table} ADD COLUMN x INT NULL");
        assert_eq!(case_last_answer(&schema, &sql), INSTANT, "{sql}");
    }
    // A database in front of a name makes another name, as before.
    assert_eq!(
        case_last_answer(
            &schema,
            "ALTER TABLE kilit_lab.orders ADD COLUMN x INT NULL"
        ),
        INSTANT
    );
    // A table that was dropped is Missing, in either case.
    for table in ["orders", "ORDERS"] {
        let sql = format!("DROP TABLE orders; ALTER TABLE {table} ADD COLUMN x INT NULL");
        assert_eq!(case_last_answer(&schema, &sql), INSTANT, "{sql}");
    }
}

#[test]
fn every_place_that_looks_up_a_table_says_what_mysql_does_with_the_name_in_another_case() {
    // (statement, the table kilit knows, the name written, what MySQL does,
    // the world kilit goes on in: A, B or C).
    // MySQL's answers with lower_case_table_names = 0: ALTER TABLE, CREATE
    // INDEX, DROP INDEX, RENAME TABLE, DROP TABLE, TRUNCATE TABLE: 1146 or
    // 1051; DROP TABLE IF EXISTS: a note; CREATE TABLE and RENAME ... TO: a
    // new table; REFERENCES with the checks on: 1824.
    let self_reference = "ALTER TABLE orders ADD COLUMN parent_id BIGINT UNSIGNED NULL;\n";
    let rows: Vec<(String, &str, &str, &str, char)> =
        vec![
        ("ALTER TABLE ORDERS ADD COLUMN x INT NULL".into(), "orders", "ORDERS", CASE_REFUSE, 'A'),
        ("ALTER TABLE `Orders` ADD COLUMN x INT NULL".into(), "orders", "Orders", CASE_REFUSE, 'A'),
        ("CREATE INDEX i3 ON ORDERS (note)".into(), "orders", "ORDERS", CASE_REFUSE, 'A'),
        ("DROP INDEX ix ON ORDERS".into(), "orders", "ORDERS", CASE_REFUSE, 'A'),
        ("RENAME TABLE ORDERS TO o2".into(), "orders", "ORDERS", CASE_REFUSE, 'A'),
        ("RENAME TABLE orders TO USERS".into(), "users", "USERS", CASE_NEW_NAME, 'C'),
        ("ALTER TABLE orders RENAME TO USERS".into(), "users", "USERS", CASE_NEW_NAME, 'C'),
        ("DROP TABLE ORDERS".into(), "orders", "ORDERS", CASE_REFUSE, 'A'),
        ("DROP TABLE users, ORDERS".into(), "orders", "ORDERS", CASE_REFUSE, 'C'),
        ("DROP TABLE IF EXISTS ORDERS".into(), "orders", "ORDERS", CASE_SKIP, 'C'),
        ("TRUNCATE TABLE ORDERS".into(), "orders", "ORDERS", CASE_REFUSE, 'A'),
        ("CREATE TABLE ORDERS (id INT PRIMARY KEY)".into(), "orders", "ORDERS", CASE_NEW_NAME, 'B'),
        (
            "ALTER TABLE orders ADD CONSTRAINT fk_u2 FOREIGN KEY (user_id) REFERENCES USERS (id)"
                .into(),
            "users",
            "USERS",
            CASE_FOREIGN_KEY,
            'A',
        ),
        (
            "CREATE TABLE t9 (id INT PRIMARY KEY, user_id BIGINT UNSIGNED, \
             CONSTRAINT fk9 FOREIGN KEY (user_id) REFERENCES USERS (id))"
                .into(),
            "users",
            "USERS",
            CASE_FOREIGN_KEY,
            'C',
        ),
        (
            format!(
                "{self_reference}ALTER TABLE orders ADD CONSTRAINT fk_p \
                 FOREIGN KEY (parent_id) REFERENCES ORDERS (id)"
            ),
            "orders",
            "ORDERS",
            CASE_FOREIGN_KEY,
            'C',
        ),
        (
            "CREATE TABLE t8 (id INT PRIMARY KEY, parent_id INT, \
             CONSTRAINT fk8 FOREIGN KEY (parent_id) REFERENCES T8 (id))"
                .into(),
            "t8",
            "T8",
            CASE_FOREIGN_KEY,
            'C',
        ),
    ];
    let schema = case_plain();
    for (sql, known, written, effect, world) in rows {
        let ending = match world {
            'A' => case_ending_a(known, written),
            'B' => case_ending_b(written),
            _ => CASE_ENDING_LOST.to_owned(),
        };
        assert_eq!(case_last_answer(&schema, &sql), UNKNOWN, "{sql}");
        assert_eq!(
            case_reason(&schema, &sql),
            format!("{} {ending}", case_sentence(known, written, effect)),
            "{sql}"
        );
    }
}

#[test]
fn truncating_without_the_word_table_in_another_letter_case_is_unknown_too() {
    // DB::table('orders')->truncate() prints "truncate table `orders`"; the
    // short form is MySQL's too.
    let schema = case_plain();
    for sql in ["TRUNCATE ORDERS", "truncate `Orders`"] {
        assert_eq!(case_last_answer(&schema, sql), UNKNOWN, "{sql}");
        assert!(
            case_reason(&schema, sql).contains("lower_case_table_names = 0"),
            "{sql}"
        );
    }
}

#[test]
fn a_foreign_key_to_a_parent_in_another_case_is_unknown_with_the_checks_on_or_off() {
    // Hand test: with the checks on and lower_case_table_names = 0 the
    // server refuses with 1824; with them off it accepts and the key points
    // at USERS, a table that does not exist.
    let schema = case_plain();
    let add = |parent: &str| {
        format!(
            "ALTER TABLE orders ADD CONSTRAINT fk_u2 FOREIGN KEY (user_id) REFERENCES {parent} (id)"
        )
    };
    // Exact name: today's answers.
    assert_eq!(
        case_last_answer(
            &schema,
            &format!("SET foreign_key_checks = 1;\n{}", add("users"))
        ),
        COPY
    );
    assert_eq!(
        case_last_answer(
            &schema,
            &format!("SET foreign_key_checks = 0;\n{}", add("`users`"))
        ),
        INPLACE
    );
    // Another case: unknown, whatever the checks say.
    for set in [
        "",
        "SET foreign_key_checks = 1;\n",
        "SET foreign_key_checks = 0;\n",
    ] {
        for parent in ["USERS", "Users", "`USERS`"] {
            let sql = format!("{set}{}", add(parent));
            assert_eq!(case_last_answer(&schema, &sql), UNKNOWN, "{sql}");
            assert_case_unknown(&schema, &sql, "users", parent.trim_matches('`'));
        }
    }
    // A parent that matches no table in any case: as before.
    assert_eq!(
        case_last_answer(
            &schema,
            &format!("SET foreign_key_checks = 0;\n{}", add("ghosts"))
        ),
        INPLACE
    );
}

#[test]
fn a_column_in_another_case_is_still_the_same_column_of_an_exactly_named_parent() {
    // Column names are compared without case on every setting.
    let schema = case_plain();
    let sql = "SET foreign_key_checks = 1;
        ALTER TABLE orders ADD CONSTRAINT fk_u2 FOREIGN KEY (USER_ID) REFERENCES users (ID)";
    assert_eq!(case_last_answer(&schema, sql), COPY);
}

#[test]
fn a_create_table_with_a_parent_in_another_case_is_unknown_and_with_the_exact_name_is_not_checked()
{
    let schema = case_plain();
    let create = "CREATE TABLE t9 (id INT PRIMARY KEY, user_id BIGINT UNSIGNED, \
                  CONSTRAINT fk9 FOREIGN KEY (user_id) REFERENCES USERS (id));\n";
    assert_eq!(case_last_answer(&schema, create), UNKNOWN);
    // The same statement with the exact name: a new table, nothing checked.
    let exact = create.replace("USERS", "users");
    assert_eq!(case_last_answer(&schema, &exact), "not checked");
}

#[test]
fn a_database_in_front_of_a_table_name_in_another_case_is_unknown() {
    // Hand test: KILIT_HAND_14.orders is 1049 and kilit_hand_14.ORDERS is
    // 1146 with lower_case_table_names = 0, both accepted with 1.
    let create = "CREATE TABLE shop.orders (id INT PRIMARY KEY, a INT);\n";
    let schema = "";
    assert_eq!(
        case_last_answer(
            schema,
            &format!("{create}ALTER TABLE shop.orders ADD COLUMN x INT NULL")
        ),
        INSTANT
    );
    assert_eq!(
        case_last_answer(
            schema,
            &format!("{create}ALTER TABLE `shop`.`orders` ADD COLUMN x INT NULL")
        ),
        INSTANT
    );
    for written in [
        "Shop.orders",
        "SHOP.ORDERS",
        "shop.Orders",
        "`SHOP`.`orders`",
    ] {
        let sql = format!("{create}ALTER TABLE {written} ADD COLUMN x INT NULL");
        assert_eq!(case_last_answer(schema, &sql), UNKNOWN, "{sql}");
        let reason = case_reason(schema, &sql);
        assert!(reason.contains("lower_case_table_names = 0"), "{reason}");
        assert!(reason.contains("shop.orders"), "{reason}");
    }
}

#[test]
fn a_table_name_with_non_ascii_letters_in_another_case_is_unknown() {
    // Hand test: ÇİZELGE, ÇIZELGE and Çizelge are all 1146 with
    // lower_case_table_names = 0 and all reach çizelge with 1.
    let schema = case_plain();
    assert_eq!(
        case_last_answer(&schema, "ALTER TABLE çizelge ADD COLUMN x INT NULL"),
        INSTANT
    );
    for written in ["ÇİZELGE", "ÇIZELGE", "Çizelge", "çİzelge"] {
        let sql = format!("ALTER TABLE {written} ADD COLUMN x INT NULL");
        assert_eq!(case_last_answer(&schema, &sql), UNKNOWN, "{sql}");
        assert_case_unknown(&schema, &sql, "çizelge", written);
    }
    // Another name altogether: as before.
    assert_eq!(
        case_last_answer(&schema, "ALTER TABLE ÇİZELGE2 ADD COLUMN x INT NULL"),
        INSTANT
    );
}

#[test]
fn a_temporary_table_name_in_another_case_is_unknown() {
    // Hand test: ALTER TABLE TMP1 on a temporary tmp1 is 1146 with 0.
    let create = "CREATE TEMPORARY TABLE tmp1 (id INT);\n";
    assert_eq!(
        case_last_answer(
            "",
            &format!("{create}ALTER TABLE tmp1 ADD COLUMN a INT NULL")
        ),
        INSTANT
    );
    let sql = format!("{create}ALTER TABLE TMP1 ADD COLUMN a INT NULL");
    assert_eq!(case_last_answer("", &sql), UNKNOWN);
    assert_case_unknown("", &sql, "tmp1", "TMP1");
}

#[test]
fn two_names_in_one_statement_that_differ_only_in_case_are_unknown() {
    // RENAME TABLE a TO b, B TO c: with 0 the second old name is another
    // table; with 1 it is the table the first pair just renamed.
    let create = "CREATE TABLE a1 (id INT PRIMARY KEY);\n";
    let sql = format!("{create}RENAME TABLE a1 TO b1, B1 TO c1");
    assert_eq!(case_last_answer("", &sql), UNKNOWN);
    assert_case_unknown("", &sql, "b1", "B1");
    // Without the second pair it is the unmeasured rename of before.
    let reason = case_reason("", &format!("{create}RENAME TABLE a1 TO b1, b1 TO c1"));
    assert!(reason.contains("has not been measured yet"), "{reason}");
    // A key that points at a table of the same statement in another case.
    let sql = "CREATE TABLE t (id INT PRIMARY KEY, parent_id INT, \
               CONSTRAINT f FOREIGN KEY (parent_id) REFERENCES T (id))";
    assert_eq!(case_last_answer("", sql), UNKNOWN);
    assert_case_unknown("", sql, "t", "T");
}

/// "COPY" when `table` is still known after `statements` (a FULLTEXT index
/// makes a new column copy it), "INSTANT" when kilit stopped trusting it
/// (the answer of any table kilit cannot see, with the FULLTEXT index as a
/// note), on the tables of `case_fulltext`.
fn case_trust_after(statements: &str, table: &str) -> String {
    case_last_answer(
        &case_fulltext(),
        &format!("{statements};\nALTER TABLE {table} ADD COLUMN x INT NULL"),
    )
}

#[test]
fn what_kilit_trusts_after_a_statement_in_another_letter_case_follows_the_hand_tests() {
    // After each statement: `ALTER TABLE <table> ADD COLUMN x INT NULL` on the
    // FULLTEXT tables. COPY: the table is known (its FULLTEXT index is
    // seen). INSTANT: the table is gone or no longer trusted, the answer of a
    // table kilit cannot see. UNKNOWN: the name is ambiguous next to its twin.
    // A) the statement fails with 0 and the migration stops: kilit goes on
    // with the table known (hand tests 1-8, 10, 11, 13, 14).
    // B) it fails with 1: ORDERS / USERS is the table, renamed, or a table of
    // its own. C) the tables it may change are lost.
    let rows: &[(&str, &[(&str, &str)])] = &[
        // A: applied to the known table, which stays known.
        (
            "ALTER TABLE ORDERS ADD COLUMN y INT NULL",
            &[("orders", COPY), ("users", COPY)],
        ),
        (
            "CREATE INDEX i3 ON ORDERS (note)",
            &[("orders", COPY), ("users", COPY)],
        ),
        (
            "DROP INDEX ix ON ORDERS",
            &[("orders", COPY), ("users", COPY)],
        ),
        (
            "TRUNCATE TABLE ORDERS",
            &[("orders", COPY), ("users", COPY)],
        ),
        (
            "ALTER TABLE orders ADD CONSTRAINT fk_u2 FOREIGN KEY (user_id) REFERENCES USERS (id)",
            &[("orders", COPY), ("users", COPY)],
        ),
        // A: the table is gone (Missing), or has another name and is known.
        ("DROP TABLE ORDERS", &[("orders", INSTANT), ("users", COPY)]),
        (
            "RENAME TABLE ORDERS TO o2",
            &[("orders", INSTANT), ("o2", COPY), ("users", COPY)],
        ),
        // B: the table is renamed to its own name in another case. The old
        // spelling is Missing, but it matches the known new one without case:
        // unknown (R2).
        (
            "RENAME TABLE users TO USERS",
            &[("users", UNKNOWN), ("USERS", COPY), ("orders", COPY)],
        ),
        (
            "RENAME TABLE orders TO ORDERS",
            &[("orders", UNKNOWN), ("ORDERS", COPY), ("users", COPY)],
        ),
        // C: the tables the statement may change are lost.
        (
            "DROP TABLE IF EXISTS ORDERS",
            &[("orders", INSTANT), ("users", COPY)],
        ),
        (
            "DROP TABLE ORDERS, users",
            &[("orders", INSTANT), ("users", INSTANT)],
        ),
        (
            "DROP TABLE users, ORDERS",
            &[("orders", INSTANT), ("users", INSTANT)],
        ),
        (
            "CREATE TABLE t9 (id INT PRIMARY KEY, user_id BIGINT UNSIGNED, \
             CONSTRAINT fk9 FOREIGN KEY (user_id) REFERENCES USERS (id))",
            &[("orders", COPY), ("users", COPY)],
        ),
        (
            "ALTER TABLE users RENAME TO USERS",
            &[("users", UNKNOWN), ("USERS", UNKNOWN), ("orders", COPY)],
        ),
    ];
    for (statement, tables) in rows {
        for (table, expected) in *tables {
            assert_eq!(
                case_trust_after(statement, table),
                *expected,
                "{statement}\n    then ALTER TABLE {table}"
            );
        }
    }
}

#[test]
fn a_statement_that_fails_with_0_is_applied_to_the_known_table_and_a_measured_failure_changes_nothing()
 {
    // A) The statement goes on as with 1 or 2, where ORDERS is orders.
    let plain = case_plain();
    // ADD FULLTEXT makes the later new column copy the table; DROP TABLE IF
    // EXISTS (C) loses the table instead, and the new column is INSTANT.
    let fulltext = "ALTER TABLE ORDERS ADD FULLTEXT ft_email (email);\n";
    assert_eq!(
        case_last_answer(
            &plain,
            &format!("{fulltext}ALTER TABLE orders ADD COLUMN x INT NULL")
        ),
        COPY
    );
    assert_eq!(
        case_last_answer(
            &plain,
            "DROP TABLE IF EXISTS ORDERS;\nALTER TABLE orders ADD COLUMN x INT NULL"
        ),
        INSTANT
    );
    // The foreign key is dropped by the statement: its column can go.
    assert_eq!(
        case_last_answer(
            &plain,
            "ALTER TABLE ORDERS DROP FOREIGN KEY fk_x;\nALTER TABLE orders DROP COLUMN user_id"
        ),
        "INPLACE, rebuilds the table, writes continue"
    );
    // A statement that fails under 1 or 2 changes nothing (1828), so the
    // column is still in the foreign key.
    assert_eq!(
        case_last_answer(
            &plain,
            "ALTER TABLE ORDERS DROP COLUMN user_id;\nALTER TABLE orders DROP COLUMN user_id"
        ),
        FAILS_1828
    );
    // DROP TABLE: orders is gone (Missing), and a table made later under the
    // name is new.
    assert_eq!(
        case_impacts(
            &plain,
            "DROP TABLE ORDERS; CREATE TABLE orders (id INT PRIMARY KEY); \
             ALTER TABLE orders ADD COLUMN x INT NULL;"
        ),
        [Impact::Unknown, Impact::NotChecked, Impact::NewTable]
    );
    // RENAME: o2 is orders, with its foreign key; orders is gone.
    assert_eq!(
        case_last_answer(
            &plain,
            "RENAME TABLE ORDERS TO o2;\nALTER TABLE o2 DROP COLUMN user_id"
        ),
        FAILS_1828
    );
    assert_eq!(
        case_last_answer(
            &plain,
            "RENAME TABLE ORDERS TO o2;\nALTER TABLE orders DROP COLUMN user_id"
        ),
        INSTANT
    );
}

#[test]
fn the_foreign_key_of_a_parent_in_another_case_is_added_when_the_checks_are_on_or_off() {
    // A (checks on or never set) and B' (checks off): the child gets the key
    // to users, so its column cannot go (1828). A column that does not fit
    // the parent is a measured failure with the checks on (3780): no key.
    let plain = case_plain();
    let column = "ALTER TABLE orders ADD COLUMN ref BIGINT UNSIGNED NULL;\n";
    let key = "ALTER TABLE orders ADD CONSTRAINT fk_ref FOREIGN KEY (ref) REFERENCES USERS (id);\n";
    let drop = "ALTER TABLE orders DROP COLUMN ref";
    for set in [
        "",
        "SET foreign_key_checks = 1;\n",
        "SET foreign_key_checks = 0;\n",
    ] {
        let sql = format!("{set}{column}{key}SET foreign_key_checks = 1;\n{drop}");
        assert_eq!(case_last_answer(&plain, &sql), FAILS_1828, "{sql}");
    }
    let mismatch = "ALTER TABLE orders ADD COLUMN ref INT NULL;\n";
    let sql = format!("{mismatch}{key}{drop}");
    assert_eq!(
        case_last_answer(&plain, &sql),
        INSTANT,
        "3780 under 1 or 2: no key"
    );
    // With the checks off the key is added, then the column of the key
    // cannot go.
    let sql = "SET foreign_key_checks = 0;
        ALTER TABLE orders ADD CONSTRAINT fk_u2 FOREIGN KEY (user_id) REFERENCES USERS (id);
        SET foreign_key_checks = 1;
        ALTER TABLE orders DROP COLUMN user_id;";
    let answers = case_answers(&plain, sql);
    assert_eq!(answers[3], FAILS_1828);
    assert_eq!(answers[1], UNKNOWN);
}

#[test]
fn creating_an_index_in_another_case_keeps_the_table_known_so_the_foreign_key_index_cannot_go() {
    // CREATE INDEX i9 ON ORDERS applies to orders (1146 with 0, so kilit goes
    // on as with 1 or 2), and dropping the only index of the foreign key
    // fails with 1553.
    let sql = "CREATE INDEX i9 ON ORDERS (email);\nALTER TABLE orders DROP INDEX fk_x;";
    let answers = case_answers(&case_plain(), sql);
    assert_eq!(answers, [UNKNOWN, FAILS_1553]);
}

#[test]
fn a_statement_in_another_letter_case_leaves_the_other_tables_alone() {
    // The table of another name is known as before.
    for statement in [
        "ALTER TABLE ORDERS ADD COLUMN y INT NULL",
        "DROP TABLE ORDERS",
        "TRUNCATE TABLE ORDERS",
    ] {
        let sql = format!("{statement};\nALTER TABLE `çizelge` ADD COLUMN x INT NULL");
        assert_eq!(case_last_answer(&case_fulltext(), &sql), INSTANT, "{sql}");
    }
    // A foreign key column of orders is still known to fail.
    let sql = "TRUNCATE TABLE ORDERS;\nALTER TABLE orders DROP COLUMN user_id";
    assert_eq!(case_last_answer(&case_plain(), sql), FAILS_1828);
    // And after a statement that fails with 0 and applies to orders with 1 or
    // 2, the table is still known: the foreign key still holds the column.
    let sql = "ALTER TABLE ORDERS ADD COLUMN y INT NULL;\nALTER TABLE orders DROP COLUMN user_id";
    assert_eq!(case_last_answer(&case_plain(), sql), FAILS_1828);
    // Where the two settings part ways the table is lost: no failure from it.
    let sql = "DROP TABLE IF EXISTS ORDERS;\nALTER TABLE orders DROP COLUMN user_id";
    assert_eq!(case_last_answer(&case_plain(), sql), INSTANT);
}

#[test]
fn a_new_name_in_another_letter_case_is_its_own_table_or_stays_ambiguous_next_to_the_table_it_matches()
 {
    let schema = case_fulltext();
    // B) CREATE TABLE ORDERS next to orders fails with 1 (1050), so kilit goes
    // on as with 0: ORDERS is a table of its own, with its own answers (no
    // FULLTEXT index), and orders is unchanged.
    let create = "CREATE TABLE ORDERS (id INT PRIMARY KEY);\n";
    assert_eq!(case_last_answer(&schema, create.trim_end()), UNKNOWN);
    assert_eq!(
        case_last_answer(
            &schema,
            &format!("{create}ALTER TABLE ORDERS ADD COLUMN x INT NULL")
        ),
        INSTANT
    );
    assert_eq!(
        case_last_answer(
            &schema,
            &format!("{create}ALTER TABLE orders ADD COLUMN x INT NULL")
        ),
        COPY
    );
    // C) the same with IF NOT EXISTS, TEMPORARY, a foreign key, or a rename
    // of another table to the name: both settings go on, the new name is lost
    // and ambiguous next to orders; orders itself stays known.
    for statement in [
        "CREATE TABLE IF NOT EXISTS ORDERS (id INT PRIMARY KEY)",
        "CREATE TABLE ORDERS (id INT PRIMARY KEY, user_id BIGINT UNSIGNED, \
         CONSTRAINT f FOREIGN KEY (user_id) REFERENCES users (id))",
        "RENAME TABLE users TO ORDERS",
        "ALTER TABLE users RENAME TO ORDERS",
    ] {
        assert_eq!(
            case_trust_after(statement, "orders"),
            COPY,
            "{statement}: orders"
        );
        let sql = format!("{statement};\nALTER TABLE ORDERS ADD COLUMN x INT NULL");
        assert_eq!(case_last_answer(&schema, &sql), UNKNOWN, "{sql}");
        assert_case_unknown(&schema, &sql, "orders", "ORDERS");
    }
    // B) the table renamed to its own name in another case: ORDERS is the
    // table (its FULLTEXT index is seen); orders is gone, and the old
    // spelling matches ORDERS without case, so a statement on it is unknown.
    let rename = "RENAME TABLE orders TO ORDERS;\n";
    assert_eq!(
        case_last_answer(
            &schema,
            &format!("{rename}ALTER TABLE ORDERS ADD COLUMN x INT NULL")
        ),
        COPY
    );
    let sql = format!("{rename}ALTER TABLE orders ADD COLUMN x INT NULL");
    assert_eq!(case_last_answer(&schema, &sql), UNKNOWN);
    assert_case_unknown(&schema, &sql, "ORDERS", "orders");
    // C) ALTER ... RENAME TO is accepted by both settings, with different
    // tables: both spellings are ambiguous.
    for (table, other) in [("orders", "ORDERS"), ("ORDERS", "orders")] {
        let sql = format!(
            "ALTER TABLE orders RENAME TO ORDERS;\nALTER TABLE {table} ADD COLUMN x INT NULL"
        );
        assert_eq!(case_last_answer(&schema, &sql), UNKNOWN, "{sql}");
        assert_case_unknown(&schema, &sql, other, table);
    }
    // Lost without a twin: the lost-table answer.
    assert_eq!(
        case_trust_after("DROP TABLE IF EXISTS ORDERS", "orders"),
        INSTANT
    );
}

#[test]
fn a_create_table_if_not_exists_in_another_letter_case_makes_a_later_alter_of_it_unknown() {
    // Laravel's Schema::createIfNotExists('ORDERS') against a dump that spells
    // the table orders: the old answer for ALTER TABLE ORDERS was orders'
    // COPY (FULLTEXT index).
    let schema = case_fulltext();
    let create = "CREATE TABLE IF NOT EXISTS ORDERS (id INT PRIMARY KEY);\n";
    assert_eq!(
        case_last_answer(&schema, create.trim_end_matches('\n')),
        UNKNOWN
    );
    let sql = format!("{create}ALTER TABLE ORDERS ADD COLUMN x INT NULL");
    assert_eq!(case_last_answer(&schema, &sql), UNKNOWN);
    assert_case_unknown(&schema, &sql, "orders", "ORDERS");
    let sql = format!("{create}ALTER TABLE orders ADD COLUMN x INT NULL");
    assert_eq!(case_last_answer(&schema, &sql), COPY);
}

#[test]
fn laravel_dropping_and_creating_a_table_in_another_letter_case_is_unknown_all_the_way_but_not_for_a_table_made_here()
 {
    // Schema::dropIfExists('ORDERS'); Schema::create('ORDERS', ...); then
    // $table->fullText('note') against a dump that spells the table orders.
    // The DROP IF EXISTS loses orders (C), so the CREATE is no longer certain
    // to fail with 1 and the ALTER stays ambiguous: all three are unknown.
    let schema = case_plain();
    let sql = "DROP TABLE IF EXISTS ORDERS;
        CREATE TABLE ORDERS (id INT PRIMARY KEY, note TEXT);
        ALTER TABLE ORDERS ADD FULLTEXT (note);";
    assert_eq!(case_answers(&schema, sql), [UNKNOWN, UNKNOWN, UNKNOWN]);
    assert_eq!(
        case_impacts(&schema, sql),
        [Impact::Unknown, Impact::Unknown, Impact::Unknown]
    );
    // With no schema the first spelling is a table made here and known (B):
    // the second CREATE fails with 1, so Posts is a table of its own, and the
    // third statement gets the measured answer of ADD FULLTEXT (lab: INPLACE,
    // rebuilds the table, writes wait) on a new table.
    let sql = "CREATE TABLE posts (id INT PRIMARY KEY, body TEXT);
        CREATE TABLE Posts (id INT PRIMARY KEY, body TEXT);
        ALTER TABLE Posts ADD FULLTEXT (body);";
    assert_eq!(
        case_answers("", sql),
        [
            "not checked",
            UNKNOWN,
            "INPLACE, rebuilds the table, writes wait"
        ]
    );
    assert_eq!(
        case_impacts("", sql),
        [Impact::NotChecked, Impact::Unknown, Impact::NewTable]
    );
}

#[test]
fn a_temporary_table_in_another_letter_case_hides_the_known_table_only_inside_the_input_it_is_in() {
    // CREATE TEMPORARY TABLE Orders: within the file ALTER TABLE orders is
    // ambiguous (with 1 the temporary Orders hides orders and the statement
    // hits it, with 0 it hits the base table): unknown, the case reason with
    // the temporary table as the one kilit knows. The next file starts a new
    // session and the temporary table is gone, so orders is known again (COPY,
    // FULLTEXT; the cli test).
    let schema = case_fulltext();
    let create = "CREATE TEMPORARY TABLE Orders (id INT);\n";
    let sql = format!("{create}ALTER TABLE orders ADD COLUMN z INT NULL");
    assert_eq!(case_last_answer(&schema, &sql), UNKNOWN);
    assert_case_unknown(&schema, &sql, "Orders", "orders");
    // Without the temporary table: known.
    assert_eq!(
        case_last_answer(&schema, "ALTER TABLE orders ADD COLUMN z INT NULL"),
        COPY
    );
}

#[test]
fn a_create_table_like_in_another_letter_case_keeps_its_reason_and_the_known_table() {
    // CREATE TABLE ORDERS LIKE users: kilit cannot read LIKE. It keeps that
    // reason, loses ORDERS (ambiguous next to orders) and leaves orders known.
    let schema = case_fulltext();
    let create = "CREATE TABLE ORDERS LIKE users";
    assert_eq!(case_last_answer(&schema, create), UNKNOWN);
    let reason = case_reason(&schema, create);
    assert!(!reason.contains("lower_case_table_names"), "{reason}");
    let sql = format!("{create};\nALTER TABLE orders ADD COLUMN x INT NULL");
    assert_eq!(case_last_answer(&schema, &sql), COPY);
    // ORDERS is lost, and ambiguous next to orders like the new name of a
    // readable CREATE TABLE.
    let sql = format!("{create};\nALTER TABLE ORDERS ADD COLUMN x INT NULL");
    assert_eq!(case_last_answer(&schema, &sql), UNKNOWN);
    assert_case_unknown(&schema, &sql, "orders", "ORDERS");
}

#[test]
fn a_statement_kilit_cannot_read_keeps_its_own_reason_and_loses_the_table_in_another_case() {
    let schema = case_fulltext();
    let sql = "ALTER TABLE ORDERS FROBNICATE x";
    assert_eq!(case_last_answer(&schema, sql), UNKNOWN);
    let reason = case_reason(&schema, sql);
    assert!(
        reason.starts_with("kilit could not read this statement"),
        "{reason}"
    );
    assert!(!reason.contains("lower_case_table_names"), "{reason}");
    assert_eq!(case_trust_after(sql, "orders"), INSTANT);
    assert_eq!(case_trust_after(sql, "users"), COPY);
    // The same statement on the exact name loses the table the same way.
    assert_eq!(
        case_trust_after("ALTER TABLE orders FROBNICATE x", "orders"),
        INSTANT
    );
}

#[test]
fn a_table_kilit_lost_still_matches_a_name_in_another_case() {
    // Lost, not Missing: the name in another case is unknown with the same
    // reason, where a Missing table gets the answer of a table kilit cannot
    // see.
    let schema = case_fulltext();
    let lose = "ALTER TABLE orders FROBNICATE x;\n";
    let sql = format!("{lose}ALTER TABLE ORDERS ADD COLUMN y INT NULL");
    assert_eq!(case_last_answer(&schema, &sql), UNKNOWN);
    assert_case_unknown(&schema, &sql, "orders", "ORDERS");
    // The exact name of a lost table: no case reason.
    assert_eq!(
        case_last_answer(
            &schema,
            &format!("{lose}ALTER TABLE orders ADD COLUMN y INT NULL")
        ),
        INSTANT
    );
}

#[test]
fn a_table_created_earlier_in_the_input_matches_a_name_in_another_case() {
    let create = "CREATE TABLE t1 (id INT PRIMARY KEY, a INT);\n";
    assert_eq!(
        case_last_answer("", &format!("{create}ALTER TABLE t1 ADD COLUMN b INT NULL")),
        INSTANT
    );
    let sql = format!("{create}ALTER TABLE T1 ADD COLUMN b INT NULL");
    assert_eq!(case_last_answer("", &sql), UNKNOWN);
    assert_case_unknown("", &sql, "t1", "T1");
    // The drop of a table created here, in another case, and after it t1.
    let sql = format!("{create}DROP TABLE T1");
    assert_case_unknown("", &sql, "t1", "T1");
}

#[test]
fn column_index_foreign_key_and_check_names_in_another_case_keep_their_answers() {
    // Hand test: ADD INDEX (AMOUNT), DROP INDEX IX, DROP FOREIGN KEY FK_X and
    // DROP CHECK CK_A all work on the lower-case names, with 0 and with 1.
    let schema = case_plain();
    for (sql, answer) in [
        ("ALTER TABLE orders ADD INDEX i3 (AMOUNT)", INPLACE),
        ("DROP INDEX IX ON orders", INPLACE),
        ("ALTER TABLE orders DROP FOREIGN KEY FK_X", INPLACE),
        ("ALTER TABLE orders DROP CHECK CK_A", INSTANT),
        ("ALTER TABLE orders ALTER CHECK CK_A NOT ENFORCED", INSTANT),
        ("ALTER TABLE orders DROP COLUMN USER_ID", FAILS_1828),
        (
            "ALTER TABLE orders MODIFY COLUMN NOTE VARCHAR(60) NULL",
            INPLACE,
        ),
    ] {
        assert_eq!(case_last_answer(&schema, sql), answer, "{sql}");
    }
    // The foreign key was found, so its column can go once it is dropped.
    assert_eq!(
        case_last_answer(
            &schema,
            "ALTER TABLE orders DROP FOREIGN KEY FK_X;\nALTER TABLE orders DROP COLUMN user_id"
        ),
        "INPLACE, rebuilds the table, writes continue"
    );
}

/// The impact of every statement of `sql` on MySQL 8.4.13, with the tables
/// of `schema`.
fn case_impacts(schema: &str, sql: &str) -> Vec<Impact> {
    let statements = parse(sql);
    let results = Knowledge::built_in().server(MYSQL_84).unwrap();
    let findings = check(&statements, &Schema::from_sql(schema), results);
    impacts(&statements, &findings)
}

#[test]
fn a_statement_is_not_on_a_new_table_because_a_name_matches_in_another_case_only() {
    // Exact name: a table created here, as before.
    assert_eq!(
        case_impacts(
            "",
            "CREATE TABLE t1 (id INT PRIMARY KEY); ALTER TABLE t1 ADD COLUMN a INT NULL;"
        ),
        [Impact::NotChecked, Impact::NewTable]
    );
    // Another case: kilit cannot tell, so it is not a new table.
    assert_eq!(
        case_impacts(
            "",
            "CREATE TABLE t1 (id INT PRIMARY KEY); ALTER TABLE T1 ADD COLUMN a INT NULL;"
        ),
        [Impact::NotChecked, Impact::Unknown]
    );
    // A table of the schema, and a name in another case: unknown, not a
    // failure and not a measured answer.
    let schema = case_fulltext();
    assert_eq!(
        case_impacts(&schema, "ALTER TABLE ORDERS ADD COLUMN a INT NULL;"),
        [Impact::Unknown]
    );
    assert_eq!(
        case_impacts(&schema, "ALTER TABLE orders ADD COLUMN a INT NULL;"),
        [Impact::WritesWait]
    );
}

#[test]
fn a_create_table_in_another_letter_case_never_makes_the_known_table_new() {
    // With orders in the schema: CREATE TABLE ORDERS fails with 1 (B), so
    // kilit goes on as with 0 and ORDERS is a new table of its own. The
    // statement itself is unknown, and the ALTER after it is on a new table.
    let schema = case_fulltext();
    let create = "CREATE TABLE ORDERS (id INT PRIMARY KEY, note TEXT);\n";
    assert_eq!(
        case_impacts(
            &schema,
            &format!("{create}ALTER TABLE ORDERS ADD FULLTEXT (note);")
        ),
        [Impact::Unknown, Impact::NewTable]
    );
    // The other spelling keeps the table's own answer.
    assert_eq!(
        case_impacts(
            &schema,
            &format!("{create}ALTER TABLE orders ADD COLUMN a INT NULL;")
        )[1],
        Impact::WritesWait
    );
    // C: IF NOT EXISTS leaves the new name ambiguous, never new.
    assert_eq!(
        case_impacts(
            &schema,
            "CREATE TABLE IF NOT EXISTS ORDERS (id INT PRIMARY KEY);\n\
             ALTER TABLE ORDERS ADD COLUMN a INT NULL;"
        ),
        [Impact::Unknown, Impact::Unknown]
    );
    // Without an orders in the schema, ORDERS is just a new table.
    assert_eq!(
        case_impacts(
            "",
            &format!("{create}ALTER TABLE ORDERS ADD COLUMN a INT NULL;")
        ),
        [Impact::NotChecked, Impact::NewTable]
    );
}

#[test]
fn a_table_made_in_the_input_stays_new_after_a_statement_in_another_case_that_fails_with_0() {
    // A) CREATE TABLE t1; ALTER TABLE T1: t1 is the table (kilit goes on as
    // with 1 or 2), so the next ALTER of t1 is on a new table; the case-only
    // statement itself never is.
    assert_eq!(
        case_impacts(
            "",
            "CREATE TABLE t1 (id INT PRIMARY KEY); ALTER TABLE T1 ADD COLUMN a INT NULL; \
             ALTER TABLE t1 ADD COLUMN b INT NULL;"
        ),
        [Impact::NotChecked, Impact::Unknown, Impact::NewTable]
    );
    // The same for a temporary table (hand test 14) and a database in front.
    assert_eq!(
        case_impacts(
            "",
            "CREATE TEMPORARY TABLE tmp1 (id INT); ALTER TABLE TMP1 ADD COLUMN a INT NULL; \
             ALTER TABLE tmp1 ADD COLUMN b INT NULL;"
        ),
        [Impact::NotChecked, Impact::Unknown, Impact::NewTable]
    );
    assert_eq!(
        case_impacts(
            "",
            "CREATE TABLE shop.orders (id INT PRIMARY KEY); ALTER TABLE Shop.orders ADD COLUMN a INT NULL; \
             ALTER TABLE shop.orders ADD COLUMN b INT NULL;"
        ),
        [Impact::NotChecked, Impact::Unknown, Impact::NewTable]
    );
}

#[test]
fn a_rename_to_a_name_in_another_letter_case_does_not_make_the_known_table_new() {
    let schema = case_fulltext();
    let impacts = case_impacts(
        &schema,
        "RENAME TABLE users TO ORDERS;\nALTER TABLE ORDERS ADD COLUMN a INT NULL;",
    );
    assert_eq!(impacts[0], Impact::Unknown);
    assert_ne!(impacts[1], Impact::NewTable);
}

#[test]
fn a_self_reference_written_as_the_table_is_known_keeps_its_answer() {
    // $table->foreignId('parent_id')->nullable()->constrained('orders'): the
    // matching key to the primary key, as before; the name in another case
    // is unknown (every_place_that_looks_up_a_table_says_...).
    let sql = "ALTER TABLE orders ADD COLUMN parent_id BIGINT UNSIGNED NULL;
        ALTER TABLE orders ADD CONSTRAINT fk_p FOREIGN KEY (parent_id) REFERENCES orders (id)";
    assert_eq!(case_last_answer(&case_plain(), sql), COPY);
    let sql = sql.replace("REFERENCES orders", "REFERENCES `orders`");
    assert_eq!(case_last_answer(&case_plain(), &sql), COPY);
}

/// The reason of statement number `at` (from 0) of `sql`, which has no answer.
fn case_reason_at(schema: &str, sql: &str, at: usize) -> String {
    match &predictions_on(MYSQL_84, schema, sql)[at] {
        Prediction::Unknown { reason } => reason.clone(),
        other => panic!("{sql}\n    statement {at}: {other:?}"),
    }
}

/// The notes of the last statement of `sql`, a measured one.
fn case_last_notes(schema: &str, sql: &str) -> Vec<String> {
    notes_of(predictions_on(MYSQL_84, schema, sql).last().unwrap()).to_vec()
}

/// `orders` and `archive`, each with a TEXT column `note`; `orders_key` ends
/// the definition of `orders`.
fn case_notes_tables(orders_key: &str) -> String {
    format!(
        "CREATE TABLE `orders` (
  `id` bigint unsigned NOT NULL AUTO_INCREMENT,
  `note` text,
  PRIMARY KEY (`id`){orders_key}
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_0900_ai_ci;
CREATE TABLE `archive` (
  `id` bigint unsigned NOT NULL AUTO_INCREMENT,
  `note` text,
  PRIMARY KEY (`id`)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_0900_ai_ci;
"
    )
}

#[test]
fn a_statement_kilit_could_not_read_earlier_makes_a_case_only_statement_unclear() {
    // RENAME TABLES (plural) is unreadable: with 0 it runs and the FULLTEXT
    // index goes on ORDERS, which orders does not have; with 1 the rename
    // fails with 1050. kilit cannot tell, so the table is lost.
    let schema = case_notes_tables("");
    let sql = "RENAME TABLES archive TO ORDERS;
        ALTER TABLE ORDERS ADD FULLTEXT ft (note);
        ALTER TABLE orders ADD COLUMN x INT NULL;";
    assert_eq!(case_answers(&schema, sql), [UNKNOWN, UNKNOWN, INSTANT]);
    let reason = case_reason_at(&schema, sql, 1);
    assert_eq!(
        reason,
        format!(
            "{} {CASE_ENDING_LOST}",
            case_sentence("orders", "ORDERS", CASE_REFUSE)
        )
    );
    // The lost table keeps the FULLTEXT index as a note.
    assert!(
        case_last_notes(&schema, sql)
            .iter()
            .any(|note| note.contains("FULLTEXT")),
        "{sql}"
    );
    // Without the unreadable statement the same ALTER is A: applied to
    // orders, which then has the FULLTEXT index and copies on a new column.
    let sql = "ALTER TABLE ORDERS ADD FULLTEXT ft (note);
        ALTER TABLE orders ADD COLUMN x INT NULL;";
    assert_eq!(case_answers(&schema, sql), [UNKNOWN, COPY]);
}

#[test]
fn a_table_of_another_database_with_the_written_name_makes_a_case_only_statement_unclear() {
    // shop.ORDERS may be what ORDERS means to the server (a default database
    // of shop): C, and the lost orders keeps its FULLTEXT index as a note.
    let schema = case_notes_tables(",\n  FULLTEXT KEY `ft` (`note`)");
    let sql = "CREATE TABLE shop.ORDERS (id INT PRIMARY KEY, note TEXT, FULLTEXT KEY ft (note));
        ALTER TABLE ORDERS DROP INDEX ft;
        ALTER TABLE orders ADD COLUMN x INT NULL;";
    assert_eq!(
        case_answers(&schema, sql),
        ["not checked", UNKNOWN, INSTANT]
    );
    assert!(
        case_reason_at(&schema, sql, 1).ends_with(CASE_ENDING_LOST),
        "{sql}"
    );
    assert!(
        case_last_notes(&schema, sql)
            .iter()
            .any(|note| note.contains("FULLTEXT")),
        "{sql}"
    );
    // Without that table the DROP INDEX is A: applied to orders, which no
    // longer has the index, so no FULLTEXT note.
    let sql = "ALTER TABLE ORDERS DROP INDEX ft;
        ALTER TABLE orders ADD COLUMN x INT NULL;";
    assert_eq!(case_answers(&schema, sql), [UNKNOWN, INSTANT]);
    assert!(
        case_last_notes(&schema, sql)
            .iter()
            .all(|note| !note.contains("FULLTEXT")),
        "{sql}"
    );
}

#[test]
fn a_create_table_next_to_a_temporary_table_of_the_same_name_is_unclear() {
    // MySQL lets a base table share the name of a temporary table, so the
    // CREATE need not fail with 1: C, and the next statement on ORDERS is
    // ambiguous too.
    let sql = "CREATE TEMPORARY TABLE orders (id INT PRIMARY KEY, note TEXT);
        CREATE TABLE ORDERS (id INT PRIMARY KEY, note VARCHAR(50));
        ALTER TABLE ORDERS ADD INDEX ix (note);";
    assert_eq!(case_answers("", sql), ["not checked", UNKNOWN, UNKNOWN]);
    assert!(case_reason_at("", sql, 1).ends_with(CASE_ENDING_LOST));
    assert_case_unknown("", sql, "orders", "ORDERS");
}

#[test]
fn renaming_a_table_in_another_case_to_a_name_some_table_has_is_unclear() {
    // RENAME TABLE ORDERS TO users, or TO orders: the new name is taken in
    // some case, and the two settings part ways. orders is lost after it.
    let schema = case_fulltext();
    for statement in [
        "RENAME TABLE ORDERS TO users",
        "RENAME TABLE ORDERS TO orders",
    ] {
        assert_eq!(case_last_answer(&schema, statement), UNKNOWN, "{statement}");
        assert!(
            case_reason(&schema, statement).ends_with(CASE_ENDING_LOST),
            "{statement}"
        );
        assert_eq!(
            case_trust_after(statement, "orders"),
            INSTANT,
            "{statement}"
        );
    }
}

#[test]
fn a_case_only_statement_that_would_fail_under_1_or_2_too_changes_nothing() {
    // ALTER TABLE ORDERS DROP COLUMN user_id: 1146 with 0; with 1 or 2 it is
    // orders and the column is in a foreign key (1828, the lab's answer). The
    // migration stops, the tables stay as they are.
    let schema = case_plain();
    let sql = "ALTER TABLE ORDERS DROP COLUMN user_id;
        ALTER TABLE orders DROP COLUMN user_id;";
    assert_eq!(case_answers(&schema, sql), [UNKNOWN, FAILS_1828]);
    assert_eq!(
        case_reason_at(&schema, sql, 0),
        format!(
            "{} {}",
            case_sentence("orders", "ORDERS", CASE_REFUSE),
            case_ending_a_failure("orders", "ORDERS")
        )
    );
    assert_eq!(
        case_reason_at(&schema, sql, 0),
        "kilit knows this table as orders. With lower_case_table_names = 0, the default on \
         Linux, ORDERS is another table, and MySQL 8.0.46 and 8.4.13 refuse the statement \
         when no table has that exact name; with 1 or 2, the defaults on Windows and macOS, \
         it is the same table. With 0 the statement fails, and with 1 or 2, where ORDERS is \
         orders, the lab's answer for it is a failure too, so the migration stops here and \
         kilit leaves the tables as they are."
    );
}

#[test]
fn a_foreign_key_to_a_parent_in_another_case_says_whether_the_checks_are_known_to_be_off() {
    let add = "ALTER TABLE orders ADD CONSTRAINT fk_u2 FOREIGN KEY (user_id) REFERENCES USERS (id)";
    let schema = case_plain();
    let known = case_sentence("users", "USERS", CASE_FOREIGN_KEY);
    // Off: both settings add the key.
    for set in [
        "SET foreign_key_checks = 0;\n",
        "SET foreign_key_checks = OFF;\n",
    ] {
        assert_eq!(
            case_reason(&schema, &format!("{set}{add}")),
            format!("{known} {CASE_ENDING_CHECKS_OFF}"),
            "{set}"
        );
    }
    // Not known to be on: the checks may be off.
    assert_eq!(
        case_reason(
            &schema,
            &format!("SET foreign_key_checks = DEFAULT;\n{add}")
        ),
        format!("{known} {CASE_ENDING_CHECKS_MAYBE_OFF}")
    );
    // On, or never set: the statement fails with 0 (1824), so A.
    for set in ["", "SET foreign_key_checks = 1;\n"] {
        assert_eq!(
            case_reason(&schema, &format!("{set}{add}")),
            format!("{known} {}", case_ending_a("users", "USERS")),
            "{set}"
        );
    }
}

#[test]
fn a_table_with_non_ascii_letters_goes_on_as_with_1_only_where_the_lab_ran_the_letters() {
    // The hand tests ran ÇİZELGE, ÇIZELGE and Çizelge against çizelge (A);
    // Ö was not run, so ÖDEME against ödeme is unclear (C).
    let schema = case_plain();
    let sql = "ALTER TABLE ÇİZELGE ADD COLUMN a INT NULL;
        ALTER TABLE çizelge MODIFY COLUMN a BIGINT NULL;";
    // The column a is in çizelge afterwards: the change of its type copies the
    // table (a column kilit does not know is unknown).
    assert_eq!(case_answers(&schema, sql), [UNKNOWN, COPY]);
    assert_eq!(
        case_reason_at(&schema, sql, 0),
        format!(
            "{} {}",
            case_sentence("çizelge", "ÇİZELGE", CASE_REFUSE),
            case_ending_a("çizelge", "ÇİZELGE")
        )
    );
    let sql = "CREATE TABLE ödeme (id INT PRIMARY KEY);
        ALTER TABLE ÖDEME ADD COLUMN a INT NULL;";
    assert_eq!(case_answers("", sql), ["not checked", UNKNOWN]);
    assert_eq!(
        case_reason_at("", sql, 1),
        format!(
            "{} {CASE_ENDING_LOST}",
            case_sentence("ödeme", "ÖDEME", CASE_REFUSE)
        )
    );
}

#[test]
fn a_rename_of_a_table_in_another_case_makes_the_new_name_the_table_made_here() {
    // A: RENAME TABLE T1 TO t2 fails with 0 (1146); kilit goes on as with 1
    // or 2, where T1 is t1: t2 is the new t1, so the ALTER of t2 is on a new
    // table.
    let sql = "CREATE TABLE t1 (id INT PRIMARY KEY, note TEXT);
        RENAME TABLE T1 TO t2;
        ALTER TABLE t2 ADD FULLTEXT (note);";
    assert_eq!(
        case_answers("", sql),
        [
            "not checked",
            UNKNOWN,
            "INPLACE, rebuilds the table, writes wait"
        ]
    );
    assert_eq!(
        case_impacts("", sql),
        [Impact::NotChecked, Impact::Unknown, Impact::NewTable]
    );
    assert_eq!(
        case_reason_at("", sql, 1),
        format!(
            "{} {}",
            case_sentence("t1", "T1", CASE_REFUSE),
            case_ending_a("t1", "T1")
        )
    );
}

// ---------------------------------------------------------------------------
// Step B1b: literal defaults on TEXT, BLOB, JSON and GEOMETRY columns (error
// 1101), dropping the primary key column, answers without a known table,
// the row size limit (error 1118), and dropping what a table does not have
// (error 1091).
//
// The expectations come from the lab (the scenarios named in each test), from
// the hand tests (manual/B1b) and from the decisions of the step. A test
// that names a lab scenario needs that scenario in matrix/.

const FAILS_1101: &str = "fails with error 1101";
const FAILS_1118: &str = "fails with error 1118";
const INSTANT_PREFIX: &str = "INSTANT";
const COPY_PREFIX: &str = "COPY, copies every row";
const INPLACE_REBUILD_PREFIX: &str = "INPLACE, rebuilds the table";

/// Everything kilit says about one statement: what the lab measured for it,
/// or why it cannot tell.
#[derive(Debug, PartialEq)]
struct Said {
    case: Option<&'static str>,
    answer: String,
    evidence: Vec<&'static str>,
    notes: Vec<String>,
    reason: String,
}

impl Said {
    /// Whether some note starts with `prefix`.
    fn has_note_starting(&self, prefix: &str) -> bool {
        self.notes.iter().any(|note| note.starts_with(prefix))
    }

    /// Whether some note holds every one of `parts`.
    fn has_note_with(&self, parts: &[&str]) -> bool {
        self.notes
            .iter()
            .any(|note| parts.iter().all(|part| note.contains(part)))
    }
}

/// What `server` says about the last statement of `sql`.
fn said_on(server: &str, schema: &str, sql: &str) -> Said {
    match predictions_on(server, schema, sql).pop().unwrap() {
        Prediction::Measured {
            case,
            verdict,
            evidence,
            notes,
        } => Said {
            case: Some(case),
            answer: match verdict {
                Verdict::Fails { code, .. } => format!("fails with error {code}"),
                other => other.to_string(),
            },
            evidence,
            notes,
            reason: String::new(),
        },
        Prediction::Unknown { reason } => Said {
            case: None,
            answer: "unknown".into(),
            evidence: Vec::new(),
            notes: Vec::new(),
            reason,
        },
        Prediction::NotChecked { reason } => Said {
            case: None,
            answer: "not checked".into(),
            evidence: Vec::new(),
            notes: Vec::new(),
            reason,
        },
    }
}

/// What kilit says about the last statement of `sql`: the same on MySQL
/// 8.0.46 and 8.4.13, or the test fails.
fn said(schema: &str, sql: &str) -> Said {
    let on_80 = said_on(MYSQL_80, schema, sql);
    let on_84 = said_on(MYSQL_84, schema, sql);
    assert_eq!(on_80, on_84, "{sql}");
    on_84
}

/// The tables of the lab, then `ALTER TABLE orders AUTO_INCREMENT = 1000`,
/// which kilit cannot follow: it stops trusting `orders`.
fn lab_with_lost_orders() -> String {
    format!("{LAB_TABLES}\nALTER TABLE orders AUTO_INCREMENT = 1000;\n")
}

/// The ways kilit can be without a table it is asked about: no table at all
/// (no --schema) and a table it stopped trusting.
fn tables_kilit_does_not_know() -> [(&'static str, String); 2] {
    [
        ("no table", String::new()),
        ("a lost table", lab_with_lost_orders()),
    ]
}

/// The lab's tables with the lab's `orders` and 62 more VARCHAR(255) NULL
/// columns, in utf8mb4: 64,607 bytes of columns and 8 of NULL flags (64,615)
/// of the 65,535 a row may have.
fn lab_with_62_wide_columns(more: &[&str]) -> String {
    let columns: Vec<String> = (1..=62)
        .map(|n| format!("ADD COLUMN s{n} VARCHAR(255) NULL"))
        .collect();
    let mut setup = vec![format!("ALTER TABLE orders {}", columns.join(", "))];
    setup.extend(more.iter().map(|statement| statement.to_string()));
    let setup: Vec<&str> = setup.iter().map(String::as_str).collect();
    lab_after(&setup)
}

/// Runs `statement` once for each of the lab's six variants (as written,
/// INSTANT, INPLACE, INPLACE with LOCK=NONE, COPY, LOCK=NONE), after
/// `session`, and checks that each answer starts as the lab measured it.
fn assert_six_variants(schema: &str, session: &str, statement: &str, expected: [&str; 6]) {
    for (variant, expected) in Variant::ALL.into_iter().zip(expected) {
        let sql = format!("{session}{statement}{}", variant.clause());
        let answer = said(schema, &sql).answer;
        assert!(
            answer.starts_with(expected),
            "{sql}\n    expected an answer starting {expected:?}, got {answer:?}"
        );
    }
}

/// What the lab measured for the shapes that follow: every variant fails
/// with 1101.
const ALL_1101: [&str; 6] = [FAILS_1101; 6];
/// A default written as an expression: COPY, and a forced INSTANT, INPLACE or
/// LOCK=NONE is refused with 1845 (add-column-text-default-expression and its
/// twins).
const EXPRESSION_DEFAULT: [&str; 6] = [
    COPY_PREFIX,
    FAILS_1845,
    FAILS_1845,
    FAILS_1845,
    COPY_PREFIX,
    FAILS_1845,
];

/// The shapes the lab measured with a literal default on a column that
/// cannot have one, in MySQL's strict mode: the column definition and the
/// scenario that measured it.
const QUOTED_DEFAULT_SHAPES: [(&str, &str); 5] = [
    (
        "c TEXT NOT NULL DEFAULT ''",
        "add-column-text-default-literal",
    ),
    (
        "c TEXT NULL DEFAULT 'x'",
        "add-column-text-nullable-default-literal",
    ),
    (
        "c JSON NULL DEFAULT '[]'",
        "add-column-json-default-literal",
    ),
    ("c BLOB DEFAULT ''", "add-column-blob-default-literal"),
    (
        "c GEOMETRY NULL DEFAULT ''",
        "add-column-geometry-default-literal",
    ),
];

#[test]
fn a_new_text_json_blob_or_geometry_column_with_a_quoted_default_fails_with_1101() {
    // $table->text('c')->default(''); $table->json('c')->nullable()->default('[]');
    // The answer comes from the statement, so the table does not matter.
    for (label, schema) in [
        ("the lab's tables", LAB_TABLES.to_owned()),
        ("no table", String::new()),
        ("a lost table", lab_with_lost_orders()),
    ] {
        for (column, scenario) in QUOTED_DEFAULT_SHAPES {
            let sql = format!("ALTER TABLE orders ADD COLUMN {column}");
            let answer = said(&schema, &sql);
            assert_eq!(answer.answer, FAILS_1101, "{label}: {sql}");
            assert_eq!(
                answer.case,
                Some("add-column-blob-literal-default"),
                "{label}: {sql}"
            );
            assert!(
                answer.evidence.contains(&scenario),
                "{label}: {sql}: {scenario} is missing from {:?}",
                answer.evidence
            );
        }
    }
}

#[test]
fn a_quoted_default_on_a_text_json_blob_or_geometry_column_fails_with_1101_in_every_variant() {
    // The lab: 1101 as written, with ALGORITHM=INSTANT, INPLACE, INPLACE with
    // LOCK=NONE, COPY and LOCK=NONE, on both servers.
    for (column, _) in QUOTED_DEFAULT_SHAPES {
        assert_six_variants(
            LAB_TABLES,
            "",
            &format!("ALTER TABLE orders ADD COLUMN {column}"),
            ALL_1101,
        );
    }
}

#[test]
fn a_quoted_default_on_text_or_blob_carries_a_note_that_kilit_assumes_strict_sql_mode() {
    // With no sql_mode set in the input the answer rests on the server's
    // default, which is strict.
    for column in [
        "c TEXT NOT NULL DEFAULT ''",
        "c BLOB DEFAULT ''",
        "c LONGTEXT NOT NULL DEFAULT ''",
    ] {
        let answer = said(
            LAB_TABLES,
            &format!("ALTER TABLE orders ADD COLUMN {column}"),
        );
        assert_eq!(answer.answer, FAILS_1101, "{column}");
        assert!(
            answer.has_note_with(&["assumes strict SQL mode, the MySQL default"]),
            "{column}: {:?}",
            answer.notes
        );
    }
}

#[test]
fn a_new_text_column_with_default_null_or_none_keeps_the_add_column_answer() {
    // $table->text('c')->nullable(); and ->default(null).
    for column in [
        "c TEXT NULL",
        "c TEXT NULL DEFAULT NULL",
        "c JSON NULL DEFAULT NULL",
    ] {
        let answer = said(
            LAB_TABLES,
            &format!("ALTER TABLE orders ADD COLUMN {column}"),
        );
        assert_eq!(answer.case, Some("add-column"), "{column}");
        assert_eq!(answer.answer, INSTANT, "{column}");
    }
}

#[test]
fn a_quoted_default_on_another_type_keeps_its_answer() {
    // Only TEXT, BLOB, JSON and GEOMETRY refuse a literal default.
    for column in [
        "c VARCHAR(50) NOT NULL DEFAULT ''",
        "c INT NOT NULL DEFAULT 0",
        "c CHAR(2) NOT NULL DEFAULT 'x'",
        "c ENUM('a','b') NOT NULL DEFAULT 'a'",
    ] {
        let answer = said(
            LAB_TABLES,
            &format!("ALTER TABLE orders ADD COLUMN {column}"),
        );
        assert_eq!(answer.case, Some("add-column"), "{column}");
        assert_eq!(answer.answer, INSTANT, "{column}");
    }
}

#[test]
fn with_an_empty_sql_mode_a_text_or_blob_column_with_an_empty_default_is_instant() {
    // SET sql_mode = ''; or a list that is not strict: add-column-text-default-
    // literal-sql-mode-empty, ...-not-strict and the BLOB twins ran INSTANT on
    // both servers (INPLACE with a rebuild when forced, COPY when forced).
    for set in [
        "SET sql_mode = ''",
        "SET sql_mode = 'NO_ENGINE_SUBSTITUTION'",
        "SET SESSION sql_mode = ''",
    ] {
        for (column, scenario) in [
            (
                "c TEXT NOT NULL DEFAULT ''",
                "add-column-text-default-literal",
            ),
            ("c BLOB DEFAULT ''", "add-column-blob-default-literal"),
        ] {
            let sql = format!("{set};\nALTER TABLE orders ADD COLUMN {column}");
            let answer = said(LAB_TABLES, &sql);
            assert_eq!(answer.answer, INSTANT, "{sql}");
            assert_eq!(
                answer.case,
                Some("add-column-blob-empty-default-not-strict"),
                "{sql}"
            );
            assert!(
                answer
                    .evidence
                    .iter()
                    .any(|id| id.starts_with(scenario) && id.contains("sql-mode")),
                "{sql}: no sql-mode twin of {scenario} in {:?}",
                answer.evidence
            );
        }
    }
}

#[test]
fn with_an_empty_sql_mode_the_variants_of_an_empty_text_default_are_the_lab_s() {
    // ok ok rebuild rebuild copy ok: INSTANT as written, INSTANT, INPLACE with
    // a rebuild, INPLACE with a rebuild, COPY, INSTANT.
    assert_six_variants(
        LAB_TABLES,
        "SET sql_mode = '';\n",
        "ALTER TABLE orders ADD COLUMN c TEXT NOT NULL DEFAULT ''",
        [
            INSTANT_PREFIX,
            INSTANT_PREFIX,
            INPLACE_REBUILD_PREFIX,
            INPLACE_REBUILD_PREFIX,
            COPY_PREFIX,
            INSTANT_PREFIX,
        ],
    );
}

#[test]
fn laravel_s_blob_and_text_columns_with_an_empty_default_are_instant_with_an_empty_sql_mode() {
    // $table->binary('e')->default(''); $table->text('a')->default('');
    // Laravel 12 and 13 print `blob not null default ''`.
    for column in [
        "`a` text not null default ''",
        "`e` blob not null default ''",
    ] {
        let answer = said(
            LAB_TABLES,
            &format!("SET sql_mode = '';\nalter table `orders` add {column}"),
        );
        assert_eq!(answer.answer, INSTANT, "{column}");
    }
}

#[test]
fn with_an_empty_sql_mode_a_nonempty_text_default_a_json_default_and_a_geometry_default_still_fail()
{
    // add-column-text-nullable-default-literal-sql-mode-empty, ...-not-strict,
    // add-column-json-default-literal-sql-mode-*, add-column-geometry-default-
    // literal-sql-mode-*: 1101 in every sql_mode.
    for set in [
        "SET sql_mode = ''",
        "SET sql_mode = 'NO_ENGINE_SUBSTITUTION'",
    ] {
        for (column, scenario) in [
            (
                "c TEXT NULL DEFAULT 'x'",
                "add-column-text-nullable-default-literal",
            ),
            (
                "c JSON NULL DEFAULT '[]'",
                "add-column-json-default-literal",
            ),
            (
                "c GEOMETRY NULL DEFAULT ''",
                "add-column-geometry-default-literal",
            ),
        ] {
            let sql = format!("{set};\nALTER TABLE orders ADD COLUMN {column}");
            let answer = said(LAB_TABLES, &sql);
            assert_eq!(answer.answer, FAILS_1101, "{sql}");
            assert!(
                answer.evidence.iter().any(|id| id.starts_with(scenario)),
                "{sql}: {:?}",
                answer.evidence
            );
        }
    }
}

#[test]
fn an_sql_mode_set_after_the_statement_does_not_change_its_answer() {
    let answers = on_mysql_84(
        LAB_TABLES,
        "ALTER TABLE orders ADD COLUMN a TEXT NOT NULL DEFAULT '';
         SET sql_mode = '';
         ALTER TABLE orders ADD COLUMN b TEXT NOT NULL DEFAULT '';",
    );
    assert_eq!(answers, [FAILS_1101, "not checked", INSTANT]);
}

#[test]
fn an_sql_mode_kilit_has_not_measured_makes_a_text_default_unknown() {
    // CONCAT(@@sql_mode, ...) is not a list kilit can read, GLOBAL does not
    // set the session's mode, and 'ANSI' is a list the lab did not run.
    for set in [
        "SET sql_mode = CONCAT(@@sql_mode, ',X')",
        "SET GLOBAL sql_mode = ''",
        "SET sql_mode = 'ANSI'",
        "SET SESSION sql_mode = 'ANSI'",
    ] {
        let sql = format!("{set};\nALTER TABLE orders ADD COLUMN c TEXT NOT NULL DEFAULT ''");
        let answer = said(LAB_TABLES, &sql);
        assert_eq!(answer.answer, "unknown", "{sql}");
        assert!(!answer.reason.is_empty(), "{sql}");
    }
}

#[test]
fn a_text_or_json_default_written_as_an_expression_copies_the_table() {
    // $table->text('c')->default(new Expression("('')")): add-column-text-
    // default-expression and add-column-json-default-expression, COPY on both
    // servers, in every sql_mode the lab ran.
    for set in [
        "",
        "SET sql_mode = '';\n",
        "SET sql_mode = 'NO_ENGINE_SUBSTITUTION';\n",
    ] {
        for column in ["c TEXT NOT NULL DEFAULT ('')", "c JSON NULL DEFAULT ('[]')"] {
            let sql = format!("{set}ALTER TABLE orders ADD COLUMN {column}");
            let answer = said(LAB_TABLES, &sql);
            assert_eq!(
                answer.case,
                Some("add-column-blob-expression-default"),
                "{sql}"
            );
            assert!(
                answer.answer.starts_with(COPY_PREFIX),
                "{sql}: {}",
                answer.answer
            );
            assert_eq!(
                said(
                    LAB_TABLES,
                    &format!("{set}ALTER TABLE orders ADD COLUMN {column}, ALGORITHM=INSTANT")
                )
                .answer,
                FAILS_1845,
                "{sql}"
            );
        }
    }
}

#[test]
fn the_variants_of_an_expression_default_on_text_are_the_lab_s() {
    // copy 1845 1845 1845 copy 1845
    assert_six_variants(
        LAB_TABLES,
        "",
        "ALTER TABLE orders ADD COLUMN c TEXT NOT NULL DEFAULT ('')",
        EXPRESSION_DEFAULT,
    );
}

#[test]
fn an_expression_default_on_a_text_column_waits_for_writes_without_a_table_too() {
    for (label, schema) in tables_kilit_does_not_know() {
        let answer = said(
            &schema,
            "ALTER TABLE orders ADD COLUMN c TEXT NOT NULL DEFAULT ('')",
        );
        assert_eq!(
            answer.case,
            Some("add-column-blob-expression-default"),
            "{label}"
        );
        assert_eq!(answer.answer, COPY, "{label}");
    }
}

#[test]
fn an_empty_expression_default_on_a_varchar_column_is_unknown() {
    // Only TEXT, BLOB and JSON were measured with an expression default of
    // this kind.
    let answer = said(
        LAB_TABLES,
        "ALTER TABLE orders ADD COLUMN c VARCHAR(10) NOT NULL DEFAULT ('')",
    );
    assert_eq!(answer.answer, "unknown");
}

#[test]
fn changing_a_column_to_text_with_a_quoted_default_fails_with_1101() {
    // $table->text('note')->default('')->change(); Laravel prints
    // `modify note text not null default ''`. modify-column-text-default-
    // literal (from VARCHAR) and modify-text-column-default-literal (from
    // TEXT): 1101 on both servers, in every variant.
    for (label, schema) in [
        ("the lab's tables", LAB_TABLES.to_owned()),
        ("no table", String::new()),
        ("a lost table", lab_with_lost_orders()),
    ] {
        for sql in [
            "ALTER TABLE orders MODIFY note TEXT NULL DEFAULT ''",
            "alter table `orders` modify `note` text not null default ''",
            "ALTER TABLE orders CHANGE note note TEXT NULL DEFAULT ''",
            "ALTER TABLE orders MODIFY note TEXT NULL DEFAULT 'x'",
        ] {
            let answer = said(&schema, sql);
            assert_eq!(answer.answer, FAILS_1101, "{label}: {sql}");
            assert_eq!(
                answer.case,
                Some("modify-column-blob-literal-default"),
                "{label}: {sql}"
            );
        }
    }
}

#[test]
fn the_evidence_of_a_text_default_on_a_modify_is_the_scenario_of_its_shape() {
    let from_varchar = said(
        LAB_TABLES,
        "ALTER TABLE orders MODIFY note TEXT NULL DEFAULT ''",
    );
    assert!(
        from_varchar
            .evidence
            .contains(&"modify-column-text-default-literal"),
        "{:?}",
        from_varchar.evidence
    );
    let schema = lab_after(&["ALTER TABLE orders ADD COLUMN c TEXT NULL"]);
    let from_text = said(&schema, "ALTER TABLE orders MODIFY c TEXT NULL DEFAULT ''");
    assert_eq!(from_text.answer, FAILS_1101);
    assert!(
        from_text
            .evidence
            .contains(&"modify-text-column-default-literal"),
        "{:?}",
        from_text.evidence
    );
}

#[test]
fn a_modify_to_text_with_a_quoted_default_fails_in_every_variant() {
    assert_six_variants(
        LAB_TABLES,
        "",
        "ALTER TABLE orders MODIFY note TEXT NULL DEFAULT ''",
        ALL_1101,
    );
}

#[test]
fn with_an_empty_sql_mode_a_modify_to_text_with_a_quoted_default_is_unknown() {
    // The lab ran it: COPY from VARCHAR, INSTANT from TEXT, with the mode
    // empty or not strict. The two shapes differ, so kilit does not answer.
    for set in [
        "SET sql_mode = ''",
        "SET sql_mode = 'NO_ENGINE_SUBSTITUTION'",
    ] {
        let sql = format!("{set};\nALTER TABLE orders MODIFY note TEXT NULL DEFAULT ''");
        assert_eq!(said(LAB_TABLES, &sql).answer, "unknown", "{sql}");
    }
}

#[test]
fn a_modify_to_text_without_a_default_keeps_its_answer() {
    // modify-column answers as before: nothing quoted, nothing refused.
    let answer = said(LAB_TABLES, "ALTER TABLE orders MODIFY note TEXT NULL");
    assert_ne!(answer.answer, FAILS_1101);
    assert_ne!(answer.case, Some("modify-column-blob-literal-default"));
}

/// The lab's tables with a TEXT column `c`, a JSON column `j` and a VARCHAR
/// column `v` added to `orders`.
fn lab_with_text_json_and_varchar() -> String {
    lab_after(&[
        "ALTER TABLE orders ADD COLUMN c TEXT NULL, ADD COLUMN j JSON NULL, ADD COLUMN v VARCHAR(20) NULL",
    ])
}

#[test]
fn setting_a_quoted_default_on_a_known_text_or_json_column_fails_with_1101() {
    // $table->text('c')->default('')->change() prints MODIFY; this is the
    // statement form ALTER COLUMN ... SET DEFAULT. set-column-default-text-
    // literal and set-column-default-json-literal: 1101 on both servers.
    let schema = lab_with_text_json_and_varchar();
    for (sql, scenario) in [
        (
            "ALTER TABLE orders ALTER COLUMN c SET DEFAULT ''",
            "set-column-default-text-literal",
        ),
        (
            "ALTER TABLE orders ALTER COLUMN j SET DEFAULT '[]'",
            "set-column-default-json-literal",
        ),
    ] {
        let answer = said(&schema, sql);
        assert_eq!(answer.answer, FAILS_1101, "{sql}");
        assert_eq!(answer.case, Some("set-column-default"), "{sql}");
        assert!(
            answer.evidence.contains(&scenario),
            "{sql}: {:?}",
            answer.evidence
        );
    }
}

#[test]
fn setting_a_quoted_default_on_a_known_text_column_fails_after_an_empty_sql_mode_too() {
    // set-column-default-text-literal-sql-mode-empty and ...-not-strict: 1101.
    let schema = lab_with_text_json_and_varchar();
    for set in [
        "SET sql_mode = ''",
        "SET sql_mode = 'NO_ENGINE_SUBSTITUTION'",
    ] {
        let sql = format!("{set};\nALTER TABLE orders ALTER COLUMN c SET DEFAULT ''");
        assert_eq!(said(&schema, &sql).answer, FAILS_1101, "{sql}");
    }
}

#[test]
fn setting_a_quoted_default_on_a_known_json_column_after_an_empty_sql_mode_is_unknown() {
    // The lab ran set-column-default-json-literal-sql-mode-empty and
    // ...-not-strict (1101 on both servers); kilit stays cautious and does not
    // bind them: unknown (decision of the step).
    let schema = lab_with_text_json_and_varchar();
    for set in [
        "SET sql_mode = ''",
        "SET sql_mode = 'NO_ENGINE_SUBSTITUTION'",
    ] {
        let sql = format!("{set};\nALTER TABLE orders ALTER COLUMN j SET DEFAULT '[]'");
        assert_eq!(said(&schema, &sql).answer, "unknown", "{sql}");
    }
}

#[test]
fn setting_a_quoted_default_on_a_known_varchar_column_keeps_its_answer() {
    let schema = lab_with_text_json_and_varchar();
    for sql in [
        "ALTER TABLE orders ALTER COLUMN v SET DEFAULT ''",
        "ALTER TABLE orders ALTER COLUMN note SET DEFAULT 'x'",
    ] {
        let answer = said(&schema, sql);
        assert_eq!(answer.answer, INSTANT, "{sql}");
        assert_eq!(answer.case, Some("set-column-default"), "{sql}");
    }
}

#[test]
fn setting_a_quoted_default_on_a_table_kilit_does_not_know_is_instant_with_a_note_about_1101() {
    for (label, schema) in tables_kilit_does_not_know() {
        let answer = said(&schema, "ALTER TABLE orders ALTER COLUMN c SET DEFAULT ''");
        assert_eq!(answer.case, Some("set-column-default"), "{label}");
        assert_eq!(answer.answer, INSTANT, "{label}");
        assert!(
            answer.has_note_starting("if the column is TEXT, BLOB or JSON: fails with error 1101"),
            "{label}: {:?}",
            answer.notes
        );
    }
}

#[test]
fn dropping_the_only_primary_key_column_of_a_known_table_copies_it() {
    // drop-primary-key-column: COPY as written (1000 rows), 1846 with
    // ALGORITHM=INSTANT, INPLACE, INPLACE with LOCK=NONE, and with LOCK=NONE;
    // COPY when forced.
    let answer = said(LAB_TABLES, "ALTER TABLE orders DROP COLUMN id");
    assert_eq!(answer.case, Some("drop-column"));
    assert_eq!(answer.answer, COPY);
    assert!(
        answer.evidence.contains(&"drop-primary-key-column"),
        "{:?}",
        answer.evidence
    );
}

#[test]
fn the_variants_of_dropping_the_primary_key_column_are_the_lab_s() {
    assert_six_variants(
        LAB_TABLES,
        "",
        "ALTER TABLE orders DROP COLUMN id",
        [
            COPY_PREFIX,
            FAILS_1846,
            FAILS_1846,
            FAILS_1846,
            COPY_PREFIX,
            FAILS_1846,
        ],
    );
}

#[test]
fn laravel_dropping_the_primary_key_column_is_the_same_statement() {
    // $table->dropColumn('id');
    let answer = said(LAB_TABLES, "alter table `orders` drop `id`");
    assert_eq!(answer.answer, COPY);
    assert!(answer.evidence.contains(&"drop-primary-key-column"));
}

#[test]
fn dropping_a_column_that_is_not_the_primary_key_keeps_its_answer() {
    for column in ["note", "email", "amount", "user_id"] {
        let answer = said(
            LAB_TABLES,
            &format!("ALTER TABLE orders DROP COLUMN {column}"),
        );
        assert_eq!(answer.case, Some("drop-column"), "{column}");
        assert_eq!(answer.answer, INSTANT, "{column}");
        assert!(
            !answer.evidence.contains(&"drop-primary-key-column"),
            "{column}"
        );
    }
}

#[test]
fn dropping_a_column_of_a_table_kilit_does_not_know_is_instant_with_a_note_about_the_primary_key() {
    // 0.1.0 said INSTANT with nothing about the primary key; MySQL copies the
    // table (drop-primary-key-column).
    for (label, schema) in tables_kilit_does_not_know() {
        let answer = said(&schema, "ALTER TABLE orders DROP COLUMN id");
        assert_eq!(answer.case, Some("drop-column"), "{label}");
        assert_eq!(answer.answer, INSTANT, "{label}");
        assert!(
            answer.has_note_with(&[
                "if the column is the primary key: ",
                "drop-primary-key-column"
            ]),
            "{label}: {:?}",
            answer.notes
        );
    }
}

#[test]
fn forcing_algorithm_instant_on_a_drop_without_a_known_table_notes_the_error_1846() {
    // drop-primary-key-column: ALGORITHM=INSTANT is refused with 1846.
    for (label, schema) in tables_kilit_does_not_know() {
        let answer = said(
            &schema,
            "ALTER TABLE orders DROP COLUMN id, ALGORITHM=INSTANT",
        );
        assert_eq!(answer.answer, INSTANT, "{label}");
        assert!(
            answer.has_note_with(&["fails with error 1846", "drop-primary-key-column"]),
            "{label}: {:?}",
            answer.notes
        );
    }
}

/// The lab's tables with `PRIMARY KEY (id, user_id)` on `orders`, as the
/// scenario drop-column-in-two-column-primary-key sets it up.
fn lab_with_two_column_primary_key() -> String {
    lab_after(&["ALTER TABLE orders DROP PRIMARY KEY, ADD PRIMARY KEY (id, user_id)"])
}

#[test]
fn dropping_the_second_column_of_a_two_column_primary_key_rebuilds_inplace_and_notes_duplicates() {
    // drop-column-in-two-column-primary-key: INPLACE with a rebuild, writes
    // continue. If the first column repeats, the key that is left does not
    // hold (drop-column-in-two-column-primary-key-duplicates): error 1062,
    // which the rows decide.
    let answer = said(
        &lab_with_two_column_primary_key(),
        "ALTER TABLE orders DROP COLUMN user_id",
    );
    assert_eq!(answer.case, Some("drop-column"));
    assert_eq!(answer.answer, INPLACE_REBUILD);
    assert!(
        answer
            .evidence
            .contains(&"drop-column-in-two-column-primary-key"),
        "{:?}",
        answer.evidence
    );
    assert!(
        answer.has_note_with(&[
            "if the column is the second of a primary key of two, and the first holds duplicate values: fails with error 1062",
            "drop-column-in-two-column-primary-key-duplicates"
        ]),
        "{:?}",
        answer.notes
    );
}

#[test]
fn the_variants_of_dropping_the_second_column_of_a_two_column_primary_key_are_the_lab_s() {
    // rebuild 1845 rebuild rebuild copy rebuild
    assert_six_variants(
        &lab_with_two_column_primary_key(),
        "",
        "ALTER TABLE orders DROP COLUMN user_id",
        [
            INPLACE_REBUILD_PREFIX,
            FAILS_1845,
            INPLACE_REBUILD_PREFIX,
            INPLACE_REBUILD_PREFIX,
            COPY_PREFIX,
            INPLACE_REBUILD_PREFIX,
        ],
    );
}

#[test]
fn dropping_the_first_column_of_a_two_column_primary_key_is_unknown() {
    // Only the second column's drop was measured.
    let answer = said(
        &lab_with_two_column_primary_key(),
        "ALTER TABLE orders DROP COLUMN id",
    );
    assert_eq!(answer.answer, "unknown");
}

#[test]
fn dropping_a_column_of_a_three_column_primary_key_is_unknown() {
    let schema =
        lab_after(&["ALTER TABLE orders DROP PRIMARY KEY, ADD PRIMARY KEY (id, user_id, amount)"]);
    for column in ["id", "user_id", "amount"] {
        let answer = said(&schema, &format!("ALTER TABLE orders DROP COLUMN {column}"));
        assert_eq!(answer.answer, "unknown", "{column}");
    }
}

#[test]
fn the_notes_of_a_drop_without_a_known_table_say_what_is_not_measured_about_keys() {
    // A column in a foreign key or in a unique index with other columns: the
    // table would decide, kilit cannot see it, so the answer carries a note.
    for (label, schema) in tables_kilit_does_not_know() {
        let answer = said(&schema, "ALTER TABLE orders DROP COLUMN note");
        assert_eq!(answer.answer, INSTANT, "{label}");
        assert!(
            answer.has_note_with(&[
                "if the column is in a foreign key or a unique index with other columns",
                "not measured"
            ]),
            "{label}: {:?}",
            answer.notes
        );
    }
}

#[test]
fn a_drop_on_a_known_table_has_no_note_about_keys_it_can_see() {
    let answer = said(LAB_TABLES, "ALTER TABLE orders DROP COLUMN note");
    assert_eq!(answer.answer, INSTANT);
    assert!(
        !answer.has_note_with(&["if the column is the primary key"]),
        "{:?}",
        answer.notes
    );
    assert!(
        !answer.has_note_with(&["a foreign key or a unique index with other columns"]),
        "{:?}",
        answer.notes
    );
}

#[test]
fn a_drop_of_a_column_in_a_multi_column_unique_index_of_a_known_table_stays_unknown() {
    let schema = lab_after(&["ALTER TABLE orders ADD UNIQUE INDEX uq_ua (user_id, amount)"]);
    assert_eq!(
        said(&schema, "ALTER TABLE orders DROP COLUMN amount").answer,
        "unknown"
    );
}

#[test]
fn an_index_on_a_table_kilit_does_not_know_notes_that_a_json_column_is_not_measured() {
    // ALTER TABLE ... ADD INDEX over a JSON column is unknown on a known
    // table (see below); without the table the measured answer carries a note.
    for (label, schema) in tables_kilit_does_not_know() {
        for sql in [
            "ALTER TABLE orders ADD INDEX ix_note (note)",
            "ALTER TABLE orders ADD UNIQUE INDEX uq_email (email)",
            "alter table `orders` add index `orders_note_index`(`note`)",
        ] {
            let answer = said(&schema, sql);
            assert!(answer.case.is_some(), "{label}: {sql}: {answer:?}");
            assert!(
                answer.has_note_with(&["if a column of the index is JSON", "not measured"]),
                "{label}: {sql}: {:?}",
                answer.notes
            );
        }
    }
}

#[test]
fn an_index_on_a_json_column_of_a_known_table_stays_unknown() {
    let schema = lab_after(&["ALTER TABLE orders ADD COLUMN j JSON NULL"]);
    assert_eq!(
        said(&schema, "ALTER TABLE orders ADD INDEX ix_j (j)").answer,
        "unknown"
    );
}

#[test]
fn an_index_on_a_known_table_has_no_note_about_json() {
    let answer = said(LAB_TABLES, "ALTER TABLE orders ADD INDEX ix_note (note)");
    assert!(answer.case.is_some());
    assert!(
        !answer.has_note_with(&["if a column of the index is JSON"]),
        "{:?}",
        answer.notes
    );
}

#[test]
fn a_check_on_a_table_kilit_does_not_know_notes_what_it_cannot_see() {
    // A taken name, a column that is not there, an AUTO_INCREMENT column, a
    // generated column, a foreign key column: the table would decide.
    for (label, schema) in tables_kilit_does_not_know() {
        for sql in [
            "ALTER TABLE orders ADD CONSTRAINT ck_amount CHECK (amount > 0)",
            "ALTER TABLE orders ADD CONSTRAINT ck_amount CHECK (amount > 0) NOT ENFORCED",
        ] {
            let answer = said(&schema, sql);
            assert!(answer.case.is_some(), "{label}: {sql}: {answer:?}");
            assert!(
                answer.has_note_with(&["AUTO_INCREMENT", "generated", "not measured"]),
                "{label}: {sql}: {:?}",
                answer.notes
            );
        }
    }
}

#[test]
fn a_check_on_an_auto_increment_column_of_a_known_table_stays_unknown() {
    assert_eq!(
        said(
            LAB_TABLES,
            "ALTER TABLE orders ADD CONSTRAINT ck CHECK (id > 0)"
        )
        .answer,
        "unknown"
    );
}

#[test]
fn a_check_on_a_plain_column_of_a_known_table_has_no_note_about_what_it_can_see() {
    let answer = said(
        LAB_TABLES,
        "ALTER TABLE orders ADD CONSTRAINT ck_amount CHECK (amount > 0)",
    );
    assert!(answer.case.is_some());
    assert!(
        !answer.has_note_with(&["AUTO_INCREMENT", "generated", "not measured"]),
        "{:?}",
        answer.notes
    );
}

const ADD_FOREIGN_KEY_USER: &str =
    "ALTER TABLE orders ADD CONSTRAINT fk_orders_user FOREIGN KEY (user_id) REFERENCES users (id)";

#[test]
fn a_foreign_key_with_a_table_kilit_does_not_know_notes_what_it_cannot_see() {
    // A missing column, NOT NULL with ON DELETE SET NULL, a MyISAM table: the
    // tables would decide. (The notes about the parent's index differ per
    // server, so each server is looked at on its own.)
    let note_parts = ["ON DELETE SET NULL", "MyISAM", "not measured"];
    let child_lost = "CREATE TABLE users (id BIGINT UNSIGNED AUTO_INCREMENT PRIMARY KEY, name VARCHAR(100) NOT NULL);
         CREATE TABLE orders (id BIGINT UNSIGNED AUTO_INCREMENT PRIMARY KEY, user_id BIGINT UNSIGNED NOT NULL);
         ALTER TABLE orders AUTO_INCREMENT = 1000;";
    let parent_missing = "CREATE TABLE orders (id BIGINT UNSIGNED AUTO_INCREMENT PRIMARY KEY, user_id BIGINT UNSIGNED NOT NULL);";
    let lab_lost = lab_with_lost_orders();
    for server in [MYSQL_80, MYSQL_84] {
        for (label, schema) in [
            ("no table", ""),
            ("a lost child", lab_lost.as_str()),
            ("the child is lost, the parent known", child_lost),
            ("the child is known, the parent is not", parent_missing),
        ] {
            let answer = said_on(server, schema, ADD_FOREIGN_KEY_USER);
            assert_eq!(answer.case, Some("add-foreign-key"), "{label} on {server}");
            assert_eq!(answer.answer, COPY, "{label} on {server}");
            assert!(
                answer.has_note_with(&note_parts),
                "{label} on {server}: {:?}",
                answer.notes
            );
        }
    }
}

#[test]
fn a_foreign_key_between_known_tables_has_no_note_about_what_it_can_see() {
    let answer = said(LAB_TABLES, ADD_FOREIGN_KEY_USER);
    assert_eq!(answer.answer, COPY);
    assert!(
        !answer.has_note_with(&["ON DELETE SET NULL", "MyISAM"]),
        "{:?}",
        answer.notes
    );
}

#[test]
fn a_foreign_key_that_sets_a_not_null_column_to_null_stays_unknown_on_known_tables() {
    let sql = "ALTER TABLE orders ADD CONSTRAINT fk FOREIGN KEY (user_id) REFERENCES users (id) ON DELETE SET NULL";
    assert_eq!(said(LAB_TABLES, sql).answer, "unknown");
}

#[test]
fn a_default_set_on_a_table_kilit_does_not_know_notes_an_expression_default() {
    // If the old default is an expression, changing it was not measured.
    for (label, schema) in tables_kilit_does_not_know() {
        let answer = said(&schema, "ALTER TABLE orders ALTER COLUMN x SET DEFAULT 'v'");
        assert_eq!(answer.case, Some("set-column-default"), "{label}");
        assert!(
            answer.has_note_with(&["has an expression default", "not measured"]),
            "{label}: {:?}",
            answer.notes
        );
    }
}

#[test]
fn a_default_changed_over_an_expression_default_of_a_known_table_stays_unknown() {
    let schema = lab_after(&["ALTER TABLE orders ADD COLUMN d INT NULL DEFAULT (1 + 1)"]);
    assert_eq!(
        said(&schema, "ALTER TABLE orders ALTER COLUMN d SET DEFAULT 5").answer,
        "unknown"
    );
}

#[test]
fn a_default_set_on_a_known_column_has_no_note_about_an_expression_default() {
    let answer = said(
        LAB_TABLES,
        "ALTER TABLE orders ALTER COLUMN amount SET DEFAULT 5",
    );
    assert_eq!(answer.answer, INSTANT);
    assert!(
        !answer.has_note_with(&["expression default"]),
        "{:?}",
        answer.notes
    );
}

/// The note a column added to a table that kilit does not know carries about
/// the 65,535 bytes of a row.
const ROW_GROWS_NOTE: &str = "if the row grows past the 65,535 bytes MySQL allows";

#[test]
fn a_new_column_on_a_table_kilit_does_not_know_notes_the_row_size_limit() {
    // B1: a real migration (snipe-it, settings) failed with 1118 where kilit
    // said INSTANT.
    for (label, schema) in tables_kilit_does_not_know() {
        let answer = said(
            &schema,
            "ALTER TABLE orders ADD COLUMN link_color VARCHAR(255) NULL",
        );
        assert_eq!(answer.case, Some("add-column"), "{label}");
        assert_eq!(answer.answer, INSTANT, "{label}");
        assert!(
            answer.has_note_starting(ROW_GROWS_NOTE),
            "{label}: {:?}",
            answer.notes
        );
    }
}

#[test]
fn a_new_column_on_a_known_table_far_from_the_row_size_limit_has_no_such_note() {
    let answer = said(
        LAB_TABLES,
        "ALTER TABLE orders ADD COLUMN coupon VARCHAR(255) NULL",
    );
    assert_eq!(answer.answer, INSTANT);
    assert!(
        !answer.has_note_starting(ROW_GROWS_NOTE),
        "{:?}",
        answer.notes
    );
}

#[test]
fn a_new_column_that_takes_a_known_table_past_65535_bytes_fails_with_1118() {
    // add-column-over-row-size-limit: the lab's orders and 62 VARCHAR(255)
    // columns (64,607 bytes of columns and 8 of NULL flags); VARCHAR(230) NULL
    // adds 920 bytes and a ninth flag byte, ends at 65,538 and fails with 1118
    // on both servers, in every variant.
    let schema = lab_with_62_wide_columns(&[]);
    let answer = said(
        &schema,
        "ALTER TABLE orders ADD COLUMN s63 VARCHAR(230) NULL",
    );
    assert_eq!(answer.answer, FAILS_1118);
    assert_eq!(answer.case, Some("add-column"));
    assert!(
        answer.evidence.contains(&"add-column-over-row-size-limit"),
        "{:?}",
        answer.evidence
    );
    assert_six_variants(
        &schema,
        "",
        "ALTER TABLE orders ADD COLUMN s63 VARCHAR(230) NULL",
        [FAILS_1118; 6],
    );
}

#[test]
fn a_new_column_that_takes_a_known_table_to_65534_bytes_is_instant() {
    // add-column-at-row-size-limit: VARCHAR(229) NULL (918 bytes) ends at
    // 65,525 bytes of columns plus 9 of NULL flags, 65,534 in all; the lab
    // ran it INSTANT on both servers.
    let schema = lab_with_62_wide_columns(&[]);
    let answer = said(
        &schema,
        "ALTER TABLE orders ADD COLUMN s63 VARCHAR(229) NULL",
    );
    assert_eq!(answer.case, Some("add-column"));
    assert_eq!(answer.answer, INSTANT);
    assert!(
        !answer.has_note_starting(ROW_GROWS_NOTE),
        "{:?}",
        answer.notes
    );
}

#[test]
fn a_laravel_string_column_that_takes_a_known_table_past_65535_bytes_fails_with_1118() {
    // $table->string('s63', 230)->nullable();
    let schema = lab_with_62_wide_columns(&[]);
    let answer = said(&schema, "alter table `orders` add `s63` varchar(230) null");
    assert_eq!(answer.answer, FAILS_1118);
}

#[test]
fn a_column_that_fails_with_1118_leaves_the_table_as_it_was() {
    // A measured failure changes no table: the next, smaller column fits.
    let schema = lab_with_62_wide_columns(&[]);
    let answers = on_mysql_84(
        &schema,
        "ALTER TABLE orders ADD COLUMN s63 VARCHAR(230) NULL;
         ALTER TABLE orders ADD COLUMN s63 VARCHAR(229) NULL;",
    );
    assert_eq!(answers, [FAILS_1118, INSTANT]);
}

#[test]
fn widening_a_varchar_past_65535_bytes_in_a_known_table_fails_with_1118() {
    // lengthen-varchar-over-row-size-limit: s63 VARCHAR(200) -> VARCHAR(230)
    // ends at 65,538. lengthen-varchar-at-row-size-limit: VARCHAR(229) ends
    // at 65,534 and runs INPLACE with no rebuild.
    let schema = lab_with_62_wide_columns(&["ALTER TABLE orders ADD COLUMN s63 VARCHAR(200) NULL"]);
    let over = said(&schema, "ALTER TABLE orders MODIFY s63 VARCHAR(230) NULL");
    assert_eq!(over.answer, FAILS_1118);
    assert_eq!(over.case, Some("lengthen-varchar"));
    assert!(
        over.evidence
            .contains(&"lengthen-varchar-over-row-size-limit"),
        "{:?}",
        over.evidence
    );
    let at = said(&schema, "ALTER TABLE orders MODIFY s63 VARCHAR(229) NULL");
    assert_eq!(at.case, Some("lengthen-varchar"));
    assert_eq!(at.answer, INPLACE);
}

#[test]
fn a_laravel_change_that_widens_a_string_past_65535_bytes_fails_with_1118() {
    // $table->string('s63', 230)->nullable()->change();
    let schema = lab_with_62_wide_columns(&["ALTER TABLE orders ADD COLUMN s63 VARCHAR(200) NULL"]);
    let answer = said(
        &schema,
        "alter table `orders` modify `s63` varchar(230) null",
    );
    assert_eq!(answer.answer, FAILS_1118);
}

/// A dump whose `orders` has a primary key `id`, a single-column foreign key
/// `user_id` to `users`, and an index on `note`.
fn lab_with_foreign_key_on_user_id() -> String {
    lab_after(&[
        "ALTER TABLE orders ADD CONSTRAINT fk_orders_user FOREIGN KEY (user_id) REFERENCES users (id)",
    ])
}

#[test]
fn dropping_a_column_a_known_table_does_not_have_is_unknown_and_names_error_1091() {
    // drop-column-missing: 1091 on both servers, in every variant. The
    // answer is unknown on purpose: the dump may be out of date.
    for (sql, scenario) in [
        (
            "ALTER TABLE orders DROP COLUMN nosuch",
            "drop-column-missing",
        ),
        ("alter table `orders` drop `nosuch`", "drop-column-missing"),
        ("ALTER TABLE orders DROP INDEX nosuch", "drop-index-missing"),
        (
            "alter table `orders` drop index `nosuch`",
            "drop-index-missing",
        ),
        (
            "ALTER TABLE orders DROP FOREIGN KEY nosuch",
            "drop-foreign-key-missing",
        ),
        (
            "alter table `orders` drop foreign key `nosuch`",
            "drop-foreign-key-missing",
        ),
    ] {
        let answer = said(LAB_TABLES, sql);
        assert_eq!(answer.answer, "unknown", "{sql}");
        assert!(answer.reason.contains("1091"), "{sql}: {}", answer.reason);
        assert!(answer.reason.contains(scenario), "{sql}: {}", answer.reason);
    }
}

#[test]
fn a_known_table_stays_known_after_a_drop_of_what_it_does_not_have() {
    // 0.1.0 lost the table here and then said INSTANT for dropping the
    // foreign key's column; MySQL refuses it with 1828.
    let schema = lab_with_foreign_key_on_user_id();
    for gone in [
        "ALTER TABLE orders DROP COLUMN nosuch",
        "ALTER TABLE orders DROP INDEX nosuch",
        "ALTER TABLE orders DROP FOREIGN KEY nosuch",
    ] {
        let sql = format!("{gone};\nALTER TABLE orders DROP COLUMN user_id");
        for answers in all_on_both(&schema, &sql) {
            assert_eq!(answers, ["unknown", FAILS_1828], "{sql}");
        }
    }
}

#[test]
fn a_drop_of_what_a_known_table_does_not_have_does_not_hide_the_other_clauses_of_later_statements()
{
    // The table stays known, so the primary key drop after it is the
    // measured COPY, not a guess on a lost table.
    let sql = "ALTER TABLE orders DROP COLUMN nosuch;
               ALTER TABLE orders DROP COLUMN id;";
    for answers in all_on_both(LAB_TABLES, sql) {
        assert_eq!(answers, ["unknown", COPY]);
    }
}

#[test]
fn dropping_a_column_in_another_letter_case_is_the_column_and_keeps_its_answer() {
    // MySQL compares column names without regard to case.
    assert_eq!(
        said(LAB_TABLES, "ALTER TABLE orders DROP COLUMN AMOUNT").answer,
        INSTANT
    );
    assert_eq!(
        said(LAB_TABLES, "ALTER TABLE orders DROP COLUMN Id").answer,
        COPY
    );
}

#[test]
fn dropping_what_a_table_kilit_does_not_know_may_have_keeps_the_measured_answer() {
    // Without the table kilit cannot tell that the column is not there.
    for (label, schema) in tables_kilit_does_not_know() {
        let answer = said(&schema, "ALTER TABLE orders DROP COLUMN nosuch");
        assert_eq!(answer.case, Some("drop-column"), "{label}");
        assert_eq!(answer.answer, INSTANT, "{label}");
    }
}

/// The lab's tables with 30 CHAR(255) latin1 NOT NULL columns more on
/// `orders` (a utf8mb4 table): 255 bytes each in the page, as the scenarios
/// add-column-at-page-row-size-limit and add-column-over-page-row-size-limit
/// set them up.
fn lab_with_30_latin1_columns() -> String {
    let columns: Vec<String> = (1..=30)
        .map(|n| format!("ADD COLUMN f{n} CHAR(255) CHARACTER SET latin1 NOT NULL DEFAULT ''"))
        .collect();
    lab_after(&[&format!("ALTER TABLE orders {}", columns.join(", "))])
}

#[test]
fn a_column_that_brings_a_row_to_the_most_innodb_keeps_in_a_page_is_instant() {
    // Lab-verified boundary (both servers): CHAR(194) latin1 NOT NULL is the
    // last one that fits.
    let answer = said(
        &lab_with_30_latin1_columns(),
        "ALTER TABLE orders ADD x CHAR(194) CHARACTER SET latin1 NOT NULL DEFAULT ''",
    );
    assert_eq!(answer.case, Some("add-column"));
    assert_eq!(answer.answer, INSTANT);
}

#[test]
fn a_column_one_character_past_the_most_innodb_keeps_in_a_page_is_unknown_for_both_servers() {
    // The two servers count the row differently here (8.0.46 refuses it with
    // 1118, 8.4.13 rebuilds), so kilit does not answer.
    let schema = lab_with_30_latin1_columns();
    let sql = "ALTER TABLE orders ADD x CHAR(195) CHARACTER SET latin1 NOT NULL DEFAULT ''";
    for server in [MYSQL_80, MYSQL_84] {
        let answer = said_on(server, &schema, sql);
        assert_eq!(answer.answer, "unknown", "{server}");
        assert!(
            answer.reason.contains("8.0.46"),
            "{server}: {}",
            answer.reason
        );
        assert!(
            answer.reason.contains("8.4.13"),
            "{server}: {}",
            answer.reason
        );
    }
}

#[test]
fn a_laravel_migration_that_adds_three_string_columns_to_a_wide_table_fails_at_the_third() {
    // B1's real case (a settings table; migration add_link_colors_to_settings),
    // reduced to a hand-written table: an INT key and 62 VARCHAR(255) NULL
    // columns in utf8mb4 take 1,022 bytes each (4 x 255 and 2 length bytes), so
    // the row has 63,388 bytes plus the null bitmap. Each added column brings
    // 1,022 more: 63 and 64 columns fit (65,420 bytes), the 65th is past the
    // 65,535 bytes MySQL allows and fails with 1118.
    let columns: String = (1..=61)
        .map(|n| format!("  `c{n}` varchar(255) DEFAULT NULL,\n"))
        .collect();
    let schema = format!(
        "CREATE TABLE `settings` (
  `id` int unsigned NOT NULL AUTO_INCREMENT,
  `header_color` varchar(255) DEFAULT NULL,
{columns}  PRIMARY KEY (`id`)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_unicode_ci;
"
    );
    let output = pretend(
        "2025_11_28_175733_add_link_colors_to_settings",
        &[
            "alter table `settings` add `link_dark_color` varchar(255) null after `header_color`",
            "alter table `settings` add `link_light_color` varchar(255) null after `header_color`",
            "alter table `settings` add `nav_link_color` varchar(255) null default '#ffffff' after `header_color`",
        ],
    );
    assert_eq!(
        pretend_on_mysql_84(&schema, &output),
        [INSTANT, INSTANT, FAILS_1118]
    );
}

// ---------------------------------------------------------------------------
// Step B1b, fix phase: keys MySQL names itself, VARCHAR columns longer than
// MySQL accepts, and the note about a record past what a page keeps.
// The expectations come from the lab (drop-foreign-key, drop-index and the
// scenarios of the row size limit), from the hand tests of the review (a
// foreign key written without a name is called orders_ibfk_1, an index
// without a name takes its first column's name, and MySQL 8.0.46 and 8.4.13
// accept the drops of both and the drop of the column afterwards; a VARCHAR
// that cannot fit in 65,535 bytes at all is refused with 1074 in strict mode
// and converted to TEXT otherwise) and from the decisions of the step.

/// `orders` with a foreign key written without a name, as in
/// `CREATE TABLE orders (..., FOREIGN KEY (user_id) REFERENCES users (id))`.
/// MySQL calls it orders_ibfk_1 and makes an index on user_id for it.
const UNNAMED_FOREIGN_KEY_TABLES: &str = "
CREATE TABLE users (
    id   BIGINT UNSIGNED AUTO_INCREMENT PRIMARY KEY,
    name VARCHAR(100) NOT NULL
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4;

CREATE TABLE orders (
    id      BIGINT UNSIGNED AUTO_INCREMENT PRIMARY KEY,
    user_id BIGINT UNSIGNED NOT NULL,
    amount  INT NOT NULL,
    email   VARCHAR(255) NULL,
    note    VARCHAR(50) NULL,
    FOREIGN KEY (user_id) REFERENCES users (id)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4;
";

/// `orders` with an index, a unique index and a FULLTEXT index written
/// without a name: MySQL names each after its first column (amount, email,
/// note).
const UNNAMED_INDEX_TABLES: &str = "
CREATE TABLE users (
    id   BIGINT UNSIGNED AUTO_INCREMENT PRIMARY KEY,
    name VARCHAR(100) NOT NULL
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4;

CREATE TABLE orders (
    id      BIGINT UNSIGNED AUTO_INCREMENT PRIMARY KEY,
    user_id BIGINT UNSIGNED NOT NULL,
    amount  INT NOT NULL,
    email   VARCHAR(255) NULL,
    note    VARCHAR(50) NULL,
    INDEX (amount),
    UNIQUE (email),
    FULLTEXT(note)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4;
";

#[test]
fn dropping_the_foreign_key_mysql_named_itself_gets_the_measured_answer_not_the_1091_unknown() {
    // ALTER TABLE orders DROP FOREIGN KEY orders_ibfk_1: accepted by MySQL
    // 8.0.46 and 8.4.13; the lab's drop-foreign-key is INPLACE, no rebuild.
    for sql in [
        "ALTER TABLE orders DROP FOREIGN KEY orders_ibfk_1",
        "alter table `orders` drop foreign key `orders_ibfk_1`",
        "ALTER TABLE orders DROP FOREIGN KEY ORDERS_IBFK_1",
    ] {
        let answer = said(UNNAMED_FOREIGN_KEY_TABLES, sql);
        assert_eq!(answer.answer, INPLACE, "{sql}: {:?}", answer.reason);
        assert_eq!(answer.case, Some("drop-foreign-key"), "{sql}");
        assert!(answer.reason.is_empty(), "{sql}: {}", answer.reason);
    }
}

#[test]
fn the_column_of_a_foreign_key_mysql_named_itself_is_not_refused_with_1828_after_the_key_is_dropped()
 {
    // Both statements are accepted by MySQL 8.0.46 and 8.4.13. kilit does
    // not know the key under the name MySQL gave it, so it stops trusting
    // the table and cannot call the second statement a failure.
    let sql = "ALTER TABLE orders DROP FOREIGN KEY orders_ibfk_1;
               ALTER TABLE orders DROP COLUMN user_id";
    let after = said(UNNAMED_FOREIGN_KEY_TABLES, sql);
    assert_ne!(after.answer, FAILS_1828, "{after:?}");
    assert_ne!(after.answer, "unknown", "{after:?}");
    // The answer is the one for a table kilit does not know.
    let lost = said(
        &lab_with_lost_orders(),
        "ALTER TABLE orders DROP COLUMN user_id",
    );
    assert_eq!(after.answer, lost.answer);
    assert_eq!(after.case, lost.case);
    // Dropping the column first is still the failure it was.
    assert_eq!(
        said(
            UNNAMED_FOREIGN_KEY_TABLES,
            "ALTER TABLE orders DROP COLUMN user_id"
        )
        .answer,
        FAILS_1828
    );
}

#[test]
fn laravel_dropping_the_foreign_key_mysql_named_itself_and_then_its_column_is_not_a_failure() {
    // A table made by plain SQL, then a migration:
    // $table->dropForeign('orders_ibfk_1'); $table->dropColumn('user_id');
    let output = pretend(
        "2026_11_05_100000_drop_user_from_orders_table",
        &[
            "alter table `orders` drop foreign key `orders_ibfk_1`",
            "alter table `orders` drop `user_id`",
        ],
    );
    let answers = pretend_on_mysql_84(UNNAMED_FOREIGN_KEY_TABLES, &output);
    assert_eq!(answers[0], INPLACE);
    assert_ne!(answers[1], FAILS_1828);
}

#[test]
fn dropping_the_index_mysql_named_after_its_column_is_not_the_1091_unknown() {
    // INDEX (amount), UNIQUE (email), FULLTEXT(note) without names: MySQL
    // calls them amount, email and note, and accepts the drops (hand tests,
    // both servers).
    for name in ["amount", "email", "note"] {
        for sql in [
            format!("ALTER TABLE orders DROP INDEX {name}"),
            format!("alter table `orders` drop index `{name}`"),
            format!("ALTER TABLE orders DROP KEY {}", name.to_uppercase()),
        ] {
            let answer = said(UNNAMED_INDEX_TABLES, &sql);
            assert!(!answer.reason.contains("1091"), "{sql}: {}", answer.reason);
        }
    }
    // The plain index is the lab's drop-index.
    let answer = said(UNNAMED_INDEX_TABLES, "ALTER TABLE orders DROP INDEX amount");
    assert_eq!(answer.answer, INPLACE);
    assert_eq!(answer.case, Some("drop-index"));
}

#[test]
fn a_varchar_too_long_for_any_row_is_unknown_never_1118_when_a_column_is_added() {
    // VARCHAR(20000) in utf8mb4 needs 80,000 bytes: MySQL refuses it with
    // 1074 in strict mode and makes it a TEXT otherwise. 1118 is the row
    // size error, a different one.
    for sql in [
        "ALTER TABLE orders ADD COLUMN c VARCHAR(20000) NULL",
        "alter table `orders` add `c` varchar(20000) null",
        "ALTER TABLE orders ADD COLUMN c VARCHAR(20000) NOT NULL DEFAULT ''",
    ] {
        let answer = said(LAB_TABLES, sql);
        assert_eq!(answer.answer, "unknown", "{sql}: {answer:?}");
        assert!(!answer.reason.is_empty(), "{sql}");
        assert!(!answer.reason.contains("1118"), "{sql}: {}", answer.reason);
    }
}

#[test]
fn a_varchar_too_long_for_any_row_is_unknown_never_1118_when_a_column_is_changed() {
    for sql in [
        "ALTER TABLE orders MODIFY email VARCHAR(20000) NULL",
        "alter table `orders` modify `email` varchar(20000) null",
        "ALTER TABLE orders CHANGE email email VARCHAR(20000) NULL",
    ] {
        let answer = said(LAB_TABLES, sql);
        assert_eq!(answer.answer, "unknown", "{sql}: {answer:?}");
        assert!(!answer.reason.contains("1118"), "{sql}: {}", answer.reason);
    }
}

#[test]
fn a_varchar_too_long_for_any_row_is_unknown_after_an_empty_sql_mode_too() {
    // Without the strict modes MySQL converts the column to TEXT; kilit has
    // not measured that and does not say 1118 either.
    for sql in [
        "SET sql_mode = '';\nALTER TABLE orders ADD COLUMN c VARCHAR(20000) NULL",
        "SET sql_mode = '';\nALTER TABLE orders MODIFY email VARCHAR(20000) NULL",
        "SET SESSION sql_mode = '';\nALTER TABLE orders ADD COLUMN c VARCHAR(20000) NULL",
    ] {
        let answer = said(LAB_TABLES, sql);
        assert_eq!(answer.answer, "unknown", "{sql}: {answer:?}");
        assert!(!answer.reason.contains("1118"), "{sql}: {}", answer.reason);
    }
}

#[test]
fn laravel_s_string_column_longer_than_mysql_accepts_is_unknown_never_1118() {
    // $table->string('c', 20000)->nullable();
    // $table->string('email', 20000)->nullable()->change();
    for (statement, sql) in [
        ("add", "alter table `orders` add `c` varchar(20000) null"),
        (
            "change",
            "alter table `orders` modify `email` varchar(20000) null",
        ),
    ] {
        let output = pretend("2026_11_05_100000_widen_orders_table", &[sql]);
        let answers = pretend_on_mysql_84(LARAVEL_TABLES, &output);
        assert_eq!(answers, ["unknown"], "{statement}");
    }
}

#[test]
fn a_varchar_at_the_longest_a_row_can_hold_keeps_its_answer() {
    // Regression: VARCHAR(16383) in utf8mb4 is the longest column MySQL
    // accepts (65,532 bytes and two for the length); the row then holds
    // nothing else, so the lab's orders fails with 1118 as before. Only the
    // length that no row can hold is unknown.
    let answer = said(
        LAB_TABLES,
        "ALTER TABLE orders ADD COLUMN c VARCHAR(16383) NULL",
    );
    assert_eq!(answer.answer, FAILS_1118, "{answer:?}");
}

/// The note a record past what a page keeps carries: the two servers count
/// it differently, so kilit cannot tell.
const PAGE_RECORD_NOTE: &str = "if the record grows past what InnoDB keeps in a page (8126 bytes)";

#[test]
fn a_new_column_on_a_table_kilit_does_not_know_notes_the_record_in_a_page_too() {
    // add-column-over-page-row-size-limit: 8.0.46 fails with 1118, 8.4.13
    // rebuilds the table (scenarios of the review, expected-B1b).
    for (label, schema) in tables_kilit_does_not_know() {
        for sql in [
            "ALTER TABLE orders ADD COLUMN link_color VARCHAR(255) NULL",
            "alter table `orders` add `link_color` varchar(255) null",
        ] {
            let answer = said(&schema, sql);
            assert_eq!(answer.answer, INSTANT, "{label}: {sql}");
            assert!(
                answer.has_note_starting(PAGE_RECORD_NOTE),
                "{label}: {sql}: {:?}",
                answer.notes
            );
            assert!(
                answer.has_note_with(&["8.0.46", "1118", "8.4.13"]),
                "{label}: {sql}: {:?}",
                answer.notes
            );
            // The note about 65,535 bytes stays next to it.
            assert!(
                answer.has_note_starting(ROW_GROWS_NOTE),
                "{label}: {sql}: {:?}",
                answer.notes
            );
        }
    }
}

#[test]
fn a_new_column_on_a_known_table_has_no_note_about_the_record_in_a_page() {
    // kilit counts every column of the table, so the note would only
    // repeat what the answer already says.
    for (label, schema) in [
        ("the lab's tables", LAB_TABLES.to_owned()),
        ("Laravel's tables", LARAVEL_TABLES.to_owned()),
    ] {
        let answer = said(
            &schema,
            "ALTER TABLE orders ADD COLUMN link_color VARCHAR(255) NULL",
        );
        assert_eq!(answer.answer, INSTANT, "{label}");
        assert!(
            !answer.has_note_starting(PAGE_RECORD_NOTE),
            "{label}: {:?}",
            answer.notes
        );
    }
}

#[test]
fn only_the_plain_new_column_carries_the_note_about_the_record_in_a_page() {
    for (label, schema) in tables_kilit_does_not_know() {
        for sql in [
            "ALTER TABLE orders DROP COLUMN note",
            "ALTER TABLE orders ADD INDEX idx_orders_amount (amount)",
            "ALTER TABLE orders RENAME COLUMN note TO memo",
        ] {
            let answer = said(&schema, sql);
            assert!(
                !answer.has_note_starting(PAGE_RECORD_NOTE),
                "{label}: {sql}: {:?}",
                answer.notes
            );
        }
    }
}

// ---------------------------------------------------------------------------
// Step B1b, second fix phase: rows that other things key, foreign keys MySQL
// renames with their table, FTS_DOC_ID, TEXT and JSON defaults after
// `SET sql_mode = ''`, and columns too long for any table.
// The expectations come from the hand tests of the review (the files
// pkprefix-*, nopkuniq-*, fk-*, md-* and toolong.sql of the second review),
// the lab's add-column and drop-column scenarios, and the step's decisions.

/// `CREATE TABLE q (...)` as the review's hand tests built it: the columns of
/// `head`, then CHAR(255) latin1 NOT NULL columns that add up to `filler`
/// bytes (the last one shorter), then the definitions of `tail`, in a utf8mb4
/// InnoDB table.
fn table_q_with_filler(head: &[&str], filler: usize, tail: &[&str]) -> String {
    let mut parts: Vec<String> = head.iter().map(|part| (*part).to_owned()).collect();
    let mut left = filler;
    let mut n = 0;
    while left > 0 {
        let size = left.min(255);
        parts.push(format!("z{n} CHAR({size}) CHARACTER SET latin1 NOT NULL"));
        left -= size;
        n += 1;
    }
    parts.extend(tail.iter().map(|part| (*part).to_owned()));
    format!(
        "CREATE TABLE q ({}) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4;\n",
        parts.join(", ")
    )
}

const ADD_CHAR_200: &str =
    "ALTER TABLE q ADD COLUMN a CHAR(200) CHARACTER SET latin1 NOT NULL DEFAULT ''";
const ADD_INT: &str = "ALTER TABLE q ADD COLUMN a INT NOT NULL DEFAULT 0";

/// `q` keyed by a prefix of a column: name VARCHAR(255) latin1 NOT NULL and
/// PRIMARY KEY (name(200)), with `filler` bytes of other columns.
fn table_q_keyed_by_a_prefix(filler: usize) -> String {
    table_q_with_filler(
        &["name VARCHAR(255) CHARACTER SET latin1 NOT NULL"],
        filler,
        &["PRIMARY KEY (name(200))"],
    )
}

/// `q` without a primary key whose first UNIQUE index of NOT NULL columns,
/// uk on u VARCHAR(700) utf8mb4, keys the rows.
fn table_q_keyed_by_a_unique_index(filler: usize) -> String {
    table_q_with_filler(&["u VARCHAR(700) NOT NULL"], filler, &["UNIQUE KEY uk (u)"])
}

#[test]
fn a_new_column_on_a_table_keyed_by_a_column_prefix_is_unknown_where_mysql_8_0_46_refuses_it() {
    // pkprefix: 7,500 bytes of filler and CHAR(200) is past the 7,646 bytes
    // (of filler, with an INT added) that MySQL 8.0.46 accepts; the prefix
    // key does not make the record shorter. kilit said INSTANT.
    let schema = table_q_keyed_by_a_prefix(7500);
    for server in [MYSQL_80, MYSQL_84] {
        let answer = said_on(server, &schema, ADD_CHAR_200);
        assert_eq!(answer.answer, "unknown", "{server}: {answer:?}");
        assert!(!answer.reason.is_empty(), "{server}");
    }
}

#[test]
fn a_new_column_on_a_table_without_a_primary_key_keyed_by_a_unique_index_is_unknown_where_mysql_8_0_46_refuses_it()
 {
    // nopkuniq: the unique index of NOT NULL columns keys the rows, so the
    // record holds its columns again (8.0.46 accepts 5,301 bytes of filler
    // with an INT added, this has 5,200 and CHAR(200)). kilit said INSTANT.
    let schema = table_q_keyed_by_a_unique_index(5200);
    for server in [MYSQL_80, MYSQL_84] {
        let answer = said_on(server, &schema, ADD_CHAR_200);
        assert_eq!(answer.answer, "unknown", "{server}: {answer:?}");
        assert!(!answer.reason.is_empty(), "{server}");
    }
}

#[test]
fn a_new_column_on_a_prefix_keyed_or_unique_keyed_table_far_from_the_page_limit_stays_instant() {
    // Regression: the same shapes with 1,000 bytes of filler are the lab's
    // add-column on a small table.
    for (label, schema) in [
        ("prefix key", table_q_keyed_by_a_prefix(1000)),
        ("unique key", table_q_keyed_by_a_unique_index(1000)),
    ] {
        for server in [MYSQL_80, MYSQL_84] {
            let answer = said_on(server, &schema, ADD_CHAR_200);
            assert_eq!(answer.case, Some("add-column"), "{label}: {server}");
            assert_eq!(answer.answer, INSTANT, "{label}: {server}");
        }
    }
}

/// `q(id BIGINT PRIMARY KEY, t TEXT, FULLTEXT KEY ft (t))` and `filler`
/// bytes of other columns.
fn table_q_with_fulltext(filler: usize) -> String {
    table_q_with_filler(
        &["id BIGINT PRIMARY KEY", "t TEXT"],
        filler,
        &["FULLTEXT KEY ft (t)"],
    )
}

#[test]
fn a_new_column_on_a_fulltext_table_is_unknown_once_the_hidden_fts_doc_id_pushes_the_record_past_the_limit()
 {
    // FULLTEXT adds a hidden FTS_DOC_ID of 8 bytes. Hand test, both servers:
    // 8,045 bytes of filler are accepted, 8,046 fail with 1118. kilit said
    // COPY at 8,046.
    let schema = table_q_with_fulltext(8046);
    for server in [MYSQL_80, MYSQL_84] {
        let answer = said_on(server, &schema, ADD_INT);
        assert_eq!(answer.answer, "unknown", "{server}: {answer:?}");
        assert!(!answer.answer.starts_with("COPY"), "{server}");
    }
}

#[test]
fn a_new_column_on_a_fulltext_table_at_the_last_filler_both_servers_accept_still_copies() {
    // Regression: add-column-with-fulltext (COPY) at 8,045 bytes of filler.
    let schema = table_q_with_fulltext(8045);
    for server in [MYSQL_80, MYSQL_84] {
        let answer = said_on(server, &schema, ADD_INT);
        assert_eq!(answer.case, Some("add-column"), "{server}");
        assert_eq!(answer.answer, COPY, "{server}");
    }
}

/// users and orders as `mysqldump` writes them: orders has the key and the
/// foreign key MySQL named orders_ibfk_1.
const IBFK_TABLES: &str = "
CREATE TABLE `users` (
  `id` bigint unsigned NOT NULL AUTO_INCREMENT,
  PRIMARY KEY (`id`)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_0900_ai_ci;
CREATE TABLE `orders` (
  `id` bigint unsigned NOT NULL AUTO_INCREMENT,
  `user_id` bigint unsigned NOT NULL,
  PRIMARY KEY (`id`),
  KEY `user_id` (`user_id`),
  CONSTRAINT `orders_ibfk_1` FOREIGN KEY (`user_id`) REFERENCES `users` (`id`)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_0900_ai_ci;
";

/// The two ways to rename `orders` to `purchases`.
const RENAMES: [&str; 2] = [
    "RENAME TABLE orders TO purchases",
    "ALTER TABLE orders RENAME TO purchases",
];

#[test]
fn dropping_the_foreign_key_by_its_renamed_name_and_then_its_column_is_not_a_failure() {
    // Hand test (fk-m.sql, fk-m2.sql), both servers: MySQL renames
    // orders_ibfk_1 to purchases_ibfk_1 with the table and runs all three
    // statements. kilit said 1091 for the drop and 1828 for the column.
    for rename in RENAMES {
        let drop_key = "ALTER TABLE purchases DROP FOREIGN KEY purchases_ibfk_1";
        let drop_column = "ALTER TABLE purchases DROP COLUMN user_id";
        let sql = format!("{rename};\n{drop_key};\n{drop_column}");
        let key = said(IBFK_TABLES, &format!("{rename};\n{drop_key}"));
        assert!(!key.reason.contains("1091"), "{rename}: {}", key.reason);
        assert!(!key.answer.starts_with("fails"), "{rename}: {key:?}");
        let column = said(IBFK_TABLES, &sql);
        assert_ne!(column.answer, FAILS_1828, "{rename}: {column:?}");
        for answers in all_on_both(IBFK_TABLES, &sql) {
            assert_ne!(answers[2], FAILS_1828, "{rename}");
        }
    }
}

#[test]
fn dropping_the_index_of_a_renamed_foreign_key_after_the_key_is_not_a_failure() {
    // Same hand test with DROP INDEX user_id: MySQL runs it (no 1553).
    for rename in RENAMES {
        let sql = format!(
            "{rename};\nALTER TABLE purchases DROP FOREIGN KEY purchases_ibfk_1;\nALTER TABLE purchases DROP INDEX user_id"
        );
        for answers in all_on_both(IBFK_TABLES, &sql) {
            assert_ne!(answers[2], FAILS_1553, "{rename}");
        }
    }
}

#[test]
fn changing_the_type_of_the_column_of_a_renamed_foreign_key_after_the_key_is_not_a_failure() {
    // fk-t.sql: MODIFY user_id INT NOT NULL after the key is gone is
    // accepted (no 3780). kilit kept the key under its old name.
    for rename in RENAMES {
        let sql = format!(
            "{rename};\nALTER TABLE purchases DROP FOREIGN KEY purchases_ibfk_1;\nALTER TABLE purchases MODIFY user_id INT NOT NULL"
        );
        for answers in all_on_both(IBFK_TABLES, &sql) {
            assert_ne!(answers[2], FAILS_3780, "{rename}");
        }
    }
}

#[test]
fn a_renamed_table_keeps_its_foreign_key_so_dropping_its_column_still_fails_with_1828() {
    // Regression: without the drop of the key, MySQL refuses the column.
    for rename in RENAMES {
        let sql = format!("{rename};\nALTER TABLE purchases DROP COLUMN user_id");
        for answers in all_on_both(IBFK_TABLES, &sql) {
            assert_eq!(answers[1], FAILS_1828, "{rename}");
        }
    }
}

#[test]
fn laravel_renaming_a_table_and_dropping_its_foreign_key_and_column_is_not_a_failure() {
    // Schema::rename('orders', 'purchases');
    // $table->dropForeign('purchases_ibfk_1'); $table->dropColumn('user_id');
    let output = pretend(
        "2026_11_10_100000_rename_orders_to_purchases",
        &[
            "rename table `orders` to `purchases`",
            "alter table `purchases` drop foreign key `purchases_ibfk_1`",
            "alter table `purchases` drop `user_id`",
        ],
    );
    let answers = pretend_on_mysql_84(IBFK_TABLES, &output);
    assert_ne!(answers[2], FAILS_1828, "{answers:?}");
}

#[test]
fn dropping_a_foreign_key_named_after_its_table_that_the_known_table_does_not_have_is_the_1091_unknown()
 {
    // Decision of B1b: any name the known table does not have is the 1091
    // unknown, also orders_ibfk_1, orders_ibfk_x and orders_ibfk_abc; the
    // table stays known, so its column stays protected.
    let schema = lab_with_foreign_key_on_user_id();
    for key in [
        "orders_ibfk_1",
        "orders_ibfk_9",
        "orders_ibfk_12",
        "orders_ibfk_x",
        "orders_ibfk_abc",
    ] {
        let drop_key = format!("ALTER TABLE orders DROP FOREIGN KEY {key}");
        let answer = said(&schema, &drop_key);
        assert_eq!(answer.answer, "unknown", "{key}: {answer:?}");
        assert!(answer.reason.contains("1091"), "{key}: {}", answer.reason);
        let sql = format!("{drop_key};\nALTER TABLE orders DROP COLUMN user_id");
        for answers in all_on_both(&schema, &sql) {
            assert_eq!(answers, ["unknown", FAILS_1828], "{key}");
        }
    }
}

#[test]
fn dropping_a_foreign_key_by_the_old_name_after_a_rename_is_the_1091_unknown_and_the_column_still_fails_with_1828()
 {
    // Hand test ibfk-rename.txt, both servers: after the rename the key is
    // purchases_ibfk_1; DROP FOREIGN KEY orders_ibfk_1 gives 1091, and
    // DROP COLUMN user_id then gives 1828.
    for rename in RENAMES {
        let drop_key = "ALTER TABLE purchases DROP FOREIGN KEY orders_ibfk_1";
        let sql = format!("{rename};\n{drop_key};\nALTER TABLE purchases DROP COLUMN user_id");
        for answers in all_on_both(IBFK_TABLES, &sql) {
            assert_eq!(answers[1], "unknown", "{rename}: {answers:?}");
            assert_eq!(answers[2], FAILS_1828, "{rename}: {answers:?}");
        }
        let key = said(IBFK_TABLES, &format!("{rename};\n{drop_key}"));
        assert!(key.reason.contains("1091"), "{rename}: {}", key.reason);
    }
}

#[test]
fn dropping_a_foreign_key_the_table_does_not_name_keeps_its_answer_when_the_table_has_an_unnamed_key()
 {
    // Decision of B1b: a known table holding a foreign key written without
    // a name may hold the dropped name (MySQL names it itself), so the drop
    // keeps its measured answer and the table is lost.
    let schema = lab_after(&["ALTER TABLE orders ADD FOREIGN KEY (user_id) REFERENCES users (id)"]);
    let drop_key = "ALTER TABLE orders DROP FOREIGN KEY orders_ibfk_1";
    let answer = said(&schema, drop_key);
    assert!(!answer.reason.contains("1091"), "{answer:?}");
    assert!(!answer.answer.starts_with("fails"), "{answer:?}");
    let sql = format!("{drop_key};\nALTER TABLE orders DROP COLUMN user_id");
    for answers in all_on_both(&schema, &sql) {
        assert_ne!(answers[1], FAILS_1828, "{answers:?}");
    }
}

#[test]
fn dropping_a_foreign_key_whose_name_is_not_one_mysql_makes_is_still_the_1091_unknown() {
    // Regression: nosuch, orders_ibfk and orders_ibfk_ (empty suffix, MySQL
    // does not rename it) are no names MySQL gives; the table stays known
    // and its column stays protected.
    let schema = lab_with_foreign_key_on_user_id();
    for key in ["nosuch", "orders_ibfk_", "orders_ibfk"] {
        let sql = format!(
            "ALTER TABLE orders DROP FOREIGN KEY {key};\nALTER TABLE orders DROP COLUMN user_id"
        );
        for answers in all_on_both(&schema, &sql) {
            assert_eq!(answers, ["unknown", FAILS_1828], "{key}");
        }
        let answer = said(
            &schema,
            &format!("ALTER TABLE orders DROP FOREIGN KEY {key}"),
        );
        assert!(answer.reason.contains("1091"), "{key}: {}", answer.reason);
    }
}

#[test]
fn changing_a_text_or_json_column_to_a_quoted_default_after_an_empty_sql_mode_is_unknown_without_saying_mysql_drops_it()
 {
    // md-m.sql, both servers: with sql_mode = '' the statement fails with
    // 1101, it does not drop the default.
    let schema = "CREATE TABLE t (id INT PRIMARY KEY, c TEXT NULL, j JSON NULL) ENGINE=InnoDB;";
    for statement in [
        "ALTER TABLE t MODIFY c TEXT NULL DEFAULT 'x'",
        "ALTER TABLE t MODIFY j JSON NULL DEFAULT '[]'",
        "alter table `t` modify `c` text null default 'x'",
    ] {
        let sql = format!("SET sql_mode = '';\n{statement}");
        let answer = said(schema, &sql);
        assert_eq!(answer.answer, "unknown", "{statement}: {answer:?}");
        assert!(!answer.reason.is_empty(), "{statement}");
        assert!(
            !answer.reason.to_lowercase().contains("drops"),
            "{statement}: {}",
            answer.reason
        );
    }
}

#[test]
fn a_column_too_long_for_any_table_is_unknown_without_a_table_to_look_at() {
    // toolong.sql, both servers: VARCHAR(70000) and CHAR(300) fail with 1074
    // in strict mode, whatever the table is.
    for (label, schema) in tables_kilit_does_not_know() {
        for sql in [
            "ALTER TABLE orders ADD COLUMN c VARCHAR(70000) NULL",
            "ALTER TABLE orders ADD COLUMN d CHAR(300) NULL",
            "alter table `orders` add `c` varchar(70000) null",
            "alter table `orders` add `d` char(300) null",
        ] {
            for server in [MYSQL_80, MYSQL_84] {
                let answer = said_on(server, &schema, sql);
                assert_eq!(answer.answer, "unknown", "{label}: {server}: {sql}");
                assert!(!answer.reason.is_empty(), "{label}: {server}: {sql}");
            }
        }
    }
}

#[test]
fn the_longest_char_without_a_table_to_look_at_keeps_its_answer() {
    // Regression: CHAR(255) is the longest CHAR; it is the lab's add-column.
    for (label, schema) in tables_kilit_does_not_know() {
        let answer = said(&schema, "ALTER TABLE orders ADD COLUMN d CHAR(255) NULL");
        assert_eq!(answer.case, Some("add-column"), "{label}");
        assert_eq!(answer.answer, INSTANT, "{label}");
    }
}
