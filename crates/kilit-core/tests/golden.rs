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
                if exception.conditions.contains(&Condition::Hidden) {
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
        "FOREIGN KEY (REF) REFERENCES USERS (ID)",
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
            assert_eq!(notes_of(&predictions[1]).len(), 1, "{name} on {server}");
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
                // exceptions kilit could not look at.
                let predictions = predictions_on(server, schema, &sql);
                assert_eq!(
                    notes_of(&predictions[0]).len(),
                    7,
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
        FOREIGN KEY (`USER_NAME`) REFERENCES `Users` (`NAME`);";
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
    // A change kilit cannot follow loses the table: a column that is not
    // there.
    let lose = "ALTER TABLE orders DROP COLUMN no_such_column;";
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
            &format!("ALTER TABLE orders DROP COLUMN no_such_column;\n{set};\n{alter}"),
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
    let lose = "ALTER TABLE orders DROP COLUMN no_such_column;";
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
    let lose = "ALTER TABLE orders DROP COLUMN no_such_column;";
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
