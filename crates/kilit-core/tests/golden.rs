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
    Schema, Shared, Statement, TableState, Variant, Verdict, Writes, check, check_sessions,
    impacts, parse, read_pretend,
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
                // A name the table does not have is no note: dropping it is
                // unknown on a known table, and on an unknown one.
                if exception.conditions.contains(&Condition::NoSuchName) {
                    continue;
                }
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
                        Prediction::Unknown { reason, .. }
                            if reason.starts_with("MODIFY and CHANGE depend on") => {}
                        // Without the table the answer is
                        // unknown, or what every table shares; the exception
                        // the table decides is a note either way.
                        Prediction::Unknown { notes, .. }
                        | Prediction::DependsOnTable { notes, .. } => {
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
            Prediction::DependsOnTable { shared, .. } => match shared {
                Shared::WritesWait => "depends on the table: writes wait".to_owned(),
                Shared::Fails => "depends on the table: fails".to_owned(),
            },
        })
        .collect()
}

/// The tables of a small Laravel app, as `php artisan schema:dump` writes them.
const LARAVEL_TABLES: &str = include_str!("../examples/mysql-schema.sql");

// The facts below are what the README promises. If a new measurement
// changes one of them, the README has to change with it.

// These facts are about tables kilit knows, so they run
// against a small schema dump. Without the tables the answers are unknown.

/// `shops` and an `orders` with a text column, as `php artisan schema:dump`
/// writes them.
const README_TABLES: &str = "create table `shops` (`id` bigint unsigned not null auto_increment, \
    primary key (`id`)) engine=InnoDB default charset=utf8mb4 collate=utf8mb4_unicode_ci;
    create table `orders` (`id` bigint unsigned not null auto_increment, `note` text null, \
    primary key (`id`)) engine=InnoDB default charset=utf8mb4 collate=utf8mb4_unicode_ci;\n";

/// The same with a `shop_id` column on `orders`.
const README_TABLES_WITH_SHOP_ID: &str =
    "create table `shops` (`id` bigint unsigned not null auto_increment, \
    primary key (`id`)) engine=InnoDB default charset=utf8mb4 collate=utf8mb4_unicode_ci;
    create table `orders` (`id` bigint unsigned not null auto_increment, \
    `shop_id` bigint unsigned null, primary key (`id`), key `orders_shop_id_index` (`shop_id`)) \
    engine=InnoDB default charset=utf8mb4 collate=utf8mb4_unicode_ci;\n";

#[test]
fn a_constrained_foreign_id_copies_the_table() {
    // $table->foreignId('shop_id')->nullable()->constrained();
    assert_eq!(
        on_mysql_84(
            README_TABLES,
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
            README_TABLES_WITH_SHOP_ID,
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
            README_TABLES,
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
            README_TABLES,
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
            README_TABLES,
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
const FAILS_1830: &str = "fails with error 1830";

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
        Prediction::Measured { notes, .. } | Prediction::Unknown { notes, .. } => notes,
        other => panic!("expected a measured or an unknown answer, got {other:?}"),
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
    let lost =
        |table: &str| format!("{LAB_TABLES}ALTER TABLE {table} CONVERT TO CHARACTER SET utf8mb4;");
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
        // A key on a table kilit does not know is unknown,
        // for the child and for the parent alike.
        assert_eq!(on_both(schema, alter), ["unknown", "unknown"], "{name}");
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
fn with_foreign_key_checks_off_a_table_kilit_does_not_know_is_unknown() {
    // A key on a table kilit does not know is unknown
    // with the checks off too; the missing column, SET NULL on a NOT NULL
    // column and MyISAM are what the tables would decide.
    let lost =
        |table: &str| format!("{LAB_TABLES}ALTER TABLE {table} CONVERT TO CHARACTER SET utf8mb4;");
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
                matches!(&predictions[1], Prediction::Unknown { .. }),
                "{name} on {server}: {:?}",
                predictions[1]
            );
        }
        assert_eq!(on_both(&schema, sql), [UNKNOWN, UNKNOWN], "{name}");
    }
}

// The three states of a table, for the child and for the parent: a failure
// needs both Known; otherwise the answer is today's, with a note for every
// exception kilit could not look at.

#[test]
fn a_foreign_key_is_never_a_failure_when_a_table_is_missing_or_lost() {
    let lost =
        |table: &str| format!("{LAB_TABLES}ALTER TABLE {table} CONVERT TO CHARACTER SET utf8mb4;");
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
            // Unknown, never COPY and never a failure.
            assert_eq!(
                on_both(schema, &sql),
                ["unknown", "unknown"],
                "{shape}, {state}"
            );
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

// Regression tests: inputs kilit once answered wrongly.

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
fn on_delete_set_null_on_a_not_null_child_fails_with_1830_and_every_other_shape_stays_unknown() {
    // The lab measured ON DELETE SET NULL on a NOT NULL column that fits its
    // parent: MySQL refuses it with 1830. A mismatch, a missing parent index
    // and ON UPDATE SET NULL were not measured with it.
    let schema = "CREATE TABLE users (id BIGINT UNSIGNED AUTO_INCREMENT PRIMARY KEY, \
                  name VARCHAR(100) NOT NULL) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4;
        CREATE TABLE orders (id INT PRIMARY KEY, user_id BIGINT UNSIGNED NOT NULL, \
        amount INT NOT NULL, un VARCHAR(100) NOT NULL, ok BIGINT UNSIGNED NULL) \
        ENGINE=InnoDB DEFAULT CHARSET=utf8mb4;";
    for (action, fitting) in [
        ("ON DELETE SET NULL", FAILS_1830),
        ("ON UPDATE SET NULL", "unknown"),
    ] {
        for (name, column, parent, answer) in [
            ("a fitting key", "user_id", "id", fitting),
            ("a type mismatch", "amount", "id", "unknown"),
            ("no parent index", "un", "name", "unknown"),
        ] {
            let sql = format!(
                "ALTER TABLE orders ADD CONSTRAINT f1 FOREIGN KEY ({column}) \
                 REFERENCES users ({parent}) {action};"
            );
            assert_answer(&format!("{action}, {name}"), schema, &sql, answer);
        }
        // A nullable child column has nothing to refuse.
        let sql = format!(
            "ALTER TABLE orders ADD CONSTRAINT f1 FOREIGN KEY (ok) REFERENCES users (id) {action};"
        );
        assert_answer(&format!("{action}, nullable"), schema, &sql, COPY);
    }
    // Both actions on one key are not the measured shape.
    let sql = "ALTER TABLE orders ADD CONSTRAINT f1 FOREIGN KEY (user_id) REFERENCES users (id) \
               ON DELETE SET NULL ON UPDATE SET NULL;";
    assert_answer("both actions", schema, sql, "unknown");
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
                Prediction::Unknown { reason, .. } => {
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
                    [Prediction::Unknown { reason, .. }] => assert!(
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
                Prediction::Unknown { reason, .. } => {
                    assert!(reason.contains("MyISAM"), "{options}: {reason}")
                }
                other => panic!("{options} on {server}: {other:?}"),
            }
        }
    }
}

#[test]
fn a_table_copied_with_like_from_a_myisam_table_is_a_known_myisam_table() {
    // By hand on MySQL 8.0.46 and 8.4.13: CREATE TABLE b LIKE a copies the
    // engine of a (ENGINE=MyISAM stays MyISAM). kilit reads LIKE, so the copy
    // is a known MyISAM table made here: ADD COLUMN gets the MyISAM
    // answer the lab measured (COPY, myisam-add-nullable-column), on a table
    // created here, and a copy of an InnoDB table is an InnoDB table
    // (INSTANT).
    let sql = "CREATE TABLE orders_copy LIKE orders;
               ALTER TABLE orders_copy ADD COLUMN c INT NULL;";
    for server in [MYSQL_80, MYSQL_84] {
        let predictions = predictions_on(server, &myisam_lab_after(&[]), sql);
        match predictions.last().unwrap() {
            Prediction::Measured {
                verdict, evidence, ..
            } => {
                assert_eq!(verdict.to_string(), COPY, "{server}");
                assert!(
                    evidence.contains(&"myisam-add-nullable-column"),
                    "{evidence:?}"
                );
            }
            other => panic!("{server}: {other:?}"),
        }
    }
    // The control: a copy of an InnoDB table is an InnoDB table.
    assert_answer("InnoDB copy", LAB_TABLES, sql, INSTANT);
    // The copy is made here, so nothing else uses it yet.
    for server in [MYSQL_80, MYSQL_84] {
        assert_eq!(
            impacts_on(server, &myisam_lab_after(&[]), sql),
            [Impact::NotChecked, Impact::NewTable],
            "{server}"
        );
    }
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
fn a_lost_table_is_unknown_whatever_its_engine() {
    // A change kilit cannot follow loses the table. (B1b: dropping a column
    // the table does not have no longer does: the table stays known.)
    let lose = "ALTER TABLE orders CONVERT TO CHARACTER SET utf8mb4;";
    let then = "ALTER TABLE orders ADD COLUMN c INT NULL;";
    let myisam = all_on_both(&myisam_lab_after(&[]), &format!("{lose}\n{then}"));
    for answers in myisam {
        assert_eq!(answers.last().unwrap(), "unknown", "{answers:?}");
    }
    // A lost InnoDB table no longer keeps the answer of the
    // plain table.
    let innodb = all_on_both(&lab_after(&[]), &format!("{lose}\n{then}"));
    for answers in innodb {
        assert_eq!(answers.last().unwrap(), "unknown", "{answers:?}");
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
fn a_missing_table_is_unknown_whatever_its_engine_was() {
    // No InnoDB answer for a table kilit does not have.
    let sql = "ALTER TABLE orders ADD COLUMN c INT NULL;";
    assert_answer("nothing known", "", sql, "unknown");
    // A MyISAM table that was dropped is missing again.
    assert_answer(
        "dropped",
        "",
        "CREATE TABLE orders (id INT PRIMARY KEY) ENGINE=MyISAM;
         DROP TABLE orders;
         ALTER TABLE orders ADD COLUMN c INT NULL;",
        "unknown",
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
    // Any form of the setting, before a CREATE TABLE without ENGINE.
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

// Step 9a, regression tests: inputs kilit once answered wrongly. Each was run
// by hand on MySQL 8.0.46 and 8.4.13; the results below are what both did.

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
fn create_table_if_not_exists_like_over_a_myisam_table_leaves_the_table_myisam_so_the_alter_is_copy()
 {
    // p6: MySQL does nothing here (by hand on MySQL 8.0.46 and 8.4.13 the
    // statement is accepted with Note 1050), the table stays MyISAM. kilit
    // reads the statement and does nothing too, so the ADD COLUMN gets the
    // MyISAM answer the lab measured: COPY.
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
        COPY,
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
            Prediction::Unknown { reason, .. } => {
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
        Prediction::Unknown { reason, .. } => reason.clone(),
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
    let lost = format!("{LAB_TABLES}ALTER TABLE orders CONVERT TO CHARACTER SET utf8mb4;");
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
            !matches!(&added, Prediction::Unknown { reason, .. } if reason.contains(not_there)),
            "{server}: {added:?}"
        );
        for default in ["(UUID_SHORT())", "(SYSDATE())", "(USER())"] {
            let sql = format!(
                "ALTER TABLE orders ADD COLUMN ref BIGINT UNSIGNED NULL DEFAULT {default};\n{modify}"
            );
            match last(&sql) {
                Prediction::Unknown { reason, .. } => {
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

// Step 9c, regression tests: inputs kilit once answered wrongly.

#[test]
fn a_space_or_comment_between_a_function_name_and_its_parenthesis_is_unknown() {
    // By hand on MySQL 8.0.46 and 8.4.13: `(SYSDATE ())`,
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
    // By hand on MySQL 8.0.46 and 8.4.13: after SET SESSION
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
    // The hand tests: KEY_BLOCK_SIZE 1, 2, 4 and 16 (not
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
    // Without the option the MyISAM answer is COPY.
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
    // Only the measured operations get an answer, never a
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
    // On DYNAMIC a FULLTEXT index changes the answer; nothing
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
            &format!("ALTER TABLE orders CONVERT TO CHARACTER SET utf8mb4;\n{set};\n{alter}"),
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
    // A written ROW_FORMAT does not depend on the
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
    // Tables created before the setting keep their answers.
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
    // Without strict mode MySQL cuts a too long key instead of
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
    // Kept for a table kilit cannot see (step 11b):
    // the statement is read, but nothing says what the table was, so it
    // may have a row format other than DYNAMIC.
    let lost = format!("{LAB_TABLES}ALTER TABLE orders CONVERT TO CHARACTER SET utf8mb4;");
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
fn an_unreadable_alter_that_writes_neither_leaves_a_dynamic_table_unknown() {
    let lose = "ALTER TABLE orders CONVERT TO CHARACTER SET utf8mb4;";
    let then = "ALTER TABLE orders ADD COLUMN coupon VARCHAR(255) NULL;";
    // The table is lost, so the later statement is unknown.
    assert_answer("dynamic", LAB_TABLES, &format!("{lose}\n{then}"), UNKNOWN);
    assert_answer(
        "dynamic written",
        &lab_with_options(" ROW_FORMAT=DYNAMIC", &[]),
        &format!("{lose}\n{then}"),
        UNKNOWN,
    );
}

#[test]
fn a_known_compressed_compact_or_redundant_table_lost_by_any_unreadable_statement_is_unknown() {
    let lose = "ALTER TABLE orders CONVERT TO CHARACTER SET utf8mb4;";
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
    // The new-table rule uses the measurement for the table's
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
    // GLOBAL, PERSIST, PERSIST_ONLY, DEFAULT, a value that is
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
        // MyISAM, COMPRESSED, COMPACT and REDUNDANT.
        for (name, schema) in [
            ("myisam", lab_tables_with(Some("MyISAM"), "")),
            ("compressed", lab_with_options(COMPRESSED, &[])),
            ("key block size", lab_with_options(" KEY_BLOCK_SIZE=8", &[])),
            ("compact", lab_with_options(COMPACT_OPTION, &[])),
            ("redundant", lab_with_options(REDUNDANT_OPTION, &[])),
            // A FULLTEXT index changes the DYNAMIC answer.
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
            README_TABLES,
            "SET FOREIGN_KEY_CHECKS=0;
             alter table `orders` add `coupon` varchar(255) null;
             SET FOREIGN_KEY_CHECKS=1;"
        ),
        ["not checked", INSTANT, "not checked"]
    );
}

// Step 9b, regression tests: inputs kilit once answered wrongly. Each was run
// by hand on MySQL 8.0.46 and 8.4.13; the results below are what both did.

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
// The answers for COMMENT come from hand tests on MySQL 8.0.46 and 8.4.13 and
// from the scenario set-table-comment: as written,
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
        Prediction::Unknown { reason, .. } => reason.clone(),
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
    let lost = format!("ALTER TABLE orders CONVERT TO CHARACTER SET utf8mb4;\n{ADD_C}");
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
    // The lab measured only an InnoDB DYNAMIC table with
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
    let lost = format!("{LAB_TABLES}ALTER TABLE orders CONVERT TO CHARACTER SET utf8mb4;");
    let sql = format!("ALTER TABLE orders COMMENT = 'x';\n{ADD_C}");
    // A missing table stays missing, and a lost one lost:
    // nothing is answered for either, and the later statement keeps the
    // FULLTEXT note.
    assert_every_answer("missing", "", &sql, &[UNKNOWN, UNKNOWN]);
    assert_every_answer("lost", &lost, &sql, &[UNKNOWN, UNKNOWN]);
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
    let lost = format!("{LAB_TABLES}ALTER TABLE orders CONVERT TO CHARACTER SET utf8mb4;");
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
            &[UNKNOWN, UNKNOWN],
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
const UNREADABLE_OPTION_ALTERS: [(&str, bool); 15] = [
    ("ALTER TABLE orders COMMENT='x' ALGORITHM=INSTANT", true),
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

// foreign_key_checks = DEFAULT. By hand on MySQL 8.0.46 and 8.4.13: the
// session ends with the checks off on both (the inverse of the global value);
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

// Step 11b, regression tests: inputs kilit once answered wrongly. By hand on
// MySQL 8.0.46 and 8.4.13 (session only): MySQL 1628 for a
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

/// The table `t1` of those hand tests: utf8mb4 VARCHAR(255) columns, one of
/// them indexed.
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
        Prediction::DependsOnTable { shared, .. } => {
            format!("{impact:?}: depends on the table, {shared:?}")
        }
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
// 1067, 1294 or 1391 on each), and so was every twin that
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
            Some(Prediction::Unknown { reason, .. }) if reason.starts_with(NOT_ACCEPTED) => {}
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
            if let Some(Prediction::Unknown { reason, .. }) =
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
                matches!(&predictions[0], Prediction::Unknown { reason, .. } if reason.starts_with(NOT_ACCEPTED)),
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
                // tq is not known, so nothing runs on it.
                [Impact::Unknown, Impact::Unknown],
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

// Step 13a, regression tests: key parts that look like a function call but
// are a column name with a prefix length, or a column name that looks like a
// number. By hand on MySQL 8.0.46 and 8.4.13, with the same result on both.

/// The table of those hand tests: the names are words MySQL reads as a
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
            if let Some(Prediction::Unknown { reason, .. }) =
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
            Some(Prediction::Unknown { reason, .. }) if reason.contains("do not accept") => {
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
                Some(Prediction::Unknown { reason, .. }) if reason.starts_with(NOT_ACCEPTED) => {}
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
            Some(Prediction::Unknown { reason: found, .. }) => {
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
                Prediction::Unknown { reason, .. } if reason.starts_with(NOT_ACCEPTED) => {}
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
fn a_name_the_table_has_no_constraint_or_index_of_is_unknown_and_the_table_stays_known() {
    // Error 3821 / 3940 on both servers (hand tests). The
    // statement is unknown and the table stays known, so the ADD COLUMN after
    // it gets no note of a table kilit does not know.
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
            assert!(!mentions_fulltext(&predictions[1]), "{server}: {statement}");
        }
    }
}

#[test]
fn a_check_dropped_twice_is_unknown_the_second_time_and_the_table_stays_known() {
    // Hand test: DROP CHECK of a name the table no longer has is error 3821.
    let sql = "ALTER TABLE orders DROP CHECK chk_amount;
               ALTER TABLE orders DROP CHECK chk_amount;";
    assert_13b(S13B_SCHEMA, sql, &[INSTANT, "unknown"]);
    for server in [MYSQL_80, MYSQL_84] {
        let predictions = predictions_on(server, S13B_SCHEMA, &format!("{sql}\n{ADD_COUPON};"));
        // The table stays known.
        assert!(!mentions_fulltext(&predictions[2]), "{server}");
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
    // DROP CHECK of an unknown name no longer takes the table.
    let lost = "ALTER TABLE orders CONVERT TO CHARACTER SET utf8mb4;\n";
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
        let reason = assert_unknown_13b(
            S13B_SCHEMA,
            statement,
            "kilit does not know the database the statements run in",
        );
        assert!(!reason.contains("could not read"), "{statement}: {reason}");
    }
    assert_unknown_13b(
        S13B_SCHEMA,
        "ALTER TABLE shop.orders DROP CHECK chk_amount",
        "so it cannot tell whether shop.orders is orders",
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
            !mentions_fulltext(predictions_on(server, S13B_SCHEMA, &sql).last().unwrap()),
            "{server}: the table should stay known"
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
                Some(Prediction::Unknown { reason, .. })
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
    // Missing: unknown, never a failure, and the letter case
    // changes nothing.
    let schema = case_plain();
    for table in ["ghosts", "GHOSTS", "Ghosts", "orders_old", "ORDERS_OLD"] {
        let sql = format!("ALTER TABLE {table} ADD COLUMN x INT NULL");
        assert_eq!(case_last_answer(&schema, &sql), UNKNOWN, "{sql}");
    }
    // A database in front of a name makes another name, as before.
    assert_eq!(
        case_last_answer(
            &schema,
            "ALTER TABLE kilit_lab.orders ADD COLUMN x INT NULL"
        ),
        UNKNOWN
    );
    // A table that was dropped is Missing, in either case.
    for table in ["orders", "ORDERS"] {
        let sql = format!("DROP TABLE orders; ALTER TABLE {table} ADD COLUMN x INT NULL");
        assert_eq!(case_last_answer(&schema, &sql), UNKNOWN, "{sql}");
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
    // A parent that matches no table in any case: a parent kilit does not
    // know makes the key unknown.
    assert_eq!(
        case_last_answer(
            &schema,
            &format!("SET foreign_key_checks = 0;\n{}", add("ghosts"))
        ),
        UNKNOWN
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
        UNKNOWN
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
    // seen). UNKNOWN: the table is gone or no longer trusted, or the name is
    // ambiguous next to its twin.
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
        ("DROP TABLE ORDERS", &[("orders", UNKNOWN), ("users", COPY)]),
        (
            "RENAME TABLE ORDERS TO o2",
            &[("orders", UNKNOWN), ("o2", COPY), ("users", COPY)],
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
            &[("orders", UNKNOWN), ("users", COPY)],
        ),
        (
            "DROP TABLE ORDERS, users",
            &[("orders", UNKNOWN), ("users", UNKNOWN)],
        ),
        (
            "DROP TABLE users, ORDERS",
            &[("orders", UNKNOWN), ("users", UNKNOWN)],
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
    // EXISTS (C) loses the table instead, and the new column is unknown.
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
        UNKNOWN
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
        UNKNOWN
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
    // Where the two settings part ways the table is lost: no failure from it,
    // and no answer either.
    let sql = "DROP TABLE IF EXISTS ORDERS;\nALTER TABLE orders DROP COLUMN user_id";
    assert_eq!(case_last_answer(&case_plain(), sql), UNKNOWN);
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
    // Lost without a twin: the lost table is unknown.
    assert_eq!(
        case_trust_after("DROP TABLE IF EXISTS ORDERS", "orders"),
        UNKNOWN
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
        CREATE TABLE ORDERS (id INT PRIMARY KEY, note TEXT) DEFAULT CHARSET=utf8mb4;
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
    let sql = "CREATE TABLE posts (id INT PRIMARY KEY, body TEXT) DEFAULT CHARSET=utf8mb4;
        CREATE TABLE Posts (id INT PRIMARY KEY, body TEXT) DEFAULT CHARSET=utf8mb4;
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
fn a_create_table_like_in_another_letter_case_is_unknown_for_the_letter_case_and_keeps_the_known_table()
 {
    // CREATE TABLE ORDERS LIKE users: kilit reads LIKE, so the statement gets
    // the letter-case reason of any new name in another case (not the
    // unreadable-statement reason), ORDERS is lost (ambiguous next to
    // orders) and orders stays known.
    let schema = case_fulltext();
    let create = "CREATE TABLE ORDERS LIKE users";
    assert_eq!(case_last_answer(&schema, create), UNKNOWN);
    let reason = assert_case_unknown(&schema, create, "orders", "ORDERS");
    assert!(!reason.starts_with("kilit could not read"), "{reason}");
    // The same words as a plain CREATE TABLE ORDERS.
    assert!(reason.contains(CASE_NEW_NAME), "{reason}");
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
    assert_eq!(case_trust_after(sql, "orders"), UNKNOWN);
    assert_eq!(case_trust_after(sql, "users"), COPY);
    // The same statement on the exact name loses the table the same way.
    assert_eq!(
        case_trust_after("ALTER TABLE orders FROBNICATE x", "orders"),
        UNKNOWN
    );
}

#[test]
fn a_table_kilit_lost_still_matches_a_name_in_another_case() {
    // Lost, not Missing: the name in another case is unknown with the case
    // reason, and the exact name is unknown with the lost-table reason.
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
        UNKNOWN
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
    let create = "CREATE TABLE ORDERS (id INT PRIMARY KEY, note TEXT) DEFAULT CHARSET=utf8mb4;\n";
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
        // The database spelling is ambiguous, so the table is lost; B1c,
        // A lost table is unknown even when the input created it.
        [Impact::NotChecked, Impact::Unknown, Impact::Unknown]
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
        Prediction::Unknown { reason, .. } => reason.clone(),
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
    assert_eq!(case_answers(&schema, sql), [UNKNOWN, UNKNOWN, UNKNOWN]);
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
        ["not checked", UNKNOWN, UNKNOWN]
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
            UNKNOWN,
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
    let sql = "CREATE TABLE t1 (id INT PRIMARY KEY, note TEXT) DEFAULT CHARSET=utf8mb4;
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
// hand tests on MySQL 8.0.46 and 8.4.13 and from the decisions of the step.
// A test that names a lab scenario needs that scenario in matrix/.

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

/// The notes of an answer that is unknown: B1c puts the table-decided
/// exceptions of an answer on a table kilit does not know there.
fn unknown_notes(prediction: &Prediction) -> Vec<String> {
    match prediction {
        Prediction::Unknown { notes, .. } => notes.clone(), // new-api
        _ => Vec::new(),
    }
}

/// What `server` says about the last statement of `sql`.
fn said_on(server: &str, schema: &str, sql: &str) -> Said {
    let prediction = predictions_on(server, schema, sql).pop().unwrap();
    let unknown_notes = unknown_notes(&prediction);
    match prediction {
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
        Prediction::Unknown { reason, .. } => Said {
            case: None,
            answer: "unknown".into(),
            evidence: Vec::new(),
            notes: unknown_notes,
            reason,
        },
        Prediction::NotChecked { reason } => Said {
            case: None,
            answer: "not checked".into(),
            evidence: Vec::new(),
            notes: Vec::new(),
            reason,
        },
        Prediction::DependsOnTable {
            case,
            shared,
            evidence,
            reason,
            notes,
        } => Said {
            case: Some(case),
            answer: match shared {
                Shared::WritesWait => "depends on the table: writes wait".into(),
                Shared::Fails => "depends on the table: fails".into(),
            },
            evidence,
            notes,
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
    format!("{LAB_TABLES}\nALTER TABLE orders CONVERT TO CHARACTER SET utf8mb4;\n")
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
fn an_expression_default_on_a_text_column_is_unknown_without_a_table() {
    for (label, schema) in tables_kilit_does_not_know() {
        let answer = said(
            &schema,
            "ALTER TABLE orders ADD COLUMN c TEXT NOT NULL DEFAULT ('')",
        );
        // The lab's COPY is the likely answer, shown in the
        // reason; a table kilit does not know could change it.
        assert_eq!(answer.answer, UNKNOWN, "{label}");
        assert!(
            answer.reason.contains("add-column-blob-expression-default")
                && answer
                    .reason
                    .contains("on a plain InnoDB table the lab measured"),
            "{label}: {}",
            answer.reason
        );
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
    // By hand on MySQL 8.0.46 and 8.4.13 (set-column-default-json-literal
    // with sql_mode '' and a non-strict mode, kept out of the lab's
    // scenarios): 1101 on both. kilit stays cautious and does not bind them:
    // unknown (decision of the step).
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
fn setting_a_quoted_default_on_a_table_kilit_does_not_know_is_unknown_with_a_note_about_1101() {
    for (label, schema) in tables_kilit_does_not_know() {
        let answer = said(&schema, "ALTER TABLE orders ALTER COLUMN c SET DEFAULT ''");
        assert!(
            answer.reason.contains("set-column-default")
                && answer
                    .reason
                    .contains("on a plain InnoDB table the lab measured"),
            "{label}: {}",
            answer.reason
        );
        assert_eq!(answer.answer, UNKNOWN, "{label}");
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
fn dropping_a_column_of_a_table_kilit_does_not_know_is_unknown_with_a_note_about_the_primary_key() {
    // 0.1.0 said INSTANT with nothing about the primary key; MySQL copies the
    // table (drop-primary-key-column).
    for (label, schema) in tables_kilit_does_not_know() {
        let answer = said(&schema, "ALTER TABLE orders DROP COLUMN id");
        assert!(
            answer.reason.contains("drop-column")
                && answer
                    .reason
                    .contains("on a plain InnoDB table the lab measured"),
            "{label}: {}",
            answer.reason
        );
        assert_eq!(answer.answer, UNKNOWN, "{label}");
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
fn forcing_algorithm_instant_on_a_drop_without_a_known_table_is_unknown_and_notes_the_error_1846() {
    // drop-primary-key-column: ALGORITHM=INSTANT is refused with 1846.
    for (label, schema) in tables_kilit_does_not_know() {
        let answer = said(
            &schema,
            "ALTER TABLE orders DROP COLUMN id, ALGORITHM=INSTANT",
        );
        assert_eq!(answer.answer, UNKNOWN, "{label}");
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
        assert_eq!(answer.answer, UNKNOWN, "{label}");
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
fn an_index_on_a_table_kilit_does_not_know_is_unknown_and_notes_that_a_json_column_is_not_measured()
{
    // ALTER TABLE ... ADD INDEX over a JSON column is unknown on a known
    // table (see below); without the table the measured answer carries a note.
    for (label, schema) in tables_kilit_does_not_know() {
        for sql in [
            "ALTER TABLE orders ADD INDEX ix_note (note)",
            "ALTER TABLE orders ADD UNIQUE INDEX uq_email (email)",
            "alter table `orders` add index `orders_note_index`(`note`)",
        ] {
            let answer = said(&schema, sql);
            assert_eq!(answer.answer, UNKNOWN, "{label}: {sql}: {answer:?}");
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
fn a_check_on_a_table_kilit_does_not_know_is_unknown_and_notes_what_it_cannot_see() {
    // A taken name, a column that is not there, an AUTO_INCREMENT column, a
    // generated column, a foreign key column: the table would decide.
    for (label, schema) in tables_kilit_does_not_know() {
        for sql in [
            "ALTER TABLE orders ADD CONSTRAINT ck_amount CHECK (amount > 0)",
            "ALTER TABLE orders ADD CONSTRAINT ck_amount CHECK (amount > 0) NOT ENFORCED",
        ] {
            let answer = said(&schema, sql);
            assert_eq!(answer.answer, UNKNOWN, "{label}: {sql}: {answer:?}");
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
fn a_foreign_key_with_a_table_kilit_does_not_know_is_unknown_and_names_the_lab_answer() {
    // A missing column, NOT NULL with ON DELETE SET NULL, a MyISAM table: the
    // tables would decide. (The notes about the parent's index differ per
    // server, so each server is looked at on its own.)
    let child_lost = "CREATE TABLE users (id BIGINT UNSIGNED AUTO_INCREMENT PRIMARY KEY, name VARCHAR(100) NOT NULL);
         CREATE TABLE orders (id BIGINT UNSIGNED AUTO_INCREMENT PRIMARY KEY, user_id BIGINT UNSIGNED NOT NULL);
         ALTER TABLE orders CONVERT TO CHARACTER SET utf8mb4;";
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
            // Unknown, with the case in the reason. The notes
            // about what the tables would decide are checked in the CLI's JSON.
            assert_eq!(answer.answer, UNKNOWN, "{label} on {server}");
            assert!(
                answer.reason.contains("add-foreign-key"),
                "{label} on {server}: {}",
                answer.reason
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
fn a_foreign_key_that_sets_a_not_null_column_to_null_on_delete_fails_with_1830_on_known_tables() {
    let sql = "ALTER TABLE orders ADD CONSTRAINT fk FOREIGN KEY (user_id) REFERENCES users (id) ON DELETE SET NULL";
    let answer = said(LAB_TABLES, sql);
    assert_eq!(answer.answer, FAILS_1830);
    assert_eq!(
        answer.evidence,
        ["add-foreign-key-set-null-not-null"],
        "the lab's own scenario is the evidence"
    );
}

#[test]
fn a_default_set_on_a_table_kilit_does_not_know_is_unknown_and_notes_an_expression_default() {
    // If the old default is an expression, changing it was not measured.
    for (label, schema) in tables_kilit_does_not_know() {
        let answer = said(&schema, "ALTER TABLE orders ALTER COLUMN x SET DEFAULT 'v'");
        assert_eq!(answer.answer, UNKNOWN, "{label}");
        assert!(
            answer.reason.contains("set-column-default"),
            "{label}: {}",
            answer.reason
        );
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
fn a_new_column_on_a_table_kilit_does_not_know_is_unknown_and_notes_the_row_size_limit() {
    // B1: a real migration (snipe-it, settings) failed with 1118 where kilit
    // said INSTANT.
    for (label, schema) in tables_kilit_does_not_know() {
        let answer = said(
            &schema,
            "ALTER TABLE orders ADD COLUMN link_color VARCHAR(255) NULL",
        );
        assert_eq!(answer.answer, UNKNOWN, "{label}");
        assert!(
            answer.reason.contains("add-column"),
            "{label}: {}",
            answer.reason
        );
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
    // By hand on MySQL 8.0.46 and 8.4.13 (add-column-at-row-size-limit, not
    // among the lab's scenarios): VARCHAR(229) NULL (918 bytes) ends at
    // 65,525 bytes of columns plus 9 of NULL flags, 65,534 in all, and runs
    // INSTANT on both.
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
    // ends at 65,538. By hand on MySQL 8.0.46 and 8.4.13
    // (lengthen-varchar-at-row-size-limit, not among the lab's scenarios):
    // VARCHAR(229) ends at 65,534 and runs INPLACE with no rebuild on both.
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
fn dropping_what_a_table_kilit_does_not_know_may_have_is_unknown() {
    // Without the table kilit cannot tell that the column is not there.
    for (label, schema) in tables_kilit_does_not_know() {
        let answer = said(&schema, "ALTER TABLE orders DROP COLUMN nosuch");
        // The likely answer is in the reason, not the headline.
        assert_eq!(answer.answer, UNKNOWN, "{label}");
        assert!(
            answer.reason.contains("drop-column"),
            "{label}: {}",
            answer.reason
        );
    }
}

/// The lab's tables with 30 CHAR(255) latin1 NOT NULL columns more on
/// `orders` (a utf8mb4 table): 255 bytes each in the page, as the hand tests
/// add-column-at-page-row-size-limit and add-column-over-page-row-size-limit
/// on MySQL 8.0.46 and 8.4.13 set them up.
fn lab_with_30_latin1_columns() -> String {
    let columns: Vec<String> = (1..=30)
        .map(|n| format!("ADD COLUMN f{n} CHAR(255) CHARACTER SET latin1 NOT NULL DEFAULT ''"))
        .collect();
    lab_after(&[&format!("ALTER TABLE orders {}", columns.join(", "))])
}

#[test]
fn a_column_that_brings_a_row_to_the_most_innodb_keeps_in_a_page_is_instant() {
    // By hand on MySQL 8.0.46 and 8.4.13 (add-column-at-page-row-size-limit):
    // CHAR(194) latin1 NOT NULL is the last one that fits.
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
// scenarios of the row size limit), from hand tests on MySQL 8.0.46 and
// 8.4.13 (a foreign key written without a name is called orders_ibfk_1, an
// index without a name takes its first column's name, and both servers
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
    // the table and cannot call the second statement a failure; it is
    // unknown, like any statement on a table kilit lost.
    let sql = "ALTER TABLE orders DROP FOREIGN KEY orders_ibfk_1;
               ALTER TABLE orders DROP COLUMN user_id";
    let after = said(UNNAMED_FOREIGN_KEY_TABLES, sql);
    assert_ne!(after.answer, FAILS_1828, "{after:?}");
    assert_eq!(after.answer, "unknown", "{after:?}");
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
fn a_new_column_on_a_table_kilit_does_not_know_is_unknown_and_notes_the_record_in_a_page_too() {
    // By hand on MySQL 8.0.46 and 8.4.13 (add-column-over-page-row-size-limit):
    // 8.0.46 fails with 1118, 8.4.13 rebuilds the table.
    for (label, schema) in tables_kilit_does_not_know() {
        for sql in [
            "ALTER TABLE orders ADD COLUMN link_color VARCHAR(255) NULL",
            "alter table `orders` add `link_color` varchar(255) null",
        ] {
            let answer = said(&schema, sql);
            assert_eq!(answer.answer, UNKNOWN, "{label}: {sql}");
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
// The expectations come from hand tests on MySQL 8.0.46 and 8.4.13 (keys on a
// prefix, tables without a primary key, renamed foreign keys, MODIFY with a
// quoted default, columns too long for any table), the lab's add-column and
// drop-column scenarios, and the step's decisions.

/// `CREATE TABLE q (...)` as those hand tests built it: the columns of
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
    // By hand: 7,500 bytes of filler and CHAR(200) is past the 7,646 bytes
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
    // By hand: the unique index of NOT NULL columns keys the rows, so the
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
    // By hand on MySQL 8.0.46 and 8.4.13: MySQL renames
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
    // By hand on MySQL 8.0.46 and 8.4.13: MODIFY user_id INT NOT NULL after
    // the key is gone is accepted (no 3780). kilit kept the key under its old
    // name.
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
    // By hand on MySQL 8.0.46 and 8.4.13: after the rename the key is
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
    // By hand on MySQL 8.0.46 and 8.4.13: with sql_mode = '' the statement
    // fails with 1101, it does not drop the default.
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
    // By hand on MySQL 8.0.46 and 8.4.13: VARCHAR(70000) and CHAR(300) fail
    // with 1074 in strict mode, whatever the table is.
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
fn the_longest_char_without_a_table_to_look_at_is_unknown_with_the_lab_answer_in_the_reason() {
    // CHAR(255) is the longest CHAR; it is the lab's add-column. B1c,
    // The headline is unknown without the table.
    for (label, schema) in tables_kilit_does_not_know() {
        let answer = said(&schema, "ALTER TABLE orders ADD COLUMN d CHAR(255) NULL");
        assert_eq!(answer.answer, UNKNOWN, "{label}");
        assert!(
            answer.reason.contains("add-column"),
            "{label}: {}",
            answer.reason
        );
    }
}

// ---------------------------------------------------------------------------
// Step B1c: no measured answer on a table kilit does not know.
//
// The expectations come from the lab (the scenarios named in each test), from
// hand tests on MySQL 8.0.46 and 8.4.13 (each comment says "By hand on MySQL
// 8.0.46 and 8.4.13") and from the decisions of the step. A table kilit does
// not know is a Lost table, or a Missing one when no --schema was given; with
// a --schema, a table the dump does not have stays unknown as before.

/// A statement MySQL accepts on `orders` and kilit does not read: it loses the
/// table.
const B1C_LOSES_ORDERS: &str = "ALTER TABLE orders CONVERT TO CHARACTER SET utf8mb4;";

/// What every answer on a table kilit does not know says about the lab.
const B1C_LIKELY: &str = "on a plain InnoDB table the lab measured";

/// The prediction for the last statement of `sql`.
fn b1c_last(server: &str, schema: &Schema, sql: &str) -> Prediction {
    let results = Knowledge::built_in().server(server).unwrap();
    check(&parse(sql), schema, results)
        .pop()
        .unwrap()
        .prediction
}

/// The reason for the last statement of `sql`; the test fails if kilit has an
/// answer.
fn b1c_reason(server: &str, schema: &Schema, sql: &str) -> String {
    match b1c_last(server, schema, sql) {
        Prediction::Unknown { reason, .. } => reason,
        other => panic!("{server}: {sql}\n    expected unknown, got {other:?}"),
    }
}

/// What kilit says about the last statement of `sql`, in the words of the
/// other tests: `unknown`, `fails with error N`, or the verdict.
fn b1c_answer(server: &str, schema: &Schema, sql: &str) -> String {
    match b1c_last(server, schema, sql) {
        Prediction::Measured {
            verdict: Verdict::Fails { code, .. },
            ..
        } => format!("fails with error {code}"),
        Prediction::Measured { verdict, .. } => verdict.to_string(),
        Prediction::Unknown { .. } => "unknown".to_owned(),
        Prediction::NotChecked { .. } => "not checked".to_owned(),
        Prediction::DependsOnTable { shared, .. } => match shared {
            Shared::WritesWait => "depends on the table: writes wait".to_owned(),
            Shared::Fails => "depends on the table: fails".to_owned(),
        },
    }
}

/// The same on both servers, or the test fails.
fn b1c_both(schema: &Schema, sql: &str) -> String {
    let on_80 = b1c_answer(MYSQL_80, schema, sql);
    let on_84 = b1c_answer(MYSQL_84, schema, sql);
    assert_eq!(on_80, on_84, "{sql}");
    on_84
}

fn b1c_is_lost(schema: &Schema, table: &str) -> bool {
    matches!(schema.table(table), TableState::Lost { .. })
}

/// The lab's tables, with `orders` lost to a statement kilit cannot read: from
/// SQL (no --schema) and from a dump.
fn b1c_lost_schemas() -> [(&'static str, Schema); 2] {
    let text = format!("{LAB_TABLES}{B1C_LOSES_ORDERS}\n");
    let from_sql = Schema::from_sql(&text);
    let from_dump = Schema::from_dump(&text);
    assert!(b1c_is_lost(&from_sql, "orders"));
    assert!(b1c_is_lost(&from_dump, "orders"));
    [
        ("lost, no --schema", from_sql),
        ("lost in a dump", from_dump),
    ]
}

/// Every way kilit can be without `orders` and still answer with the plain
/// table's measurement today: lost, and missing with no --schema.
fn b1c_tables_kilit_does_not_know() -> Vec<(&'static str, Schema)> {
    let mut all = b1c_lost_schemas().into_iter().collect::<Vec<_>>();
    all.push(("no --schema", Schema::from_sql("")));
    all
}

#[derive(Clone, Copy, Debug)]
enum B1cState {
    Lost,
    LostInADump,
    WithoutASchema,
    NotInTheDump,
}

/// The lab's tables as a scenario measured them, as SQL text.
fn b1c_lab_text(scenario: &ScenarioResult) -> String {
    let mut sql = lab_tables_with(scenario.engine.as_deref(), &lab_options(scenario));
    for statement in &scenario.setup {
        sql.push_str(statement);
        sql.push_str(";\n");
    }
    sql
}

/// The table the scenario's statement alters.
fn b1c_table_of(scenario: &ScenarioResult) -> String {
    match &parse(&scenario.statement)[0].kind {
        kilit_core::StatementKind::Alter(alter) => alter.table.clone(),
        other => panic!("{}: expected an ALTER TABLE, got {other:?}", scenario.id),
    }
}

/// What kilit knows about the table the scenario alters.
fn b1c_schema(state: B1cState, scenario: &ScenarioResult) -> Schema {
    let table = b1c_table_of(scenario);
    let loses = format!("ALTER TABLE {table} CONVERT TO CHARACTER SET utf8mb4;\n");
    match state {
        B1cState::Lost => {
            let schema = Schema::from_sql(&format!("{}{loses}", b1c_lab_text(scenario)));
            assert!(b1c_is_lost(&schema, &table), "{}", scenario.id);
            schema
        }
        B1cState::LostInADump => {
            let schema = Schema::from_dump(&format!("{}{loses}", b1c_lab_text(scenario)));
            assert!(b1c_is_lost(&schema, &table), "{}", scenario.id);
            schema
        }
        B1cState::WithoutASchema => Schema::from_sql(""),
        B1cState::NotInTheDump => {
            Schema::from_dump("CREATE TABLE unrelated (id INT PRIMARY KEY) ENGINE=InnoDB;")
        }
    }
}

/// What one lab result does, in the words of the answer kilit gives for a
/// table it does not know.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum B1cWord {
    WritesContinue,
    WritesWait,
    Fails,
    /// The lab could not tell.
    Unclear,
}

fn b1c_word(verdict: Option<Verdict>) -> B1cWord {
    match verdict {
        Some(Verdict::Fails { .. }) => B1cWord::Fails,
        Some(Verdict::Runs {
            writes: Writes::Blocked,
            ..
        }) => B1cWord::WritesWait,
        Some(Verdict::Runs {
            writes: Writes::Allowed,
            ..
        }) => B1cWord::WritesContinue,
        _ => B1cWord::Unclear,
    }
}

/// The case with this id, whichever list it is in.
fn b1c_case(id: &str) -> Option<&'static kilit_core::Case> {
    every_case().into_iter().find(|case| case.id == id)
}

/// What the lab measured for `variant` on `server` in each situation a table
/// kilit does not know may be in, worked out from the matrix and the case's
/// name alone: the case on a plain table, each of its exceptions that a table
/// decides (not the hidden ones: rows and history), and the same change on a
/// MyISAM, COMPRESSED, COMPACT and REDUNDANT table (`myisam-<case>`, ...),
/// each with its own exceptions. `None` when one of those four was never
/// measured: such a change cannot be answered for an unknown table.
fn b1c_what_every_table_may_do(
    results: &kilit_core::ServerResults,
    case_id: &str,
    variant: Variant,
) -> Option<Vec<B1cWord>> {
    let mut words = Vec::new();
    let mut add_case = |case: &kilit_core::Case| {
        let first = case.scenarios[0];
        words.push(b1c_word(results.scenario(first)?.verdict_for(variant)));
        for exception in case.exceptions {
            if exception.conditions.contains(&Condition::Hidden) {
                continue;
            }
            let first = exception.scenarios[0];
            words.push(b1c_word(results.scenario(first)?.verdict_for(variant)));
        }
        Some(())
    };
    add_case(b1c_case(case_id)?)?;
    for prefix in ["myisam", "compressed", "compact", "redundant"] {
        add_case(b1c_case(&format!("{prefix}-{case_id}"))?)?;
    }
    Some(words)
}

/// For every scenario of every case and exception, on both
/// servers, in all six variants: the statement on a table kilit does not know
/// is unknown; or a failure that comes from the statement alone (1674, 1221,
/// and 1101 where the type is in the statement); or the answer every table it may
/// be shares. That last one is worked out again here from the matrix: the
/// case and each exception a table decides and the change on MyISAM,
/// COMPRESSED, COMPACT and REDUNDANT tables are all measured, and none lets
/// writes continue; it fails only when every one of them fails. Where the
/// table is not in a --schema dump it is unknown, whatever the failure.
fn b1c_assert_nothing_runs_on(state: B1cState) {
    let mut shared_answers = 0;
    for results in Knowledge::built_in().servers() {
        let server = results.server.name();
        for case in every_case() {
            let groups = std::iter::once(case.scenarios)
                .chain(case.exceptions.iter().map(|exception| exception.scenarios));
            for scenarios in groups {
                for id in scenarios {
                    let scenario = results.scenario(id).unwrap();
                    let schema = b1c_schema(state, scenario);
                    for variant in Variant::ALL {
                        let prediction = predict(scenario, variant, &server, &schema);
                        let fine = match &prediction {
                            Prediction::Unknown { .. } => true,
                            Prediction::Measured {
                                case: found,
                                verdict: Verdict::Fails { code, .. },
                                ..
                            } if !matches!(state, B1cState::NotInTheDump) => {
                                (*code == 1674 && *found == "add-column-binlog-unsafe-default")
                                    || (*code == 1221 && *found == "add-fulltext-index-with-order")
                                    || (*code == 1101
                                        && [
                                            "add-column-blob-literal-default",
                                            "modify-column-blob-literal-default",
                                        ]
                                        .contains(found))
                            }
                            Prediction::DependsOnTable {
                                case: found,
                                shared,
                                ..
                            } if !matches!(state, B1cState::NotInTheDump) => {
                                match b1c_what_every_table_may_do(results, found, variant) {
                                    Some(words) => {
                                        let all_fail =
                                            words.iter().all(|word| *word == B1cWord::Fails);
                                        let all_wait_or_fail = words.iter().all(|word| {
                                            matches!(word, B1cWord::Fails | B1cWord::WritesWait)
                                        });
                                        all_wait_or_fail
                                            && *shared
                                                == if all_fail {
                                                    Shared::Fails
                                                } else {
                                                    Shared::WritesWait
                                                }
                                    }
                                    None => false,
                                }
                            }
                            _ => false,
                        };
                        if matches!(prediction, Prediction::DependsOnTable { .. }) {
                            shared_answers += 1;
                        }
                        assert!(
                            fine,
                            "{id} as {variant} on {server}, {state:?}: {prediction:?}"
                        );
                    }
                }
            }
        }
    }
    // The check above is not empty: the FULLTEXT scenarios get that answer.
    assert_eq!(
        shared_answers > 0,
        !matches!(state, B1cState::NotInTheDump),
        "{state:?}"
    );
}

#[test]
fn a_table_kilit_lost_gets_only_unknown_a_failure_from_the_statement_or_what_every_table_shares() {
    b1c_assert_nothing_runs_on(B1cState::Lost);
}

#[test]
fn a_table_lost_in_a_dump_gets_only_unknown_a_failure_from_the_statement_or_what_every_table_shares()
 {
    b1c_assert_nothing_runs_on(B1cState::LostInADump);
}

#[test]
fn a_table_without_a_schema_gets_only_unknown_a_failure_from_the_statement_or_what_every_table_shares()
 {
    b1c_assert_nothing_runs_on(B1cState::WithoutASchema);
}

#[test]
fn a_table_missing_from_a_dump_is_unknown_for_every_scenario_whatever_the_failure() {
    // Regression: the dump decides, so the table is not there.
    b1c_assert_nothing_runs_on(B1cState::NotInTheDump);
}

#[test]
fn a_new_column_on_a_table_kilit_lost_is_unknown_and_shows_what_the_lab_measured() {
    // The loser comes first in the input, so the reason can name its line.
    let sql = format!("{B1C_LOSES_ORDERS}\nALTER TABLE orders ADD COLUMN c INT NULL;");
    for (how, schema) in [
        ("no --schema", Schema::from_sql(LAB_TABLES)),
        ("a dump", Schema::from_dump(LAB_TABLES)),
    ] {
        for server in [MYSQL_80, MYSQL_84] {
            let reason = b1c_reason(server, &schema, &sql);
            for part in [
                "orders",
                "kilit lost track of",
                "line 1",
                B1C_LIKELY,
                "add-column",
            ] {
                assert!(reason.contains(part), "{how}, {server}: {part}: {reason}");
            }
        }
    }
}

#[test]
fn a_new_column_without_a_schema_is_unknown_and_says_no_schema_was_given() {
    let schema = Schema::from_sql("");
    for server in [MYSQL_80, MYSQL_84] {
        let reason = b1c_reason(server, &schema, "ALTER TABLE orders ADD COLUMN c INT NULL;");
        for part in ["orders", "no --schema was given", B1C_LIKELY, "add-column"] {
            assert!(reason.contains(part), "{server}: {part}: {reason}");
        }
    }
}

#[test]
fn a_table_dropped_or_renamed_earlier_in_the_input_is_unknown_without_a_schema_and_the_reason_says_which()
 {
    // Dropped, then altered: MySQL refuses the second statement (1146). A
    // table created in the input and then dropped is not known either.
    let schema = Schema::from_sql(LAB_TABLES);
    for server in [MYSQL_80, MYSQL_84] {
        let dropped = b1c_reason(
            server,
            &schema,
            "DROP TABLE orders;\nALTER TABLE orders ADD COLUMN c INT NULL;",
        );
        for part in ["orders", "drops", "line 1", B1C_LIKELY, "add-column"] {
            assert!(dropped.contains(part), "{server}: {part}: {dropped}");
        }
        let renamed = b1c_reason(
            server,
            &schema,
            "RENAME TABLE orders TO purchases;\nALTER TABLE orders ADD COLUMN c INT NULL;",
        );
        for part in ["orders", "renames", "purchases", "line 1", B1C_LIKELY] {
            assert!(renamed.contains(part), "{server}: {part}: {renamed}");
        }
    }
}

#[test]
fn nothing_but_a_failure_from_the_statement_alone_fails_on_a_table_kilit_does_not_know() {
    // The failures that stay on such a table: 1674 (a default that is not safe
    // for the binary log) and 1101 where the TEXT, BLOB, JSON or GEOMETRY type
    // and its quoted default are both in the statement. The type is written in
    // MODIFY and CHANGE too.
    for (how, schema) in b1c_tables_kilit_does_not_know() {
        for column in BINLOG_UNSAFE_COLUMNS {
            assert_eq!(
                b1c_both(&schema, &format!("ALTER TABLE orders ADD COLUMN {column};")),
                "fails with error 1674",
                "{how}: {column}"
            );
        }
        for (column, _) in QUOTED_DEFAULT_SHAPES {
            assert_eq!(
                b1c_both(&schema, &format!("ALTER TABLE orders ADD COLUMN {column};")),
                FAILS_1101,
                "{how}: {column}"
            );
        }
        for statement in [
            "ALTER TABLE orders MODIFY note TEXT NULL DEFAULT ''",
            "ALTER TABLE orders MODIFY note TEXT NULL DEFAULT 'x'",
            "ALTER TABLE orders CHANGE note note TEXT NULL DEFAULT ''",
            "alter table `orders` modify `note` text not null default ''",
        ] {
            assert_eq!(
                b1c_both(&schema, statement),
                FAILS_1101,
                "{how}: {statement}"
            );
        }
    }
}

#[test]
fn renaming_a_column_or_setting_a_default_on_a_table_kilit_does_not_know_is_unknown() {
    // The lab's rename-column has exceptions (3108, 3959) and so has
    // set-column-default (1101): neither is an answer without the table.
    for (how, schema) in b1c_tables_kilit_does_not_know() {
        for statement in [
            "ALTER TABLE orders RENAME COLUMN note TO memo",
            "ALTER TABLE orders ALTER COLUMN amount SET DEFAULT 0",
            "ALTER TABLE orders ALTER COLUMN note SET DEFAULT 'x'",
            "alter table `orders` rename column `note` to `memo`",
        ] {
            assert_eq!(b1c_both(&schema, statement), UNKNOWN, "{how}: {statement}");
        }
    }
}

#[test]
fn a_forced_algorithm_or_lock_on_a_table_kilit_does_not_know_is_unknown() {
    // A table can change what a forced ALGORITHM does (ADD INDEX with
    // ALGORITHM=INSTANT is 1845, or 1170 on a TEXT column).
    for (how, schema) in b1c_tables_kilit_does_not_know() {
        for statement in [
            "ALTER TABLE orders ADD INDEX ix_amount (amount), ALGORITHM=INSTANT",
            "ALTER TABLE orders ADD INDEX ix_amount (amount), ALGORITHM=INPLACE, LOCK=NONE",
            "ALTER TABLE orders ADD COLUMN c INT NULL, ALGORITHM=INPLACE, LOCK=NONE",
            "ALTER TABLE orders ADD COLUMN c INT NULL, ALGORITHM=COPY",
            "ALTER TABLE orders DROP COLUMN note, ALGORITHM=COPY",
            "ALTER TABLE orders DROP COLUMN note, LOCK=NONE",
        ] {
            assert_eq!(b1c_both(&schema, statement), UNKNOWN, "{how}: {statement}");
        }
    }
}

#[test]
fn a_table_created_earlier_in_the_input_stays_known() {
    // Regression: nothing about a new table is lost, so its answers are the
    // lab's, and a FULLTEXT index it was created with still makes a new
    // column a copy.
    let schema = Schema::from_sql("");
    let create = "CREATE TABLE tags (id BIGINT UNSIGNED AUTO_INCREMENT PRIMARY KEY, \
                  body TEXT NULL, FULLTEXT KEY ft_body (body)) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4;\n";
    assert_eq!(
        b1c_both(
            &schema,
            &format!("{create}ALTER TABLE tags ADD COLUMN c INT NULL;")
        ),
        COPY
    );
    let plain = "CREATE TABLE tags (id BIGINT UNSIGNED AUTO_INCREMENT PRIMARY KEY, \
                 body TEXT NULL) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4;\n";
    assert_eq!(
        b1c_both(
            &schema,
            &format!("{plain}ALTER TABLE tags ADD COLUMN c INT NULL;")
        ),
        INSTANT
    );
}

#[test]
fn a_table_missing_from_a_dump_stays_unknown_whatever_the_statement() {
    // Regression: with a --schema the dump decides; the table is not
    // there, so even a failure from the statement alone is unknown.
    let dump = Schema::from_dump(LAB_TABLES);
    for server in [MYSQL_80, MYSQL_84] {
        for statement in [
            "ALTER TABLE coupons ADD COLUMN c INT NULL",
            "ALTER TABLE coupons ADD COLUMN token CHAR(36) NOT NULL DEFAULT (UUID())",
            "ALTER TABLE coupons ADD COLUMN c TEXT NOT NULL DEFAULT ''",
        ] {
            let reason = b1c_reason(server, &dump, statement);
            assert!(reason.contains("the dump may be out of date"), "{reason}");
        }
    }
}

#[test]
fn a_qualified_table_name_whose_table_part_kilit_has_is_unknown_and_names_both_spellings() {
    // With a dump, without one, and for a table the input
    // creates. kilit does not know which database `shop` is.
    let created =
        "CREATE TABLE orders (id BIGINT UNSIGNED AUTO_INCREMENT PRIMARY KEY) ENGINE=InnoDB;\n";
    for (how, schema, input) in [
        ("a dump", Schema::from_dump(LAB_TABLES), String::new()),
        ("no --schema", Schema::from_sql(LAB_TABLES), String::new()),
        ("created here", Schema::from_sql(""), created.to_owned()),
    ] {
        for server in [MYSQL_80, MYSQL_84] {
            let sql = format!("{input}ALTER TABLE shop.orders ADD COLUMN c INT NULL;");
            let reason = b1c_reason(server, &schema, &sql);
            assert!(reason.contains("shop.orders"), "{how}, {server}: {reason}");
            assert!(
                reason.matches("orders").count() >= 2,
                "{how}, {server}: {reason}"
            );
            assert!(
                !reason.contains("which database"),
                "{how}, {server}: {reason}"
            );
        }
    }
}

#[test]
fn a_qualified_name_whose_table_part_kilit_does_not_have_is_unknown_as_any_unseen_table() {
    // Regression: `shop.coupons` next to no `coupons`.
    for (how, schema) in [
        ("no --schema", Schema::from_sql(LAB_TABLES)),
        ("a dump", Schema::from_dump(LAB_TABLES)),
    ] {
        assert_eq!(
            b1c_both(&schema, "ALTER TABLE shop.coupons ADD COLUMN c INT NULL;"),
            UNKNOWN,
            "{how}"
        );
    }
}

/// Every form of DROP CHECK, DROP CONSTRAINT, ALTER CHECK and ALTER
/// CONSTRAINT of a name no table has, with the error MySQL gave by hand on
/// 8.0.46 and 8.4.13 and the lab scenario that measured it.
const B1C_MISSING_CHECKS: [(&str, &str, &str); 7] = [
    (
        "ALTER TABLE orders DROP CHECK nosuch",
        "3821",
        "drop-check-missing",
    ),
    (
        "ALTER TABLE orders ALTER CHECK nosuch NOT ENFORCED",
        "3821",
        "alter-check-missing",
    ),
    (
        "ALTER TABLE orders ALTER CHECK nosuch ENFORCED",
        "3821",
        "alter-check-missing",
    ),
    (
        "ALTER TABLE orders DROP CONSTRAINT nosuch",
        "3940",
        "drop-constraint-missing",
    ),
    (
        "ALTER TABLE orders ALTER CONSTRAINT nosuch NOT ENFORCED",
        "3940",
        "alter-constraint-missing",
    ),
    (
        "ALTER TABLE orders ALTER CONSTRAINT nosuch ENFORCED",
        "3940",
        "alter-constraint-missing",
    ),
    (
        "alter table `orders` drop check `nosuch`",
        "3821",
        "drop-check-missing",
    ),
];

#[test]
fn dropping_or_altering_a_check_the_known_table_does_not_have_is_unknown_with_its_error() {
    // By hand on MySQL 8.0.46 and 8.4.13, with no CHECK and next to a CHECK of
    // another name, as written and with every ALGORITHM and LOCK: DROP CHECK
    // and ALTER CHECK fail with 3821, DROP CONSTRAINT and ALTER CONSTRAINT
    // with 3940.
    let with_check =
        lab_after(&["ALTER TABLE orders ADD CONSTRAINT chk_amount CHECK (amount > 0)"]);
    for schema in [LAB_TABLES.to_owned(), with_check] {
        for (statement, error, scenario) in B1C_MISSING_CHECKS {
            for variant in Variant::ALL {
                let sql = format!("{statement}{}", variant.clause());
                for server in [MYSQL_80, MYSQL_84] {
                    let reason = b1c_reason(server, &Schema::from_sql(&schema), &sql);
                    assert!(reason.contains(error), "{server}: {sql}: {reason}");
                    assert!(reason.contains(scenario), "{server}: {sql}: {reason}");
                }
            }
        }
    }
}

#[test]
fn the_table_stays_known_after_a_check_it_does_not_have_is_dropped_or_altered() {
    // A FULLTEXT index makes a new column a copy (add-column-with-fulltext):
    // the table is still known, so the next statement gets that answer, not a
    // lost table's.
    let schema = Schema::from_sql(&lab_after(&[
        "ALTER TABLE orders ADD COLUMN body TEXT NULL",
        "ALTER TABLE orders ADD FULLTEXT INDEX ft_body (body)",
    ]));
    for (statement, _, _) in B1C_MISSING_CHECKS {
        let sql = format!("{statement};\nALTER TABLE orders ADD COLUMN c INT NULL;");
        for server in [MYSQL_80, MYSQL_84] {
            assert_eq!(b1c_answer(server, &schema, &sql), COPY, "{server}: {sql}");
        }
    }
}

#[test]
fn dropping_a_check_on_a_table_kilit_does_not_know_is_unknown() {
    // The table is not known, so neither is the name.
    for (how, schema) in b1c_tables_kilit_does_not_know() {
        for (statement, _, _) in B1C_MISSING_CHECKS {
            assert_eq!(b1c_both(&schema, statement), UNKNOWN, "{how}: {statement}");
        }
    }
}

/// An `orders` with an automatic default, a plain index and a FULLTEXT index,
/// as mysqldump writes it. A new column on it is a copy.
const B1C_FULLTEXT_ORDERS: &str = "CREATE TABLE `orders` (
  `id` bigint unsigned NOT NULL AUTO_INCREMENT,
  `user_id` bigint unsigned NOT NULL,
  `amount` int NOT NULL DEFAULT '0',
  `email` varchar(255) DEFAULT NULL,
  `note` text,
  PRIMARY KEY (`id`),
  KEY `ix_amount` (`amount`),
  FULLTEXT KEY `ft_note` (`note`)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_0900_ai_ci;
";

#[test]
fn the_five_statements_kilit_follows_are_unknown_themselves_and_keep_their_table_known() {
    // By hand on MySQL 8.0.46 and 8.4.13 (SHOW CREATE TABLE before
    // and after): none of them changes anything kilit models, or only what
    // the effect says. The next ADD COLUMN gets the FULLTEXT table's answer.
    let schema = Schema::from_dump(B1C_FULLTEXT_ORDERS);
    let mut problems = Vec::new();
    for statement in [
        "ALTER TABLE orders AUTO_INCREMENT = 100",
        "ALTER TABLE orders AUTO_INCREMENT=5000",
        "ALTER TABLE orders ALTER COLUMN amount DROP DEFAULT",
        "ALTER TABLE orders ALTER COLUMN email DROP DEFAULT",
        "ALTER TABLE orders RENAME INDEX ix_amount TO ix_total",
        "ALTER TABLE orders RENAME KEY ix_amount TO ix_total",
        "ALTER TABLE orders RENAME INDEX ft_note TO ft_text",
        "CREATE FULLTEXT INDEX ft_email ON orders (email)",
        "ALTER TABLE orders ENGINE = 'InnoDB'",
        "ALTER TABLE orders ENGINE = `InnoDB`",
        "ALTER TABLE orders ENGINE=innodb",
        "alter table `orders` auto_increment = 100",
        "alter table `orders` alter column `amount` drop default",
    ] {
        let sql = format!("{statement};\nALTER TABLE orders ADD COLUMN c INT NULL;");
        for server in [MYSQL_80, MYSQL_84] {
            let answers: Vec<String> = check(
                &parse(&sql),
                &schema,
                Knowledge::built_in().server(server).unwrap(),
            )
            .into_iter()
            .map(|finding| match finding.prediction {
                Prediction::Unknown { .. } => "unknown".to_owned(),
                Prediction::Measured { verdict, .. } => verdict.to_string(),
                other => format!("{other:?}"),
            })
            .collect();
            if answers != [UNKNOWN, COPY] {
                problems.push(format!("{server}: {statement}\n    got {answers:?}"));
            }
        }
    }
    assert!(problems.is_empty(), "\n{}", problems.join("\n"));
}

#[test]
fn dropping_a_default_leaves_the_column_without_one_and_the_others_alone() {
    // By hand on MySQL 8.0.46 and 8.4.13: ALTER COLUMN c DROP DEFAULT leaves
    // the column with no default (a column without one is accepted too).
    let schema = Schema::from_sql(&format!(
        "{B1C_FULLTEXT_ORDERS}ALTER TABLE orders ALTER COLUMN amount DROP DEFAULT;
         ALTER TABLE orders ALTER COLUMN email DROP DEFAULT;"
    ));
    let TableState::Known(table) = schema.table("orders") else {
        panic!("orders was lost");
    };
    assert_eq!(table.column("amount").unwrap().default, None);
    assert_eq!(table.column("email").unwrap().default, None);
    assert_eq!(table.columns.len(), 5);
    assert_eq!(table.indexes.len(), 2);
}

#[test]
fn renaming_an_index_changes_its_name_and_a_refusal_changes_nothing() {
    // By hand on MySQL 8.0.46 and 8.4.13: RENAME INDEX a TO b renames; a
    // missing index is 1176, a taken name 1061 (letter case does not matter),
    // PRIMARY 1280; renaming to the same name or to another case is accepted.
    let base = lab_after(&[
        "ALTER TABLE orders ADD INDEX ix_amount (amount)",
        "ALTER TABLE orders ADD UNIQUE INDEX uq_email (email)",
    ]);
    let schema = Schema::from_sql(&base);
    let drop_old = "ALTER TABLE orders DROP INDEX ix_amount";
    let drop_new = "ALTER TABLE orders DROP INDEX ix_total";
    for server in [MYSQL_80, MYSQL_84] {
        // Control: the index has its name; dropping it is the lab's answer.
        assert_eq!(b1c_answer(server, &schema, drop_old), INPLACE, "{server}");
        for rename in [
            "ALTER TABLE orders RENAME INDEX ix_amount TO ix_total",
            "ALTER TABLE orders RENAME KEY ix_amount TO ix_total",
            "ALTER TABLE orders RENAME INDEX IX_AMOUNT TO ix_total",
            "ALTER TABLE orders RENAME INDEX ix_amount TO ix_total, ALGORITHM=INPLACE",
        ] {
            let sql = format!("{rename};\n{drop_new}");
            assert_eq!(
                b1c_answer(server, &schema, &sql),
                INPLACE,
                "{server}: {sql}"
            );
            // The old name is gone: dropping it is the 1091 unknown.
            let sql = format!("{rename};\n{drop_old}");
            assert_eq!(
                b1c_answer(server, &schema, &sql),
                UNKNOWN,
                "{server}: {sql}"
            );
        }
        for (rename, error) in [
            ("ALTER TABLE orders RENAME INDEX nosuch TO ix_total", "1176"),
            ("ALTER TABLE orders RENAME KEY nosuch TO ix_total", "1176"),
            (
                "ALTER TABLE orders RENAME INDEX ix_amount TO uq_email",
                "1061",
            ),
            (
                "ALTER TABLE orders RENAME INDEX ix_amount TO UQ_EMAIL",
                "1061",
            ),
            (
                "ALTER TABLE orders RENAME INDEX ix_amount TO `PRIMARY`",
                "1280",
            ),
            (
                "ALTER TABLE orders RENAME INDEX `PRIMARY` TO ix_total",
                "1280",
            ),
        ] {
            let reason = b1c_reason(server, &schema, rename);
            assert!(reason.contains(error), "{server}: {rename}: {reason}");
            // The table is as it was: the index keeps its name.
            let sql = format!("{rename};\n{drop_old}");
            assert_eq!(
                b1c_answer(server, &schema, &sql),
                INPLACE,
                "{server}: {sql}"
            );
            let sql = format!("{rename};\n{drop_new}");
            assert_eq!(
                b1c_answer(server, &schema, &sql),
                UNKNOWN,
                "{server}: {sql}"
            );
        }
        for rename in [
            "ALTER TABLE orders RENAME INDEX ix_amount TO ix_amount",
            "ALTER TABLE orders RENAME INDEX ix_amount TO IX_AMOUNT",
        ] {
            let sql = format!("{rename};\n{drop_old}");
            assert_eq!(
                b1c_answer(server, &schema, &sql),
                INPLACE,
                "{server}: {sql}"
            );
        }
    }
}

#[test]
fn a_fulltext_index_made_with_create_index_is_the_same_index_as_with_alter_table() {
    // By hand on MySQL 8.0.46 and 8.4.13: CREATE FULLTEXT INDEX gives the same
    // SHOW CREATE TABLE as ALTER TABLE ... ADD FULLTEXT. A new column on the
    // table is then a copy, whichever statement made the index.
    let then = "ALTER TABLE orders ADD COLUMN c INT NULL;";
    for create in [
        "CREATE FULLTEXT INDEX ft_note ON orders (note)",
        "create fulltext index `ft_note` on `orders` (`note`)",
        "ALTER TABLE orders ADD FULLTEXT INDEX ft_note (note)",
    ] {
        let schema = Schema::from_sql(LAB_TABLES);
        let sql = format!("{create};\n{then}");
        for server in [MYSQL_80, MYSQL_84] {
            assert_eq!(
                b1c_answer(server, &schema, &sql),
                COPY,
                "{server}: {create}"
            );
        }
        // The same index is a second FULLTEXT index for the next one.
        let more = "CREATE FULLTEXT INDEX ft_note2 ON orders (note)";
        let sql = format!("{create};\n{more}");
        assert_eq!(
            b1c_both(&schema, &sql),
            b1c_both(
                &schema,
                &format!("ALTER TABLE orders ADD FULLTEXT INDEX ft_note (note);\n{more}")
            ),
            "{create}"
        );
    }
}

#[test]
fn a_create_index_that_mysql_refuses_changes_no_table() {
    // By hand on MySQL 8.0.46 and 8.4.13: a taken name is 1061, a column the
    // table lacks 1072, a table that is not there 1146.
    let schema = Schema::from_sql(&lab_after(&["ALTER TABLE orders ADD INDEX ix (amount)"]));
    for create in [
        "CREATE INDEX ix ON orders (email)",
        "CREATE INDEX IX ON orders (email)",
        "CREATE UNIQUE INDEX ix ON orders (email)",
        "CREATE FULLTEXT INDEX ix ON orders (note)",
        "CREATE INDEX iy ON orders (nosuch)",
        "CREATE FULLTEXT INDEX fy ON orders (nosuch)",
    ] {
        for server in [MYSQL_80, MYSQL_84] {
            assert_eq!(
                b1c_answer(server, &schema, create),
                UNKNOWN,
                "{server}: {create}"
            );
            // The table is as it was: the one index `ix` still drops with the
            // lab's answer, and a new column is still the plain table's.
            let sql = format!("{create};\nALTER TABLE orders DROP INDEX ix");
            assert_eq!(
                b1c_answer(server, &schema, &sql),
                INPLACE,
                "{server}: {sql}"
            );
        }
    }
}

#[test]
fn dropping_a_default_of_a_column_the_table_lacks_is_unknown_and_changes_no_table() {
    // By hand on MySQL 8.0.46 and 8.4.13: 1054.
    let schema = Schema::from_dump(B1C_FULLTEXT_ORDERS);
    let sql = "ALTER TABLE orders ALTER COLUMN nosuch DROP DEFAULT;
               ALTER TABLE orders ADD COLUMN c INT NULL;";
    for server in [MYSQL_80, MYSQL_84] {
        let answers = b1c_answer(server, &schema, sql);
        assert_eq!(answers, COPY, "{server}");
        let reason = b1c_reason(
            server,
            &schema,
            "ALTER TABLE orders ALTER COLUMN nosuch DROP DEFAULT",
        );
        assert!(reason.contains("1054"), "{server}: {reason}");
    }
}

#[test]
fn a_varchar_or_varbinary_past_what_a_row_holds_changes_no_table_after_a_strict_set_in_the_input() {
    // By hand on MySQL 8.0.46 and 8.4.13: strict mode refuses VARCHAR(70000)
    // and VARBINARY(70000) in ADD and MODIFY (1074), so the table is as it
    // was and a later column is INSTANT, not 1118. The input has to say it
    // is strict: kilit does not guess the server's sql_mode.
    let schema = Schema::from_sql(LAB_TABLES);
    let then = "ALTER TABLE orders ADD COLUMN c INT NULL;";
    for strict in [
        "SET sql_mode = 'STRICT_TRANS_TABLES';\n",
        "SET sql_mode = 'STRICT_ALL_TABLES';\n",
        "SET SESSION sql_mode = 'NO_ENGINE_SUBSTITUTION,STRICT_TRANS_TABLES';\n",
    ] {
        for statement in [
            "ALTER TABLE orders ADD COLUMN big VARCHAR(70000) NULL",
            "ALTER TABLE orders ADD COLUMN big VARBINARY(70000) NULL",
            "ALTER TABLE orders ADD COLUMN big VARCHAR(16384) CHARACTER SET utf8mb4 NULL",
            "ALTER TABLE orders MODIFY note VARCHAR(70000) NULL",
            "alter table `orders` add `big` varchar(70000) null",
        ] {
            for server in [MYSQL_80, MYSQL_84] {
                let answers = [
                    b1c_answer(server, &schema, &format!("{strict}{statement}")),
                    b1c_answer(server, &schema, &format!("{strict}{statement};\n{then}")),
                ];
                assert_eq!(answers[0], UNKNOWN, "{server}: {strict}{statement}");
                assert_eq!(answers[1], INSTANT, "{server}: {strict}{statement}");
            }
        }
    }
    // The column was never added: dropping it is the 1091 unknown, where a
    // table that took the column would say INSTANT.
    for server in [MYSQL_80, MYSQL_84] {
        let sql = "SET sql_mode = 'STRICT_TRANS_TABLES';
                   ALTER TABLE orders ADD COLUMN big VARCHAR(70000) NULL;
                   ALTER TABLE orders DROP COLUMN big;";
        assert_eq!(b1c_answer(server, &schema, sql), UNKNOWN, "{server}");
    }
}

#[test]
fn a_varchar_or_varbinary_past_what_a_row_holds_loses_the_table_when_the_input_sets_no_strict_mode()
{
    // Without a SET in the input the server's sql_mode is not known: in a
    // non-strict mode the column becomes MEDIUMTEXT or MEDIUMBLOB, so kilit
    // stops trusting the table.
    let schema = Schema::from_sql(LAB_TABLES);
    for statement in [
        "ALTER TABLE orders ADD COLUMN big VARCHAR(70000) NULL",
        "ALTER TABLE orders ADD COLUMN big VARBINARY(70000) NULL",
        "ALTER TABLE orders MODIFY note VARCHAR(70000) NULL",
        "alter table `orders` add `big` varchar(70000) null",
    ] {
        for server in [MYSQL_80, MYSQL_84] {
            let sql = format!("{statement};\nALTER TABLE orders ADD COLUMN c INT NULL;");
            let reason = b1c_reason(server, &schema, &sql);
            assert!(
                reason.contains("orders") && reason.contains("kilit lost track of"),
                "{server}: {statement}: {reason}"
            );
        }
    }
}

#[test]
fn a_strict_set_in_an_earlier_session_does_not_make_a_later_session_strict() {
    // Each input file is a session of its own: the SET in the first does
    // not count for the second.
    let schema = Schema::from_sql(LAB_TABLES);
    let first = parse("SET sql_mode = 'STRICT_TRANS_TABLES';");
    let second = parse(
        "ALTER TABLE orders ADD COLUMN big VARCHAR(70000) NULL;
         ALTER TABLE orders ADD COLUMN c INT NULL;",
    );
    for server in [MYSQL_80, MYSQL_84] {
        let results = Knowledge::built_in().server(server).unwrap();
        let findings = check_sessions(&[&first, &second], &schema, results);
        match &findings.last().unwrap().prediction {
            Prediction::Unknown { reason, .. } => assert!(
                reason.contains("orders") && reason.contains("kilit lost track of"),
                "{server}: {reason}"
            ),
            other => panic!("{server}: expected unknown, got {other:?}"),
        }
    }
}

#[test]
fn a_varchar_past_what_a_row_holds_loses_the_table_after_a_sql_mode_that_is_not_strict() {
    // By hand on MySQL 8.0.46 and 8.4.13: with sql_mode = '' (or
    // NO_ENGINE_SUBSTITUTION) VARCHAR(70000) becomes MEDIUMTEXT and
    // VARBINARY(70000) MEDIUMBLOB, so kilit cannot follow the column.
    let schema = Schema::from_sql(LAB_TABLES);
    for set in [
        "SET sql_mode = ''",
        "SET sql_mode = 'NO_ENGINE_SUBSTITUTION'",
        "SET SESSION sql_mode = ''",
    ] {
        for statement in [
            "ALTER TABLE orders ADD COLUMN big VARCHAR(70000) NULL",
            "ALTER TABLE orders ADD COLUMN big VARBINARY(70000) NULL",
            "ALTER TABLE orders MODIFY note VARCHAR(70000) NULL",
        ] {
            let sql = format!("{set};\n{statement};\nALTER TABLE orders ADD COLUMN c INT NULL;");
            for server in [MYSQL_80, MYSQL_84] {
                let reason = b1c_reason(server, &schema, &sql);
                assert!(
                    reason.contains("orders") && reason.contains("kilit lost track of"),
                    "{server}: {set}; {statement}: {reason}"
                );
            }
        }
    }
}

#[test]
fn a_char_or_binary_past_255_changes_no_table_in_any_sql_mode() {
    // By hand on MySQL 8.0.46 and 8.4.13: CHAR(300) and BINARY(300) are 1074
    // with every sql_mode. The answer to the later column is the same as
    // without the statement.
    let schema = Schema::from_sql(LAB_TABLES);
    let then = "ALTER TABLE orders ADD COLUMN c INT NULL;";
    for set in [
        "",
        "SET sql_mode = '';\n",
        "SET sql_mode = 'NO_ENGINE_SUBSTITUTION';\n",
    ] {
        for server in [MYSQL_80, MYSQL_84] {
            let alone = b1c_answer(server, &schema, &format!("{set}{then}"));
            assert_ne!(alone, UNKNOWN, "{server}: {set}");
            for statement in [
                "ALTER TABLE orders ADD COLUMN d CHAR(300) NULL",
                "ALTER TABLE orders ADD COLUMN d BINARY(300) NULL",
                "ALTER TABLE orders MODIFY note CHAR(300) NULL",
            ] {
                let sql = format!("{set}{statement};\n{then}");
                assert_eq!(
                    b1c_answer(server, &schema, &sql),
                    alone,
                    "{server}: {set}{statement}"
                );
            }
            // The column was never added: dropping it is the 1091 unknown.
            let sql = format!(
                "{set}ALTER TABLE orders ADD COLUMN d CHAR(300) NULL;
                 ALTER TABLE orders DROP COLUMN d;"
            );
            assert_eq!(
                b1c_answer(server, &schema, &sql),
                UNKNOWN,
                "{server}: {set}"
            );
        }
    }
}

#[test]
fn a_modify_that_strict_mode_refuses_leaves_the_column_as_it_was() {
    // By hand on MySQL 8.0.46 and 8.4.13: MODIFY note VARCHAR(70000) is 1074
    // in strict mode, so note stays VARCHAR(50) and a later change from 50 to
    // 60 is the lab's INPLACE, as without the statement.
    let schema = Schema::from_sql(LAB_TABLES);
    let strict = "SET sql_mode = 'STRICT_TRANS_TABLES';\n";
    let later = "ALTER TABLE orders MODIFY note VARCHAR(60) NULL;";
    for server in [MYSQL_80, MYSQL_84] {
        let alone = b1c_answer(server, &schema, later);
        assert_ne!(alone, UNKNOWN, "{server}");
        let sql = format!("{strict}ALTER TABLE orders MODIFY note VARCHAR(70000) NULL;\n{later}");
        assert_eq!(b1c_answer(server, &schema, &sql), alone, "{server}");
        // Without a strict SET in the input the column may have become
        // MEDIUMTEXT, and kilit stops trusting the table.
        let sql = format!("ALTER TABLE orders MODIFY note VARCHAR(70000) NULL;\n{later}");
        let reason = b1c_reason(server, &schema, &sql);
        assert!(reason.contains("kilit lost track of"), "{server}: {reason}");
    }
}

#[test]
fn a_foreign_key_column_of_a_length_kilit_cannot_count_is_unknown_and_does_not_panic() {
    // 4611686018427387904 characters of utf8mb4 are more bytes than a u64
    // holds. A debug build panicked on the multiplication, a release build
    // wrapped and said "fails with error 6125".
    let schema = Schema::from_sql(LAB_TABLES);
    let sql = "ALTER TABLE users ADD UNIQUE INDEX uq_name (name);
               ALTER TABLE orders ADD COLUMN big VARCHAR(4611686018427387904) NULL;
               ALTER TABLE orders ADD CONSTRAINT fk_big FOREIGN KEY (big) REFERENCES users (name);";
    for server in [MYSQL_80, MYSQL_84] {
        let results = Knowledge::built_in().server(server).unwrap();
        let findings = check(&parse(sql), &schema, results);
        assert_eq!(findings.len(), 3);
        assert!(
            matches!(findings[2].prediction, Prediction::Unknown { .. }),
            "{server}: {:?}",
            findings[2].prediction
        );
    }
}

#[test]
fn a_byte_count_kilit_cannot_make_is_written_as_more_bytes_than_kilit_can_count() {
    // Small fix b: the number is not printed as 18446744073709551615.
    let schema = Schema::from_sql(LAB_TABLES);
    for server in [MYSQL_80, MYSQL_84] {
        let reason = b1c_reason(
            server,
            &schema,
            "ALTER TABLE orders ADD COLUMN big VARCHAR(4611686018427387904) NULL",
        );
        assert!(
            reason.contains("more bytes than kilit can count"),
            "{server}: {reason}"
        );
        assert!(
            !reason.contains("18446744073709551615"),
            "{server}: {reason}"
        );
        // A count kilit can make is still printed.
        let reason = b1c_reason(
            server,
            &schema,
            "ALTER TABLE orders ADD COLUMN big VARCHAR(70000) NULL",
        );
        assert!(reason.contains("280000"), "{server}: {reason}");
    }
}

#[test]
fn the_four_shapes_the_lab_never_measured_are_unknown_on_a_known_table() {
    // By hand on MySQL 8.0.46 and 8.4.13 MySQL refuses some of
    // them and runs others; the lab has none of them, so kilit has no answer.
    let wide = Schema::from_sql(
        "CREATE TABLE w (id BIGINT UNSIGNED PRIMARY KEY, a VARCHAR(255) NOT NULL, \
         b VARCHAR(255) NOT NULL, c VARCHAR(255) NOT NULL, d VARCHAR(255) NOT NULL, \
         note VARCHAR(50) NULL) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4;
         ",
    );
    let lab = Schema::from_sql(&lab_after(&[
        "ALTER TABLE orders ADD INDEX ix_amount (amount)",
    ]));
    let mut problems = Vec::new();
    for (statement, schema) in [
        // a NOT NULL date or time column without a default.
        ("ALTER TABLE orders ADD COLUMN d DATE NOT NULL", &lab),
        ("ALTER TABLE orders ADD COLUMN d DATETIME NOT NULL", &lab),
        ("ALTER TABLE orders ADD COLUMN d TIMESTAMP NOT NULL", &lab),
        ("alter table `orders` add `d` date not null", &lab),
        // a column the table lacks, a name the table has, a key past
        // 3072 bytes, a prefix longer than its column.
        ("ALTER TABLE orders ADD INDEX ix_x (nosuch)", &lab),
        ("CREATE INDEX ix_x ON orders (nosuch)", &lab),
        ("ALTER TABLE orders ADD INDEX ix_amount (email)", &lab),
        ("CREATE INDEX ix_amount ON orders (email)", &lab),
        ("ALTER TABLE w ADD INDEX ix_big (a, b, c, d)", &wide),
        ("ALTER TABLE w ADD INDEX ix_note (note(100))", &wide),
        ("ALTER TABLE orders ADD INDEX ix_note (note(100))", &lab),
        // a column moved after itself.
        (
            "ALTER TABLE orders MODIFY note VARCHAR(50) NULL AFTER note",
            &lab,
        ),
    ] {
        for server in [MYSQL_80, MYSQL_84] {
            let got = b1c_answer(server, schema, statement);
            if got != UNKNOWN {
                problems.push(format!("{server}: {statement}\n    got {got}"));
            }
        }
    }
    assert!(problems.is_empty(), "\n{}", problems.join("\n"));
    // Their neighbors keep their answers.
    for (statement, answer) in [
        ("ALTER TABLE orders ADD COLUMN d DATE NULL", INSTANT),
        ("ALTER TABLE orders ADD INDEX ix_email (email)", INPLACE),
        ("ALTER TABLE orders ADD INDEX ix_note (note(10))", INPLACE),
    ] {
        for server in [MYSQL_80, MYSQL_84] {
            assert_eq!(
                b1c_answer(server, &lab, statement),
                answer,
                "{server}: {statement}"
            );
        }
    }
}

#[test]
fn dropping_a_fulltext_or_a_unique_index_is_the_measured_drop_index_answer_with_its_own_scenario() {
    // The lab: drop-fulltext-index and drop-unique-index are INPLACE, no
    // rebuild, writes allowed on both servers, the same as drop-index.
    for (setup, drop, scenario) in [
        (
            "ALTER TABLE orders ADD FULLTEXT INDEX ft_orders_note (note)",
            "DROP INDEX ft_orders_note",
            "drop-fulltext-index",
        ),
        (
            "ALTER TABLE orders ADD UNIQUE INDEX uq_orders_amount (amount)",
            "DROP INDEX uq_orders_amount",
            "drop-unique-index",
        ),
    ] {
        let schema = lab_after(&[setup]);
        let answer = said(&schema, &format!("ALTER TABLE orders {drop}"));
        assert_eq!(answer.case, Some("drop-index"), "{scenario}");
        assert_eq!(answer.answer, INPLACE, "{scenario}");
        assert!(
            answer.evidence.contains(&scenario),
            "{scenario} is missing from {:?}",
            answer.evidence
        );
    }
}

/// users and orders as mysqldump writes them for a foreign key MySQL named
/// `orders_ibfk_abc` (a name a person gave it), with the index that serves it
/// written as a KEY of the same name.
fn b1c_ibfk_tables(name: &str) -> String {
    format!(
        "CREATE TABLE `users` (
  `id` bigint unsigned NOT NULL AUTO_INCREMENT,
  PRIMARY KEY (`id`)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_0900_ai_ci;
CREATE TABLE `orders` (
  `id` bigint unsigned NOT NULL AUTO_INCREMENT,
  `user_id` bigint unsigned NOT NULL,
  PRIMARY KEY (`id`),
  KEY `{name}` (`user_id`),
  CONSTRAINT `{name}` FOREIGN KEY (`user_id`) REFERENCES `users` (`id`)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_0900_ai_ci;
"
    )
}

#[test]
fn an_index_named_like_a_foreign_key_stays_unknown_to_drop_after_the_table_is_renamed() {
    // By hand on MySQL 8.0.46 and 8.4.13: the key MySQL made for the
    // foreign key keeps a hidden flag after RENAME TABLE (the foreign key
    // becomes purchases_ibfk_abc, the index keeps its name). ADD INDEX ix then
    // drops that automatic index, and DROP INDEX ix is 1553. A KEY a person
    // wrote is never dropped, and a dump cannot tell the two apart.
    for name in ["orders_ibfk_abc", "orders_ibfk_1"] {
        let schema = Schema::from_dump(&b1c_ibfk_tables(name));
        for rename in [
            "RENAME TABLE orders TO purchases",
            "ALTER TABLE orders RENAME TO purchases",
        ] {
            let sql = format!(
                "{rename};
                 ALTER TABLE purchases ADD INDEX ix (user_id);
                 ALTER TABLE purchases DROP INDEX ix;"
            );
            for server in [MYSQL_80, MYSQL_84] {
                assert_eq!(
                    b1c_answer(server, &schema, &sql),
                    UNKNOWN,
                    "{server}: {name}, {rename}"
                );
            }
        }
        // Control, as before the rename: unknown too.
        let sql = "ALTER TABLE orders ADD INDEX ix (user_id);
                   ALTER TABLE orders DROP INDEX ix;";
        for server in [MYSQL_80, MYSQL_84] {
            assert_eq!(
                b1c_answer(server, &schema, sql),
                UNKNOWN,
                "{server}: {name}"
            );
        }
    }
}

#[test]
fn an_index_renamed_by_hand_is_no_longer_the_automatic_one_after_the_table_is_renamed() {
    // By hand on MySQL 8.0.46 and 8.4.13: RENAME INDEX of the foreign key's
    // index takes the hidden flag away, so ADD INDEX ix then DROP INDEX ix is
    // accepted, and the foreign key keeps its serving index.
    let schema = Schema::from_dump(&b1c_ibfk_tables("orders_ibfk_abc"));
    let sql = "ALTER TABLE orders RENAME INDEX orders_ibfk_abc TO ix_renamed;
               RENAME TABLE orders TO purchases;
               ALTER TABLE purchases ADD INDEX ix (user_id);
               ALTER TABLE purchases DROP INDEX ix;";
    for server in [MYSQL_80, MYSQL_84] {
        assert_eq!(b1c_answer(server, &schema, sql), INPLACE, "{server}");
        // The old name is gone.
        let sql = "ALTER TABLE orders RENAME INDEX orders_ibfk_abc TO ix_renamed;
                   ALTER TABLE orders DROP INDEX orders_ibfk_abc;";
        assert_eq!(b1c_answer(server, &schema, sql), UNKNOWN, "{server}");
    }
}

/// `q` has no primary key, a UNIQUE index on a prefix of the nullable-free
/// column `a`, and a UNIQUE index on `k`, a VARCHAR(700) utf8mb4: with the
/// filler of CHAR(255) latin1 columns it is the lab's shape of the hand test.
fn b1c_table_q(unique_on_a: &str) -> String {
    let filler: Vec<String> = (1..=20)
        .map(|n| format!("f{n} CHAR(255) CHARACTER SET latin1 NOT NULL"))
        .collect();
    format!(
        "CREATE TABLE q (a VARCHAR(20) NOT NULL, k VARCHAR(700) NOT NULL, {}, \
         UNIQUE KEY u1 ({unique_on_a}), UNIQUE KEY u2 (k)) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4;\n",
        filler.join(", ")
    )
}

#[test]
fn a_unique_index_on_a_prefix_does_not_key_the_rows_so_a_page_full_table_is_unknown() {
    // By hand on MySQL 8.0.46 and 8.4.13: InnoDB skips the prefix index
    // and keys the rows by u2, so with 5,100 bytes of filler ADD x CHAR(200)
    // latin1 NOT NULL fails with 1118 on 8.0.46 and rebuilds the table on
    // 8.4.13. The two servers differ, so kilit has no answer.
    let schema = Schema::from_sql(&b1c_table_q("a(10)"));
    let add = "ALTER TABLE q ADD COLUMN x CHAR(200) CHARACTER SET latin1 NOT NULL DEFAULT ''";
    for server in [MYSQL_80, MYSQL_84] {
        assert_eq!(b1c_answer(server, &schema, add), UNKNOWN, "{server}");
    }
}

#[test]
fn a_unique_index_on_a_whole_column_keys_the_rows_and_the_same_column_is_instant() {
    // Control. By hand on MySQL 8.0.46 and 8.4.13: u1 on all of `a` keys the
    // rows, and the same ADD COLUMN is INSTANT on both servers.
    let schema = Schema::from_sql(&b1c_table_q("a"));
    let add = "ALTER TABLE q ADD COLUMN x CHAR(200) CHARACTER SET latin1 NOT NULL DEFAULT ''";
    for server in [MYSQL_80, MYSQL_84] {
        assert_eq!(b1c_answer(server, &schema, add), INSTANT, "{server}");
    }
}

#[test]
fn dropping_a_temporary_table_with_a_comment_inside_is_read_as_without_it() {
    // A comment between DROP and TEMPORARY does not make it a drop of the
    // base table. By hand on MySQL 8.0.46 and 8.4.13: DROP TEMPORARY TABLE
    // leaves a base table of the same name alone. both forms leave the
    // base table known, so the ALTER gets its measured answer (INSTANT), and
    // the two forms give the same answer.
    let dump = Schema::from_dump(LAB_TABLES);
    let alter = "ALTER TABLE orders ADD COLUMN c INT NULL;";
    for drop in [
        "DROP TEMPORARY TABLE IF EXISTS orders",
        "DROP TEMPORARY TABLE orders",
    ] {
        let commented = drop.replacen("DROP ", "DROP /* x */ ", 1);
        for server in [MYSQL_80, MYSQL_84] {
            let plain = b1c_answer(server, &dump, &format!("{drop};\n{alter}"));
            let with = b1c_answer(server, &dump, &format!("{commented};\n{alter}"));
            assert_eq!(plain, INSTANT, "{server}: {drop}");
            assert_eq!(with, plain, "{server}: {commented}");
        }
    }
    for server in [MYSQL_80, MYSQL_84] {
        let answer = b1c_answer(
            server,
            &dump,
            &format!("DROP /* x */ TEMPORARY TABLE IF EXISTS orders;\n{alter}"),
        );
        assert_eq!(answer, INSTANT, "{server}");
    }
}

#[test]
fn making_a_primary_key_column_nullable_is_unknown_and_does_not_panic() {
    // By hand on MySQL 8.0.46 and 8.4.13 a primary key
    // column cannot be NULL; the lab has no scenario for the MODIFY, so
    // kilit has no answer, and its row size sums must not fail on the shape.
    let schema = Schema::from_sql(
        "CREATE TABLE p (id INT NOT NULL, note VARCHAR(10) NULL, PRIMARY KEY (id)) \
         ENGINE=InnoDB DEFAULT CHARSET=utf8mb4;",
    );
    for statement in [
        "ALTER TABLE p MODIFY id INT NULL",
        "alter table `p` modify `id` int null",
    ] {
        for server in [MYSQL_80, MYSQL_84] {
            assert_eq!(
                b1c_answer(server, &schema, statement),
                UNKNOWN,
                "{server}: {statement}"
            );
        }
    }
}

// ---------------------------------------------------------------------------
// Step B1c, fix round: one test for each shape found in review. Each MySQL
// fact below was run by hand on MySQL 8.0.46 and 8.4.13.

/// A parent with a unique index on `email` and a child with a foreign key to
/// each of `id` and `email`.
const FIX_USERS_AND_ORDERS: &str = "CREATE TABLE `users` (
  `id` bigint unsigned NOT NULL AUTO_INCREMENT,
  `email` varchar(100) NOT NULL,
  PRIMARY KEY (`id`),
  UNIQUE KEY `users_email_unique` (`email`)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_unicode_ci;
CREATE TABLE `orders` (
  `id` bigint unsigned NOT NULL AUTO_INCREMENT,
  `user_id` bigint unsigned NOT NULL,
  `user_email` varchar(100) NOT NULL,
  `note` varchar(50) DEFAULT NULL,
  PRIMARY KEY (`id`),
  KEY `orders_user_id_foreign` (`user_id`),
  KEY `orders_user_email_foreign` (`user_email`),
  CONSTRAINT `orders_user_id_foreign` FOREIGN KEY (`user_id`) REFERENCES `users` (`id`),
  CONSTRAINT `orders_user_email_foreign` FOREIGN KEY (`user_email`) REFERENCES `users` (`email`)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_unicode_ci;
";

#[test]
fn a_child_kilit_lost_still_keeps_the_parent_index_and_the_parent_column_type() {
    // By hand: with the child's foreign keys in place, dropping the unique
    // index users_email_unique is 1553 and MODIFY users.id INT UNSIGNED is
    // 3780. A child kilit stopped trusting may still have them, so the lab's
    // answer for the plain table must not be given.
    let lose = "ALTER TABLE orders CONVERT TO CHARACTER SET utf8mb4 COLLATE utf8mb4_unicode_ci;\n";
    let drop_index = "ALTER TABLE users DROP INDEX users_email_unique";
    let modify = "ALTER TABLE users MODIFY id INT UNSIGNED NOT NULL AUTO_INCREMENT";
    for (how, schema) in [
        ("from SQL", Schema::from_sql(FIX_USERS_AND_ORDERS)),
        ("from a dump", Schema::from_dump(FIX_USERS_AND_ORDERS)),
    ] {
        for server in [MYSQL_80, MYSQL_84] {
            // Control: with the child known, both are unknown too (the lab
            // has not measured a referenced index or column).
            for statement in [drop_index, modify] {
                assert_eq!(
                    b1c_answer(server, &schema, statement),
                    UNKNOWN,
                    "{how}, {server}: {statement}"
                );
            }
            for statement in [drop_index, modify] {
                let sql = format!("{lose}{statement};");
                assert_eq!(
                    b1c_answer(server, &schema, &sql),
                    UNKNOWN,
                    "{how}, {server}: {sql}"
                );
            }
        }
    }
}

#[test]
fn a_foreign_key_added_to_a_table_kilit_does_not_know_makes_the_parent_index_drop_unknown() {
    // No --schema: users is created here, orders is in no schema. The key
    // from orders.user_email makes dropping users_email_unique 1553.
    let schema = Schema::from_sql(
        "CREATE TABLE users (id BIGINT UNSIGNED NOT NULL PRIMARY KEY, \
         email VARCHAR(100) NOT NULL, UNIQUE KEY users_email_unique (email)) ENGINE=InnoDB;",
    );
    let key = "ALTER TABLE orders ADD CONSTRAINT fk_e FOREIGN KEY (user_email) \
               REFERENCES users (email);\n";
    let drop_index = "ALTER TABLE users DROP INDEX users_email_unique";
    for server in [MYSQL_80, MYSQL_84] {
        assert_eq!(
            b1c_answer(server, &schema, drop_index),
            INPLACE,
            "{server}: control"
        );
        assert_eq!(
            b1c_answer(server, &schema, &format!("{key}{drop_index}")),
            UNKNOWN,
            "{server}"
        );
    }
}

#[test]
fn create_table_if_not_exists_without_a_schema_may_keep_a_table_kilit_cannot_see() {
    // No --schema: a table named posts may exist, and then the statement does
    // nothing. After a DROP TABLE posts it creates the table.
    let schema = Schema::from_sql("");
    let create = "CREATE TABLE IF NOT EXISTS posts (id INT PRIMARY KEY, body TEXT NULL);\n";
    let add = "ALTER TABLE posts ADD COLUMN c INT NULL;";
    for server in [MYSQL_80, MYSQL_84] {
        let reason = b1c_reason(server, &schema, &format!("{create}{add}"));
        assert!(
            reason.contains(
                "kilit lost track of posts (line 1: CREATE TABLE IF NOT EXISTS keeps a table \
                 of the name that no --schema shows, if there is one)"
            ),
            "{server}: {reason}"
        );
    }
}

#[test]
fn create_table_if_not_exists_after_a_drop_of_the_name_creates_the_table() {
    // The DROP TABLE ran, so no table of the name is left and the statement
    // creates it: it is known, and a table created here.
    let create = "CREATE TABLE IF NOT EXISTS posts (id INT PRIMARY KEY, body TEXT NULL);\n";
    let add = "ALTER TABLE posts ADD COLUMN c INT NULL;";
    let dump = Schema::from_dump(
        "CREATE TABLE `posts` (`id` int NOT NULL, PRIMARY KEY (`id`)) ENGINE=InnoDB;",
    );
    let mut problems = Vec::new();
    for (how, schema, before) in [
        (
            "a table the input made",
            Schema::from_sql(""),
            "CREATE TABLE posts (id INT PRIMARY KEY);\nDROP TABLE posts;\n",
        ),
        (
            "a table no schema shows",
            Schema::from_sql(""),
            "DROP TABLE posts;\n",
        ),
        ("a table of the dump", dump, "DROP TABLE posts;\n"),
    ] {
        for server in [MYSQL_80, MYSQL_84] {
            let got = b1c_answer(server, &schema, &format!("{before}{create}{add}"));
            if got != INSTANT {
                problems.push(format!("{how}, {server}: got {got}"));
            }
        }
    }
    assert!(problems.is_empty(), "\n{}", problems.join("\n"));
}

#[test]
fn two_indexes_renamed_into_each_others_names_lose_the_table() {
    // By hand: RENAME INDEX a TO b, RENAME INDEX b TO a swaps the names, so
    // DROP INDEX a drops what was b, the index of the foreign key: 1553.
    let schema = Schema::from_dump(
        "CREATE TABLE `users` (`id` bigint unsigned NOT NULL AUTO_INCREMENT, PRIMARY KEY (`id`)) \
         ENGINE=InnoDB DEFAULT CHARSET=utf8mb4;
         CREATE TABLE `orders` (`id` bigint unsigned NOT NULL AUTO_INCREMENT, \
         `user_id` bigint unsigned NOT NULL, `amount` int NOT NULL, PRIMARY KEY (`id`), \
         KEY `a` (`amount`), KEY `b` (`user_id`), \
         CONSTRAINT `fk_user` FOREIGN KEY (`user_id`) REFERENCES `users` (`id`)) \
         ENGINE=InnoDB DEFAULT CHARSET=utf8mb4;",
    );
    let swap = "ALTER TABLE orders RENAME INDEX a TO b, RENAME INDEX b TO a;\n";
    for server in [MYSQL_80, MYSQL_84] {
        let sql = format!("{swap}ALTER TABLE orders DROP INDEX a;");
        let predictions = predictions_on_schema(server, &schema, &sql);
        assert!(
            matches!(predictions[0], Prediction::Unknown { .. }),
            "{server}"
        );
        match &predictions[1] {
            Prediction::Unknown { reason, notes } => {
                assert!(reason.contains("kilit lost track of orders"), "{reason}");
                assert!(
                    notes.iter().any(|note| note.contains("1553")),
                    "{server}: {notes:?}"
                );
            }
            other => panic!("{server}: {other:?}"),
        }
        // A RENAME INDEX beside any other clause loses the table too.
        for clause in [
            "ALTER TABLE orders RENAME INDEX a TO c, ADD COLUMN x INT NULL",
            "ALTER TABLE orders ADD COLUMN x INT NULL, RENAME INDEX a TO c",
            "ALTER TABLE orders RENAME INDEX a TO c, RENAME INDEX b TO d",
        ] {
            let reason = b1c_reason(
                server,
                &schema,
                &format!("{clause};\nALTER TABLE orders ADD COLUMN y INT NULL;"),
            );
            assert!(
                reason.contains("kilit lost track of orders"),
                "{server}: {clause}: {reason}"
            );
        }
        // Alone, one rename keeps the table.
        assert_eq!(
            b1c_answer(
                server,
                &schema,
                "ALTER TABLE orders RENAME INDEX a TO c;\nALTER TABLE orders DROP INDEX c;"
            ),
            INPLACE,
            "{server}"
        );
    }
}

/// kilit's predictions for every statement of `sql` against `schema`.
fn predictions_on_schema(server: &str, schema: &Schema, sql: &str) -> Vec<Prediction> {
    let results = Knowledge::built_in().server(server).unwrap();
    check(&parse(sql), schema, results)
        .into_iter()
        .map(|finding| finding.prediction)
        .collect()
}

#[test]
fn dropping_a_unique_index_of_not_null_columns_on_a_table_without_a_primary_key_is_unknown() {
    // By hand: the unique index of NOT NULL columns keys the rows, so dropping
    // it is 1846 for INSTANT and INPLACE and copies the table as written. The
    // lab dropped a unique index on a table with a primary key.
    let schema = Schema::from_sql(
        "CREATE TABLE `codes` (`code` int NOT NULL, `note` varchar(50) DEFAULT NULL, \
         UNIQUE KEY `codes_code_unique` (`code`)) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4;",
    );
    for server in [MYSQL_80, MYSQL_84] {
        for sql in [
            "ALTER TABLE codes DROP INDEX codes_code_unique",
            "ALTER TABLE codes DROP INDEX codes_code_unique, ALGORITHM=INSTANT",
            "ALTER TABLE codes DROP INDEX codes_code_unique, ALGORITHM=INPLACE",
        ] {
            assert_eq!(b1c_answer(server, &schema, sql), UNKNOWN, "{server}: {sql}");
        }
    }
}

#[test]
fn a_prefix_that_makes_a_key_past_3072_bytes_is_unknown() {
    // By hand: 1071 for body(800) and for summary(500), body(400) in utf8mb4.
    let schema = Schema::from_sql(
        "CREATE TABLE `posts` (`id` bigint unsigned NOT NULL AUTO_INCREMENT, \
         `title` varchar(255) NOT NULL, `summary` varchar(600) NOT NULL, \
         `body` varchar(1000) NOT NULL, `content` text, PRIMARY KEY (`id`)) \
         ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_unicode_ci;",
    );
    for server in [MYSQL_80, MYSQL_84] {
        for sql in [
            "ALTER TABLE posts ADD INDEX i (body(800))",
            "ALTER TABLE posts ADD INDEX i2 (summary(500), body(400))",
        ] {
            let reason = b1c_reason(server, &schema, sql);
            assert!(
                reason.contains("past the 3072 bytes"),
                "{server}: {sql}: {reason}"
            );
        }
        // Control: a prefix that fits keeps the lab's answer.
        assert_eq!(
            b1c_answer(
                server,
                &schema,
                "ALTER TABLE posts ADD INDEX i3 (body(100))"
            ),
            INPLACE,
            "{server}"
        );
    }
}

#[test]
fn a_prefix_longer_than_its_column_or_on_an_integer_is_unknown() {
    // By hand: 1089 for a prefix longer than a CHAR or a VARBINARY column,
    // and for any prefix on an INT.
    let schema = Schema::from_sql(
        "CREATE TABLE `codes` (`id` int NOT NULL, `code` char(10) NOT NULL, \
         `bin` varbinary(10) NOT NULL, `n` int NOT NULL, PRIMARY KEY (`id`)) \
         ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_unicode_ci;",
    );
    for server in [MYSQL_80, MYSQL_84] {
        for sql in [
            "ALTER TABLE codes ADD INDEX i1 (code(20))",
            "ALTER TABLE codes ADD INDEX i2 (bin(20))",
            "ALTER TABLE codes ADD INDEX i3 (n(2))",
        ] {
            assert_eq!(b1c_answer(server, &schema, sql), UNKNOWN, "{server}: {sql}");
        }
        // Control: a prefix inside the column.
        assert_eq!(
            b1c_answer(server, &schema, "ALTER TABLE codes ADD INDEX i4 (code(5))"),
            INPLACE,
            "{server}"
        );
    }
}

#[test]
fn dropping_the_constraint_mysql_named_for_an_unnamed_foreign_key_is_unknown_and_loses_the_table() {
    // By hand: both statements run (DROP CONSTRAINT orders_ibfk_1 drops the
    // key, then user_id goes). kilit does not give the key its automatic name
    // in CREATE TABLE, so it must not call the column drop a failure.
    let sql = "CREATE TABLE users (id BIGINT UNSIGNED NOT NULL PRIMARY KEY);
        CREATE TABLE orders (id BIGINT UNSIGNED NOT NULL PRIMARY KEY, \
        user_id BIGINT UNSIGNED NOT NULL, FOREIGN KEY (user_id) REFERENCES users (id));
        ALTER TABLE orders DROP CONSTRAINT orders_ibfk_1;
        ALTER TABLE orders DROP COLUMN user_id;";
    let schema = Schema::from_sql("");
    for server in [MYSQL_80, MYSQL_84] {
        let predictions = predictions_on_schema(server, &schema, sql);
        assert!(
            matches!(predictions[2], Prediction::Unknown { .. }),
            "{server}: {:?}",
            predictions[2]
        );
        match &predictions[3] {
            Prediction::Unknown { reason, .. } => {
                assert!(
                    reason.contains("kilit lost track of orders"),
                    "{server}: {reason}"
                )
            }
            other => panic!("{server}: {other:?}"),
        }
    }
}

#[test]
fn fulltext_indexes_past_3072_bytes_keep_the_measured_answer() {
    // By hand: FULLTEXT has no 3072-byte limit. A VARCHAR(1000) in utf8mb4 is
    // 4000 bytes, and title (1020) with summary (2400) is 3420: MySQL accepts
    // both, and the lab measured add-fulltext-index.
    let schema = Schema::from_sql(
        "CREATE TABLE `posts` (`id` bigint unsigned NOT NULL AUTO_INCREMENT, \
         `title` varchar(255) NOT NULL, `summary` varchar(600) NOT NULL, \
         `body` varchar(1000) NOT NULL, `content` text, PRIMARY KEY (`id`)) \
         ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_unicode_ci;",
    );
    for server in [MYSQL_80, MYSQL_84] {
        for sql in [
            "ALTER TABLE posts ADD FULLTEXT INDEX ft_b (body)",
            "ALTER TABLE posts ADD FULLTEXT INDEX ft_ts (title, summary)",
        ] {
            assert_eq!(
                b1c_answer(server, &schema, sql),
                "INPLACE, rebuilds the table, writes wait",
                "{server}: {sql}"
            );
        }
    }
}

#[test]
fn create_fulltext_index_with_a_forced_algorithm_or_lock_is_unknown_and_adds_no_index() {
    // By hand: CREATE FULLTEXT INDEX ... ALGORITHM=INSTANT and LOCK=NONE fail
    // with 1846, as ADD FULLTEXT does (the lab's add-fulltext-index), so the
    // table has no FULLTEXT index afterwards and a new column is INSTANT.
    let schema = Schema::from_dump(
        "CREATE TABLE `orders` (`id` bigint unsigned NOT NULL AUTO_INCREMENT, \
         `amount` int NOT NULL, `note` varchar(50) DEFAULT NULL, \
         `email` varchar(100) DEFAULT NULL, PRIMARY KEY (`id`), KEY `i_amount` (`amount`)) \
         ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_unicode_ci;",
    );
    let then = "ALTER TABLE orders ADD COLUMN y INT NULL;";
    for server in [MYSQL_80, MYSQL_84] {
        for forced in ["ALGORITHM=INSTANT", "LOCK=NONE"] {
            let create = format!("CREATE FULLTEXT INDEX ft ON orders (note) {forced}");
            let reason = b1c_reason(server, &schema, &create);
            assert!(
                reason.contains("fails with error 1846 (add-fulltext-index)"),
                "{server}: {create}: {reason}"
            );
            assert_eq!(
                b1c_answer(server, &schema, &format!("{create};\n{then}")),
                INSTANT,
                "{server}: {create}"
            );
        }
        // ADD FULLTEXT with the same algorithm fails the same way.
        let sql =
            format!("ALTER TABLE orders ADD FULLTEXT INDEX ft (note), ALGORITHM=INSTANT;\n{then}");
        assert_eq!(b1c_answer(server, &schema, &sql), INSTANT, "{server}");
        // Without a forced variant: unknown, the lab's answer in the reason,
        // and the table has the index afterwards.
        let reason = b1c_reason(server, &schema, "CREATE FULLTEXT INDEX ft ON orders (note)");
        assert!(
            reason.contains(
                "CREATE FULLTEXT INDEX has not been measured itself; MySQL runs it as ALTER \
                 TABLE ... ADD FULLTEXT, by hand on MySQL 8.0.46 and 8.4.13, and the lab \
                 measured that: INPLACE, rebuilds the table, writes wait (add-fulltext-index)"
            ),
            "{server}: {reason}"
        );
        assert_eq!(
            b1c_answer(
                server,
                &schema,
                &format!("CREATE FULLTEXT INDEX ft ON orders (note);\n{then}")
            ),
            COPY,
            "{server}"
        );
    }
}

#[test]
fn an_engine_alter_on_a_table_the_dump_lacks_is_unknown_and_never_a_1674_failure() {
    // By hand: with a dump, the table orders is not there, so MySQL refuses
    // the first statement and the migration stops: nothing after it runs.
    let dump = Schema::from_dump(
        "CREATE TABLE `posts` (`id` int NOT NULL, `body` text, PRIMARY KEY (`id`), \
         FULLTEXT KEY `ft` (`body`)) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4;",
    );
    let sql = "ALTER TABLE orders ENGINE=InnoDB;
        ALTER TABLE orders ADD COLUMN t CHAR(36) NULL DEFAULT (UUID());
        ALTER TABLE orders2 ADD COLUMN t CHAR(36) NULL DEFAULT (UUID());";
    for server in [MYSQL_80, MYSQL_84] {
        let predictions = predictions_on_schema(server, &dump, sql);
        assert_eq!(predictions.len(), 3);
        for (n, prediction) in predictions.iter().enumerate() {
            match prediction {
                Prediction::Unknown { reason, .. } => assert!(
                    reason.contains("the dump may be out of date"),
                    "{server}: statement {n}: {reason}"
                ),
                other => panic!("{server}: statement {n}: {other:?}"),
            }
        }
    }
}

#[test]
fn a_temporary_table_named_like_the_child_does_not_hide_the_real_child_keys() {
    // By hand on MySQL 8.0.46 and 8.4.13: CREATE TEMPORARY TABLE orders
    // leaves the real orders alone, so its foreign key to users.email still
    // makes DROP INDEX users_email_unique fail with 1553.
    let schema = Schema::from_dump(FIX_USERS_AND_ORDERS);
    for create in [
        "CREATE TEMPORARY TABLE orders (id INT)",
        "CREATE TEMPORARY TABLE IF NOT EXISTS orders (id INT)",
    ] {
        let sql = format!("{create};\nALTER TABLE users DROP INDEX users_email_unique;");
        for server in [MYSQL_80, MYSQL_84] {
            let reason = b1c_reason(server, &schema, &sql);
            assert!(
                reason.contains(
                    "a foreign key of a table kilit does not know or lost track of may point at users"
                ),
                "{server}: {create}: {reason}"
            );
        }
    }
}

#[test]
fn a_key_of_a_text_prefix_and_a_char_column_past_3072_bytes_is_unknown() {
    // By hand on MySQL 8.0.46 and 8.4.13: t(767) is 3068 bytes in utf8mb4, and
    // with c CHAR(20) (80 bytes) the key is 3148 bytes: 1071. With id INT
    // (4 bytes) it is 3072: accepted. Integers count their size, CHAR and
    // VARCHAR their character set, BINARY and VARBINARY their bytes.
    let schema = Schema::from_sql(
        "CREATE TABLE `p` (`id` int NOT NULL, `t` text, `c` char(20) NOT NULL, \
         `bn` binary(20) NOT NULL, `vb` varbinary(100) NOT NULL, `j` json DEFAULT NULL, \
         PRIMARY KEY (`id`)) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 \
         COLLATE=utf8mb4_unicode_ci;",
    );
    for server in [MYSQL_80, MYSQL_84] {
        let reason = b1c_reason(server, &schema, "ALTER TABLE p ADD INDEX i16 (t(767), c)");
        assert!(
            reason.contains("the key takes 3148 bytes"),
            "{server}: {reason}"
        );
        assert_eq!(
            b1c_answer(server, &schema, "ALTER TABLE p ADD INDEX i21 (t(767), id)"),
            INPLACE,
            "{server}"
        );
        for sql in [
            "ALTER TABLE p ADD INDEX i19 (vb(100), t(750))",
            "ALTER TABLE p ADD INDEX i20 (j(10))",
        ] {
            assert_eq!(b1c_answer(server, &schema, sql), UNKNOWN, "{server}: {sql}");
        }
    }
}

// ---------------------------------------------------------------------------
// What every table it may be shares: ADD FULLTEXT INDEX on a table kilit does
// not know.
//
// The expectations come from the lab's matrix (add-fulltext-index,
// add-second-fulltext-index and the MyISAM, COMPRESSED, COMPACT and REDUNDANT
// scenarios, both servers identical), from hand tests on MySQL 8.0.46 and
// 8.4.13 (1072 for a column the table lacks, 1061 for a taken index name) and
// from the owner's rules: every answer makes writes wait: writes wait; every
// answer fails: fails; anything else is unknown.

const INPLACE_REBUILD_WAIT: &str = "INPLACE, rebuilds the table, writes wait";

/// The answer of the last statement of `sql`, which must be "depends on the
/// table": what every table shares, its notes and its reason.
fn b1c_depends(server: &str, schema: &Schema, sql: &str) -> (Shared, Vec<String>, String) {
    match b1c_last(server, schema, sql) {
        Prediction::DependsOnTable {
            shared,
            notes,
            reason,
            ..
        } => (shared, notes, reason),
        other => panic!("{server}: {sql}\n    expected depends-on-the-table, got {other:?}"),
    }
}

/// Every way kilit can lack `orders` and still say what all tables share:
/// no --schema, a table it lost, a table it lost in a dump.
fn b1c_tables_kilit_does_not_know_with_dump() -> Vec<(&'static str, Schema)> {
    b1c_tables_kilit_does_not_know()
}

/// ALGORITHM and LOCK clauses, in the order of the lab's variants, with what
/// the lab measured for ADD FULLTEXT INDEX on every table: the default and
/// ALGORITHM=INPLACE make writes wait (a MyISAM table refuses INPLACE with
/// 1845, the others accept it); INSTANT, INPLACE with LOCK=NONE and LOCK=NONE
/// fail on every one (1845 or 1846). ALGORITHM=COPY runs everywhere, but on
/// the InnoDB tables the lab did not see whether writes wait for a forced
/// COPY, so one answer is "writes not measured" and no word is shared.
const B1C_FULLTEXT_VARIANTS: [(&str, Option<Shared>); 6] = [
    ("", Some(Shared::WritesWait)),
    (", ALGORITHM=INSTANT", Some(Shared::Fails)),
    (", ALGORITHM=INPLACE", Some(Shared::WritesWait)),
    (", ALGORITHM=INPLACE, LOCK=NONE", Some(Shared::Fails)),
    (", ALGORITHM=COPY", None),
    (", LOCK=NONE", Some(Shared::Fails)),
];

#[test]
fn adding_a_fulltext_index_to_a_table_kilit_does_not_know_makes_writes_wait() {
    // $table->fullText('note');
    for (how, schema) in b1c_tables_kilit_does_not_know_with_dump() {
        for server in [MYSQL_80, MYSQL_84] {
            let (shared, notes, reason) = b1c_depends(
                server,
                &schema,
                "ALTER TABLE orders ADD FULLTEXT INDEX ft_orders_note (note)",
            );
            assert_eq!(shared, Shared::WritesWait, "{how}, {server}");
            assert!(reason.contains("writes wait"), "{how}, {server}: {reason}");
            assert!(
                reason.contains("add-fulltext-index"),
                "{how}, {server}: {reason}"
            );
            // Each table it may be is a note.
            for part in [
                "if the table is MyISAM",
                "if the table is COMPRESSED",
                "if the table is COMPACT",
                "if the table is REDUNDANT",
                "if the table already has a FULLTEXT index",
                "if the table has no column note: fails with error 1072",
                "fails with error 1061",
            ] {
                assert!(
                    notes.iter().any(|note| note.contains(part)),
                    "{how}, {server}: {part}: {notes:?}"
                );
            }
        }
    }
}

#[test]
fn a_forced_algorithm_or_lock_on_a_fulltext_index_of_a_table_kilit_does_not_know_follows_the_lab() {
    for (how, schema) in b1c_tables_kilit_does_not_know_with_dump() {
        for (clause, expected) in B1C_FULLTEXT_VARIANTS {
            let sql =
                format!("ALTER TABLE orders ADD FULLTEXT INDEX ft_orders_note (note){clause}");
            for server in [MYSQL_80, MYSQL_84] {
                match expected {
                    Some(expected) => {
                        let (shared, _, _) = b1c_depends(server, &schema, &sql);
                        assert_eq!(shared, expected, "{how}, {server}: {sql}");
                    }
                    None => assert_eq!(
                        b1c_answer(server, &schema, &sql),
                        UNKNOWN,
                        "{how}, {server}: {sql}"
                    ),
                }
            }
        }
    }
}

#[test]
fn a_myisam_table_that_refuses_inplace_is_a_failing_note_not_the_headline() {
    // ALGORITHM=INPLACE: the MyISAM scenarios fail with 1845, every other
    // table accepts it and makes writes wait, so the headline is "writes
    // wait" and the failure is a note.
    let schema = Schema::from_sql("");
    for server in [MYSQL_80, MYSQL_84] {
        let (shared, notes, _) = b1c_depends(
            server,
            &schema,
            "ALTER TABLE orders ADD FULLTEXT INDEX ft_orders_note (note), ALGORITHM=INPLACE",
        );
        assert_eq!(shared, Shared::WritesWait, "{server}");
        assert!(
            notes
                .iter()
                .any(|note| note.contains("if the table is MyISAM") && note.contains("1845")),
            "{server}: {notes:?}"
        );
    }
}

#[test]
fn a_fulltext_index_on_a_table_missing_from_a_dump_is_unknown() {
    // The dump decides: the table is not there, so the answer is not what
    // any table would do.
    let dump = Schema::from_dump("CREATE TABLE unrelated (id INT PRIMARY KEY) ENGINE=InnoDB;");
    for (clause, _) in B1C_FULLTEXT_VARIANTS {
        let sql = format!("ALTER TABLE orders ADD FULLTEXT INDEX ft_orders_note (note){clause}");
        for server in [MYSQL_80, MYSQL_84] {
            assert_eq!(b1c_answer(server, &dump, &sql), UNKNOWN, "{server}: {sql}");
        }
    }
}

#[test]
fn a_column_an_index_or_a_foreign_key_on_a_table_kilit_does_not_know_stays_unknown() {
    // Some table it may be lets writes continue (ADD COLUMN, ADD INDEX), or
    // a form of the change has not been measured on every format (ADD FOREIGN
    // KEY with the checks on), so no word is shared.
    for (how, schema) in b1c_tables_kilit_does_not_know_with_dump() {
        for statement in [
            "ALTER TABLE orders ADD COLUMN c INT NULL",
            "ALTER TABLE orders ADD INDEX ix_amount (amount)",
            "ALTER TABLE orders ADD UNIQUE INDEX uq_email (email)",
            "ALTER TABLE orders ADD CONSTRAINT fk_user FOREIGN KEY (user_id) REFERENCES users (id)",
        ] {
            for server in [MYSQL_80, MYSQL_84] {
                assert_eq!(
                    b1c_answer(server, &schema, statement),
                    UNKNOWN,
                    "{how}, {server}: {statement}"
                );
            }
        }
    }
}

#[test]
fn create_fulltext_index_on_a_table_kilit_does_not_know_stays_unknown() {
    // The statement itself is not measured.
    for (how, schema) in b1c_tables_kilit_does_not_know_with_dump() {
        for server in [MYSQL_80, MYSQL_84] {
            assert_eq!(
                b1c_answer(
                    server,
                    &schema,
                    "CREATE FULLTEXT INDEX ft_orders_note ON orders (note)"
                ),
                UNKNOWN,
                "{how}, {server}"
            );
        }
    }
}

#[test]
fn a_statement_after_a_fulltext_index_on_a_table_kilit_does_not_know_is_still_unknown() {
    // Nothing is learned about the table: a new column is no more known than
    // before.
    for (how, schema) in b1c_tables_kilit_does_not_know_with_dump() {
        let sql = "ALTER TABLE orders ADD FULLTEXT INDEX ft_orders_note (note);
                   ALTER TABLE orders ADD COLUMN c INT NULL;";
        for server in [MYSQL_80, MYSQL_84] {
            assert_eq!(b1c_answer(server, &schema, sql), UNKNOWN, "{how}, {server}");
        }
    }
}

#[test]
fn adding_a_fulltext_index_to_a_known_dynamic_table_keeps_the_lab_answer() {
    // Regression: add-fulltext-index, INPLACE with a rebuild, writes wait.
    let schema = Schema::from_sql(LAB_TABLES);
    for server in [MYSQL_80, MYSQL_84] {
        assert_eq!(
            b1c_answer(
                server,
                &schema,
                "ALTER TABLE orders ADD FULLTEXT INDEX ft_orders_note (note)"
            ),
            INPLACE_REBUILD_WAIT,
            "{server}"
        );
    }
}

/// The lab's tables with `orders` on `engine` with `options`, and a TEXT
/// column `body` when `column` is given (the lab's setup of the new
/// scenarios, with the column type changed for the shapes it did not
/// measure).
fn b1c_fulltext_table(engine: Option<&str>, options: &str, column: Option<&str>) -> Schema {
    let mut sql = lab_tables_with(engine, options);
    if let Some(column) = column {
        sql.push_str(&format!(
            "ALTER TABLE orders ADD COLUMN body {column} NULL;\n"
        ));
    }
    Schema::from_sql(&sql)
}

#[test]
fn a_fulltext_index_on_a_known_myisam_table_is_the_lab_answer_for_text_and_for_varchar() {
    // myisam-add-fulltext-index (VARCHAR) and myisam-add-fulltext-index-text:
    // COPY, every row copied, writes wait; the default and COPY run, and the
    // other four variants fail with 1845.
    let expected = [COPY, FAILS_1845, FAILS_1845, FAILS_1845, COPY, FAILS_1845];
    for (column, statement) in [
        (
            Some("TEXT"),
            "ALTER TABLE orders ADD FULLTEXT INDEX ft_orders_body (body)",
        ),
        (None, "ALTER TABLE orders ADD FULLTEXT INDEX ft_note (note)"),
    ] {
        let schema = b1c_fulltext_table(Some("MyISAM"), "", column);
        for ((clause, _), expected) in B1C_FULLTEXT_VARIANTS.iter().zip(expected) {
            for server in [MYSQL_80, MYSQL_84] {
                assert_eq!(
                    b1c_answer(server, &schema, &format!("{statement}{clause}")),
                    expected,
                    "{server}: {statement}{clause}"
                );
            }
        }
    }
}

#[test]
fn a_fulltext_index_on_a_text_column_of_a_known_compressed_compact_or_redundant_table_is_the_lab_answer()
 {
    // compressed-, compact- and redundant-add-fulltext-index: INPLACE with a
    // rebuild, nothing copied, writes wait; INSTANT, INPLACE with LOCK=NONE
    // and LOCK=NONE fail with 1846; ALGORITHM=COPY runs, and the lab did not
    // see whether writes wait for it.
    let expected = [
        INPLACE_REBUILD_WAIT,
        FAILS_1846,
        INPLACE_REBUILD_WAIT,
        FAILS_1846,
        COPY_NOT_MEASURED,
        FAILS_1846,
    ];
    for options in [
        " ROW_FORMAT=COMPRESSED",
        " KEY_BLOCK_SIZE=8",
        " ROW_FORMAT=COMPACT",
        " ROW_FORMAT=REDUNDANT",
    ] {
        let schema = b1c_fulltext_table(None, options, Some("TEXT"));
        for ((clause, _), expected) in B1C_FULLTEXT_VARIANTS.iter().zip(expected) {
            let sql =
                format!("ALTER TABLE orders ADD FULLTEXT INDEX ft_orders_body (body){clause}");
            for server in [MYSQL_80, MYSQL_84] {
                assert_eq!(
                    b1c_answer(server, &schema, &sql),
                    expected,
                    "{server}:{options}: {sql}"
                );
            }
        }
    }
}

#[test]
fn other_fulltext_shapes_on_a_known_compressed_compact_redundant_or_myisam_table_stay_unknown() {
    // The lab measured one TEXT column (MyISAM: one VARCHAR or TEXT column)
    // on a table with no FULLTEXT index. A second index, two columns and
    // MEDIUMTEXT are not measured there.
    for (engine, options) in [
        (Some("MyISAM"), ""),
        (None, " ROW_FORMAT=COMPRESSED"),
        (None, " KEY_BLOCK_SIZE=8"),
        (None, " ROW_FORMAT=COMPACT"),
        (None, " ROW_FORMAT=REDUNDANT"),
    ] {
        let text = b1c_fulltext_table(engine, options, Some("TEXT"));
        let medium = b1c_fulltext_table(engine, options, Some("MEDIUMTEXT"));
        for server in [MYSQL_80, MYSQL_84] {
            let cases = [
                (
                    "a second FULLTEXT index",
                    &text,
                    "ALTER TABLE orders ADD FULLTEXT INDEX ft_a (body);
                     ALTER TABLE orders ADD FULLTEXT INDEX ft_b (note)",
                ),
                (
                    "two columns",
                    &text,
                    "ALTER TABLE orders ADD FULLTEXT INDEX ft_both (body, note)",
                ),
                (
                    "a MEDIUMTEXT column",
                    &medium,
                    "ALTER TABLE orders ADD FULLTEXT INDEX ft_orders_body (body)",
                ),
            ];
            for (what, schema, sql) in cases {
                assert_eq!(
                    b1c_answer(server, schema, sql),
                    UNKNOWN,
                    "{server}:{engine:?}{options}: {what}"
                );
            }
        }
    }
}

#[test]
fn a_fulltext_index_on_a_varchar_column_of_a_known_compressed_compact_or_redundant_table_stays_unknown()
 {
    // Only a TEXT column was measured on these row formats.
    let statement = "ALTER TABLE orders ADD FULLTEXT INDEX ft_note (note)";
    for options in [
        " ROW_FORMAT=COMPRESSED",
        " KEY_BLOCK_SIZE=8",
        " ROW_FORMAT=COMPACT",
        " ROW_FORMAT=REDUNDANT",
    ] {
        let schema = b1c_fulltext_table(None, options, None);
        for server in [MYSQL_80, MYSQL_84] {
            assert_eq!(
                b1c_answer(server, &schema, statement),
                UNKNOWN,
                "{server}:{options}"
            );
        }
    }
}

// ---------------------------------------------------------------------------
// Step B1c, fix round 3: a binary TEXT column cannot be FULLTEXT, a table the
// input dropped is gone, and DROP INDEX has shapes the lab has not measured.
//
// The expectations come from hand tests on MySQL 8.0.46 and 8.4.13 (the round
// 3 records: TEXT CHARACTER SET binary is a BLOB, ADD FULLTEXT on it fails
// with 1283 on MyISAM, COMPACT and DYNAMIC tables; ALTER on a dropped or
// renamed-away table fails with 1146; DROP INDEX with ALGORITHM=INSTANT fails
// with 1846 or 1845 for shapes the lab has not measured) and from the owner's
// decisions for the step.

/// The lab's tables with the default charset of `orders` changed to binary.
fn round3_lab_tables_with_a_binary_orders(engine: Option<&str>, options: &str) -> Schema {
    let text = lab_tables_with(engine, options);
    let at = text.find("CREATE TABLE orders").unwrap();
    let (users, orders) = text.split_at(at);
    let orders = orders.replacen("DEFAULT CHARSET=utf8mb4", "DEFAULT CHARSET=binary", 1);
    assert!(orders.contains("DEFAULT CHARSET=binary"));
    Schema::from_sql(&format!(
        "{users}{orders}ALTER TABLE orders ADD COLUMN body TEXT NULL;\n"
    ))
}

#[test]
fn a_fulltext_index_on_a_text_column_with_character_set_binary_is_unknown_on_every_known_row_format_and_myisam()
 {
    // MySQL makes TEXT CHARACTER SET binary a BLOB; ADD FULLTEXT then fails with
    // 1283 (by hand, both servers, MyISAM, COMPACT and DYNAMIC). The lab
    // measured a utf8mb4 TEXT column only, so kilit gives no measured answer.
    // $table->text('body')->charset('binary'); $table->fullText('body');
    for (engine, options) in [
        (Some("MyISAM"), ""),
        (None, " ROW_FORMAT=COMPRESSED"),
        (None, " KEY_BLOCK_SIZE=8"),
        (None, " ROW_FORMAT=COMPACT"),
        (None, " ROW_FORMAT=REDUNDANT"),
    ] {
        let schema = b1c_fulltext_table(engine, options, Some("TEXT CHARACTER SET binary"));
        for ((clause, _), _) in B1C_FULLTEXT_VARIANTS.iter().zip(0..) {
            let sql =
                format!("ALTER TABLE orders ADD FULLTEXT INDEX ft_orders_body (body){clause}");
            for server in [MYSQL_80, MYSQL_84] {
                assert_eq!(
                    b1c_answer(server, &schema, &sql),
                    UNKNOWN,
                    "{server}:{engine:?}{options}: {sql}"
                );
            }
        }
    }
}

#[test]
fn a_fulltext_index_on_a_text_column_of_a_binary_table_is_unknown_on_every_known_row_format_and_myisam()
 {
    // The column has no charset of its own, so it takes the table's: binary.
    for (engine, options) in [
        (Some("MyISAM"), ""),
        (None, " ROW_FORMAT=COMPRESSED"),
        (None, " KEY_BLOCK_SIZE=8"),
        (None, " ROW_FORMAT=COMPACT"),
        (None, " ROW_FORMAT=REDUNDANT"),
    ] {
        let schema = round3_lab_tables_with_a_binary_orders(engine, options);
        for server in [MYSQL_80, MYSQL_84] {
            assert_eq!(
                b1c_answer(
                    server,
                    &schema,
                    "ALTER TABLE orders ADD FULLTEXT INDEX ft_orders_body (body)"
                ),
                UNKNOWN,
                "{server}:{engine:?}{options}"
            );
        }
    }
}

#[test]
fn a_fulltext_index_on_a_plain_utf8mb4_text_column_keeps_the_measured_answer_next_to_a_binary_one()
{
    // Regression: the binary column is the exception, the utf8mb4 TEXT column
    // of the same kind of table keeps the lab's answer (COPY on MyISAM,
    // INPLACE with a rebuild on COMPACT; both tested above).
    let myisam = b1c_fulltext_table(Some("MyISAM"), "", Some("TEXT"));
    let compact = b1c_fulltext_table(None, " ROW_FORMAT=COMPACT", Some("TEXT"));
    for server in [MYSQL_80, MYSQL_84] {
        let sql = "ALTER TABLE orders ADD FULLTEXT INDEX ft_orders_body (body)";
        assert_eq!(b1c_answer(server, &myisam, sql), COPY, "{server}");
        assert_eq!(
            b1c_answer(server, &compact, sql),
            "INPLACE, rebuilds the table, writes wait",
            "{server}"
        );
    }
}

#[test]
fn a_fulltext_index_on_a_table_the_input_dropped_is_unknown() {
    // By hand on both servers: CREATE TABLE y; DROP TABLE y; ALTER TABLE y ADD
    // FULLTEXT ... gives 1146. Without --schema the table is gone, not unseen.
    let schema = Schema::from_sql("");
    for server in [MYSQL_80, MYSQL_84] {
        for sql in [
            "CREATE TABLE y (id INT PRIMARY KEY, note TEXT);
             DROP TABLE y;
             ALTER TABLE y ADD FULLTEXT INDEX ft (note)",
            "DROP TABLE y;
             ALTER TABLE y ADD FULLTEXT INDEX ft (note)",
            "CREATE TABLE y (id INT PRIMARY KEY, note TEXT);
             DROP TABLE y;
             ALTER TABLE y ADD FULLTEXT INDEX ft (note), ALGORITHM=INPLACE",
        ] {
            assert_eq!(b1c_answer(server, &schema, sql), UNKNOWN, "{server}: {sql}");
        }
    }
}

#[test]
fn a_fulltext_index_on_a_table_the_input_renamed_away_is_unknown() {
    // By hand on both servers: CREATE TABLE r; RENAME TABLE r TO z; ALTER TABLE
    // r ADD FULLTEXT ... gives 1146.
    let schema = Schema::from_sql("");
    for server in [MYSQL_80, MYSQL_84] {
        for sql in [
            "CREATE TABLE r (id INT PRIMARY KEY, note TEXT);
             RENAME TABLE r TO z;
             ALTER TABLE r ADD FULLTEXT INDEX ft (note)",
            "RENAME TABLE r TO z;
             ALTER TABLE r ADD FULLTEXT INDEX ft (note)",
        ] {
            assert_eq!(b1c_answer(server, &schema, sql), UNKNOWN, "{server}: {sql}");
        }
    }
}

#[test]
fn a_fulltext_index_on_the_new_name_of_a_renamed_table_keeps_the_measured_answer() {
    // The table is there under its new name: known, the lab's answer.
    let schema = Schema::from_sql("");
    for server in [MYSQL_80, MYSQL_84] {
        assert_eq!(
            b1c_answer(
                server,
                &schema,
                "CREATE TABLE r (id INT PRIMARY KEY, note TEXT) DEFAULT CHARSET=utf8mb4;
                 RENAME TABLE r TO z;
                 ALTER TABLE z ADD FULLTEXT INDEX ft (note)"
            ),
            "INPLACE, rebuilds the table, writes wait",
            "{server}"
        );
    }
}

#[test]
fn a_table_created_dropped_and_created_again_is_known_and_keeps_its_answer() {
    let schema = Schema::from_sql("");
    for server in [MYSQL_80, MYSQL_84] {
        assert_eq!(
            b1c_answer(
                server,
                &schema,
                "CREATE TABLE y (id INT PRIMARY KEY, note TEXT) DEFAULT CHARSET=utf8mb4;
                 DROP TABLE y;
                 CREATE TABLE y (id INT PRIMARY KEY, note TEXT) DEFAULT CHARSET=utf8mb4;
                 ALTER TABLE y ADD FULLTEXT INDEX ft (note)"
            ),
            "INPLACE, rebuilds the table, writes wait",
            "{server}"
        );
    }
}

#[test]
fn dropping_an_index_of_a_table_kilit_does_not_know_is_unknown_in_every_variant() {
    // DROP INDEX has shapes the lab has not measured: by hand, ALGORITHM=INSTANT
    // is 1846 for a NOT NULL UNIQUE index on a table without a primary key and
    // 1845 for a multi-column UNIQUE and for one of several FULLTEXT indexes,
    // so not even the forced variants may be answered for a table that may be
    // any of these.
    // $table->dropUnique('orders_email_unique');
    const CLAUSES: [&str; 6] = [
        "",
        ", ALGORITHM=INSTANT",
        ", ALGORITHM=INPLACE",
        ", ALGORITHM=INPLACE, LOCK=NONE",
        ", ALGORITHM=COPY",
        ", LOCK=NONE",
    ];
    for (how, schema) in b1c_tables_kilit_does_not_know() {
        for clause in CLAUSES {
            for index in ["ix_orders_amount", "orders_email_unique"] {
                let sql = format!("ALTER TABLE orders DROP INDEX {index}{clause}");
                for server in [MYSQL_80, MYSQL_84] {
                    assert_eq!(
                        b1c_answer(server, &schema, &sql),
                        UNKNOWN,
                        "{how}, {server}: {sql}"
                    );
                }
            }
        }
    }
}

#[test]
fn dropping_an_index_of_a_table_missing_from_a_dump_is_unknown_in_every_variant() {
    let dump = Schema::from_dump("CREATE TABLE unrelated (id INT PRIMARY KEY) ENGINE=InnoDB;");
    for clause in ["", ", ALGORITHM=INSTANT", ", ALGORITHM=INPLACE, LOCK=NONE"] {
        let sql = format!("ALTER TABLE orders DROP INDEX ix_orders_amount{clause}");
        for server in [MYSQL_80, MYSQL_84] {
            assert_eq!(b1c_answer(server, &dump, &sql), UNKNOWN, "{server}: {sql}");
        }
    }
}

#[test]
fn dropping_an_index_the_lab_measured_on_a_known_table_keeps_its_answer() {
    // Regression: drop-index on the lab's own table is INPLACE, no rebuild,
    // writes continue (matrix: drop-index).
    let schema = Schema::from_sql(&format!(
        "{LAB_TABLES}ALTER TABLE orders ADD INDEX idx_orders_amount (amount);\n"
    ));
    for server in [MYSQL_80, MYSQL_84] {
        assert_eq!(
            b1c_answer(
                server,
                &schema,
                "ALTER TABLE orders DROP INDEX idx_orders_amount"
            ),
            INPLACE,
            "{server}"
        );
    }
}

#[test]
fn the_reason_of_a_writes_wait_answer_on_a_table_kilit_does_not_know_names_the_hand_tests() {
    // Every answer the lab measured, or a hand test showed, for the tables it
    // may be makes writes wait or fails.
    for (how, schema) in b1c_tables_kilit_does_not_know() {
        for server in [MYSQL_80, MYSQL_84] {
            let (shared, _, reason) = b1c_depends(
                server,
                &schema,
                "ALTER TABLE orders ADD FULLTEXT INDEX ft_orders_note (note)",
            );
            assert_eq!(shared, Shared::WritesWait, "{how}, {server}");
            assert!(
                reason.contains(
                    "every answer the lab measured, or a hand test showed, for the tables it \
                     may be makes writes wait or fails: on a plain InnoDB table"
                ),
                "{how}, {server}: {reason}"
            );
        }
    }
}

// ---------------------------------------------------------------------------
// Foreign key actions: the lab measured every change of the twelve twins on a
// table with a key that cascades or sets NULL, and on the parent of such a key;
// each one is what its twin is, on both servers.

/// The scenarios with a foreign key action, by the prefix that names the key
/// and the twins they copy.
const KEY_ACTION_SCENARIOS: [(&str, &[&str]); 10] = [
    (
        "on-delete-cascade-",
        &[
            "add-nullable-column",
            "drop-column",
            "rename-column",
            "set-column-default",
            "int-to-bigint",
            "add-index",
            "add-unique-index",
            "drop-index",
            "add-fulltext-index",
            "make-column-nullable",
            "varchar-50-to-60",
            "drop-foreign-key",
        ],
    ),
    (
        "on-delete-set-null-",
        &[
            "add-nullable-column",
            "drop-column",
            "rename-column",
            "set-column-default",
            "int-to-bigint",
            "add-index",
            "add-unique-index",
            "drop-index",
            "add-fulltext-index",
            "make-column-nullable",
            "varchar-50-to-60",
            "drop-foreign-key",
        ],
    ),
    ("on-update-cascade-", &["add-index", "make-column-nullable"]),
    (
        "on-update-set-null-",
        &["add-index", "make-column-nullable"],
    ),
    (
        "on-delete-and-update-cascade-",
        &["add-index", "make-column-nullable"],
    ),
    ("foreign-key-", &["add-index", "make-column-nullable"]),
    (
        "on-delete-restrict-",
        &["add-index", "make-column-nullable"],
    ),
    (
        "on-delete-no-action-",
        &["add-index", "make-column-nullable"],
    ),
    (
        "cascade-parent-",
        &["add-index", "make-column-nullable", "add-nullable-column"],
    ),
    ("set-null-parent-", &["add-index"]),
];

/// Every pair of a foreign key action scenario and its twin.
fn key_action_pairs() -> Vec<(String, String)> {
    let mut pairs = Vec::new();
    for (prefix, twins) in KEY_ACTION_SCENARIOS {
        for twin in twins {
            pairs.push((format!("{prefix}{twin}"), (*twin).to_owned()));
        }
    }
    pairs
}

#[test]
fn the_lab_measured_forty_scenarios_with_a_foreign_key_action_on_both_servers() {
    assert_eq!(key_action_pairs().len(), 40);
    for results in Knowledge::built_in().servers() {
        for (id, twin) in key_action_pairs() {
            assert!(
                results.scenario(&id).is_some(),
                "{id} is missing on {}",
                results.server.name()
            );
            assert!(
                results.scenario(&twin).is_some(),
                "{twin} is missing on {}",
                results.server.name()
            );
        }
    }
}

#[test]
fn every_foreign_key_action_scenario_gave_what_its_twin_gave_in_all_six_runs() {
    // The lab's own observations: the key makes no difference to the verdict
    // or to any of the six runs, on either server.
    for results in Knowledge::built_in().servers() {
        let server = results.server.name();
        for (id, twin) in key_action_pairs() {
            let scenario = results.scenario(&id).unwrap();
            let twin = results.scenario(&twin).unwrap();
            for variant in Variant::ALL {
                assert_eq!(
                    observation(scenario, variant),
                    observation(twin, variant),
                    "{id} as {variant} on {server} differs from {}",
                    twin.id
                );
                assert_eq!(
                    scenario.verdict_for(variant),
                    twin.verdict_for(variant),
                    "{id} as {variant} on {server} differs from {}",
                    twin.id
                );
            }
        }
    }
}

#[test]
fn every_foreign_key_action_scenario_is_answered_in_the_case_of_its_twin_and_is_its_evidence() {
    for results in Knowledge::built_in().servers() {
        let server = results.server.name();
        for (id, twin_id) in key_action_pairs() {
            let scenario = results.scenario(&id).unwrap();
            let twin = results.scenario(&twin_id).unwrap();
            let own = predict(
                scenario,
                Variant::SERVER_DEFAULT,
                &server,
                &lab_schema(scenario),
            );
            let theirs = predict(twin, Variant::SERVER_DEFAULT, &server, &lab_schema(twin));
            let (
                Prediction::Measured {
                    case,
                    verdict,
                    evidence,
                    ..
                },
                Prediction::Measured {
                    case: twin_case,
                    verdict: twin_verdict,
                    ..
                },
            ) = (own, theirs)
            else {
                panic!("{id} or {twin_id} on {server} has no measured answer");
            };
            assert_eq!(case, twin_case, "{id} on {server}");
            assert_eq!(verdict, twin_verdict, "{id} on {server}");
            assert!(
                evidence.contains(&id.as_str()),
                "{id} is not in the evidence of its own answer on {server}: {evidence:?}"
            );
        }
    }
}

/// `users` and `orders` as mysqldump writes them, with `orders` holding the
/// foreign key `key` on `user_id` (a nullable column when the key sets NULL).
fn orders_with_a_key(key: &str) -> String {
    let nullable = if key.contains("SET NULL") {
        "NULL"
    } else {
        "NOT NULL"
    };
    format!(
        "CREATE TABLE `users` (
  `id` bigint unsigned NOT NULL AUTO_INCREMENT,
  `name` varchar(100) NOT NULL,
  PRIMARY KEY (`id`)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_0900_ai_ci;
CREATE TABLE `orders` (
  `id` bigint unsigned NOT NULL AUTO_INCREMENT,
  `user_id` bigint unsigned {nullable},
  `amount` int NOT NULL,
  `email` varchar(255) DEFAULT NULL,
  `note` varchar(50) DEFAULT NULL,
  `body` varchar(50) DEFAULT NULL,
  PRIMARY KEY (`id`),
  KEY `ix_amount` (`amount`),
  KEY `fk_orders_user` (`user_id`),
  CONSTRAINT `fk_orders_user` FOREIGN KEY (`user_id`) REFERENCES `users` (`id`) {key}
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_0900_ai_ci;
"
    )
}

#[test]
fn a_table_with_a_key_that_cascades_or_sets_null_keeps_the_answer_of_a_table_without_one() {
    // The six changes that let writes continue through INPLACE on the plain
    // tables, and the lab's twins of them, with the key in the dump. Not one
    // is different on a table with ON DELETE CASCADE, ON DELETE SET NULL,
    // ON UPDATE CASCADE or ON UPDATE SET NULL.
    for key in [
        "ON DELETE CASCADE",
        "ON DELETE SET NULL",
        "ON UPDATE CASCADE",
        "ON UPDATE SET NULL",
        "ON DELETE CASCADE ON UPDATE CASCADE",
    ] {
        let schema = orders_with_a_key(key);
        for (operation, answer, evidence) in [
            (
                "ADD INDEX ix_email (email)",
                INPLACE,
                "on-delete-cascade-add-index",
            ),
            (
                "ADD UNIQUE INDEX uq_email (email)",
                INPLACE,
                "on-delete-cascade-add-unique-index",
            ),
            (
                "DROP INDEX ix_amount",
                INPLACE,
                "on-delete-cascade-drop-index",
            ),
            (
                "MODIFY COLUMN amount INT NULL",
                "INPLACE, rebuilds the table, writes continue",
                "on-delete-cascade-make-column-nullable",
            ),
            (
                "MODIFY COLUMN note VARCHAR(60) NULL",
                INPLACE,
                "on-delete-cascade-varchar-50-to-60",
            ),
            (
                "DROP FOREIGN KEY fk_orders_user",
                INPLACE,
                "on-delete-cascade-drop-foreign-key",
            ),
        ] {
            let sql = format!("ALTER TABLE orders {operation}");
            let said = said(&schema, &sql);
            assert_eq!(said.answer, answer, "{key}: {operation}: {:?}", said.reason);
            assert!(
                said.evidence.contains(&evidence),
                "{key}: {operation}: {:?}",
                said.evidence
            );
        }
    }
}

#[test]
fn a_table_that_another_tables_key_cascades_or_sets_null_to_keeps_the_answer_too() {
    // order_items.order_id references orders with ON DELETE CASCADE or SET
    // NULL: the lab measured a new nullable column (INSTANT), an index and
    // making a column nullable on the parent.
    for (action, nullable) in [("CASCADE", "NOT NULL"), ("SET NULL", "NULL")] {
        let schema = format!(
            "{}CREATE TABLE `order_items` (
  `id` bigint unsigned NOT NULL AUTO_INCREMENT,
  `order_id` bigint unsigned {nullable},
  PRIMARY KEY (`id`),
  KEY `fk_order_items_order` (`order_id`),
  CONSTRAINT `fk_order_items_order` FOREIGN KEY (`order_id`) REFERENCES `orders` (`id`) \
  ON DELETE {action}
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_0900_ai_ci;
",
            orders_with_a_key("ON DELETE CASCADE")
        );
        for (operation, answer, evidence) in [
            (
                "ADD COLUMN coupon VARCHAR(255) NULL",
                INSTANT,
                "cascade-parent-add-nullable-column",
            ),
            (
                "ADD INDEX ix_email (email)",
                INPLACE,
                "cascade-parent-add-index",
            ),
            (
                "MODIFY COLUMN amount INT NULL",
                "INPLACE, rebuilds the table, writes continue",
                "cascade-parent-make-column-nullable",
            ),
        ] {
            let said = said(&schema, &format!("ALTER TABLE orders {operation}"));
            assert_eq!(
                said.answer, answer,
                "{action}: {operation}: {}",
                said.reason
            );
            assert!(
                said.evidence.contains(&evidence),
                "{action}: {operation}: {:?}",
                said.evidence
            );
        }
    }
}

#[test]
fn a_new_key_with_on_delete_cascade_set_null_or_set_default_copies_the_table_like_one_without() {
    // $table->foreign('user_id')->references('id')->on('users')->cascadeOnDelete();
    // ->nullOnDelete() on a nullable column; MySQL also accepts SET DEFAULT
    // (InnoDB takes it in ADD FOREIGN KEY and in CREATE TABLE).
    let nullable = "ALTER TABLE orders MODIFY COLUMN user_id BIGINT UNSIGNED NULL";
    for (setup, action, evidence) in [
        (
            vec![],
            "ON DELETE CASCADE",
            "add-foreign-key-on-delete-cascade",
        ),
        (
            vec![nullable],
            "ON DELETE SET NULL",
            "add-foreign-key-on-delete-set-null",
        ),
        (
            vec![],
            "ON DELETE SET DEFAULT",
            "add-foreign-key-on-delete-set-default",
        ),
    ] {
        let sql = format!(
            "SET foreign_key_checks = 1;
             ALTER TABLE orders ADD CONSTRAINT fk_orders_user FOREIGN KEY (user_id) \
             REFERENCES users (id) {action}"
        );
        let said = said(&lab_after(&setup), &sql);
        assert_eq!(said.answer, COPY, "{action}");
        assert!(
            said.evidence.contains(&evidence),
            "{action}: {:?}",
            said.evidence
        );
        assert_eq!(said.case, Some("add-foreign-key"), "{action}");
    }
}

#[test]
fn a_new_key_with_on_delete_cascade_or_set_null_with_foreign_key_checks_off_runs_inplace() {
    let nullable = "ALTER TABLE orders MODIFY COLUMN user_id BIGINT UNSIGNED NULL";
    for (setup, action, evidence) in [
        (
            vec![],
            "ON DELETE CASCADE",
            "add-foreign-key-checks-off-on-delete-cascade",
        ),
        (
            vec![nullable],
            "ON DELETE SET NULL",
            "add-foreign-key-checks-off-on-delete-set-null",
        ),
    ] {
        let sql = format!(
            "SET foreign_key_checks = 0;
             ALTER TABLE orders ADD CONSTRAINT fk_orders_user FOREIGN KEY (user_id) \
             REFERENCES users (id) {action}"
        );
        let said = said(&lab_after(&setup), &sql);
        assert_eq!(said.answer, INPLACE, "{action}");
        assert!(
            said.evidence.contains(&evidence),
            "{action}: {:?}",
            said.evidence
        );
        assert_eq!(said.case, Some("add-foreign-key-checks-off"), "{action}");
    }
}

#[test]
fn a_new_key_that_sets_a_not_null_column_to_null_on_delete_fails_with_1830_with_checks_on_or_off() {
    // $table->foreignId('user_id')->constrained()->nullOnDelete() on the
    // NOT NULL column: the lab's add-foreign-key-set-null-not-null and
    // add-foreign-key-checks-off-set-null-not-null, in all six runs, on both
    // servers.
    let key = "ALTER TABLE orders ADD CONSTRAINT fk_orders_user FOREIGN KEY (user_id) \
               REFERENCES users (id) ON DELETE SET NULL";
    for (set, evidence) in [
        (
            "SET foreign_key_checks = 1;",
            "add-foreign-key-set-null-not-null",
        ),
        (
            "SET foreign_key_checks = 0;",
            "add-foreign-key-checks-off-set-null-not-null",
        ),
    ] {
        for clause in [
            "",
            ", ALGORITHM=INSTANT",
            ", ALGORITHM=INPLACE",
            ", ALGORITHM=INPLACE, LOCK=NONE",
            ", ALGORITHM=COPY",
            ", LOCK=NONE",
        ] {
            let said = said(LAB_TABLES, &format!("{set}\n{key}{clause}"));
            assert_eq!(said.answer, FAILS_1830, "{set}{clause}");
            assert_eq!(said.evidence, [evidence], "{set}{clause}");
        }
    }
}

#[test]
fn a_key_that_fails_with_1830_leaves_the_tables_as_they_were() {
    // The statement after a measured failure: orders is the table it was, so
    // a new column is INSTANT, the column takes an index, and dropping it
    // does not fail with 1828 (no key is there); the index goes with it.
    let sql = "ALTER TABLE orders ADD CONSTRAINT fk_orders_user FOREIGN KEY (user_id) \
               REFERENCES users (id) ON DELETE SET NULL;
               ALTER TABLE orders ADD COLUMN coupon VARCHAR(255) NULL;
               ALTER TABLE orders ADD INDEX ix_user (user_id);
               ALTER TABLE orders DROP COLUMN user_id;";
    assert_eq!(
        case_answers(LAB_TABLES, sql),
        [
            FAILS_1830,
            INSTANT,
            INPLACE,
            "INPLACE, rebuilds the table, writes continue"
        ]
    );
}

#[test]
fn a_key_that_sets_null_on_delete_and_is_accepted_makes_its_column_part_of_the_table() {
    // The key on a NULL column is accepted, and dropping the column then
    // fails with 1828 as for any other foreign key column.
    let sql = "ALTER TABLE orders MODIFY COLUMN user_id BIGINT UNSIGNED NULL;
               ALTER TABLE orders ADD CONSTRAINT fk_orders_user FOREIGN KEY (user_id) \
               REFERENCES users (id) ON DELETE SET NULL;
               ALTER TABLE orders DROP COLUMN user_id;";
    let answers = case_answers(LAB_TABLES, sql);
    assert_eq!(answers[1], COPY);
    assert_eq!(answers[2], "fails with error 1828");
}

const SET_NULL_KEY: &str = "ALTER TABLE orders ADD CONSTRAINT fk_orders_user FOREIGN KEY (user_id) \
                            REFERENCES users (id) ON DELETE SET NULL";

#[test]
fn a_set_null_key_on_a_not_null_column_is_unknown_with_a_note_on_a_table_kilit_does_not_know() {
    // The 1830 measurement is a candidate there: the table may have a NULL
    // column, so the answer is not a failure, and the note says what the lab
    // measured.
    for (how, schema) in tables_kilit_does_not_know() {
        for server in [MYSQL_80, MYSQL_84] {
            let said = said_on(server, &schema, SET_NULL_KEY);
            assert_eq!(said.answer, UNKNOWN, "{how} on {server}");
            assert!(
                said.has_note_with(&["fails with error 1830", "add-foreign-key-set-null-not-null"]),
                "{how} on {server}: {:?}",
                said.notes
            );
        }
    }
}

#[test]
fn a_set_null_key_on_a_nullable_column_has_no_1830_note() {
    // The column is NULL, so the exception does not apply and is no note on
    // a table kilit knows.
    let schema = lab_after(&["ALTER TABLE orders MODIFY COLUMN user_id BIGINT UNSIGNED NULL"]);
    let said = said(&schema, SET_NULL_KEY);
    assert_eq!(said.answer, COPY);
    assert!(!said.has_note_with(&["1830"]), "{:?}", said.notes);
}

#[test]
fn a_key_that_sets_null_on_update_or_on_two_columns_or_next_to_other_changes_is_not_the_measured_shape()
 {
    // The lab measured one column and ON DELETE SET NULL. Anything else
    // keeps "the lab has not measured that".
    let two = "CREATE TABLE users (a INT NOT NULL, b INT NOT NULL, PRIMARY KEY (a, b)) \
               ENGINE=InnoDB DEFAULT CHARSET=utf8mb4;
        CREATE TABLE orders (id INT PRIMARY KEY, a INT NOT NULL, b INT NOT NULL, \
        KEY k (a, b)) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4;";
    for sql in [
        "ALTER TABLE orders ADD CONSTRAINT f FOREIGN KEY (a, b) REFERENCES users (a, b) \
         ON DELETE SET NULL",
        "ALTER TABLE orders ADD CONSTRAINT f FOREIGN KEY (a, b) REFERENCES users (a, b) \
         ON UPDATE SET NULL",
    ] {
        assert_answer("two columns", two, sql, UNKNOWN);
    }
    // Without the action the same key is the plain one.
    let sql = "ALTER TABLE orders ADD CONSTRAINT f FOREIGN KEY (a, b) REFERENCES users (a, b)";
    assert_answer("two columns, no action", two, sql, COPY);
}

#[test]
fn making_the_column_of_a_set_null_key_not_null_is_unknown_with_1830_and_the_table_stays() {
    // By hand on MySQL 8.0.46 and 8.4.13: MODIFY and CHANGE (also with
    // ALGORITHM=COPY) that make the column of a SET NULL key NOT NULL fail
    // with 1830, with ON DELETE SET NULL and with ON UPDATE SET NULL, and
    // leave the table as it was.
    for key in ["ON DELETE SET NULL", "ON UPDATE SET NULL"] {
        let schema = orders_with_a_key(key);
        for statement in [
            "ALTER TABLE orders MODIFY COLUMN user_id BIGINT UNSIGNED NOT NULL",
            "ALTER TABLE orders CHANGE COLUMN user_id user_id BIGINT UNSIGNED NOT NULL",
            "ALTER TABLE orders MODIFY COLUMN user_id BIGINT UNSIGNED NOT NULL, ALGORITHM=COPY",
        ] {
            let sql =
                format!("{statement};\nALTER TABLE orders ADD COLUMN coupon VARCHAR(255) NULL;");
            for server in [MYSQL_80, MYSQL_84] {
                let results = Knowledge::built_in().server(server).unwrap();
                let findings = check(&parse(&sql), &Schema::from_dump(&schema), results);
                match &findings[0].prediction {
                    Prediction::Unknown { reason, .. } => assert!(
                        reason.contains("1830"),
                        "{server}: {key}: {statement}: {reason}"
                    ),
                    other => panic!("{server}: {key}: {statement}: {other:?}"),
                }
                // The table is as it was: the next answer is the lab's.
                assert_eq!(
                    b1c_answer(server, &Schema::from_dump(&schema), &sql),
                    INSTANT,
                    "{server}: {key}: {statement}"
                );
            }
        }
    }
}

#[test]
fn making_the_column_of_a_cascade_key_not_null_is_not_refused() {
    // By hand: the control with ON DELETE CASCADE is accepted (the table is
    // rebuilt), so it is no 1830 and not unknown for that reason.
    let schema = orders_with_a_key("ON DELETE CASCADE")
        .replace("NOT NULL,\n  `amount`", "NULL,\n  `amount`");
    let sql = "ALTER TABLE orders MODIFY COLUMN user_id BIGINT UNSIGNED NOT NULL";
    for server in [MYSQL_80, MYSQL_84] {
        let results = Knowledge::built_in().server(server).unwrap();
        let findings = check(&parse(sql), &Schema::from_dump(&schema), results);
        if let Prediction::Unknown { reason, .. } = &findings[0].prediction {
            assert!(!reason.contains("1830"), "{server}: {reason}");
        }
    }
}

#[test]
fn a_create_table_with_a_set_null_key_on_a_not_null_column_is_refused_and_loses_the_table() {
    // By hand on MySQL 8.0.46 and 8.4.13: CREATE TABLE items with ON DELETE
    // SET NULL or ON UPDATE SET NULL on a NOT NULL column fails with 1830 and
    // the table is not there, also with foreign_key_checks = 0.
    let users = "CREATE TABLE users (id BIGINT UNSIGNED AUTO_INCREMENT PRIMARY KEY, \
                 name VARCHAR(100) NOT NULL) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4;\n";
    for (checks, action) in [
        ("", "ON DELETE SET NULL"),
        ("", "ON UPDATE SET NULL"),
        ("SET foreign_key_checks = 0;\n", "ON DELETE SET NULL"),
    ] {
        let sql = format!(
            "{checks}CREATE TABLE items (id BIGINT UNSIGNED AUTO_INCREMENT PRIMARY KEY, \
             user_id BIGINT UNSIGNED NOT NULL, CONSTRAINT fk_items_user FOREIGN KEY (user_id) \
             REFERENCES users (id) {action}) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4;
             ALTER TABLE items ADD COLUMN coupon VARCHAR(255) NULL;"
        );
        for server in [MYSQL_80, MYSQL_84] {
            let reason = b1c_reason(server, &Schema::from_sql(users), &sql);
            assert!(
                reason.contains("items") && reason.contains("kilit lost track of"),
                "{server}: {action}: {reason}"
            );
        }
    }
}

#[test]
fn a_create_table_with_a_set_null_key_on_a_null_column_or_a_set_default_key_is_kept() {
    // By hand: the control (SET NULL on a NULL column) and SET DEFAULT, also
    // on a NOT NULL column with a default, are accepted by CREATE TABLE.
    let users = "CREATE TABLE users (id BIGINT UNSIGNED AUTO_INCREMENT PRIMARY KEY, \
                 name VARCHAR(100) NOT NULL) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4;\n";
    for (column, action) in [
        ("user_id BIGINT UNSIGNED NULL", "ON DELETE SET NULL"),
        ("user_id BIGINT UNSIGNED NULL", "ON DELETE SET DEFAULT"),
        ("user_id BIGINT UNSIGNED NULL", "ON UPDATE SET DEFAULT"),
        (
            "user_id BIGINT UNSIGNED NOT NULL DEFAULT 1",
            "ON DELETE SET DEFAULT",
        ),
    ] {
        let sql = format!(
            "CREATE TABLE items (id BIGINT UNSIGNED AUTO_INCREMENT PRIMARY KEY, {column}, \
             CONSTRAINT fk_items_user FOREIGN KEY (user_id) REFERENCES users (id) {action}) \
             ENGINE=InnoDB DEFAULT CHARSET=utf8mb4;
             ALTER TABLE items ADD COLUMN coupon VARCHAR(255) NULL;"
        );
        let answers = case_answers(users, &sql);
        assert_eq!(answers[1], INSTANT, "{column}, {action}");
    }
}

// ---------------------------------------------------------------------------
// FULLTEXT on a known InnoDB DYNAMIC table: the shapes the lab measured, the
// binary TEXT column MySQL refuses (1283), and an explicit ASC or DESC (1221).

const FULLTEXT_REBUILD_WAIT: &str = "INPLACE, rebuilds the table, writes wait";
const FULLTEXT_SECOND: &str = "INPLACE, no rebuild, writes wait";
const FAILS_1283: &str = "fails with error 1283";
const FAILS_1221: &str = "fails with error 1221";

/// `posts` with `columns` and `options` after the closing parenthesis.
fn posts(columns: &str, options: &str) -> String {
    format!("CREATE TABLE `posts` (`id` int NOT NULL, {columns}, PRIMARY KEY (`id`)) {options};\n")
}

const UTF8MB4: &str = "ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_0900_ai_ci";

#[test]
fn a_fulltext_index_on_one_or_two_text_columns_gets_the_lab_answer_on_a_dynamic_table() {
    // $table->fullText('body'); $table->fullText(['title', 'body']);
    for (columns, index) in [
        ("`title` varchar(100) DEFAULT NULL", "`title`"),
        ("`body` text", "`body`"),
        ("`body` mediumtext", "`body`"),
        ("`body` longtext", "`body`"),
        (
            "`title` varchar(100) DEFAULT NULL, `body` text",
            "`title`, `body`",
        ),
        (
            "`title` varchar(100), `body` varchar(200)",
            "`title`, `body`",
        ),
        ("`title` text, `body` longtext", "`title`, `body`"),
        ("`title` mediumtext, `body` varchar(10)", "`title`, `body`"),
    ] {
        let schema = posts(columns, UTF8MB4);
        let sql = format!("ALTER TABLE posts ADD FULLTEXT INDEX ft ({index})");
        let said = said(&schema, &sql);
        assert_eq!(said.answer, FULLTEXT_REBUILD_WAIT, "{columns}");
        assert_eq!(said.case, Some("add-fulltext-index"), "{columns}");
        for scenario in [
            "add-fulltext-index",
            "add-fulltext-index-text",
            "add-fulltext-index-mediumtext",
            "add-fulltext-index-longtext",
            "add-fulltext-index-two-columns",
        ] {
            assert!(
                said.evidence.contains(&scenario),
                "{columns}: {:?}",
                said.evidence
            );
        }
    }
}

#[test]
fn a_fulltext_index_on_a_column_in_a_character_set_kilit_can_tell_is_measured_whatever_the_set() {
    // The table's character set, or the column's own.
    for schema in [
        posts("`body` text", "ENGINE=InnoDB DEFAULT CHARSET=latin1"),
        posts(
            "`body` text CHARACTER SET latin1 COLLATE latin1_swedish_ci",
            UTF8MB4,
        ),
        posts("`body` text COLLATE utf8mb4_unicode_ci", UTF8MB4),
    ] {
        assert_answer(
            &schema,
            &schema,
            "ALTER TABLE posts ADD FULLTEXT INDEX ft (body)",
            FULLTEXT_REBUILD_WAIT,
        );
    }
}

#[test]
fn a_fulltext_index_on_a_text_column_in_the_binary_character_set_fails_with_1283() {
    // By hand on MySQL 8.0.46 and 8.4.13: TEXT CHARACTER SET binary is a
    // BLOB, and a table with DEFAULT CHARSET=binary makes its TEXT columns
    // BLOBs; FULLTEXT on either is 1283.
    for (label, schema) in [
        (
            "the column's set",
            posts("`body` text CHARACTER SET binary", UTF8MB4),
        ),
        (
            "the table's set",
            posts("`body` text", "ENGINE=InnoDB DEFAULT CHARSET=binary"),
        ),
        (
            "the column's set, backticks and capitals",
            posts("`body` TEXT CHARACTER SET `BINARY` NULL", UTF8MB4),
        ),
    ] {
        let said = said(&schema, "ALTER TABLE posts ADD FULLTEXT INDEX ft (body)");
        assert_eq!(said.answer, FAILS_1283, "{label}");
        assert_eq!(said.evidence, ["add-fulltext-index-binary-text"], "{label}");
        assert_eq!(said.case, Some("add-fulltext-index"), "{label}");
    }
}

#[test]
fn a_fulltext_index_in_the_binary_character_set_fails_in_all_six_runs() {
    let schema = posts("`body` text CHARACTER SET binary", UTF8MB4);
    for clause in [
        "",
        ", ALGORITHM=INSTANT",
        ", ALGORITHM=INPLACE",
        ", ALGORITHM=INPLACE, LOCK=NONE",
        ", ALGORITHM=COPY",
        ", LOCK=NONE",
    ] {
        let sql = format!("ALTER TABLE posts ADD FULLTEXT INDEX ft (body){clause}");
        assert_answer(clause, &schema, &sql, FAILS_1283);
    }
}

#[test]
fn a_fulltext_index_that_fails_with_1283_leaves_the_table_as_it_was() {
    // The index is not there afterwards: the next one is the table's first
    // (it rebuilds), not a second.
    let schema = posts(
        "`body` text CHARACTER SET binary, `title` varchar(50)",
        UTF8MB4,
    );
    let sql = "ALTER TABLE posts ADD FULLTEXT INDEX ft (body);
               ALTER TABLE posts ADD FULLTEXT INDEX ft2 (title);";
    assert_eq!(
        case_answers(&schema, sql),
        [FAILS_1283, FULLTEXT_REBUILD_WAIT]
    );
}

#[test]
fn a_fulltext_index_on_a_binary_varchar_blob_or_varbinary_is_unknown_and_names_the_1283_measurement()
 {
    // Not measured: kilit says what was, the TEXT column in the binary set.
    for (label, schema) in [
        (
            "VARCHAR in the binary set",
            posts("`body` varchar(50) CHARACTER SET binary", UTF8MB4),
        ),
        (
            "VARCHAR in a binary table",
            posts("`body` varchar(50)", "ENGINE=InnoDB DEFAULT CHARSET=binary"),
        ),
        ("BLOB", posts("`body` blob", UTF8MB4)),
        ("VARBINARY", posts("`body` varbinary(50)", UTF8MB4)),
        (
            "MEDIUMTEXT in the binary set",
            posts("`body` mediumtext CHARACTER SET binary", UTF8MB4),
        ),
    ] {
        for server in [MYSQL_80, MYSQL_84] {
            let reason = b1c_reason(
                server,
                &Schema::from_sql(&schema),
                "ALTER TABLE posts ADD FULLTEXT INDEX ft (body)",
            );
            assert!(
                reason.contains("1283") && reason.contains("FULLTEXT"),
                "{label} on {server}: {reason}"
            );
        }
    }
}

#[test]
fn a_fulltext_index_on_a_shape_the_lab_did_not_measure_is_unknown_on_a_dynamic_table() {
    // CHAR, TINYTEXT, three columns, other types, and a character set kilit
    // cannot tell (a table that writes none).
    for (label, schema, index) in [
        ("CHAR", posts("`code` char(10)", UTF8MB4), "code"),
        ("TINYTEXT", posts("`body` tinytext", UTF8MB4), "body"),
        (
            "three columns",
            posts("`a` varchar(10), `b` text, `c` text", UTF8MB4),
            "a, b, c",
        ),
        (
            "a VARCHAR and a CHAR",
            posts("`a` varchar(10), `b` char(10)", UTF8MB4),
            "a, b",
        ),
        ("JSON", posts("`body` json", UTF8MB4), "body"),
        ("an integer", posts("`body` int", UTF8MB4), "body"),
        (
            "no character set written",
            posts("`body` text", "ENGINE=InnoDB"),
            "body",
        ),
        (
            "no table options at all",
            "CREATE TABLE posts (id INT PRIMARY KEY, body TEXT);".to_owned(),
            "body",
        ),
    ] {
        for server in [MYSQL_80, MYSQL_84] {
            let reason = b1c_reason(
                server,
                &Schema::from_sql(&schema),
                &format!("ALTER TABLE posts ADD FULLTEXT INDEX ft ({index})"),
            );
            assert!(
                reason.contains("FULLTEXT") && reason.contains("measured"),
                "{label} on {server}: {reason}"
            );
        }
    }
}

#[test]
fn a_fulltext_index_names_the_column_character_set_it_could_not_tell() {
    let reason = case_reason(
        "CREATE TABLE posts (id INT PRIMARY KEY, body TEXT);",
        "ALTER TABLE posts ADD FULLTEXT INDEX ft (body)",
    );
    assert!(
        reason.contains("posts") && reason.contains("character set"),
        "{reason}"
    );
}

#[test]
fn a_second_fulltext_index_takes_the_same_shapes() {
    // The table has a FULLTEXT index on `title`.
    let with_ft = |columns: &str, options: &str| {
        format!(
            "CREATE TABLE `posts` (`id` int NOT NULL, `title` varchar(100), {columns}, \
             PRIMARY KEY (`id`), FULLTEXT KEY `ft_title` (`title`)) {options};\n"
        )
    };
    for (columns, index) in [
        ("`body` varchar(50)", "body"),
        ("`body` text", "body"),
        ("`body` longtext", "body"),
        ("`body` text", "title, body"),
    ] {
        let schema = with_ft(columns, UTF8MB4);
        assert_answer(
            columns,
            &schema,
            &format!("ALTER TABLE posts ADD FULLTEXT INDEX ft2 ({index})"),
            FULLTEXT_SECOND,
        );
    }
    for (label, columns, index, options) in [
        ("CHAR", "`body` char(10)", "body", UTF8MB4),
        (
            "three columns",
            "`b` text, `c` text",
            "title, b, c",
            UTF8MB4,
        ),
        ("no character set", "`body` text", "body", "ENGINE=InnoDB"),
    ] {
        let schema = with_ft(columns, options);
        assert_answer(
            label,
            &schema,
            &format!("ALTER TABLE posts ADD FULLTEXT INDEX ft2 ({index})"),
            UNKNOWN,
        );
    }
}

#[test]
fn a_fulltext_index_on_a_table_kilit_does_not_know_still_makes_writes_wait_and_notes_1283() {
    // Whatever the shape, every table it may be makes writes wait: no
    // --schema, a lost table, and 1283 is one thing the column may bring.
    for (how, schema) in tables_kilit_does_not_know() {
        for index in ["note", "note, email", "note, email, id"] {
            let said = said(
                &schema,
                &format!("ALTER TABLE orders ADD FULLTEXT INDEX ft ({index})"),
            );
            assert_eq!(
                said.answer, "depends on the table: writes wait",
                "{how}: {index}"
            );
            if !index.contains(',') {
                assert!(
                    said.has_note_with(&[
                        "fails with error 1283",
                        "add-fulltext-index-binary-text"
                    ]),
                    "{how}: {index}: {:?}",
                    said.notes
                );
            }
        }
    }
}

#[test]
fn myisam_and_row_format_tables_without_a_character_set_stay_unknown_for_fulltext() {
    // By decision: the character set of a column that names none cannot be
    // told, whatever the engine or the row format.
    for options in [
        "ENGINE=MyISAM",
        "ENGINE=InnoDB ROW_FORMAT=COMPACT",
        "ENGINE=InnoDB ROW_FORMAT=COMPRESSED",
        "ENGINE=InnoDB ROW_FORMAT=REDUNDANT",
        "ENGINE=InnoDB ROW_FORMAT=DYNAMIC",
    ] {
        let schema = posts("`body` text", options);
        assert_answer(
            options,
            &schema,
            "ALTER TABLE posts ADD FULLTEXT INDEX ft (body)",
            UNKNOWN,
        );
    }
    // With a character set the MyISAM table has its measured answer.
    let schema = posts("`body` text", "ENGINE=MyISAM DEFAULT CHARSET=utf8mb4");
    assert_answer(
        "MyISAM with a set",
        &schema,
        "ALTER TABLE posts ADD FULLTEXT INDEX ft (body)",
        "COPY, copies every row, writes wait",
    );
}

#[test]
fn an_explicit_asc_or_desc_in_a_fulltext_index_fails_with_1221_in_all_six_runs() {
    // $table->rawIndex() is not FULLTEXT, but a hand-written migration can:
    // MySQL refuses any written order on a FULLTEXT key part.
    for order in ["ASC", "DESC", "desc", "Asc"] {
        for clause in [
            "",
            ", ALGORITHM=INSTANT",
            ", ALGORITHM=INPLACE",
            ", ALGORITHM=INPLACE, LOCK=NONE",
            ", ALGORITHM=COPY",
            ", LOCK=NONE",
        ] {
            let sql = format!(
                "ALTER TABLE orders ADD FULLTEXT INDEX ft_orders_note (note {order}){clause}"
            );
            let said = said(LAB_TABLES, &sql);
            assert_eq!(said.answer, FAILS_1221, "{sql}");
            assert_eq!(
                said.evidence,
                ["add-fulltext-index-desc", "add-fulltext-index-asc"],
                "{sql}"
            );
            assert_eq!(said.case, Some("add-fulltext-index-with-order"), "{sql}");
        }
    }
    for form in [
        "ALTER TABLE orders ADD FULLTEXT KEY ft (note DESC)",
        "ALTER TABLE `orders` ADD FULLTEXT `ft` (`note` DESC)",
        "alter table orders add fulltext index ft (note desc)",
    ] {
        assert_answer(form, LAB_TABLES, form, FAILS_1221);
    }
}

#[test]
fn an_ordered_fulltext_index_fails_with_1221_on_every_kind_of_table() {
    // The error comes from the statement alone: by hand on MySQL 8.0.46 and
    // 8.4.13 a MyISAM and a COMPACT table refuse it alike, so a table kilit
    // does not know, and a new table, get it too.
    let sql = "ALTER TABLE orders ADD FULLTEXT INDEX ft (note DESC)";
    for options in [
        "ENGINE=MyISAM DEFAULT CHARSET=utf8mb4",
        "ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 ROW_FORMAT=COMPACT",
        "ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 ROW_FORMAT=COMPRESSED",
        "ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 ROW_FORMAT=REDUNDANT",
        "ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 ROW_FORMAT=DYNAMIC",
    ] {
        let schema =
            format!("CREATE TABLE orders (id INT PRIMARY KEY, note VARCHAR(50)) {options};");
        let said = said(&schema, sql);
        assert_eq!(said.answer, FAILS_1221, "{options}");
        assert_eq!(
            said.case,
            Some("add-fulltext-index-with-order"),
            "{options}"
        );
    }
    // A table kilit lost, one it never saw (no --schema), and a new table.
    for (how, schema) in tables_kilit_does_not_know() {
        let said = said(&schema, sql);
        assert_eq!(said.answer, FAILS_1221, "{how}");
    }
    let created = "CREATE TABLE orders (id INT PRIMARY KEY, note VARCHAR(50));";
    assert_eq!(
        case_answers("", &format!("{created}\n{sql};")),
        ["not checked", FAILS_1221]
    );
}

#[test]
fn an_ordered_fulltext_index_on_a_table_the_dump_lacks_is_unknown() {
    // The dump decides: a table it does not have is not there, so not even a
    // failure from the statement alone is given.
    let schema = Schema::from_dump(LAB_TABLES);
    for server in [MYSQL_80, MYSQL_84] {
        assert_eq!(
            b1c_answer(
                server,
                &schema,
                "ALTER TABLE nosuch ADD FULLTEXT INDEX ft (note DESC)"
            ),
            UNKNOWN,
            "{server}"
        );
    }
}

#[test]
fn an_ordered_fulltext_index_notes_what_was_measured_where() {
    let said = said(
        "CREATE TABLE orders (id INT PRIMARY KEY, note VARCHAR(50)) ENGINE=MyISAM DEFAULT CHARSET=utf8mb4;",
        "ALTER TABLE orders ADD FULLTEXT INDEX ft (note DESC)",
    );
    assert!(
        said.has_note_with(&["MyISAM", "COMPACT"]),
        "{:?}",
        said.notes
    );
}

#[test]
fn a_fulltext_index_that_fails_with_1221_leaves_the_table_as_it_was() {
    // The ordered form is refused and not followed: the next FULLTEXT index
    // is the table's first, so it rebuilds the table.
    let sql = "ALTER TABLE orders ADD FULLTEXT INDEX ft_a (note DESC);
               ALTER TABLE orders ADD FULLTEXT INDEX ft_b (note);
               ALTER TABLE orders ADD FULLTEXT INDEX ft_c (email);";
    assert_eq!(
        case_answers(LAB_TABLES, sql),
        [FAILS_1221, FULLTEXT_REBUILD_WAIT, FULLTEXT_SECOND]
    );
}

#[test]
fn a_column_named_desc_or_asc_is_no_order_on_a_fulltext_key_part() {
    let schema = "CREATE TABLE orders (id INT PRIMARY KEY, `desc` VARCHAR(50), `asc` VARCHAR(50)) \
                  ENGINE=InnoDB DEFAULT CHARSET=utf8mb4;";
    for sql in [
        "ALTER TABLE orders ADD FULLTEXT INDEX ft (`desc`)",
        "ALTER TABLE orders ADD FULLTEXT INDEX ft (`asc`)",
        "ALTER TABLE orders ADD FULLTEXT INDEX ft (`desc`, `asc`)",
        "alter table `orders` add fulltext `orders_desc_fulltext`(`desc`)",
    ] {
        assert_answer(sql, schema, sql, FULLTEXT_REBUILD_WAIT);
    }
}

#[test]
fn a_btree_or_plain_index_with_desc_keeps_its_answer() {
    // By hand on MySQL 8.0.46 and 8.4.13: DESC with USING BTREE, or on a plain
    // key, is accepted.
    for sql in [
        "ALTER TABLE orders ADD INDEX ib (amount DESC) USING BTREE",
        "ALTER TABLE orders ADD INDEX ib (amount DESC)",
        "ALTER TABLE orders ADD INDEX ib (amount ASC)",
        "ALTER TABLE orders ADD UNIQUE INDEX uq (email DESC)",
        "CREATE INDEX ib ON orders (amount DESC)",
    ] {
        let answer = case_last_answer(LAB_TABLES, sql);
        assert!(answer == INPLACE || answer == UNKNOWN, "{sql}: {answer}");
        assert_ne!(answer, FAILS_1221, "{sql}");
    }
    assert_answer(
        "btree",
        LAB_TABLES,
        "ALTER TABLE orders ADD INDEX ib (amount DESC) USING BTREE",
        INPLACE,
    );
}

#[test]
fn an_ordered_spatial_or_hash_index_is_unknown_and_names_1221_and_is_not_followed() {
    // By hand on MySQL 8.0.46 and 8.4.13: SPATIAL (p DESC) and USING HASH
    // with ASC or DESC are 1221 too.
    let schema = "CREATE TABLE g (id INT PRIMARY KEY, p POINT NOT NULL SRID 0, a INT) \
                  ENGINE=InnoDB DEFAULT CHARSET=utf8mb4;";
    for sql in [
        "ALTER TABLE g ADD SPATIAL INDEX sp (p DESC)",
        "ALTER TABLE g ADD SPATIAL INDEX sp (p ASC)",
        "ALTER TABLE g ADD INDEX ih (a DESC) USING HASH",
        "ALTER TABLE g ADD INDEX ih (a ASC) USING HASH",
        "ALTER TABLE g ADD INDEX ih USING HASH (a DESC)",
    ] {
        for server in [MYSQL_80, MYSQL_84] {
            let reason = b1c_reason(server, &Schema::from_sql(schema), sql);
            assert!(reason.contains("1221"), "{server}: {sql}: {reason}");
        }
        // Not followed: the next index of the same name is not a repeat.
        let twice = format!("{sql};\n{sql};");
        let reasons: Vec<String> = [MYSQL_80, MYSQL_84]
            .iter()
            .map(|server| b1c_reason(server, &Schema::from_sql(schema), &twice))
            .collect();
        for reason in reasons {
            assert!(reason.contains("1221"), "{sql}: {reason}");
        }
    }
}

#[test]
fn create_fulltext_index_with_an_order_or_on_a_binary_text_column_is_unknown_with_the_error_and_not_followed()
 {
    // By hand on MySQL 8.0.46 and 8.4.13: CREATE FULLTEXT INDEX (note DESC) is
    // 1221, and on a binary TEXT column 1283; the table stays as it was.
    let binary = posts(
        "`body` text CHARACTER SET binary, `title` varchar(50)",
        UTF8MB4,
    );
    for (schema, create, code, next) in [
        (
            LAB_TABLES.to_owned(),
            "CREATE FULLTEXT INDEX ft_a ON orders (note DESC)",
            "1221",
            "ALTER TABLE orders ADD FULLTEXT INDEX ft_b (note)",
        ),
        (
            LAB_TABLES.to_owned(),
            "CREATE FULLTEXT INDEX ft_a ON orders (note ASC)",
            "1221",
            "ALTER TABLE orders ADD FULLTEXT INDEX ft_b (note)",
        ),
        (
            binary,
            "CREATE FULLTEXT INDEX ft_a ON posts (body)",
            "1283",
            "ALTER TABLE posts ADD FULLTEXT INDEX ft_b (title)",
        ),
    ] {
        for server in [MYSQL_80, MYSQL_84] {
            let sql = format!("{create};\n{next};");
            let schema = Schema::from_sql(&schema);
            let results = Knowledge::built_in().server(server).unwrap();
            let findings = check(&parse(&sql), &schema, results);
            match &findings[0].prediction {
                Prediction::Unknown { reason, .. } => {
                    assert!(reason.contains(code), "{server}: {create}: {reason}");
                }
                other => panic!("{server}: {create}: {other:?}"),
            }
            // The table has no FULLTEXT index, so the next one rebuilds it.
            assert_eq!(
                b1c_answer(server, &schema, &sql),
                FULLTEXT_REBUILD_WAIT,
                "{server}: {create}"
            );
        }
    }
}

#[test]
fn create_fulltext_index_on_a_measured_shape_is_still_unknown_and_followed() {
    // Regression: the statement itself is not measured, but MySQL runs it as
    // the ALTER, so the table has the index afterwards.
    let sql = "CREATE FULLTEXT INDEX ft_a ON orders (note);
               ALTER TABLE orders ADD FULLTEXT INDEX ft_b (email);";
    for server in [MYSQL_80, MYSQL_84] {
        let schema = Schema::from_sql(LAB_TABLES);
        let results = Knowledge::built_in().server(server).unwrap();
        let findings = check(&parse(sql), &schema, results);
        assert!(matches!(findings[0].prediction, Prediction::Unknown { .. }));
        assert_eq!(
            b1c_answer(server, &schema, sql),
            FULLTEXT_SECOND,
            "{server}"
        );
    }
}

#[test]
fn a_create_table_with_a_refused_fulltext_spatial_or_hash_key_loses_the_table() {
    // By hand on MySQL 8.0.46 and 8.4.13: FULLTEXT (note DESC) and
    // (note ASC) are 1221, also on MyISAM; FULLTEXT on a binary TEXT column
    // (the column's set or the table's) is 1283; SPATIAL (p DESC) and
    // KEY (a DESC) USING HASH are 1221. None creates the table.
    for create in [
        "CREATE TABLE t (id INT PRIMARY KEY, note VARCHAR(50), FULLTEXT KEY f (note DESC)) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4",
        "CREATE TABLE t (id INT PRIMARY KEY, note VARCHAR(50), FULLTEXT KEY f (note ASC)) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4",
        "CREATE TABLE t (id INT PRIMARY KEY, note VARCHAR(50), FULLTEXT KEY f (note DESC)) ENGINE=MyISAM DEFAULT CHARSET=utf8mb4",
        "CREATE TABLE t (id INT PRIMARY KEY, body TEXT CHARACTER SET binary NULL, FULLTEXT KEY f (body)) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4",
        "CREATE TABLE t (id INT PRIMARY KEY, body TEXT NULL, FULLTEXT KEY f (body)) ENGINE=InnoDB DEFAULT CHARSET=binary",
        "CREATE TABLE t (id INT PRIMARY KEY, p POINT NOT NULL SRID 0, SPATIAL KEY sp (p DESC)) ENGINE=InnoDB",
        "CREATE TABLE t (id INT PRIMARY KEY, a INT, KEY ih (a DESC) USING HASH) ENGINE=InnoDB",
    ] {
        let sql = format!("{create};\nALTER TABLE t ADD COLUMN x TINYINT NULL;");
        for server in [MYSQL_80, MYSQL_84] {
            let reason = b1c_reason(server, &Schema::from_sql(""), &sql);
            assert!(
                reason.contains("kilit lost track of t"),
                "{server}: {create}: {reason}"
            );
        }
    }
}

#[test]
fn a_create_table_with_an_accepted_fulltext_or_ordered_btree_key_is_kept() {
    // The controls of the hand tests: FULLTEXT without an order, a BTREE
    // index with DESC, and a plain KEY (a DESC).
    for create in [
        "CREATE TABLE t (id INT PRIMARY KEY, note VARCHAR(50), FULLTEXT KEY f (note)) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4",
        "CREATE TABLE t (id INT PRIMARY KEY, a INT, KEY ib (a DESC) USING BTREE) ENGINE=InnoDB",
        "CREATE TABLE t (id INT PRIMARY KEY, a INT, KEY ib (a DESC)) ENGINE=InnoDB",
    ] {
        let sql = format!("{create};\nALTER TABLE t ADD COLUMN x TINYINT NULL;");
        let answers = case_answers("", &sql);
        // A FULLTEXT index turns a new column into a copy.
        let expected = if create.contains("FULLTEXT") {
            COPY
        } else {
            INSTANT
        };
        assert_eq!(answers[1], expected, "{create}");
    }
}

// ---------------------------------------------------------------------------
// Settings named in a `/*! */` comment that kilit leaves out: MySQL runs the
// text of such a comment (by hand on MySQL 8.0.46 and 8.4.13, the mysql
// client sends it), so from its line on kilit no longer trusts the setting.

const FOREIGN_KEY_ON_USER: &str = "ALTER TABLE orders ADD CONSTRAINT fk_orders_user FOREIGN KEY \
                                   (user_id) REFERENCES users (id)";
const UUID_COLUMN: &str = "ALTER TABLE orders ADD COLUMN token CHAR(36) NOT NULL DEFAULT (UUID())";
const OVER_LONG: &str = "ALTER TABLE orders ADD COLUMN big VARCHAR(70000) NULL";
const NEXT_COLUMN: &str = "ALTER TABLE orders ADD COLUMN c INT NULL";

/// The answers to each statement of `sql` on both servers (the same on both,
/// or the test fails).
fn answers_of(schema: &str, sql: &str) -> Vec<String> {
    case_answers(schema, sql)
}

#[test]
fn a_sql_mode_set_in_a_left_out_comment_makes_an_over_long_varchar_lose_the_table() {
    // By hand on MySQL 8.0.46 and 8.4.13: after a strict SET, `/*!40101 SET
    // SQL_MODE='' */;` makes VARCHAR(70000) a MEDIUMTEXT, so the table is
    // not what kilit follows.
    let strict = "SET sql_mode = 'STRICT_TRANS_TABLES';";
    // The control: no comment, strict mode, 1074 and the table is unchanged.
    assert_eq!(
        answers_of(
            LAB_TABLES,
            &format!("{strict}\n{OVER_LONG};\n{NEXT_COLUMN};")
        ),
        ["not checked", UNKNOWN, INSTANT]
    );
    for comment in [
        "/*!40101 SET SQL_MODE='' */;",
        "/*!40101 SET sql_mode = 'NO_ENGINE_SUBSTITUTION' */;",
        "/*!40101 SET @OLD_SQL_MODE=@@SQL_MODE, SQL_MODE='NO_AUTO_VALUE_ON_ZERO' */;",
        "/*!40101 SET SQL_MODE=@OLD_SQL_MODE */;",
    ] {
        let sql = format!("{strict}\n{comment}\n{OVER_LONG};\n{NEXT_COLUMN};");
        for server in [MYSQL_80, MYSQL_84] {
            let reason = b1c_reason(server, &Schema::from_sql(LAB_TABLES), &sql);
            assert!(
                reason.contains("orders") && reason.contains("kilit lost track of"),
                "{server}: {comment}: {reason}"
            );
            assert!(
                reason.contains("line 2 runs a /*! */ comment that names sql_mode")
                    && reason.contains("kilit does not read such comments"),
                "{server}: {comment}: {reason}"
            );
        }
    }
}

#[test]
fn a_left_out_comment_after_the_over_long_column_does_not_change_that_statement() {
    // The comment comes later: the strict SET is still the setting the
    // statement runs under, so the table is unchanged.
    let sql = format!(
        "SET sql_mode = 'STRICT_TRANS_TABLES';\n{OVER_LONG};\n/*!40101 SET SQL_MODE='' */;\n{NEXT_COLUMN};"
    );
    assert_eq!(
        answers_of(LAB_TABLES, &sql),
        ["not checked", UNKNOWN, INSTANT]
    );
}

#[test]
fn foreign_key_checks_set_in_a_left_out_comment_makes_a_new_key_unknown() {
    // By hand: `/*!40014 SET FOREIGN_KEY_CHECKS=0 */;` then ADD FOREIGN KEY is
    // INPLACE, so kilit's COPY (checks on) would be wrong.
    for comment in [
        "/*!40014 SET FOREIGN_KEY_CHECKS=0 */;",
        "/*!40014 SET @OLD_FOREIGN_KEY_CHECKS=@@FOREIGN_KEY_CHECKS, FOREIGN_KEY_CHECKS=0 */;",
        "/*!40014 SET FOREIGN_KEY_CHECKS=@OLD_FOREIGN_KEY_CHECKS */;",
        "/*!40014 set foreign_key_checks=1 */;",
    ] {
        let sql = format!("{comment}\n{FOREIGN_KEY_ON_USER};");
        for server in [MYSQL_80, MYSQL_84] {
            let reason = b1c_reason(server, &Schema::from_sql(LAB_TABLES), &sql);
            assert!(
                reason.contains("line 1 runs a /*! */ comment that names foreign_key_checks")
                    && reason.contains("kilit does not read such comments"),
                "{server}: {comment}: {reason}"
            );
        }
    }
    // The control: the same key without the comment copies the table.
    assert_eq!(
        answers_of(LAB_TABLES, &format!("{FOREIGN_KEY_ON_USER};")),
        [COPY]
    );
}

#[test]
fn a_statement_before_a_left_out_comment_keeps_its_answer() {
    // From its line on: the key before the comment runs with checks on.
    let sql =
        format!("{FOREIGN_KEY_ON_USER};\n/*!40014 SET FOREIGN_KEY_CHECKS=0 */;\n{NEXT_COLUMN};");
    let answers = answers_of(LAB_TABLES, &sql);
    assert_eq!(answers[0], COPY);
    assert_eq!(answers[1], INSTANT);
}

#[test]
fn the_mysqldump_trailer_after_the_last_statement_changes_no_answer() {
    // mysqldump ends a file with lines that restore the settings; they run
    // after the last statement, not before it.
    for trailer in [
        "/*!40014 SET FOREIGN_KEY_CHECKS=@OLD_FOREIGN_KEY_CHECKS */;",
        "/*!40101 SET SQL_MODE=@OLD_SQL_MODE */;",
        "/*!40101 SET SQL_MODE=@OLD_SQL_MODE */;\n/*!40014 SET FOREIGN_KEY_CHECKS=@OLD_FOREIGN_KEY_CHECKS */;",
    ] {
        assert_eq!(
            answers_of(LAB_TABLES, &format!("{FOREIGN_KEY_ON_USER};\n{trailer}\n")),
            [COPY],
            "{trailer}"
        );
        assert_eq!(
            answers_of(LAB_TABLES, &format!("{NEXT_COLUMN};\n{trailer}\n")),
            [INSTANT],
            "{trailer}"
        );
    }
}

#[test]
fn a_set_that_reads_foreign_key_checks_again_after_the_comment_makes_the_key_known_again() {
    // A SET kilit can read after the comment fixes the setting.
    let sql = format!(
        "/*!40014 SET FOREIGN_KEY_CHECKS=0 */;\nSET foreign_key_checks = 1;\n{FOREIGN_KEY_ON_USER};"
    );
    let answers = answers_of(LAB_TABLES, &sql);
    assert_eq!(
        answers.last().map(String::as_str),
        Some(COPY),
        "{answers:?}"
    );
}

#[test]
fn sql_log_bin_set_in_a_left_out_comment_makes_the_binary_log_failures_unknown() {
    // The 1674 failures depend on the binary log being on.
    assert_eq!(
        answers_of(LAB_TABLES, &format!("{UUID_COLUMN};")),
        [FAILS_1674]
    );
    for comment in [
        "/*!80000 SET @@SESSION.SQL_LOG_BIN= 0 */;",
        "/*!32312 SET @OLD_SQL_LOG_BIN=@@SQL_LOG_BIN, SQL_LOG_BIN=0 */;",
        "/*!80000 SET sql_log_bin = 1 */;",
    ] {
        let sql = format!("{comment}\n{UUID_COLUMN};");
        for server in [MYSQL_80, MYSQL_84] {
            let reason = b1c_reason(server, &Schema::from_sql(LAB_TABLES), &sql);
            assert!(
                reason.contains("sql_log_bin") && reason.contains("/*! */"),
                "{server}: {comment}: {reason}"
            );
        }
    }
}

#[test]
fn old_alter_table_set_in_a_left_out_comment_makes_every_later_alter_unknown() {
    // With it ON MySQL copies the table.
    assert_eq!(
        answers_of(LAB_TABLES, &format!("{NEXT_COLUMN};")),
        [INSTANT]
    );
    // the second ADD names another column; the same column twice would
    // be error 1060.
    let another_column = "ALTER TABLE orders ADD COLUMN d INT NULL";
    let sql = format!("{NEXT_COLUMN};\n/*!80000 SET old_alter_table = ON */;\n{another_column};");
    for server in [MYSQL_80, MYSQL_84] {
        let reason = b1c_reason(server, &Schema::from_sql(LAB_TABLES), &sql);
        assert!(
            reason.contains("line 2 runs a /*! */ comment that names old_alter_table"),
            "{server}: {reason}"
        );
    }
    // What ran before keeps its answer.
    assert_eq!(answers_of(LAB_TABLES, &sql)[0], INSTANT);
}

#[test]
fn restrict_fk_on_non_standard_key_set_in_a_left_out_comment_makes_the_index_rules_unknown() {
    // The plain index on users.name: 1822 on 8.0.46, 6125 on 8.4.13 as
    // measured; named, the server no longer decides in a way kilit can tell.
    let setup = lab_after(&[OC_NAME, "ALTER TABLE users ADD INDEX idx_users_name (name)"]);
    let key = format!("ALTER TABLE orders {NAME}");
    let [on_80, on_84] = on_both(&setup, &format!("{key};"));
    assert_eq!([on_80.as_str(), on_84.as_str()], [COPY, FAILS_6125]);
    for comment in [
        "/*!80019 SET restrict_fk_on_non_standard_key = OFF */;",
        "/*!80019 SET SESSION restrict_fk_on_non_standard_key=0 */;",
    ] {
        assert_eq!(
            on_both(&setup, &format!("{comment}\n{key};")),
            [UNKNOWN, UNKNOWN],
            "{comment}"
        );
    }
}

#[test]
fn binlog_format_set_in_a_left_out_comment_makes_the_expression_defaults_unknown() {
    // The lab measured 1674 with binlog_format = ROW.
    for comment in [
        "/*!50000 SET binlog_format = 'STATEMENT' */;",
        "/*!50000 SET SESSION binlog_format=MIXED */;",
    ] {
        assert_eq!(
            answers_of(LAB_TABLES, &format!("{comment}\n{UUID_COLUMN};"))
                .last()
                .map(String::as_str),
            Some(UNKNOWN),
            "{comment}"
        );
    }
}

#[test]
fn row_format_strict_mode_and_engine_settings_in_a_left_out_comment_are_named_like_a_set() {
    let create = "CREATE TABLE t (id BIGINT UNSIGNED PRIMARY KEY, amount INT NOT NULL, \
                  email VARCHAR(255) NULL) DEFAULT CHARSET=utf8mb4";
    // innodb_default_row_format: every table without a ROW_FORMAT is unknown.
    assert_eq!(
        answers_of(
            LAB_TABLES,
            &format!("/*!80000 SET GLOBAL innodb_default_row_format = COMPACT */;\n{NEXT_COLUMN};")
        ),
        [UNKNOWN]
    );
    // innodb_strict_mode: a table created after it.
    assert_eq!(
        answers_of(
            "",
            &format!("/*!80000 SET innodb_strict_mode = OFF */;\n{create};\nALTER TABLE t ADD COLUMN c INT NULL;")
        )
        .last()
        .map(String::as_str),
        Some(UNKNOWN)
    );
    // default_storage_engine and default_tmp_storage_engine.
    assert_eq!(
        answers_of(
            "",
            &format!("/*!40101 SET default_storage_engine = MyISAM */;\n{create};\nALTER TABLE t ADD COLUMN c INT NULL;")
        )
        .last()
        .map(String::as_str),
        Some(UNKNOWN)
    );
    assert_eq!(
        answers_of(
            "",
            "/*!80000 SET default_tmp_storage_engine = MyISAM */;
             CREATE TEMPORARY TABLE tt (id INT, email VARCHAR(255)) DEFAULT CHARSET=utf8mb4;
             ALTER TABLE tt ADD INDEX ie (email);"
        )
        .last()
        .map(String::as_str),
        Some(UNKNOWN)
    );
    // Created before the comment: today's answer.
    assert_eq!(
        answers_of(
            "",
            &format!("{create};\n/*!80000 SET innodb_strict_mode = OFF */;\nALTER TABLE t ADD COLUMN c INT NULL;")
        )
        .last()
        .map(String::as_str),
        Some(INSTANT)
    );
}

#[test]
fn a_left_out_comment_that_names_no_followed_setting_changes_nothing() {
    // The lines of a mysqldump header that set nothing kilit follows.
    let header = "/*!40101 SET @OLD_CHARACTER_SET_CLIENT=@@CHARACTER_SET_CLIENT */;
/*!40101 SET @OLD_CHARACTER_SET_RESULTS=@@CHARACTER_SET_RESULTS */;
/*!40101 SET @OLD_COLLATION_CONNECTION=@@COLLATION_CONNECTION */;
/*!50503 SET NAMES utf8mb4 */;
/*!40103 SET @OLD_TIME_ZONE=@@TIME_ZONE */;
/*!40103 SET TIME_ZONE='+00:00' */;
/*!40014 SET @OLD_UNIQUE_CHECKS=@@UNIQUE_CHECKS, UNIQUE_CHECKS=0 */;
/*!40111 SET @OLD_SQL_NOTES=@@SQL_NOTES, SQL_NOTES=0 */;
";
    let statements = format!(
        "{FOREIGN_KEY_ON_USER};\n{NEXT_COLUMN};\n{UUID_COLUMN};\n{OVER_LONG};\n{NEXT_COLUMN};"
    );
    let without = answers_of(LAB_TABLES, &statements);
    assert_eq!(
        answers_of(LAB_TABLES, &format!("{header}{statements}")),
        without
    );
    // Between the statements too.
    let between = format!(
        "{FOREIGN_KEY_ON_USER};\n{header}{NEXT_COLUMN};\n{header}{UUID_COLUMN};\n{OVER_LONG};\n{header}{NEXT_COLUMN};"
    );
    assert_eq!(answers_of(LAB_TABLES, &between), without);
}

#[test]
fn a_mysqldump_header_in_the_schema_file_changes_no_answer() {
    // The schema file is not a session: its comments set nothing.
    let header =
        "/*!40014 SET @OLD_FOREIGN_KEY_CHECKS=@@FOREIGN_KEY_CHECKS, FOREIGN_KEY_CHECKS=0 */;
/*!40101 SET @OLD_SQL_MODE=@@SQL_MODE, SQL_MODE='NO_AUTO_VALUE_ON_ZERO' */;
";
    let dump = format!("{header}{LAB_TABLES}");
    for server in [MYSQL_80, MYSQL_84] {
        let results = Knowledge::built_in().server(server).unwrap();
        let findings = check(
            &parse(&format!("{FOREIGN_KEY_ON_USER};\n{UUID_COLUMN};")),
            &Schema::from_dump(&dump),
            results,
        );
        assert!(
            matches!(
                &findings[0].prediction,
                Prediction::Measured { verdict, .. } if verdict.to_string() == COPY
            ),
            "{server}: {:?}",
            findings[0].prediction
        );
    }
}

#[test]
fn a_global_setting_in_a_left_out_comment_is_unclear_in_the_files_after_it() {
    // SET GLOBAL (and PERSIST) outlast the session: the next input file
    // starts with the setting unclear too. A session setting does not.
    let schema = Schema::from_sql(LAB_TABLES);
    for (comment, later, global_unknown) in [
        (
            "/*!40014 SET GLOBAL foreign_key_checks = 0 */;",
            FOREIGN_KEY_ON_USER,
            true,
        ),
        (
            "/*!80000 SET PERSIST foreign_key_checks = 0 */;",
            FOREIGN_KEY_ON_USER,
            true,
        ),
        (
            "/*!40014 SET FOREIGN_KEY_CHECKS=0 */;",
            FOREIGN_KEY_ON_USER,
            false,
        ),
        ("/*!80000 SET GLOBAL sql_log_bin = 0 */;", UUID_COLUMN, true),
        ("/*!80000 SET sql_log_bin = 0 */;", UUID_COLUMN, false),
        (
            "/*!80000 SET GLOBAL old_alter_table = ON */;",
            NEXT_COLUMN,
            true,
        ),
        (
            "/*!80000 SET PERSIST old_alter_table = ON */;",
            NEXT_COLUMN,
            true,
        ),
        ("/*!80000 SET old_alter_table = ON */;", NEXT_COLUMN, false),
    ] {
        let first = parse(&format!("{comment}\nSELECT 1;"));
        let second = parse(&format!("{later};"));
        for server in [MYSQL_80, MYSQL_84] {
            let results = Knowledge::built_in().server(server).unwrap();
            let findings = check_sessions(&[&first, &second], &schema, results);
            let last = &findings.last().unwrap().prediction;
            let unknown = matches!(last, Prediction::Unknown { .. });
            assert_eq!(unknown, global_unknown, "{server}: {comment}: {last:?}");
        }
    }
}

// ---------------------------------------------------------------------------
// Columns at the limits of their type and of the row, by hand on MySQL
// 8.0.46 and 8.4.13 in strict mode and with sql_mode = '': a row past 65,535
// bytes is refused (1118); a column longer than its type is refused in strict
// mode (1074) and made a TEXT or BLOB type in the other modes.

/// What MySQL did with `ALTER TABLE orders ADD COLUMN c <definition> NULL` on
/// the lab's `orders`, in strict mode and with sql_mode = ''.
#[derive(Clone, Copy, Debug, PartialEq)]
enum AddedColumn {
    /// Error 1118: the row is too large.
    RowTooLarge,
    /// Accepted.
    Accepted,
    /// Error 1074 in strict mode, the column made another type otherwise.
    TooLongForItsType,
    /// Error 1074 in every mode.
    TooLongInEveryMode,
}

const ADD_COLUMN_LIMITS: [(&str, AddedColumn); 12] = [
    (
        "VARCHAR(16383) CHARACTER SET utf8mb4",
        AddedColumn::RowTooLarge,
    ),
    (
        "VARCHAR(16384) CHARACTER SET utf8mb4",
        AddedColumn::TooLongForItsType,
    ),
    (
        "VARCHAR(65532) CHARACTER SET latin1",
        AddedColumn::RowTooLarge,
    ),
    (
        "VARCHAR(65533) CHARACTER SET latin1",
        AddedColumn::RowTooLarge,
    ),
    (
        "VARCHAR(65535) CHARACTER SET latin1",
        AddedColumn::RowTooLarge,
    ),
    (
        "VARCHAR(65536) CHARACTER SET latin1",
        AddedColumn::TooLongForItsType,
    ),
    ("VARBINARY(65532)", AddedColumn::RowTooLarge),
    ("VARBINARY(65533)", AddedColumn::RowTooLarge),
    ("VARBINARY(65535)", AddedColumn::RowTooLarge),
    ("VARBINARY(65536)", AddedColumn::TooLongForItsType),
    ("BINARY(255)", AddedColumn::Accepted),
    ("BINARY(256)", AddedColumn::TooLongInEveryMode),
];

#[test]
fn an_added_column_at_the_limits_is_refused_changed_or_accepted_as_mysql_does() {
    for (mode, strict) in [
        ("SET sql_mode = 'STRICT_TRANS_TABLES';\n", true),
        ("SET sql_mode = '';\n", false),
    ] {
        for (definition, outcome) in ADD_COLUMN_LIMITS {
            let sql =
                format!("{mode}ALTER TABLE orders ADD COLUMN c {definition} NULL;\n{NEXT_COLUMN};");
            for server in [MYSQL_80, MYSQL_84] {
                let results = Knowledge::built_in().server(server).unwrap();
                let findings = check(&parse(&sql), &Schema::from_sql(LAB_TABLES), results);
                let (first, second) = (&findings[1].prediction, &findings[2].prediction);
                let label = format!("{server}, strict {strict}: {definition}");
                match outcome {
                    AddedColumn::RowTooLarge => {
                        assert!(
                            matches!(
                                first,
                                Prediction::Measured {
                                    verdict: Verdict::Fails { code: 1118, .. },
                                    ..
                                }
                            ),
                            "{label}: {first:?}"
                        );
                        // The table is as it was.
                        assert!(
                            matches!(second, Prediction::Measured { .. }),
                            "{label}: {second:?}"
                        );
                    }
                    AddedColumn::Accepted => {
                        assert!(
                            matches!(first, Prediction::Measured { verdict, .. }
                                if verdict.to_string() == INSTANT),
                            "{label}: {first:?}"
                        );
                    }
                    AddedColumn::TooLongForItsType | AddedColumn::TooLongInEveryMode => {
                        assert!(
                            matches!(first, Prediction::Unknown { .. }),
                            "{label}: {first:?}"
                        );
                        let changed = outcome == AddedColumn::TooLongForItsType && !strict;
                        match second {
                            Prediction::Unknown { reason, .. } if changed => assert!(
                                reason.contains("kilit lost track of orders"),
                                "{label}: {reason}"
                            ),
                            Prediction::Measured { verdict, .. } if !changed => {
                                assert_eq!(verdict.to_string(), INSTANT, "{label}");
                            }
                            other => panic!("{label}: {other:?}"),
                        }
                    }
                }
            }
        }
    }
}

/// Which of the three CREATE TABLE forms MySQL accepted for a column: the
/// column alone and NULL, alone and NOT NULL, and NULL next to a BIGINT
/// primary key.
const CREATE_TABLE_LIMITS: [(&str, [bool; 3]); 12] = [
    ("VARCHAR(16383) CHARACTER SET utf8mb4", [true, true, false]),
    (
        "VARCHAR(16384) CHARACTER SET utf8mb4",
        [false, false, false],
    ),
    ("VARCHAR(65532) CHARACTER SET latin1", [true, true, false]),
    ("VARCHAR(65533) CHARACTER SET latin1", [false, true, false]),
    ("VARCHAR(65535) CHARACTER SET latin1", [false, false, false]),
    ("VARCHAR(65536) CHARACTER SET latin1", [false, false, false]),
    ("VARBINARY(65532)", [true, true, false]),
    ("VARBINARY(65533)", [false, true, false]),
    ("VARBINARY(65535)", [false, false, false]),
    ("VARBINARY(65536)", [false, false, false]),
    ("BINARY(255)", [true, true, true]),
    ("BINARY(256)", [false, false, false]),
];

#[test]
fn a_create_table_at_the_limits_keeps_the_table_mysql_makes_and_loses_the_one_it_refuses() {
    for session in [
        "",
        "SET sql_mode = 'STRICT_TRANS_TABLES';\n",
        "SET sql_mode = '';\n",
    ] {
        for (definition, accepted) in CREATE_TABLE_LIMITS {
            for (form, kept) in [
                (format!("CREATE TABLE t (c {definition} NULL)"), accepted[0]),
                (
                    format!("CREATE TABLE t (c {definition} NOT NULL)"),
                    accepted[1],
                ),
                (
                    format!(
                        "CREATE TABLE t (id BIGINT UNSIGNED AUTO_INCREMENT PRIMARY KEY, c {definition} NULL)"
                    ),
                    accepted[2],
                ),
            ] {
                let sql = format!(
                    "{session}{form} ENGINE=InnoDB DEFAULT CHARSET=utf8mb4;\n\
                     ALTER TABLE t ADD COLUMN x TINYINT NULL;"
                );
                for server in [MYSQL_80, MYSQL_84] {
                    let results = Knowledge::built_in().server(server).unwrap();
                    let findings = check(&parse(&sql), &Schema::from_sql(""), results);
                    let last = &findings.last().unwrap().prediction;
                    let lost = matches!(last, Prediction::Unknown { reason, .. }
                        if reason.contains("kilit lost track of t"));
                    assert_eq!(!lost, kept, "{server}: {session}{form}: {last:?}");
                }
            }
        }
    }
}

/// The scenarios whose statement MySQL refuses, with the error the lab saw in
/// every run.
const REFUSED_SCENARIOS: [(&str, u16); 5] = [
    ("add-foreign-key-set-null-not-null", 1830),
    ("add-foreign-key-checks-off-set-null-not-null", 1830),
    ("add-fulltext-index-binary-text", 1283),
    ("add-fulltext-index-desc", 1221),
    ("add-fulltext-index-asc", 1221),
];

#[test]
fn the_lab_saw_the_refusals_in_every_run_on_both_servers_and_accepted_set_default() {
    for results in Knowledge::built_in().servers() {
        let server = results.server.name();
        for (id, error) in REFUSED_SCENARIOS {
            let scenario = results
                .scenario(id)
                .unwrap_or_else(|| panic!("{id} on {server}"));
            for variant in Variant::ALL {
                assert!(
                    matches!(
                        scenario.verdict_for(variant),
                        Some(Verdict::Fails { code, .. }) if code == error
                    ),
                    "{id} as {variant} on {server}: {:?}",
                    scenario.verdict_for(variant)
                );
            }
        }
        // SET DEFAULT is accepted by MySQL 8.0.46 and 8.4.13: it copies the
        // table as the plain key does.
        let set_default = results
            .scenario("add-foreign-key-on-delete-set-default")
            .unwrap();
        let plain = results.scenario("add-foreign-key").unwrap();
        for variant in Variant::ALL {
            assert_eq!(
                set_default.verdict_for(variant),
                plain.verdict_for(variant),
                "{variant} on {server}"
            );
        }
    }
}

// ---------------------------------------------------------------------------
// Step B1d: answers on foreign keys, FULLTEXT indexes and statements that
// MySQL refuses as a whole. The facts come from hand tests on MySQL 8.0.46
// and 8.4.13 (the same on both) and from the lab's own results; the shapes
// and the errors are named in each test.

/// What kilit says about the last statement of `statements`, run in order
/// on `schema`: the same on both servers, or the test fails.
fn said_in_order(schema: &str, statements: &[&str]) -> Said {
    let sql: String = statements
        .iter()
        .map(|statement| format!("{statement};\n"))
        .collect();
    said(schema, &sql)
}

/// What kilit says about the last of `statements` when the statement at
/// `without` is left out.
fn said_in_order_without(schema: &str, statements: &[&str], without: usize) -> Said {
    let kept: Vec<&str> = statements
        .iter()
        .enumerate()
        .filter(|(at, _)| *at != without)
        .map(|(_, statement)| *statement)
        .collect();
    said_in_order(schema, &kept)
}

const B1D_MODIFY_NULL: &str = "ALTER TABLE orders MODIFY COLUMN user_id BIGINT UNSIGNED NULL";
const B1D_SET_NULL_KEY: &str = "ALTER TABLE orders ADD CONSTRAINT fk_orders_user FOREIGN KEY \
                                   (user_id) REFERENCES users (id) ON DELETE SET NULL";
const B1D_SET_NULL_KEY_AGAIN: &str = "ALTER TABLE orders ADD CONSTRAINT fk_again FOREIGN KEY \
                                         (user_id) REFERENCES users (id) ON DELETE SET NULL";

#[test]
fn a_modify_not_null_beside_the_drop_of_the_set_null_key_makes_the_column_not_null() {
    // By hand on MySQL 8.0.46 and 8.4.13: with the key dropped in the
    // same ALTER, MODIFY user_id ... NOT NULL is accepted (rebuilt, no rows
    // copied), the table then shows user_id NOT NULL with the key's index
    // left, and DROP COLUMN user_id is accepted too (rebuilt), not 1828.
    for modify in [
        "ALTER TABLE orders DROP FOREIGN KEY fk_orders_user, MODIFY user_id BIGINT UNSIGNED NOT NULL",
        "ALTER TABLE orders MODIFY user_id BIGINT UNSIGNED NOT NULL, DROP FOREIGN KEY fk_orders_user",
    ] {
        let drop_column = "ALTER TABLE orders DROP COLUMN user_id";
        let said = said_in_order(
            LAB_TABLES,
            &[B1D_MODIFY_NULL, B1D_SET_NULL_KEY, modify, drop_column],
        );
        assert_ne!(said.answer, FAILS_1828, "{modify}");
        assert_eq!(
            said.answer, "INPLACE, rebuilds the table, writes continue",
            "{modify}"
        );
        // The column is NOT NULL now: a SET NULL key on it fails with 1830, as
        // on the lab's own NOT NULL column (a NOT NULL column gave 1830).
        let said = said_in_order(
            LAB_TABLES,
            &[
                B1D_MODIFY_NULL,
                B1D_SET_NULL_KEY,
                modify,
                B1D_SET_NULL_KEY_AGAIN,
            ],
        );
        assert_eq!(said.answer, FAILS_1830, "{modify}");
    }
}

#[test]
fn a_modify_not_null_that_mysql_refuses_with_1830_leaves_the_column_nullable() {
    // By hand (the table after the refusals): MODIFY, CHANGE and
    // MODIFY with ALGORITHM=COPY on the column of a SET NULL key fail with
    // 1830, and the table shows user_id DEFAULT NULL. With the key dropped,
    // a new SET NULL key on user_id is accepted, so
    // it copies the table; it would be 1830 if the column had become NOT NULL.
    for modify in [
        "ALTER TABLE orders MODIFY COLUMN user_id BIGINT UNSIGNED NOT NULL",
        "ALTER TABLE orders CHANGE COLUMN user_id user_id BIGINT UNSIGNED NOT NULL",
        "ALTER TABLE orders MODIFY COLUMN user_id BIGINT UNSIGNED NOT NULL, ALGORITHM=COPY",
    ] {
        let said = said_in_order(
            LAB_TABLES,
            &[
                B1D_MODIFY_NULL,
                B1D_SET_NULL_KEY,
                modify,
                "ALTER TABLE orders DROP FOREIGN KEY fk_orders_user",
                B1D_SET_NULL_KEY_AGAIN,
            ],
        );
        assert_eq!(said.answer, COPY, "{modify}");
    }
}

#[test]
fn a_modify_to_utf8mb4_beside_a_fulltext_index_on_a_binary_text_column_is_followed() {
    // By hand: body TEXT CHARACTER SET binary refuses a FULLTEXT index
    // (1283), but MODIFY body TEXT CHARACTER SET utf8mb4 with ADD FULLTEXT in
    // the same ALTER is accepted, and a second FULLTEXT index on body is
    // accepted without a rebuild (rebuilt 0, no rows copied).
    let statements = [
        "ALTER TABLE orders ADD COLUMN body TEXT CHARACTER SET binary NULL",
        "ALTER TABLE orders MODIFY body TEXT CHARACTER SET utf8mb4 NULL, ADD FULLTEXT INDEX ft_body (body)",
        "ALTER TABLE orders ADD FULLTEXT INDEX ft_body2 (body)",
    ];
    let second = said_in_order(LAB_TABLES, &statements);
    assert_ne!(second.answer, FAILS_1283);
    assert_eq!(second.answer, FULLTEXT_SECOND);
    // The first ALTER does not fail either.
    let first = said_in_order(LAB_TABLES, &statements[..2]);
    assert!(!first.answer.starts_with("fails"), "{}", first.answer);
}

#[test]
fn an_index_or_a_table_named_hash_with_desc_is_not_a_hash_index() {
    // By hand on both servers: ADD INDEX hash (amount DESC) on orders,
    // CREATE TABLE hash, ALTER TABLE hash ADD INDEX ic (c DESC) and CREATE
    // INDEX ic2 ON hash (c DESC, id) are accepted (rebuilt 0, no rows copied).
    let statements = [
        "ALTER TABLE orders ADD INDEX hash (amount DESC)",
        "CREATE TABLE hash (id INT PRIMARY KEY, c INT)",
        "ALTER TABLE hash ADD INDEX ic (c DESC)",
        "CREATE INDEX ic2 ON hash (c DESC, id)",
        "ALTER TABLE orders ADD COLUMN zz INT NULL",
    ];
    assert_eq!(said_in_order(LAB_TABLES, &statements[..1]).answer, INPLACE);
    assert_eq!(said_in_order(LAB_TABLES, &statements[..3]).answer, INPLACE);
    // CREATE INDEX is followed and not measured on its own: unknown, but the
    // reason is not the 1221 of a hash index.
    let create_index = said_in_order(LAB_TABLES, &statements[..4]);
    assert!(
        create_index.answer == INPLACE || create_index.answer == UNKNOWN,
        "{}",
        create_index.answer
    );
    assert!(
        !create_index.reason.contains("1221"),
        "{}",
        create_index.reason
    );
    // The same ALTER in the last place: the table is as the lab has it.
    assert_eq!(said_in_order(LAB_TABLES, &statements).answer, INSTANT);
}

#[test]
fn a_hash_index_with_desc_is_still_unknown_and_names_1221_also_when_it_is_named_hash() {
    // By hand: USING HASH with ASC or DESC is 1221 (spatial/fulltext/hash
    // index and explicit index order).
    for statement in [
        "ALTER TABLE orders ADD INDEX hash (amount DESC) USING HASH",
        "ALTER TABLE orders ADD INDEX ih (amount DESC) USING HASH",
    ] {
        let said = said_in_order(LAB_TABLES, &[statement]);
        assert_eq!(said.answer, UNKNOWN, "{statement}");
        assert!(said.reason.contains("1221"), "{statement}: {}", said.reason);
    }
}

#[test]
fn a_create_table_if_not_exists_of_a_known_table_with_a_refused_key_leaves_the_table_known() {
    // By hand: CREATE TABLE IF NOT EXISTS orders (... FULLTEXT (note
    // DESC)) is 1221 and orders is as it was (SHOW CREATE TABLE unchanged);
    // the same with a SET NULL key on a NOT NULL column is accepted with note
    // 1050, and orders is unchanged. A FULLTEXT index followed into the table
    // would turn the next ADD COLUMN into a COPY.
    let add_column = "ALTER TABLE orders ADD COLUMN zz INT NULL";
    for create in [
        "CREATE TABLE IF NOT EXISTS orders (id INT PRIMARY KEY, note VARCHAR(50), FULLTEXT (note DESC))",
        "CREATE TABLE IF NOT EXISTS orders (id INT PRIMARY KEY, uid INT NOT NULL, \
         FOREIGN KEY (uid) REFERENCES users (id) ON DELETE SET NULL)",
    ] {
        let said = said_in_order(LAB_TABLES, &[create, add_column]);
        assert_eq!(said.answer, INSTANT, "{create}");
    }
    let said = said_in_order(
        LAB_TABLES,
        &[
            "CREATE TABLE IF NOT EXISTS users (id INT PRIMARY KEY, note VARCHAR(50), FULLTEXT (note DESC))",
            "ALTER TABLE users ADD COLUMN zz INT NULL",
        ],
    );
    let control = said_in_order(LAB_TABLES, &["ALTER TABLE users ADD COLUMN zz INT NULL"]);
    assert_eq!(said.answer, control.answer);
    assert_eq!(said.answer, INSTANT);
}

#[test]
fn a_refused_create_temporary_table_leaves_the_base_table_known() {
    // The FULLTEXT (note DESC) key is 1221 in any CREATE TABLE, so no
    // temporary orders exists and the ALTER is for the base table, which has
    // no FULLTEXT index.
    let said = said_in_order(
        LAB_TABLES,
        &[
            "CREATE TEMPORARY TABLE orders (id INT PRIMARY KEY, note VARCHAR(50), FULLTEXT (note DESC))",
            "ALTER TABLE orders ADD COLUMN zz INT NULL",
        ],
    );
    assert_eq!(said.answer, INSTANT);
}

#[test]
fn a_myisam_table_with_a_set_null_key_on_a_not_null_column_is_kept() {
    // By hand: MyISAM takes the key clause and ignores it, so the table
    // is made (SHOW CREATE TABLE shows the plain KEY). The lab's
    // myisam-add-nullable-column is a COPY.
    let said = said_in_order(
        LAB_TABLES,
        &[
            "CREATE TABLE mo2 (id INT PRIMARY KEY, user_id BIGINT UNSIGNED NOT NULL, \
             FOREIGN KEY (user_id) REFERENCES users (id) ON DELETE SET NULL) ENGINE=MyISAM",
            "ALTER TABLE mo2 ADD COLUMN zz INT NULL",
        ],
    );
    assert_eq!(said.answer, COPY, "{}", said.reason);
}

#[test]
fn a_create_table_with_a_varbinary_past_its_type_gives_the_over_long_column_reason() {
    // By hand (under a non-strict sql_mode): under sql_mode = '' VARBINARY(65536) and
    // VARCHAR(70000) become BLOB and TEXT types (note 1246) in CREATE TABLE
    // with or without a primary key, NULL or NOT NULL; it is no row too large.
    for definition in [
        "c VARBINARY(65536) NULL",
        "c VARBINARY(65536) NOT NULL",
        "id BIGINT UNSIGNED AUTO_INCREMENT PRIMARY KEY, c VARBINARY(65536) NULL",
        "c VARCHAR(70000) CHARACTER SET latin1 NULL",
    ] {
        let sql = format!(
            "SET sql_mode = '';\nCREATE TABLE t ({definition}) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4;\n\
             ALTER TABLE t ADD COLUMN x TINYINT NULL;"
        );
        let reason = case_reason("", &sql);
        assert!(
            reason.contains("kilit lost track of t"),
            "{definition}: {reason}"
        );
        assert!(
            reason.contains("longer than its type holds")
                || reason.contains("more than its type holds"),
            "{definition}: {reason}"
        );
        assert!(!reason.contains("1118"), "{definition}: {reason}");
    }
}

const B1D_VIRTUAL_COLUMN: &str = "ALTER TABLE orders ADD COLUMN v VARCHAR(50) AS (note) VIRTUAL";
const B1D_STORED_COLUMN: &str = "ALTER TABLE orders ADD COLUMN s VARCHAR(50) AS (note) STORED";
const B1D_NEXT_COLUMN: &str = "ALTER TABLE orders ADD COLUMN zz INT NULL";

#[test]
fn a_fulltext_index_on_a_virtual_generated_column_is_unknown_with_3106_and_not_followed() {
    // By hand: 3106 in all six variants, on both servers.
    for variant in Variant::ALL {
        let statement = format!(
            "ALTER TABLE orders ADD FULLTEXT INDEX fv (v){}",
            variant.clause()
        );
        let said = said_in_order(LAB_TABLES, &[B1D_VIRTUAL_COLUMN, &statement]);
        assert_eq!(said.answer, UNKNOWN, "{variant}");
        assert!(said.reason.contains("3106"), "{variant}: {}", said.reason);
    }
    // The index is not there: the next column is the answer without the
    // statement, not the COPY a FULLTEXT index would give.
    let statements = [
        B1D_VIRTUAL_COLUMN,
        "ALTER TABLE orders ADD FULLTEXT INDEX fv (v)",
        B1D_NEXT_COLUMN,
    ];
    let said = said_in_order(LAB_TABLES, &statements);
    assert_ne!(said.answer, COPY);
    assert_eq!(
        said.answer,
        said_in_order_without(LAB_TABLES, &statements, 1).answer
    );
    assert_eq!(said.answer, INSTANT);
}

#[test]
fn a_create_table_with_a_fulltext_index_on_a_virtual_generated_column_loses_the_table() {
    // By hand: CREATE TABLE gv (... g ... VIRTUAL, FULLTEXT (g))
    // fails with 3106 and the table is not made.
    let said = said_in_order(
        "",
        &[
            "CREATE TABLE gv (id INT PRIMARY KEY, note VARCHAR(50), \
             g VARCHAR(50) AS (note) VIRTUAL, FULLTEXT (g)) DEFAULT CHARSET=utf8mb4",
            "ALTER TABLE gv ADD COLUMN x INT NULL",
        ],
    );
    assert_eq!(said.answer, UNKNOWN);
    assert!(
        said.reason.contains("kilit lost track of gv"),
        "{}",
        said.reason
    );
}

#[test]
fn a_fulltext_index_on_a_stored_generated_column_is_unknown_and_followed() {
    // By hand: STORED is accepted (a rebuild to add
    // FTS_DOC_ID) in ALTER and in CREATE TABLE. The lab has not measured the
    // shape, so the answer is unknown, and the index is on the table.
    let statements = [
        B1D_STORED_COLUMN,
        "ALTER TABLE orders ADD FULLTEXT INDEX fs (s)",
        B1D_NEXT_COLUMN,
    ];
    let said = said_in_order(LAB_TABLES, &statements[..2]);
    assert_eq!(said.answer, UNKNOWN);
    assert!(said.reason.contains("not been measured"), "{}", said.reason);
    // A FULLTEXT index on the table makes the next column a COPY.
    assert_eq!(said_in_order(LAB_TABLES, &statements).answer, COPY);
    assert_ne!(
        said_in_order_without(LAB_TABLES, &statements, 1).answer,
        COPY
    );
    // CREATE TABLE is accepted: the table is kept, with the index.
    let said = said_in_order(
        "",
        &[
            "CREATE TABLE gs (id INT PRIMARY KEY, note VARCHAR(50), \
             g VARCHAR(50) AS (note) STORED, FULLTEXT (g)) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4",
            "ALTER TABLE gs ADD COLUMN x INT NULL",
        ],
    );
    assert!(!said.reason.contains("kilit lost track"), "{}", said.reason);
}

#[test]
fn a_fulltext_index_on_a_utf16_utf16le_utf32_or_ucs2_column_is_unknown_and_not_followed() {
    // By hand: 1283 for VARCHAR and TEXT columns in these
    // character sets, in ALTER and in CREATE TABLE (no table is made).
    for (column, character_set) in [
        ("VARCHAR(50)", "utf16"),
        ("VARCHAR(50)", "utf32"),
        ("VARCHAR(50)", "ucs2"),
        ("TEXT", "utf16"),
        ("TEXT", "utf16le"),
        ("TEXT", "utf32"),
        ("TEXT", "ucs2"),
    ] {
        let add_column =
            format!("ALTER TABLE orders ADD COLUMN c1 {column} CHARACTER SET {character_set} NULL");
        let statements = [
            add_column.as_str(),
            "ALTER TABLE orders ADD FULLTEXT INDEX f1 (c1)",
            B1D_NEXT_COLUMN,
        ];
        let said = said_in_order(LAB_TABLES, &statements[..2]);
        assert_eq!(said.answer, UNKNOWN, "{column} {character_set}");
        // Not followed: the next column is not the COPY of a FULLTEXT index.
        let next = said_in_order(LAB_TABLES, &statements);
        assert_ne!(next.answer, COPY, "{column} {character_set}");
        assert_eq!(
            next.answer,
            said_in_order_without(LAB_TABLES, &statements, 1).answer,
            "{column} {character_set}"
        );
        // CREATE TABLE: no table.
        let create = format!(
            "CREATE TABLE tx (id INT PRIMARY KEY, c1 {column} CHARACTER SET {character_set}, \
             FULLTEXT (c1)) ENGINE=InnoDB"
        );
        let said = said_in_order(
            "",
            &[create.as_str(), "ALTER TABLE tx ADD COLUMN x INT NULL"],
        );
        assert_eq!(said.answer, UNKNOWN, "{create}");
        assert!(
            said.reason.contains("kilit lost track of tx"),
            "{create}: {}",
            said.reason
        );
    }
}

#[test]
fn a_foreign_key_that_sets_null_on_update_on_a_not_null_column_is_unknown_and_not_followed() {
    // By hand: ON UPDATE SET NULL and ON DELETE SET NULL ON
    // UPDATE CASCADE on the NOT NULL user_id are 1830, and DROP COLUMN user_id
    // is then accepted (not 1828). Kilit has not measured these shapes.
    let drop_column = "ALTER TABLE orders DROP COLUMN user_id";
    let control = said_in_order(LAB_TABLES, &[drop_column]);
    assert_ne!(control.answer, FAILS_1828);
    for key in ["ON UPDATE SET NULL", "ON DELETE SET NULL ON UPDATE CASCADE"] {
        let add_key = format!(
            "ALTER TABLE orders ADD CONSTRAINT fk_u FOREIGN KEY (user_id) REFERENCES users (id) {key}"
        );
        let said = said_in_order(LAB_TABLES, &[add_key.as_str()]);
        assert_eq!(said.answer, UNKNOWN, "{key}");
        let dropped = said_in_order(LAB_TABLES, &[add_key.as_str(), drop_column]);
        assert_ne!(dropped.answer, FAILS_1828, "{key}");
        assert_eq!(dropped.answer, control.answer, "{key}");
    }
    // ON DELETE SET NULL alone keeps its measured failure.
    let said = said_in_order(LAB_TABLES, &[B1D_SET_NULL_KEY]);
    assert_eq!(said.answer, FAILS_1830);
}

#[test]
fn a_global_setting_in_the_trailer_of_a_file_is_unclear_in_the_next_file() {
    // mysqldump's last lines run after the last statement. By hand on MySQL 8.0.46
    // and 8.4.13: /*!40014 SET GLOBAL FOREIGN_KEY_CHECKS=0 */ makes the next session see
    // 0, where adding a key does not copy the table; a session setting ends
    // with its session. The last statement of the first file keeps its
    // answer either way.
    let schema = Schema::from_sql(LAB_TABLES);
    for (trailer, later_is_unknown) in [
        ("/*!40014 SET GLOBAL FOREIGN_KEY_CHECKS=0 */;", true),
        ("/*!80000 SET PERSIST foreign_key_checks = 0 */;", true),
        (
            "/*!40014 SET FOREIGN_KEY_CHECKS=@OLD_FOREIGN_KEY_CHECKS */;",
            false,
        ),
        ("/*!40014 SET FOREIGN_KEY_CHECKS=0 */;", false),
    ] {
        let first = parse(&format!("{NEXT_COLUMN};\n{trailer}\n"));
        let second = parse(&format!("{FOREIGN_KEY_ON_USER};"));
        for server in [MYSQL_80, MYSQL_84] {
            let results = Knowledge::built_in().server(server).unwrap();
            let findings = check_sessions(&[&first, &second], &schema, results);
            assert_eq!(findings.len(), 2);
            match &findings[0].prediction {
                Prediction::Measured { verdict, .. } => {
                    assert_eq!(verdict.to_string(), INSTANT, "{server}: {trailer}");
                }
                other => panic!("{server}: {trailer}: {other:?}"),
            }
            match &findings[1].prediction {
                Prediction::Unknown { .. } => {
                    assert!(later_is_unknown, "{server}: {trailer}: unknown")
                }
                Prediction::Measured { verdict, .. } => {
                    assert!(!later_is_unknown, "{server}: {trailer}: {verdict}");
                    assert_eq!(verdict.to_string(), COPY, "{server}: {trailer}");
                }
                other => panic!("{server}: {trailer}: {other:?}"),
            }
        }
    }
}

#[test]
fn a_comment_that_makes_a_setting_unclear_names_the_output_line_under_pretend() {
    // Laravel prints a statement on one line, so a comment's "line" in a
    // reason is the line of the statement in the output, not a line inside
    // the statement's text.
    let key = "alter table `orders` add constraint `orders_user_id_foreign` foreign key \
               (`user_id`) references `users` (`id`)";
    let comment = "/*!40014 SET FOREIGN_KEY_CHECKS=0 */";
    let results = Knowledge::built_in().server(MYSQL_84).unwrap();
    for printed in [
        // The comment before the key's first word: the key's own line.
        vec![
            "alter table `orders` add `c` int null".to_owned(),
            format!("{comment} {key}"),
        ],
        // The comment after the last statement of its line: that line.
        vec![
            format!("alter table `orders` add `c` int null; {comment}"),
            key.to_owned(),
        ],
    ] {
        let statements: Vec<&str> = printed.iter().map(String::as_str).collect();
        let output = pretend("2026_01_01_000000_add_things", &statements);
        let expected = output
            .lines()
            .position(|line| line.contains(comment))
            .unwrap()
            + 1;
        assert!(expected > 4, "{output}");
        let pretend = read_pretend(&output).expect("the output of migrate --pretend");
        let all: Vec<Statement> = pretend
            .migrations
            .into_iter()
            .flat_map(|migration| migration.statements)
            .collect();
        let findings = check(&all, &Schema::from_sql(LAB_TABLES), results);
        assert_eq!(findings.len(), 2, "{output}");
        let Prediction::Unknown { reason, .. } = &findings[1].prediction else {
            panic!("{output}: {:?}", findings[1].prediction);
        };
        assert!(
            reason.contains(&format!("line {expected} runs a /*! */ comment")),
            "expected line {expected}: {reason}"
        );
    }
}

// ---------------------------------------------------------------------------
// Step B1d, refusals: a refusal is judged on the table as the whole
// ALTER leaves it. Each shape was run by hand on MySQL 8.0.46 and 8.4.13
// (the same on both): the ALTER that MySQL refuses changes nothing, and the
// ALTER that it accepts changes the table as all its clauses say.

const TWO_SET_NULL_KEYS: &str = "CREATE TABLE ch (id INT PRIMARY KEY, uid BIGINT UNSIGNED NULL, \
     CONSTRAINT fk_a FOREIGN KEY (uid) REFERENCES users (id) ON DELETE SET NULL, \
     CONSTRAINT fk_b FOREIGN KEY (uid) REFERENCES users (id) ON UPDATE SET NULL)";
const UNNAMED_SET_NULL_KEY: &str = "CREATE TABLE ch (id INT PRIMARY KEY, \
     uid BIGINT UNSIGNED NULL, \
     FOREIGN KEY (uid) REFERENCES users (id) ON DELETE SET NULL)";
const DROP_UID: &str = "ALTER TABLE ch DROP COLUMN uid";

#[test]
fn a_refused_modify_not_null_beside_the_drop_of_one_of_two_set_null_keys_leaves_both_keys() {
    // Two SET NULL keys on uid; one ALTER drops fk_a and makes uid NOT NULL.
    // MySQL refuses it with 1830 (fk_b still needs a NULL column), so fk_a is
    // still there: after DROP FOREIGN KEY fk_b, DROP COLUMN uid fails 1828
    // (needed in fk_a).
    let refused = "ALTER TABLE ch DROP FOREIGN KEY fk_a, MODIFY uid BIGINT UNSIGNED NOT NULL";
    let drop_fk_b = "ALTER TABLE ch DROP FOREIGN KEY fk_b";
    let said = said_in_order(
        LAB_TABLES,
        &[TWO_SET_NULL_KEYS, refused, drop_fk_b, DROP_UID],
    );
    assert_eq!(said.answer, FAILS_1828);
    // The refused ALTER itself is not answered as a measured success.
    let refused_said = said_in_order(LAB_TABLES, &[TWO_SET_NULL_KEYS, refused]);
    assert_eq!(refused_said.answer, "unknown");
}

#[test]
fn an_unnamed_set_null_key_dropped_by_its_name_beside_a_modify_loses_the_table() {
    // MySQL names the key ch_ibfk_1 and accepts both ALTERs below (the table
    // then has uid NOT NULL, the key gone, its index left), and DROP COLUMN
    // uid is accepted. kilit does not give names to unnamed keys, so it
    // loses the table: unknown, never "fails with error 1828".
    for drop in ["DROP FOREIGN KEY ch_ibfk_1", "DROP CONSTRAINT ch_ibfk_1"] {
        let alter = format!("ALTER TABLE ch {drop}, MODIFY uid BIGINT UNSIGNED NOT NULL");
        let said = said_in_order(LAB_TABLES, &[UNNAMED_SET_NULL_KEY, &alter, DROP_UID]);
        assert_ne!(said.answer, FAILS_1828, "{drop}");
        assert_eq!(said.answer, "unknown", "{drop}");
        assert!(
            said.reason.contains("lost track of ch"),
            "{drop}: {}",
            said.reason
        );
    }
}

#[test]
fn a_refused_modify_not_null_beside_a_new_set_null_key_leaves_the_table_as_it_was() {
    // MODIFY uid ... NOT NULL plus ADD a SET NULL key on uid in one ALTER:
    // 1830, nothing changes (no key, uid still NULL). DROP COLUMN uid after
    // it is answered as on the table without the key, not 1828.
    let create = "CREATE TABLE ch (id INT PRIMARY KEY, uid BIGINT UNSIGNED NULL)";
    let refused = "ALTER TABLE ch MODIFY uid BIGINT UNSIGNED NOT NULL, ADD CONSTRAINT fk_u \
                   FOREIGN KEY (uid) REFERENCES users (id) ON DELETE SET NULL";
    let said = said_in_order(LAB_TABLES, &[create, refused, DROP_UID]);
    let without_the_key = said_in_order(LAB_TABLES, &[create, DROP_UID]);
    assert_ne!(said.answer, FAILS_1828);
    assert_eq!(said, without_the_key);
}

#[test]
fn a_refused_add_not_null_column_beside_a_new_set_null_key_adds_no_column() {
    // ADD COLUMN uid ... NOT NULL plus a SET NULL key on it: 1830, so the
    // table has no uid. DROP COLUMN uid after it is 1091 on the server; the
    // lab never measured that, so kilit says unknown, not 1828.
    let create = "CREATE TABLE ch (id INT PRIMARY KEY)";
    let refused = "ALTER TABLE ch ADD COLUMN uid BIGINT UNSIGNED NOT NULL, ADD CONSTRAINT fk_u \
                   FOREIGN KEY (uid) REFERENCES users (id) ON DELETE SET NULL";
    let said = said_in_order(LAB_TABLES, &[create, refused, DROP_UID]);
    assert_ne!(said.answer, FAILS_1828);
    assert_eq!(said.answer, "unknown");
}

#[test]
fn a_refused_fulltext_index_on_a_new_binary_text_column_adds_neither_column_nor_index() {
    // ADD COLUMN b TEXT CHARACTER SET binary plus ADD FULLTEXT (b): 1283, so
    // there is no column b and no index. ADD COLUMN b INT after it is
    // accepted, and it is the plain add-column answer (no FULLTEXT index on
    // the table).
    let binary_text = "ALTER TABLE orders ADD COLUMN b TEXT CHARACTER SET binary NULL, \
                       ADD FULLTEXT INDEX fb (b)";
    let add_b = "ALTER TABLE orders ADD COLUMN b INT NULL";
    let said = said_in_order(LAB_TABLES, &[binary_text, add_b]);
    let control = said_in_order(LAB_TABLES, &[add_b]);
    assert_eq!(said.case, Some("add-column"));
    assert_eq!(said.answer, INSTANT);
    assert_eq!(said, control);
}

#[test]
fn dropping_and_adding_back_a_binary_text_column_with_a_fulltext_index_is_followed() {
    // bin is TEXT CHARACTER SET binary. DROP COLUMN bin, ADD COLUMN bin TEXT
    // (utf8mb4), ADD FULLTEXT (bin) in one ALTER is accepted by MySQL (hand
    // test). kilit follows it: the table then has one FULLTEXT index, fb, on a
    // TEXT column of a character set that is not binary, so the later ADD
    // FULLTEXT is a second FULLTEXT index on a known table and gets the lab's
    // add-second-fulltext-index answer.
    let statements = [
        "ALTER TABLE orders ADD COLUMN bin TEXT CHARACTER SET binary NULL",
        "ALTER TABLE orders DROP COLUMN bin, ADD COLUMN bin TEXT NULL, \
         ADD FULLTEXT INDEX fb (bin)",
        "ALTER TABLE orders ADD FULLTEXT INDEX fn (note)",
    ];
    let said = said_in_order(LAB_TABLES, &statements);
    assert_eq!(said.answer, FULLTEXT_SECOND, "{}", said.reason);
    assert!(!said.answer.contains("fails"));
    assert_ne!(said.answer, FULLTEXT_REBUILD_WAIT);
    assert!(said.reason.is_empty(), "{}", said.reason);
    // The control: without the first ADD FULLTEXT the table has none, and
    // the same statement is the first FULLTEXT index.
    let control = said_in_order(LAB_TABLES, &[statements[2]]);
    assert_eq!(control.answer, FULLTEXT_REBUILD_WAIT);
}

// A refused ADD FULLTEXT on a binary TEXT column written with a prefix, and
// a refused ALTER that renames the table. By hand on MySQL 8.0.46 and
// 8.4.13 (the same on both).

#[test]
fn a_fulltext_index_on_a_binary_text_column_with_a_prefix_fails_and_is_not_followed() {
    // bin is TEXT CHARACTER SET binary. ADD FULLTEXT INDEX f (bin(10)) is
    // refused with error 1283, so the table has no FULLTEXT index: the next
    // ADD COLUMN gets the plain add-column answer, the same as without the
    // refused statement.
    let add_bin = "ALTER TABLE orders ADD COLUMN bin TEXT CHARACTER SET binary NULL";
    let fulltext = "ALTER TABLE orders ADD FULLTEXT INDEX f (bin(10))";
    let add_zz = "ALTER TABLE orders ADD COLUMN zz INT NULL";
    let said = said_in_order(LAB_TABLES, &[add_bin, fulltext, add_zz]);
    let control = said_in_order(LAB_TABLES, &[add_bin, add_zz]);
    assert_eq!(said.case, Some("add-column"));
    assert_eq!(said.answer, INSTANT);
    assert_eq!(said, control);
    // The refused statement itself is not given a measured success.
    let refused = said_in_order(LAB_TABLES, &[add_bin, fulltext]);
    assert_ne!(refused.answer, FULLTEXT_REBUILD_WAIT);
    assert_ne!(refused.answer, FULLTEXT_SECOND);
}

#[test]
fn a_create_table_with_a_prefix_fulltext_index_on_a_binary_text_column_loses_the_table() {
    // MySQL refuses the CREATE TABLE (1283), so there is no table; kilit does
    // not check CREATE TABLE and must not keep a table it may not have.
    let create = "CREATE TABLE a5 (id INT PRIMARY KEY, bin TEXT CHARACTER SET binary, \
                  FULLTEXT INDEX f (bin(10)))";
    let said = said_in_order(
        LAB_TABLES,
        &[create, "ALTER TABLE a5 ADD COLUMN zz INT NULL"],
    );
    assert_eq!(said.answer, "unknown");
    assert_ne!(said.answer, INSTANT);
}

#[test]
fn a_refused_rename_with_a_modify_not_null_on_a_set_null_key_column_changes_nothing() {
    // ALTER TABLE ch RENAME TO chb, MODIFY uid ... NOT NULL: uid has a SET
    // NULL key, so MySQL refuses it (1830). ch stays, chb does not exist.
    let create = "CREATE TABLE ch (id INT PRIMARY KEY, uid BIGINT UNSIGNED NULL, \
                  CONSTRAINT fk_ch FOREIGN KEY (uid) REFERENCES users (id) ON DELETE SET NULL)";
    let refused = "ALTER TABLE ch RENAME TO chb, MODIFY uid BIGINT UNSIGNED NOT NULL";
    let add_q_to_chb = "ALTER TABLE chb ADD COLUMN q INT NULL";
    let add_q_to_ch = "ALTER TABLE ch ADD COLUMN q INT NULL";
    // chb does not exist: never the add-column answer of a known table.
    let on_chb = said_in_order(LAB_TABLES, &[create, refused, add_q_to_chb]);
    assert_ne!(on_chb.answer, INSTANT);
    assert_eq!(on_chb.answer, "unknown");
    // ch keeps its answers, as if the refused ALTER had not been there.
    let on_ch = said_in_order(LAB_TABLES, &[create, refused, add_q_to_ch]);
    let control = said_in_order(LAB_TABLES, &[create, add_q_to_ch]);
    assert_eq!(on_ch, control);
    let drop_uid = said_in_order(LAB_TABLES, &[create, refused, DROP_UID_OF_CH]);
    assert_eq!(drop_uid.answer, FAILS_1828);
}

const DROP_UID_OF_CH: &str = "ALTER TABLE ch DROP COLUMN uid";

// A refused ADD FULLTEXT that sits beside ADD COLUMN, and a prefix FULLTEXT on a VIRTUAL generated column or a CHAR column. By
// hand on MySQL 8.0.46 and 8.4.13 (the same on both).

#[test]
fn a_refused_fulltext_beside_the_binary_text_column_it_adds_leaves_the_table_as_it_was() {
    // One ALTER adds bin (TEXT CHARACTER SET binary) and a FULLTEXT index on
    // `BIN`(10): MySQL refuses it with 1283, so the table gets neither the
    // column nor the index. A later ADD COLUMN bin INT therefore gets the
    // plain add-column answer, the same as without the refused statement.
    let refused = "ALTER TABLE orders ADD COLUMN bin TEXT CHARACTER SET binary NULL, \
                   ADD FULLTEXT INDEX f (`BIN`(10))";
    let add_bin = "ALTER TABLE orders ADD COLUMN bin INT NULL";
    let said = said_in_order(LAB_TABLES, &[refused, add_bin]);
    let control = said_in_order(LAB_TABLES, &[add_bin]);
    assert_eq!(said.case, Some("add-column"));
    assert_eq!(said.answer, INSTANT);
    assert_eq!(said, control);
}

const G1_TABLE: &str = "CREATE TABLE `g1` (`id` int NOT NULL, `note` text, \
    `g` text GENERATED ALWAYS AS (`note`) VIRTUAL, `c` char(20) DEFAULT NULL, \
    `j` json DEFAULT NULL, PRIMARY KEY (`id`)) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4;\n";

#[test]
fn a_prefix_fulltext_index_on_a_virtual_generated_column_is_unknown_naming_3106_and_not_followed() {
    let fulltext = "ALTER TABLE g1 ADD FULLTEXT INDEX f (g(10))";
    let add_zz = "ALTER TABLE g1 ADD COLUMN zz INT NULL";
    let refused = said_in_order(G1_TABLE, &[fulltext]);
    assert_eq!(refused.answer, "unknown");
    assert!(refused.reason.contains("3106"), "{}", refused.reason);
    // Not followed: the next ADD COLUMN keeps the add-column answer, the
    // same as without the refused statement.
    let said = said_in_order(G1_TABLE, &[fulltext, add_zz]);
    let control = said_in_order(G1_TABLE, &[add_zz]);
    assert_eq!(said.case, Some("add-column"));
    assert_eq!(said.answer, INSTANT);
    assert_eq!(said, control);
}

#[test]
fn a_prefix_fulltext_index_on_a_char_column_is_unknown_not_a_measured_fulltext_answer() {
    // MySQL accepts it (no rebuild measured here), but the lab measured
    // FULLTEXT only on TEXT columns without a prefix.
    let said = said_in_order(G1_TABLE, &["ALTER TABLE g1 ADD FULLTEXT INDEX f (c(10))"]);
    assert_eq!(said.answer, "unknown");
    assert_ne!(said.answer, FULLTEXT_REBUILD_WAIT);
    assert_ne!(said.answer, FULLTEXT_SECOND);
}

// A primary key on a column of an ON DELETE SET
// NULL key. By hand on MySQL 8.0.46 and 8.4.13 (the same on both): ADD
// PRIMARY KEY (uid), MODIFY uid ... PRIMARY KEY, CHANGE uid uid ... PRIMARY
// KEY, DROP PRIMARY KEY + ADD PRIMARY KEY (uid), and DROP PRIMARY KEY +
// MODIFY uid + ADD PRIMARY KEY (id, uid) all fail with 1830 and leave the
// table as it was; MODIFY uid ... NULL with ADD PRIMARY KEY (uid) in one
// statement fails with 1171; a later DROP INDEX of the key's only index
// then fails with 1553 and a later DROP COLUMN uid with 1828.

const B1D_PK_USERS: &str = "CREATE TABLE users (id BIGINT UNSIGNED AUTO_INCREMENT PRIMARY KEY, \
    name VARCHAR(100) NOT NULL) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4;\n";

/// Tables of a SET NULL key without a primary key (n1 to n4) and with one
/// (p1, p2), as the hand test built them.
const B1D_PK_TABLES: &str = "\
    CREATE TABLE n1 (id BIGINT UNSIGNED NOT NULL, uid BIGINT UNSIGNED NULL, \
        CONSTRAINT f1 FOREIGN KEY (uid) REFERENCES users (id) ON DELETE SET NULL);\n\
    CREATE TABLE n2 (id BIGINT UNSIGNED NOT NULL, uid BIGINT UNSIGNED NULL, \
        CONSTRAINT f2 FOREIGN KEY (uid) REFERENCES users (id) ON DELETE SET NULL);\n\
    CREATE TABLE n3 (id BIGINT UNSIGNED NOT NULL, uid BIGINT UNSIGNED NULL, \
        CONSTRAINT f3 FOREIGN KEY (uid) REFERENCES users (id) ON DELETE SET NULL);\n\
    CREATE TABLE n4 (id BIGINT UNSIGNED NOT NULL, uid BIGINT UNSIGNED NULL, \
        CONSTRAINT f4 FOREIGN KEY (uid) REFERENCES users (id) ON DELETE SET NULL);\n\
    CREATE TABLE p1 (id BIGINT UNSIGNED NOT NULL PRIMARY KEY, uid BIGINT UNSIGNED NULL, \
        CONSTRAINT g1 FOREIGN KEY (uid) REFERENCES users (id) ON DELETE SET NULL);\n\
    CREATE TABLE p2 (id BIGINT UNSIGNED NOT NULL PRIMARY KEY, uid BIGINT UNSIGNED NULL, \
        CONSTRAINT g2 FOREIGN KEY (uid) REFERENCES users (id) ON DELETE SET NULL);\n";

/// A table `s` with a SET NULL key and its own index `ix`.
const B1D_PK_S_TABLE: &str = "CREATE TABLE `s` (`id` bigint unsigned NOT NULL, \
    `uid` bigint unsigned DEFAULT NULL, `v` int DEFAULT NULL, KEY `ix` (`uid`), \
    CONSTRAINT `fs` FOREIGN KEY (`uid`) REFERENCES `users` (`id`) ON DELETE SET NULL) \
    ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_0900_ai_ci;\n";

#[test]
fn a_primary_key_on_a_set_null_key_column_is_not_followed_so_dropping_the_column_fails_or_is_unknown()
 {
    // ALTER TABLE n1 ADD PRIMARY KEY (uid)
    // ALTER TABLE n2 MODIFY uid BIGINT UNSIGNED PRIMARY KEY
    // ALTER TABLE n4 CHANGE uid uid BIGINT UNSIGNED PRIMARY KEY
    // ALTER TABLE p1 DROP PRIMARY KEY, ADD PRIMARY KEY (uid)
    // ALTER TABLE p2 DROP PRIMARY KEY, MODIFY uid BIGINT UNSIGNED, ADD PRIMARY KEY (id, uid)
    // n2 and n4 used to lose the table (the inline PRIMARY KEY was not
    // read); they are refused with 1830 now, the table kept.
    let mut problems = Vec::new();
    let schema = format!("{B1D_PK_USERS}{B1D_PK_TABLES}");
    for (table, change) in [
        ("n1", "ADD PRIMARY KEY (uid)"),
        ("n2", "MODIFY uid BIGINT UNSIGNED PRIMARY KEY"),
        ("n4", "CHANGE uid uid BIGINT UNSIGNED PRIMARY KEY"),
        ("p1", "DROP PRIMARY KEY, ADD PRIMARY KEY (uid)"),
        (
            "p2",
            "DROP PRIMARY KEY, MODIFY uid BIGINT UNSIGNED, ADD PRIMARY KEY (id, uid)",
        ),
    ] {
        let alter = format!("ALTER TABLE {table} {change};\n");
        let drop_column = format!("ALTER TABLE {table} DROP COLUMN uid;\n");
        let answers = answers_of(&schema, &format!("{alter}{drop_column}"));
        // the inline PRIMARY KEY of n2 and n4 is read now. It is
        // refused with 1830 like the other forms and leaves the table as it
        // was, so the later drop fails with 1828 for every table.
        if answers[1] != FAILS_1828 {
            problems.push(format!("{table}: {change}: got {answers:?}"));
        }
        // The refused statement is not a measured success.
        if answers[0] != UNKNOWN {
            problems.push(format!("{table}: {change}: first answer {answers:?}"));
        }
    }
    assert!(problems.is_empty(), "{problems:#?}");
}

#[test]
fn a_primary_key_on_a_set_null_key_column_made_by_a_modify_and_an_add_is_not_followed() {
    // ALTER TABLE n3 MODIFY uid BIGINT UNSIGNED, ADD PRIMARY KEY (uid)
    // By hand on MySQL 8.0.46 and 8.4.13: the key stays and the column stays
    // nullable, so a later DROP COLUMN uid fails with 1828.
    let schema = format!("{B1D_PK_USERS}{B1D_PK_TABLES}");
    let sql = "ALTER TABLE n3 MODIFY uid BIGINT UNSIGNED, ADD PRIMARY KEY (uid);\n\
               ALTER TABLE n3 DROP COLUMN uid;\n";
    assert_eq!(answers_of(&schema, sql).pop().unwrap(), FAILS_1828);
}

#[test]
fn the_same_primary_key_forms_on_tables_made_in_the_input_leave_the_dropped_column_failing_with_1828()
 {
    // The same forms on tables the input creates.
    let create_s = "CREATE TABLE s (id BIGINT UNSIGNED NOT NULL, uid BIGINT UNSIGNED NULL, \
        v INT NULL, CONSTRAINT fs FOREIGN KEY (uid) REFERENCES users (id) \
        ON DELETE SET NULL) ENGINE=InnoDB;\n";
    let create_s_with_key = "CREATE TABLE s (id BIGINT UNSIGNED NOT NULL PRIMARY KEY, \
        uid BIGINT UNSIGNED NULL, v INT NULL, CONSTRAINT fs FOREIGN KEY (uid) \
        REFERENCES users (id) ON DELETE SET NULL) ENGINE=InnoDB;\n";
    for (create, change) in [
        (create_s, "ADD PRIMARY KEY (uid)"),
        (create_s_with_key, "DROP PRIMARY KEY, ADD PRIMARY KEY (uid)"),
        (
            create_s_with_key,
            "DROP PRIMARY KEY, MODIFY uid BIGINT UNSIGNED, ADD PRIMARY KEY (id, uid)",
        ),
    ] {
        let sql = format!(
            "{B1D_PK_USERS}{create}ALTER TABLE s {change};\nALTER TABLE s DROP COLUMN uid;\n"
        );
        let answers = answers_of("", &sql);
        let n = answers.len();
        assert_eq!(answers[n - 1], FAILS_1828, "{change}");
    }
}

#[test]
fn a_modify_with_null_and_a_primary_key_on_a_set_null_key_column_is_not_followed_so_the_index_stays()
 {
    // By hand on MySQL 8.0.46 and 8.4.13: ALTER TABLE s MODIFY uid BIGINT
    // UNSIGNED NULL, ADD PRIMARY KEY (uid) fails with 1171, then ALTER TABLE s
    // DROP INDEX ix fails with 1553.
    let schema = format!("{B1D_PK_USERS}{B1D_PK_S_TABLE}");
    let sql = "ALTER TABLE s MODIFY uid BIGINT UNSIGNED NULL, ADD PRIMARY KEY (uid);\n\
               ALTER TABLE s DROP INDEX ix;\n";
    assert_eq!(answers_of(&schema, sql).pop().unwrap(), FAILS_1553);
}

#[test]
fn dropping_the_only_index_of_a_set_null_key_still_fails_with_1553_after_a_refused_primary_key() {
    // By hand on MySQL 8.0.46 and 8.4.13: the primary key on uid is refused
    // (1830), the table is unchanged, so DROP INDEX ix fails with 1553.
    let schema = format!("{B1D_PK_USERS}{B1D_PK_S_TABLE}");
    for sql in [
        // In one statement with a MODIFY.
        "ALTER TABLE s MODIFY uid BIGINT UNSIGNED, ADD PRIMARY KEY (uid);\n\
         ALTER TABLE s DROP INDEX ix;\n",
        // Alone, after a MODIFY of its own.
        "ALTER TABLE s MODIFY uid BIGINT UNSIGNED NULL;\n\
         ALTER TABLE s ADD PRIMARY KEY (uid);\n\
         ALTER TABLE s DROP INDEX ix;\n",
    ] {
        assert_eq!(answers_of(&schema, sql).pop().unwrap(), FAILS_1553, "{sql}");
    }
}

#[test]
fn swapping_two_column_names_with_two_changes_is_followed_so_the_key_column_is_the_one_that_cannot_be_dropped()
 {
    // By hand on MySQL 8.0.46 and 8.4.13: ALTER TABLE s1 CHANGE id uid BIGINT
    // UNSIGNED NOT NULL, CHANGE uid x BIGINT UNSIGNED NULL is accepted, each
    // clause against the names before the statement: uid is the old id (the
    // primary key) and x the old uid (the SET NULL key's column). kilit follows
    // it, so DROP COLUMN x fails with 1828 and DROP COLUMN uid, the primary
    // key column, is neither 1828 nor 1830. The swap itself is not refused.
    let schema = "CREATE TABLE `users` (`id` bigint unsigned NOT NULL AUTO_INCREMENT, \
        `name` varchar(100) NOT NULL, PRIMARY KEY (`id`)) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4;\n\
        CREATE TABLE `s1` (`id` bigint unsigned NOT NULL, `uid` bigint unsigned DEFAULT NULL, \
        PRIMARY KEY (`id`), KEY `fs1` (`uid`), CONSTRAINT `fs1` FOREIGN KEY (`uid`) \
        REFERENCES `users` (`id`) ON DELETE SET NULL) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4;\n";
    let swap = "ALTER TABLE s1 CHANGE id uid BIGINT UNSIGNED NOT NULL, \
                CHANGE uid x BIGINT UNSIGNED NULL;\n";
    for drop in ["x", "uid"] {
        let sql = format!("{swap}ALTER TABLE s1 DROP COLUMN {drop};\n");
        for (at, name) in ["the swap", "the drop"].into_iter().enumerate() {
            let sql_to = if at == 0 {
                swap.to_owned()
            } else {
                sql.clone()
            };
            let said = said(schema, &sql_to);
            if at == 1 && drop == "x" {
                assert_eq!(said.answer, FAILS_1828, "{name} of {drop}");
            } else {
                assert_ne!(said.answer, FAILS_1828, "{name} of {drop}");
            }
            assert_ne!(said.answer, FAILS_1830, "{name} of {drop}");
            assert!(
                !said.reason.contains(NOT_ACCEPTED),
                "{name} of {drop}: {}",
                said.reason
            );
        }
    }
}

// By hand on MySQL 8.0.46 and 8.4.13 (the same on both): an ALTER with a
// change MySQL may refuse that also names one column in two clauses. kilit
// cannot tell how MySQL takes it and loses the table.

#[test]
fn a_dropped_fulltext_index_and_a_not_null_set_null_key_column_beside_a_column_drop_and_add_is_refused_and_keeps_the_index()
 {
    // DROP x with ADD x is followed (accepted by hand). The ALTER is
    // refused as a whole with 1830 (MODIFY uid NOT NULL under a SET NULL
    // key), so the FULLTEXT index stays and the later ADD COLUMN w is the
    // measured add-column-with-fulltext answer: COPY.
    let schema = "CREATE TABLE `users` (`id` bigint unsigned NOT NULL AUTO_INCREMENT, \
        `name` varchar(100) NOT NULL, PRIMARY KEY (`id`)) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4;\n\
        CREATE TABLE `n` (`id` bigint unsigned NOT NULL, `uid` bigint unsigned DEFAULT NULL, \
        `x` bigint unsigned DEFAULT NULL, `code` varchar(20) DEFAULT NULL, `body` text, \
        PRIMARY KEY (`id`), FULLTEXT KEY `ft` (`body`), KEY `f1` (`uid`), \
        CONSTRAINT `f1` FOREIGN KEY (`uid`) REFERENCES `users` (`id`) ON DELETE SET NULL) \
        ENGINE=InnoDB DEFAULT CHARSET=utf8mb4;\n";
    let first = "ALTER TABLE n DROP INDEX ft, MODIFY uid BIGINT UNSIGNED NOT NULL, \
                 DROP COLUMN x, ADD COLUMN x INT NULL;\n";
    // The ALTER itself has several changes, which the lab measured one at a
    // time: unknown.
    assert_eq!(said(schema, first).answer, "unknown");
    let said = said(
        schema,
        &format!("{first}ALTER TABLE n ADD COLUMN w INT NULL;\n"),
    );
    assert_eq!(said.answer, COPY, "{}", said.reason);
    assert_eq!(said.case, Some("add-column"));
    assert_eq!(said.evidence, ["add-column-with-fulltext"]);
}

#[test]
fn a_refused_binary_text_fulltext_beside_a_column_drop_and_add_does_not_make_a_later_add_column_copy()
 {
    // DROP x with ADD x is followed. The first ALTER fails with 1283
    // (FULLTEXT on a binary TEXT column), so there is no q and no index, the
    // table is unchanged, and a later ADD COLUMN w runs INSTANT, never COPY.
    let schema = "CREATE TABLE `users` (`id` bigint unsigned NOT NULL AUTO_INCREMENT, \
        `name` varchar(100) NOT NULL, PRIMARY KEY (`id`)) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4;\n\
        CREATE TABLE `n` (`id` bigint unsigned NOT NULL, `uid` bigint unsigned DEFAULT NULL, \
        `x` bigint unsigned DEFAULT NULL, `code` varchar(20) DEFAULT NULL, \
        PRIMARY KEY (`id`), KEY `f1` (`uid`), \
        CONSTRAINT `f1` FOREIGN KEY (`uid`) REFERENCES `users` (`id`) ON DELETE SET NULL) \
        ENGINE=InnoDB DEFAULT CHARSET=utf8mb4;\n";
    let first = "ALTER TABLE n ADD COLUMN q TEXT CHARACTER SET binary, ADD FULLTEXT ft (q), \
                 DROP COLUMN x, ADD COLUMN x INT NULL;\n";
    // The ALTER itself is never a measured success.
    let refused = said(schema, first);
    assert!(
        refused.answer == FAILS_1283 || refused.answer == "unknown",
        "{refused:?}"
    );
    let said = said(
        schema,
        &format!("{first}ALTER TABLE n ADD COLUMN w INT NULL;\n"),
    );
    assert_eq!(said.answer, INSTANT, "{}", said.reason);
    assert_eq!(said.case, Some("add-column"));
    assert!(!said.answer.starts_with(COPY_PREFIX));
}

// kilit's cautious rule, not a MySQL fact: an ALTER with a change MySQL may
// refuse (MODIFY of a SET NULL key's column to NOT NULL, 1830) beside a
// clause kilit cannot follow loses the table, and a later statement on it is
// unknown, never a measured answer.

#[test]
fn a_clause_kilit_cannot_follow_beside_a_change_mysql_may_refuse_loses_the_table() {
    // the first case adds a column the table has. By hand on MySQL
    // 8.0.46 and 8.4.13 that is error 1060 (as is the MODIFY: 1830), so the
    // ALTER is refused as a whole and the table is kept, not lost: the next
    // ADD COLUMN is INSTANT. The other cases name a clause kilit does not
    // follow, so they still lose the table.
    let schema = "CREATE TABLE `users` (`id` bigint unsigned NOT NULL AUTO_INCREMENT, \
        PRIMARY KEY (`id`)) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4;\n\
        CREATE TABLE `t` (`id` bigint unsigned NOT NULL AUTO_INCREMENT, \
        `uid` bigint unsigned DEFAULT NULL, `note` varchar(100) DEFAULT NULL, \
        PRIMARY KEY (`id`), KEY `fk_u` (`uid`), \
        CONSTRAINT `fk_u` FOREIGN KEY (`uid`) REFERENCES `users` (`id`) ON DELETE SET NULL) \
        ENGINE=InnoDB DEFAULT CHARSET=utf8mb4;\n";
    let cases = [
        (
            "ALTER TABLE t MODIFY uid BIGINT UNSIGNED NOT NULL, ADD COLUMN note INT NULL;",
            "1060",
            false,
        ),
        (
            "ALTER TABLE t MODIFY uid BIGINT UNSIGNED NOT NULL, ADD COLUMN z INT UNIQUE;",
            "options kilit does not follow",
            true,
        ),
        (
            "ALTER TABLE t MODIFY uid BIGINT UNSIGNED NOT NULL, ADD COLUMN z INT UNIQUE;",
            "beside a change MySQL may refuse",
            true,
        ),
    ];
    for (first, words, lost) in cases {
        let sql = format!("{first}\nALTER TABLE t ADD COLUMN q INT;\n");
        let later = said(schema, &sql);
        if lost {
            assert_eq!(later.answer, "unknown", "{first}");
            assert!(later.reason.contains(words), "{first}: {}", later.reason);
            assert_ne!(later.answer, INSTANT, "{first}");
        } else {
            let refused = said(schema, first);
            assert_eq!(refused.answer, "unknown", "{first}");
            assert!(
                refused.reason.contains(words),
                "{first}: {}",
                refused.reason
            );
            assert_eq!(later.answer, INSTANT, "{first}");
            assert_eq!(later.case, Some("add-column"), "{first}");
        }
    }
}

// ---------------------------------------------------------------------------
// The tables kilit follows cannot be made wrong by valid
// input. Every expectation below comes from hand tests on MySQL 8.0.46 and
// 8.4.13 (the same on both) or from a lab measurement named in the test.
// The tables are the ones the hand tests used, written as mysqldump writes
// them.

/// `users` and `orders` of the hand tests: `orders` has no key.
const USERS_AND_ORDERS_DUMP: &str = "CREATE TABLE `users` (
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
  PRIMARY KEY (`id`),
  UNIQUE KEY `orders_email_unique` (`email`)
) ENGINE=InnoDB AUTO_INCREMENT=3 DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_0900_ai_ci;
";

/// The same, with the foreign key `orders_user_id_foreign` on `orders`.
const USERS_AND_ORDERS_WITH_FK_DUMP: &str = "CREATE TABLE `users` (
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
  PRIMARY KEY (`id`),
  UNIQUE KEY `orders_email_unique` (`email`),
  KEY `orders_user_id_foreign` (`user_id`),
  CONSTRAINT `orders_user_id_foreign` FOREIGN KEY (`user_id`) REFERENCES `users` (`id`)
) ENGINE=InnoDB AUTO_INCREMENT=3 DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_0900_ai_ci;
";

/// The tables of the hand tests for the names a table lacks or has: `orders`
/// with a foreign key, an index and a CHECK, `invoices` with a foreign key and
/// a CHECK of their own, `nopk` without a primary key, `other` with an index,
/// and `s3` with a CHECK and a UNIQUE index of one name.
const TABLES_WITH_NAMES_DUMP: &str = "CREATE TABLE `users` (
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
  PRIMARY KEY (`id`),
  UNIQUE KEY `orders_email_unique` (`email`),
  KEY `orders_user_id_foreign` (`user_id`),
  KEY `ix_amount` (`amount`),
  CONSTRAINT `orders_user_id_foreign` FOREIGN KEY (`user_id`) REFERENCES `users` (`id`),
  CONSTRAINT `orders_amount_check` CHECK ((`amount` > 0))
) ENGINE=InnoDB AUTO_INCREMENT=3 DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_0900_ai_ci;
CREATE TABLE `invoices` (
  `id` bigint unsigned NOT NULL,
  `user_id` bigint unsigned NOT NULL,
  `total` int NOT NULL,
  PRIMARY KEY (`id`),
  KEY `invoices_user_id_foreign` (`user_id`),
  CONSTRAINT `invoices_user_id_foreign` FOREIGN KEY (`user_id`) REFERENCES `users` (`id`),
  CONSTRAINT `invoices_total_check` CHECK ((`total` > 0))
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_0900_ai_ci;
CREATE TABLE `nopk` (
  `a` int DEFAULT NULL,
  `b` int DEFAULT NULL
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_0900_ai_ci;
CREATE TABLE `other` (
  `id` int NOT NULL,
  `c` int DEFAULT NULL,
  PRIMARY KEY (`id`),
  KEY `ix_other` (`c`)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_0900_ai_ci;
CREATE TABLE `s3` (
  `id` int NOT NULL,
  `c` int DEFAULT NULL,
  PRIMARY KEY (`id`),
  UNIQUE KEY `dup` (`c`),
  CONSTRAINT `dup` CHECK ((`c` > 0))
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_0900_ai_ci;
";

/// A statement after a refused one: it is measured (INSTANT, add-column)
/// only on a table kilit still knows, and unknown on one it lost.
fn add_probe_column(table: &str) -> String {
    format!("ALTER TABLE {table} ADD COLUMN probe_col INT NULL;")
}

/// What kilit says about each statement of the files, run in order as
/// sessions of their own, against `dump` (the `--schema` file) or, with
/// `None`, against nothing.
struct FilesRun {
    statements: Vec<Statement>,
    findings: Vec<kilit_core::Finding>,
}

impl FilesRun {
    fn on(server: &str, dump: Option<&str>, files: &[&str]) -> FilesRun {
        let parsed: Vec<Vec<Statement>> = files.iter().map(|file| parse(file)).collect();
        let sessions: Vec<&[Statement]> = parsed.iter().map(Vec::as_slice).collect();
        let schema = match dump {
            Some(dump) => Schema::from_dump(dump),
            None => Schema::new(),
        };
        let results = Knowledge::built_in().server(server).unwrap();
        let findings = check_sessions(&sessions, &schema, results);
        FilesRun {
            statements: parsed.into_iter().flatten().collect(),
            findings,
        }
    }

    fn answers(&self) -> Vec<String> {
        self.findings
            .iter()
            .map(|finding| answer_word(&finding.prediction))
            .collect()
    }

    fn impacts(&self) -> Vec<Impact> {
        impacts(&self.statements, &self.findings)
    }

    /// The reason of statement `at`, which must be unknown.
    fn reason(&self, at: usize) -> String {
        match &self.findings[at].prediction {
            Prediction::Unknown { reason, .. } => reason.clone(),
            other => panic!("statement {at}: expected unknown, got {other:?}"),
        }
    }

    /// The tables each statement waits for, as the metadata lock warning
    /// names them.
    fn locked(&self) -> Vec<Vec<String>> {
        kilit_core::locked_tables(&self.statements, &self.findings)
    }

    fn last_answer(&self) -> String {
        self.answers().pop().unwrap()
    }

    fn last_impact(&self) -> Impact {
        self.impacts().pop().unwrap()
    }
}

fn answer_word(prediction: &Prediction) -> String {
    match prediction {
        Prediction::Measured {
            verdict: Verdict::Fails { code, .. },
            ..
        } => format!("fails with error {code}"),
        Prediction::Measured { verdict, .. } => verdict.to_string(),
        Prediction::Unknown { .. } => "unknown".to_owned(),
        Prediction::NotChecked { .. } => "not checked".to_owned(),
        Prediction::DependsOnTable { shared, .. } => match shared {
            Shared::WritesWait => "depends on the table: writes wait".to_owned(),
            Shared::Fails => "depends on the table: fails".to_owned(),
        },
    }
}

/// The run on both servers, which must answer and weigh every statement the
/// same way; the run on 8.4.13.
fn run_files_on_both_servers(dump: Option<&str>, files: &[&str]) -> FilesRun {
    let on_80 = FilesRun::on(MYSQL_80, dump, files);
    let on_84 = FilesRun::on(MYSQL_84, dump, files);
    assert_eq!(on_80.answers(), on_84.answers(), "{files:?}");
    assert_eq!(on_80.impacts(), on_84.impacts(), "{files:?}");
    assert_eq!(on_80.locked(), on_84.locked(), "{files:?}");
    on_84
}

/// One input file against the dump `dump`.
fn run_on_dump(dump: &str, sql: &str) -> FilesRun {
    run_files_on_both_servers(Some(dump), &[sql])
}

/// One input file and no `--schema`.
fn run_without_dump(sql: &str) -> FilesRun {
    run_files_on_both_servers(None, &[sql])
}

/// The statement at `at` must be unknown and name the error.
fn assert_refused_with(run: &FilesRun, at: usize, code: &str, what: &str) -> Result<(), String> {
    if run.answers()[at] != "unknown" || run.impacts()[at] != Impact::Unknown {
        return Err(format!(
            "{what}: expected unknown, got {:?} / {:?}",
            run.answers()[at],
            run.impacts()[at]
        ));
    }
    let reason = run.reason(at);
    if !reason.contains(code) {
        return Err(format!("{what}: the reason does not name {code}: {reason}"));
    }
    Ok(())
}

/// The reasons of the refusals this step adds also say that the error comes
/// from hand tests on both servers.
fn assert_refused_by_hand(run: &FilesRun, at: usize, code: &str, what: &str) -> Result<(), String> {
    assert_refused_with(run, at, code, what)?;
    let reason = run.reason(at);
    if !reason.contains("8.0.46 and 8.4.13") {
        return Err(format!(
            "{what}: the reason does not say which servers: {reason}"
        ));
    }
    Ok(())
}

// CREATE TABLE of a table the dump has.

const CREATE_ORDERS_AGAIN: &str =
    "CREATE TABLE orders (id BIGINT UNSIGNED AUTO_INCREMENT PRIMARY KEY, note VARCHAR(50));";

#[test]
fn create_table_of_a_table_the_dump_has_is_unknown_and_says_why() {
    // By hand on MySQL 8.0.46 and 8.4.13: CREATE TABLE orders where orders
    // exists is error 1050 and changes nothing.
    let run = run_on_dump(USERS_AND_ORDERS_DUMP, CREATE_ORDERS_AGAIN);
    assert_eq!(run.answers(), ["unknown"]);
    assert_eq!(run.impacts(), [Impact::Unknown]);
    let reason = run.reason(0);
    // The three things the reason says: MySQL refuses it (1050), where the
    // advice is (a database with production's migrations, or the dump is of
    // another database), and the guard that prints the CREATE where it would
    // skip it.
    for words in [
        "1050",
        "migrate --pretend",
        "production's migrations",
        "hasTable",
    ] {
        assert!(reason.contains(words), "{words}: {reason}");
    }
}

#[test]
fn the_tables_of_the_dump_stay_when_a_create_table_names_one_so_later_statements_are_measured_on_them()
 {
    // The input creates orders with (id, note); the dump's orders has
    // (id, user_id, amount, email, note). MySQL refuses the CREATE, so every
    // later statement runs on the dump's table.
    let run = run_on_dump(
        USERS_AND_ORDERS_DUMP,
        "CREATE TABLE orders (id BIGINT UNSIGNED AUTO_INCREMENT PRIMARY KEY, note VARCHAR(50));
         ALTER TABLE orders MODIFY amount BIGINT NOT NULL;
         ALTER TABLE orders ADD COLUMN email INT NULL;
         ALTER TABLE orders ADD COLUMN probe_col INT NULL;",
    );
    // amount is a column of the dump only: change-integer-type, a copy.
    assert_eq!(run.answers()[1], COPY);
    assert_eq!(run.impacts()[1], Impact::WritesWait);
    // email is a column of the dump only: adding it again is 1060.
    assert_eq!(run.answers()[2], "unknown");
    assert!(run.reason(2).contains("1060"), "{}", run.reason(2));
    assert_eq!(run.answers()[3], INSTANT);
    assert_eq!(run.impacts()[3], Impact::WritesContinue);
    // Nothing is created here, so the ALTERs wait for the metadata lock.
    assert_eq!(run.locked()[1], ["orders"]);
    assert_eq!(run.locked()[3], ["orders"]);
    assert!(
        run.findings
            .iter()
            .all(|finding| finding.created_here.is_empty())
    );
}

#[test]
fn a_create_table_of_a_dump_table_that_the_input_dropped_first_makes_a_new_table() {
    // DROP TABLE orders; CREATE TABLE orders (...): the table is new (a
    // Laravel dropIfExists then create).
    let run = run_on_dump(
        USERS_AND_ORDERS_DUMP,
        "DROP TABLE IF EXISTS orders;
         CREATE TABLE orders (id BIGINT UNSIGNED AUTO_INCREMENT PRIMARY KEY, note VARCHAR(50)) DEFAULT CHARSET=utf8mb4;
         ALTER TABLE orders ADD COLUMN probe_col INT NULL;",
    );
    assert_eq!(run.answers(), ["not checked", "not checked", INSTANT]);
    assert_eq!(
        run.impacts(),
        [Impact::NotChecked, Impact::NotChecked, Impact::NewTable]
    );
}

#[test]
fn a_create_table_if_not_exists_of_a_dump_table_leaves_the_dump_s_table() {
    // By hand on MySQL 8.0.46 and 8.4.13: accepted with Note 1050, the table
    // is unchanged.
    let run = run_on_dump(
        USERS_AND_ORDERS_DUMP,
        "CREATE TABLE IF NOT EXISTS orders (id INT PRIMARY KEY);
         ALTER TABLE orders ADD COLUMN email INT NULL;
         ALTER TABLE orders ADD COLUMN probe_col INT NULL;",
    );
    assert_eq!(run.answers()[0], "not checked");
    assert_eq!(run.answers()[1], "unknown");
    assert!(run.reason(1).contains("1060"), "{}", run.reason(1));
    assert_eq!(run.answers()[2], INSTANT);
    assert_eq!(run.impacts()[2], Impact::WritesContinue);
}

#[test]
fn a_create_table_of_a_name_the_dump_lacks_makes_a_new_table() {
    let run = run_on_dump(
        USERS_AND_ORDERS_DUMP,
        "CREATE TABLE fresh (id INT PRIMARY KEY) DEFAULT CHARSET=utf8mb4;
         ALTER TABLE fresh ADD COLUMN probe_col INT NULL;",
    );
    assert_eq!(run.answers(), ["not checked", INSTANT]);
    assert_eq!(run.impacts(), [Impact::NotChecked, Impact::NewTable]);
}

#[test]
fn a_create_table_like_of_a_dump_table_is_refused_too_and_the_dump_s_table_stays() {
    // CREATE TABLE users_copy LIKE users where users_copy is in the dump: 1050.
    let dump = format!(
        "{USERS_AND_ORDERS_DUMP}CREATE TABLE `users_copy` (`id` int NOT NULL, PRIMARY KEY (`id`)) \
         ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_0900_ai_ci;\n"
    );
    let run = run_on_dump(
        &dump,
        "CREATE TABLE users_copy LIKE users;
         ALTER TABLE users_copy ADD COLUMN name INT NULL;
         ALTER TABLE users_copy ADD COLUMN probe_col INT NULL;",
    );
    assert_eq!(run.answers()[0], "unknown");
    assert!(run.reason(0).contains("1050"), "{}", run.reason(0));
    // users has a column name, users_copy of the dump does not: the dump's
    // table stays, so adding name is measured.
    assert_eq!(run.answers()[1], INSTANT);
    assert_eq!(run.answers()[2], INSTANT);
}

// A table swapped onto a live name is not new.

const CREATE_ORDERS_NEW: &str = "CREATE TABLE orders_new (id BIGINT UNSIGNED AUTO_INCREMENT PRIMARY KEY, \
     amount INT NOT NULL, note VARCHAR(50)) DEFAULT CHARSET=utf8mb4;";

const SWAPS_ONTO_ORDERS: [&str; 3] = [
    "RENAME TABLE orders TO orders_old, orders_new TO orders;",
    "RENAME TABLE orders TO orders_old;\nRENAME TABLE orders_new TO orders;",
    "ALTER TABLE orders RENAME TO orders_old;\nALTER TABLE orders_new RENAME TO orders;",
];

#[test]
fn a_new_table_swapped_onto_a_name_of_the_dump_gets_its_measured_impact() {
    // CREATE TABLE orders_new; RENAME TABLE orders TO orders_old, orders_new
    // TO orders; then ALTERs of orders: the live table with every row.
    for swap in SWAPS_ONTO_ORDERS {
        let sql = format!(
            "{CREATE_ORDERS_NEW}\n{swap}\n\
             ALTER TABLE orders MODIFY amount BIGINT NOT NULL;\n\
             ALTER TABLE orders ADD COLUMN email INT NULL;\n\
             ALTER TABLE orders_old ADD COLUMN probe_col INT NULL;"
        );
        let run = run_on_dump(USERS_AND_ORDERS_DUMP, &sql);
        let n = run.answers().len();
        // The swapped table is orders_new's: change-integer-type, a copy.
        assert_eq!(run.answers()[n - 3], COPY, "{swap}");
        assert_eq!(run.impacts()[n - 3], Impact::WritesWait, "{swap}");
        // It has no email column (the old orders had one): INSTANT, and
        // writes continue.
        assert_eq!(run.answers()[n - 2], INSTANT, "{swap}");
        assert_eq!(run.impacts()[n - 2], Impact::WritesContinue, "{swap}");
        // The old live table, kept as orders_old, was there before the input.
        assert_eq!(run.answers()[n - 1], INSTANT, "{swap}");
        assert_eq!(run.impacts()[n - 1], Impact::WritesContinue, "{swap}");
        // Both are named by the metadata lock warning, orders_new's table
        // under its new name.
        assert_eq!(run.locked()[n - 3], ["orders"], "{swap}");
        assert_eq!(run.locked()[n - 1], ["orders_old"], "{swap}");
    }
}

#[test]
fn a_new_table_swapped_onto_a_name_the_input_used_as_an_existing_table_is_not_new_without_a_dump() {
    // No --schema: the input alters or renames orders without creating it,
    // so the name held a table from before the input.
    for (name, before, swap) in [
        ("renamed", "", SWAPS_ONTO_ORDERS[0]),
        ("renamed by ALTER", "", SWAPS_ONTO_ORDERS[2]),
        (
            "altered",
            "ALTER TABLE orders ADD COLUMN x INT NULL;\n",
            SWAPS_ONTO_ORDERS[0],
        ),
    ] {
        let sql = format!(
            "{before}{CREATE_ORDERS_NEW}\n{swap}\n\
             ALTER TABLE orders MODIFY amount BIGINT NOT NULL;"
        );
        let run = run_without_dump(&sql);
        assert_eq!(run.last_answer(), COPY, "{name}");
        assert_eq!(run.last_impact(), Impact::WritesWait, "{name}");
    }
}

#[test]
fn a_new_table_renamed_onto_a_fresh_name_stays_new() {
    for rename in [
        "RENAME TABLE orders_new TO orders_v2;",
        "ALTER TABLE orders_new RENAME TO orders_v2;",
    ] {
        for dump in [Some(USERS_AND_ORDERS_DUMP), None] {
            let sql = format!(
                "{CREATE_ORDERS_NEW}\n{rename}\nALTER TABLE orders_v2 MODIFY amount BIGINT NOT NULL;"
            );
            let run = run_files_on_both_servers(dump, &[&sql]);
            assert_eq!(run.last_answer(), COPY, "{rename}");
            assert_eq!(run.last_impact(), Impact::NewTable, "{rename}");
        }
    }
}

// Temporary tables live in their session. By hand on MySQL
// 8.0.46 and 8.4.13: a temporary table hides the base table of its name,
// DROP TABLE and DROP TEMPORARY TABLE drop the temporary one, ALTER and
// TRUNCATE act on it, RENAME TABLE does not see it, ALTER ... RENAME TO
// moves it, and it ends with the session.

const CREATE_TEMPORARY_ORDERS: &str =
    "CREATE TEMPORARY TABLE orders (id INT PRIMARY KEY, tmp_col INT NULL);";

/// `amount` is a column of the base table only and `tmp_col` of the
/// temporary one only: adding it says which table a statement was on.
const ADD_AMOUNT_TO_ORDERS: &str = "ALTER TABLE orders ADD COLUMN amount INT NULL;";
const ADD_TMP_COL_TO_ORDERS: &str = "ALTER TABLE orders ADD COLUMN tmp_col INT NULL;";

/// The statement after `sql` adds `tmp_col`: INSTANT on the base table, with
/// writes continuing.
fn assert_base_table_is_back(sql: &str, what: &str) {
    let run = run_on_dump(
        USERS_AND_ORDERS_DUMP,
        &format!("{sql}\n{ADD_TMP_COL_TO_ORDERS}"),
    );
    assert_eq!(run.last_answer(), INSTANT, "{what}");
    assert_eq!(run.last_impact(), Impact::WritesContinue, "{what}");
}

/// The statement after `sql` adds `amount`: INSTANT on a new table that has
/// no such column, whatever the base table is.
fn assert_temporary_table_is_still_there(sql: &str, what: &str) {
    let run = run_on_dump(
        USERS_AND_ORDERS_DUMP,
        &format!("{sql}\n{ADD_AMOUNT_TO_ORDERS}"),
    );
    assert_eq!(run.last_answer(), INSTANT, "{what}");
    assert_eq!(run.last_impact(), Impact::NewTable, "{what}");
}

#[test]
fn drop_temporary_table_if_exists_of_a_base_table_leaves_it_so_the_next_alter_is_measured() {
    let run = run_on_dump(
        USERS_AND_ORDERS_DUMP,
        "DROP TEMPORARY TABLE IF EXISTS orders;\nALTER TABLE orders ADD COLUMN probe_col INT NULL;",
    );
    assert_eq!(run.answers()[1], INSTANT);
    assert_eq!(run.impacts()[1], Impact::WritesContinue);
    // And the table is still the dump's: amount exists.
    let run = run_on_dump(
        USERS_AND_ORDERS_DUMP,
        "DROP TEMPORARY TABLE IF EXISTS orders;\nALTER TABLE orders MODIFY amount BIGINT NOT NULL;",
    );
    assert_eq!(run.last_answer(), COPY);
}

#[test]
fn drop_temporary_table_without_a_temporary_table_is_unknown_naming_1051_and_leaves_the_base_table()
{
    // Without IF EXISTS: error 1051 (Unknown table), the base table is
    // untouched.
    let run = run_on_dump(
        USERS_AND_ORDERS_DUMP,
        "DROP TEMPORARY TABLE orders;\nALTER TABLE orders ADD COLUMN probe_col INT NULL;",
    );
    assert_eq!(run.answers()[0], "unknown");
    assert!(run.reason(0).contains("1051"), "{}", run.reason(0));
    assert_eq!(run.answers()[1], INSTANT);
    assert_eq!(run.impacts()[1], Impact::WritesContinue);
    // Two names, one of them has no temporary table: 1051 for it and nothing
    // is dropped.
    let run = run_on_dump(
        USERS_AND_ORDERS_DUMP,
        &format!(
            "{CREATE_TEMPORARY_ORDERS}\nDROP TEMPORARY TABLE orders, users;\n{ADD_AMOUNT_TO_ORDERS}"
        ),
    );
    assert_eq!(run.answers()[1], "unknown");
    assert!(run.reason(1).contains("1051"), "{}", run.reason(1));
    // The temporary orders is still there (it has no amount column).
    assert_eq!(run.last_answer(), INSTANT);
    assert_eq!(run.last_impact(), Impact::NewTable);
}

#[test]
fn drop_temporary_table_with_if_exists_of_a_name_without_one_is_not_checked_and_drops_nothing() {
    // Accepted with Note 1051 for every name that has none; the base tables
    // stay.
    let run = run_on_dump(
        USERS_AND_ORDERS_DUMP,
        "DROP TEMPORARY TABLE IF EXISTS orders, users, nosuch;
         ALTER TABLE orders ADD COLUMN probe_col INT NULL;
         ALTER TABLE users ADD COLUMN probe_col INT NULL;",
    );
    assert_eq!(run.answers(), ["not checked", INSTANT, INSTANT]);
    assert_eq!(
        run.impacts(),
        [
            Impact::NotChecked,
            Impact::WritesContinue,
            Impact::WritesContinue
        ]
    );
}

#[test]
fn dropping_a_temporary_table_makes_the_base_table_visible_again() {
    for drop in [
        "DROP TEMPORARY TABLE orders;",
        "DROP TEMPORARY TABLE IF EXISTS orders;",
        // DROP TABLE drops the temporary one only.
        "DROP TABLE orders;",
        "DROP TABLE IF EXISTS orders;",
    ] {
        assert_base_table_is_back(&format!("{CREATE_TEMPORARY_ORDERS}\n{drop}"), drop);
    }
    // The temporary table is gone: the next CREATE TEMPORARY of the name is
    // new again.
    assert_base_table_is_back(
        &format!(
            "{CREATE_TEMPORARY_ORDERS}\nDROP TABLE orders;\n{CREATE_TEMPORARY_ORDERS}\nDROP TEMPORARY TABLE orders;"
        ),
        "twice",
    );
}

#[test]
fn drop_table_of_a_temporary_name_beside_a_base_name_drops_the_temporary_one_and_the_base_one() {
    // DROP TABLE orders, users with a temporary orders: the temporary orders
    // and the base users go; the base orders is back. Then users is gone.
    let run = run_on_dump(
        USERS_AND_ORDERS_DUMP,
        &format!(
            "{CREATE_TEMPORARY_ORDERS}
             DROP TABLE orders, users;
             {ADD_TMP_COL_TO_ORDERS}
             ALTER TABLE users ADD COLUMN probe_col INT NULL;"
        ),
    );
    assert_eq!(run.answers()[2], INSTANT);
    assert_eq!(run.impacts()[2], Impact::WritesContinue);
    // MySQL 1146: users is gone.
    assert_eq!(run.answers()[3], "unknown");
    assert_eq!(run.impacts()[3], Impact::Unknown);
}

#[test]
fn alter_and_truncate_of_a_name_act_on_its_temporary_table_and_leave_the_base_table_alone() {
    // ALTER TABLE orders ADD COLUMN extra on the temporary orders; adding
    // amount works (the temporary table has none): a new table.
    let run = run_on_dump(
        USERS_AND_ORDERS_DUMP,
        &format!(
            "{CREATE_TEMPORARY_ORDERS}\nALTER TABLE orders ADD COLUMN extra INT NULL;\n{ADD_AMOUNT_TO_ORDERS}"
        ),
    );
    assert_eq!(run.answers()[1], INSTANT);
    assert_eq!(run.impacts()[1], Impact::NewTable);
    assert_eq!(run.last_answer(), INSTANT);
    assert_eq!(run.last_impact(), Impact::NewTable);
    // MODIFY amount on the temporary table is 1054 (it has no amount), and
    // dropping the temporary table brings the base table back, untouched
    // by that refusal.
    let run = run_on_dump(
        USERS_AND_ORDERS_DUMP,
        &format!(
            "{CREATE_TEMPORARY_ORDERS}
             ALTER TABLE orders MODIFY amount BIGINT NOT NULL;
             DROP TEMPORARY TABLE orders;
             ALTER TABLE orders MODIFY amount BIGINT NOT NULL;"
        ),
    );
    assert_eq!(run.answers()[1], "unknown");
    assert_eq!(run.impacts()[1], Impact::Unknown);
    assert_eq!(run.last_answer(), COPY);
    assert_eq!(run.last_impact(), Impact::WritesWait);
    // TRUNCATE acts on the temporary table: it waits for no lock, and the
    // base table comes back when the temporary one is dropped.
    let run = run_on_dump(
        USERS_AND_ORDERS_DUMP,
        &format!(
            "{CREATE_TEMPORARY_ORDERS}\nTRUNCATE TABLE orders;\nDROP TEMPORARY TABLE orders;\n{ADD_TMP_COL_TO_ORDERS}"
        ),
    );
    assert!(run.locked()[1].is_empty(), "{:?}", run.locked());
    assert_eq!(run.last_answer(), INSTANT);
    assert_eq!(run.last_impact(), Impact::WritesContinue);
}

#[test]
fn rename_table_does_not_see_a_temporary_table_so_it_renames_the_base_table() {
    // RENAME TABLE orders TO orders_tmp with a temporary orders renames the
    // base table; the temporary one keeps its name. kilit may
    // follow that or lose the names; it never gives orders_tmp the temporary
    // table's columns.
    let run = run_on_dump(
        USERS_AND_ORDERS_DUMP,
        &format!(
            "{CREATE_TEMPORARY_ORDERS}
             RENAME TABLE orders TO orders_tmp;
             ALTER TABLE orders_tmp ADD COLUMN tmp_col INT NULL;
             ALTER TABLE orders ADD COLUMN amount INT NULL;"
        ),
    );
    // orders_tmp is the base table, which has no tmp_col: INSTANT with
    // writes continuing, or unknown because kilit lost the name; never
    // the 1060 of the temporary table.
    match run.answers()[2].as_str() {
        "unknown" => assert!(!run.reason(2).contains("1060"), "{}", run.reason(2)),
        answer => {
            assert_eq!(answer, INSTANT);
            assert_eq!(run.impacts()[2], Impact::WritesContinue);
        }
    }
    // orders is the temporary table, which has no amount: INSTANT on a new
    // table, or unknown because kilit lost the name; never the 1060 of the
    // base table.
    match run.last_answer().as_str() {
        "unknown" => assert!(!run.reason(3).contains("1060"), "{}", run.reason(3)),
        answer => {
            assert_eq!(answer, INSTANT);
            assert_eq!(run.impacts()[3], Impact::NewTable);
        }
    }
}

#[test]
fn rename_table_of_a_name_that_only_a_temporary_table_has_is_refused_and_creates_nothing() {
    // By hand: CREATE TEMPORARY TABLE t2; RENAME TABLE t2 TO t2b is error
    // 1146, and t2b does not exist. kilit may keep or lose t2, but never
    // gives t2b a measured answer.
    let run = run_on_dump(
        USERS_AND_ORDERS_DUMP,
        "CREATE TEMPORARY TABLE t2 (id INT PRIMARY KEY);
         RENAME TABLE t2 TO t2b;
         ALTER TABLE t2b ADD COLUMN probe_col INT NULL;",
    );
    assert_eq!(run.answers()[1], "unknown");
    assert_eq!(run.answers()[2], "unknown");
    assert_eq!(run.impacts()[2], Impact::Unknown);
}

#[test]
fn alter_table_rename_to_moves_the_temporary_table_and_leaves_the_base_table() {
    // By hand: ALTER TABLE orders RENAME TO o3 with a temporary orders
    // renames the temporary one; the base orders stays, visible again.
    let run = run_on_dump(
        USERS_AND_ORDERS_DUMP,
        &format!(
            "{CREATE_TEMPORARY_ORDERS}
             ALTER TABLE orders RENAME TO o3;
             ALTER TABLE orders ADD COLUMN tmp_col INT NULL;
             ALTER TABLE o3 ADD COLUMN tmp_col INT NULL;
             ALTER TABLE o3 ADD COLUMN amount INT NULL;"
        ),
    );
    // orders is the base table again: tmp_col is new on it, writes continue.
    assert_eq!(run.answers()[2], INSTANT);
    assert_eq!(run.impacts()[2], Impact::WritesContinue);
    // o3 is the temporary table. kilit may follow the move or lose
    // the name; it never answers o3 as the base table (which has
    // amount and no tmp_col): a measured answer for tmp_col or a 1060 for
    // amount.
    assert_eq!(run.answers()[3], "unknown");
    assert!(!run.reason(3).is_empty());
    assert_ne!(run.answers()[4], COPY);
    assert_ne!(run.impacts()[4], Impact::WritesContinue);
    assert_ne!(run.impacts()[4], Impact::WritesWait);
    if run.answers()[4] == "unknown" {
        assert!(!run.reason(4).contains("1060"), "{}", run.reason(4));
    } else {
        assert_eq!(run.impacts()[4], Impact::NewTable);
    }
}

#[test]
fn a_new_input_file_has_the_base_table_back_and_the_temporary_table_gone() {
    // The first file creates a temporary orders and a temporary tmp_only;
    // the second is a new session.
    let first = format!(
        "{CREATE_TEMPORARY_ORDERS}\nCREATE TEMPORARY TABLE tmp_only (id INT PRIMARY KEY);\n"
    );
    let second = "ALTER TABLE orders ADD COLUMN tmp_col INT NULL;
                  ALTER TABLE orders MODIFY amount BIGINT NOT NULL;
                  ALTER TABLE tmp_only ADD COLUMN probe_col INT NULL;";
    let run = run_files_on_both_servers(Some(USERS_AND_ORDERS_DUMP), &[&first, second]);
    // orders is the dump's table: tmp_col is new, amount exists. Both wait
    // for the metadata lock, nothing is created here.
    assert_eq!(run.answers()[2], INSTANT);
    assert_eq!(run.impacts()[2], Impact::WritesContinue);
    assert_eq!(run.answers()[3], COPY);
    assert_eq!(run.impacts()[3], Impact::WritesWait);
    assert_eq!(run.locked()[2], ["orders"]);
    assert_eq!(run.locked()[3], ["orders"]);
    // tmp_only ended with the first session: error 1146 on the server.
    assert_eq!(run.answers()[4], "unknown");
    assert_eq!(run.impacts()[4], Impact::Unknown);
}

#[test]
fn a_temporary_table_of_one_file_is_not_a_table_created_here_in_the_next_file_without_a_dump() {
    // Without --schema the second file's orders is a table kilit never saw:
    // unknown, not "a table created here".
    let run = run_files_on_both_servers(
        None,
        &[
            &format!("{CREATE_TEMPORARY_ORDERS}\n"),
            "ALTER TABLE orders ADD COLUMN probe_col INT NULL;",
        ],
    );
    assert_eq!(run.last_answer(), "unknown");
    assert_eq!(run.last_impact(), Impact::Unknown);
    // The real table is named by the metadata lock warning.
    assert_eq!(run.locked()[1], ["orders"]);
}

#[test]
fn a_temporary_table_made_in_the_same_file_is_still_a_table_created_here() {
    // Control: inside its own session the temporary table is new.
    let run = run_without_dump(&format!(
        "{CREATE_TEMPORARY_ORDERS}\nALTER TABLE orders ADD COLUMN probe_col INT NULL;"
    ));
    assert_eq!(run.last_answer(), INSTANT);
    assert_eq!(run.last_impact(), Impact::NewTable);
}

#[test]
fn the_temporary_table_hides_the_base_table_until_it_is_dropped() {
    // Both statements name amount: on the temporary table it is new, on the
    // base table it is 1060.
    assert_temporary_table_is_still_there(CREATE_TEMPORARY_ORDERS, "created");
    assert_temporary_table_is_still_there(
        &format!("{CREATE_TEMPORARY_ORDERS}\nDROP TEMPORARY TABLE IF EXISTS nosuch;"),
        "an unrelated DROP TEMPORARY",
    );
    let run = run_on_dump(
        USERS_AND_ORDERS_DUMP,
        &format!("{CREATE_TEMPORARY_ORDERS}\nDROP TEMPORARY TABLE orders;\n{ADD_AMOUNT_TO_ORDERS}"),
    );
    assert_eq!(run.last_answer(), "unknown");
    assert!(run.reason(2).contains("1060"), "{}", run.reason(2));
    assert_eq!(run.last_impact(), Impact::Unknown);
}

// Renaming a table renames the parent of the foreign keys that
// point at it. By hand on MySQL 8.0.46 and 8.4.13: after RENAME TABLE users
// TO people (and ALTER TABLE users RENAME TO people) the key of orders
// references people, and MODIFY id INT UNSIGNED on people is error 3780,
// as it is on users without the rename.

#[test]
fn changing_the_referenced_column_of_a_renamed_parent_is_unknown_as_without_the_rename() {
    // By hand: MODIFY id INT UNSIGNED NOT NULL AUTO_INCREMENT on people is
    // error 3780 after the rename. Without the key, the same change is the
    // lab's change-integer-type.
    let alone = "ALTER TABLE users MODIFY id INT UNSIGNED NOT NULL AUTO_INCREMENT;";
    let control = run_on_dump(USERS_AND_ORDERS_WITH_FK_DUMP, alone);
    assert_eq!(control.last_answer(), "unknown");
    assert!(
        control.reason(0).contains("referenced"),
        "{}",
        control.reason(0)
    );
    assert_eq!(
        run_on_dump(USERS_AND_ORDERS_DUMP, alone).last_answer(),
        COPY
    );
    for rename in [
        "RENAME TABLE users TO people;",
        "ALTER TABLE users RENAME TO people;",
    ] {
        let run = run_on_dump(
            USERS_AND_ORDERS_WITH_FK_DUMP,
            &format!(
                "{rename}\nALTER TABLE people MODIFY id INT UNSIGNED NOT NULL AUTO_INCREMENT;"
            ),
        );
        assert_eq!(run.last_answer(), "unknown", "{rename}");
        assert_eq!(run.last_impact(), Impact::Unknown, "{rename}");
        assert!(
            run.reason(1).contains("referenced"),
            "{rename}: {}",
            run.reason(1)
        );
        // The same rename without the key gives the lab's answer.
        let no_key = run_on_dump(
            USERS_AND_ORDERS_DUMP,
            &format!(
                "{rename}\nALTER TABLE people MODIFY id INT UNSIGNED NOT NULL AUTO_INCREMENT;"
            ),
        );
        assert_eq!(no_key.last_answer(), COPY, "{rename}");
    }
}

#[test]
fn renaming_a_parent_twice_keeps_the_key_with_the_last_name() {
    let run = run_on_dump(
        USERS_AND_ORDERS_WITH_FK_DUMP,
        "RENAME TABLE users TO people;
         RENAME TABLE people TO members;
         ALTER TABLE members MODIFY id INT UNSIGNED NOT NULL AUTO_INCREMENT;",
    );
    assert_eq!(run.last_answer(), "unknown");
    assert!(run.reason(2).contains("referenced"), "{}", run.reason(2));
}

#[test]
fn a_parent_without_a_child_key_is_still_changed_with_a_measured_answer_after_a_rename() {
    // Control: no foreign key points at users, so the rename changes
    // nothing about its columns.
    let run = run_on_dump(
        USERS_AND_ORDERS_DUMP,
        "RENAME TABLE users TO people;
         ALTER TABLE people MODIFY id INT UNSIGNED NOT NULL AUTO_INCREMENT;",
    );
    assert_eq!(run.last_answer(), COPY);
}

#[test]
fn the_keys_of_a_table_kilit_does_not_know_follow_their_parent_s_rename() {
    // The dump has users only. The ADD FOREIGN KEY is on orders, which kilit
    // does not know; the key still points at users, and after RENAME TABLE
    // users TO people at people.
    let dump = "CREATE TABLE `users` (
  `id` bigint unsigned NOT NULL AUTO_INCREMENT,
  `name` varchar(100) NOT NULL,
  PRIMARY KEY (`id`)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_0900_ai_ci;
";
    let key = "ALTER TABLE orders ADD CONSTRAINT orders_user_id_foreign FOREIGN KEY (user_id) \
               REFERENCES users (id);";
    let modify = "MODIFY id INT UNSIGNED NOT NULL AUTO_INCREMENT;";
    // Without the rename the key is a reason for unknown.
    let plain = run_on_dump(dump, &format!("{key}\nALTER TABLE users {modify}"));
    assert_eq!(plain.last_answer(), "unknown");
    // With it the key follows the parent: unknown the same way.
    let renamed = run_on_dump(
        dump,
        &format!("{key}\nRENAME TABLE users TO people;\nALTER TABLE people {modify}"),
    );
    assert_eq!(renamed.last_answer(), "unknown");
    // Without the key the same MODIFY of the renamed table is the lab's
    // change-integer-type, so the unknown above comes from the key.
    let no_key = run_on_dump(
        dump,
        &format!("RENAME TABLE users TO people;\nALTER TABLE people {modify}"),
    );
    assert_eq!(no_key.last_answer(), COPY);
}

// Names a known table lacks or already has. By hand on MySQL
// 8.0.46 and 8.4.13 (the same on both) each statement below is refused with
// the error named. Where the table says "kept", the table is as it was and
// the next ADD COLUMN is measured; where it says "lost", kilit no longer
// trusts the table and the next ADD COLUMN is unknown.

/// (tables to probe afterwards, statement, error) of the refusals after which
/// kilit keeps the table.
const REFUSALS_THAT_KEEP_THE_TABLE: [(&[&str], &str, &str); 19] = [
    (
        &["orders"],
        "ALTER TABLE orders ALTER COLUMN zzz DROP DEFAULT",
        "1054",
    ),
    (
        &["orders"],
        "ALTER TABLE orders ADD COLUMN note VARCHAR(50) NULL",
        "1060",
    ),
    (
        &["orders"],
        "ALTER TABLE orders ADD COLUMN note INT NULL",
        "1060",
    ),
    (&["orders"], "ALTER TABLE orders DROP COLUMN zzz", "1091"),
    (
        &["orders"],
        "ALTER TABLE orders ADD INDEX orders_email_unique (amount)",
        "1061",
    ),
    (
        &["orders"],
        "ALTER TABLE orders ADD INDEX ix_amount (note)",
        "1061",
    ),
    (
        &["orders"],
        "ALTER TABLE orders RENAME INDEX nosuch TO ix_new",
        "1176",
    ),
    (
        &["orders"],
        "ALTER TABLE orders RENAME INDEX ix_amount TO orders_email_unique",
        "1061",
    ),
    (
        &["orders"],
        "ALTER TABLE orders ADD CONSTRAINT orders_user_id_foreign FOREIGN KEY (user_id) REFERENCES users (id)",
        "1826",
    ),
    (
        &["orders"],
        "ALTER TABLE orders ADD CONSTRAINT invoices_user_id_foreign FOREIGN KEY (user_id) REFERENCES users (id)",
        "1826",
    ),
    (
        &["orders"],
        "ALTER TABLE orders ADD PRIMARY KEY (amount)",
        "1068",
    ),
    (&["nopk"], "ALTER TABLE nopk DROP PRIMARY KEY", "1091"),
    (
        &["orders"],
        "ALTER TABLE orders ADD CONSTRAINT orders_amount_check CHECK (amount < 100000)",
        "3822",
    ),
    (
        &["orders"],
        "ALTER TABLE orders ADD CONSTRAINT invoices_total_check CHECK (amount < 100000)",
        "3822",
    ),
    (&["orders", "users"], "RENAME TABLE orders TO users", "1050"),
    (
        &["orders", "users"],
        "ALTER TABLE orders RENAME TO users",
        "1050",
    ),
    (
        &["orders", "invoices"],
        "RENAME TABLE orders TO invoices",
        "1050",
    ),
    (&["orders"], "RENAME TABLE orders TO orders", "1050"),
    // One name on a CHECK and on a UNIQUE index.
    (&["s3"], "ALTER TABLE s3 DROP CONSTRAINT dup", "3939"),
];

/// The same for the refusals after which kilit loses the table.
const REFUSALS_THAT_LOSE_THE_TABLE: [(&str, &str, &str); 11] = [
    ("orders", "ALTER TABLE orders MODIFY zzz INT NULL", "1054"),
    (
        "orders",
        "ALTER TABLE orders CHANGE zzz yyy INT NULL",
        "1054",
    ),
    (
        "orders",
        "ALTER TABLE orders RENAME COLUMN zzz TO yyy",
        "1054",
    ),
    (
        "orders",
        "ALTER TABLE orders ALTER COLUMN zzz SET DEFAULT 1",
        "1054",
    ),
    (
        "orders",
        "ALTER TABLE orders ALTER COLUMN zzz SET VISIBLE",
        "1054",
    ),
    (
        "orders",
        "ALTER TABLE orders ALTER COLUMN zzz SET INVISIBLE",
        "1054",
    ),
    (
        "orders",
        "ALTER TABLE orders CHANGE email note VARCHAR(50) NULL",
        "1060",
    ),
    (
        "orders",
        "ALTER TABLE orders RENAME COLUMN email TO note",
        "1060",
    ),
    (
        "orders",
        "ALTER TABLE orders RENAME COLUMN email TO NOTE",
        "1060",
    ),
    (
        "orders",
        "ALTER TABLE orders CHANGE email Note VARCHAR(50) NULL",
        "1060",
    ),
    ("orders", "ALTER TABLE orders MODIFY ZZZ INT NULL", "1054"),
];

#[test]
fn a_name_a_known_table_has_or_lacks_is_unknown_naming_the_error_and_the_table_stays_known() {
    // The reasons of the refusals B1b and B1c made say the error and the lab
    // scenario; the ones this step adds also say that it comes from hand
    // tests on MySQL 8.0.46 and 8.4.13.
    let from_the_lab = ["1091", "1061", "1176"];
    let mut problems = Vec::new();
    for (probes, statement, code) in REFUSALS_THAT_KEEP_THE_TABLE {
        let mut sql = format!("{statement};\n");
        for table in probes {
            sql.push_str(&add_probe_column(table));
            sql.push('\n');
        }
        let run = run_on_dump(TABLES_WITH_NAMES_DUMP, &sql);
        let refused = if from_the_lab.contains(&code) && !statement.contains("nopk") {
            assert_refused_with(&run, 0, code, statement)
        } else {
            assert_refused_by_hand(&run, 0, code, statement)
        };
        if let Err(problem) = refused {
            problems.push(problem);
        }
        for (at, table) in probes.iter().enumerate() {
            if run.answers()[at + 1] != INSTANT || run.impacts()[at + 1] != Impact::WritesContinue {
                problems.push(format!(
                    "{statement}: the table {table} should be kept, got {} / {:?}",
                    run.answers()[at + 1],
                    run.impacts()[at + 1]
                ));
            }
        }
    }
    assert!(problems.is_empty(), "{problems:#?}");
}

#[test]
fn a_column_a_known_table_lacks_or_a_name_it_has_for_a_change_is_unknown_and_loses_the_table() {
    let mut problems = Vec::new();
    for (table, statement, code) in REFUSALS_THAT_LOSE_THE_TABLE {
        let run = run_on_dump(
            TABLES_WITH_NAMES_DUMP,
            &format!("{statement};\n{}", add_probe_column(table)),
        );
        // The parser cannot read SET VISIBLE and SET INVISIBLE: unknown with
        // another reason, and the table lost.
        let unreadable = statement.contains("SET VISIBLE") || statement.contains("SET INVISIBLE");
        if unreadable {
            if run.answers()[0] != "unknown" {
                problems.push(format!("{statement}: expected unknown"));
            }
        } else if let Err(problem) = assert_refused_by_hand(&run, 0, code, statement) {
            problems.push(problem);
        }
        if run.answers()[1] != "unknown" || run.impacts()[1] != Impact::Unknown {
            problems.push(format!(
                "{statement}: the table should be lost, got {} / {:?}",
                run.answers()[1],
                run.impacts()[1]
            ));
        }
    }
    assert!(problems.is_empty(), "{problems:#?}");
}

#[test]
fn a_refused_add_column_of_a_name_the_table_has_keeps_the_definition_kilit_had() {
    // note is VARCHAR(50) in the dump. After the refused ADD COLUMN note INT
    // the table is as it was: MODIFY note to the same definition is the
    // measured modify-same-definition, and shortening it is a copy.
    for add in [
        "ALTER TABLE orders ADD COLUMN note INT NULL;",
        "ALTER TABLE orders ADD COLUMN note VARCHAR(10) NULL;",
    ] {
        let run = run_on_dump(
            TABLES_WITH_NAMES_DUMP,
            &format!(
                "{add}\nALTER TABLE orders MODIFY note VARCHAR(50) NULL;\n\
                 ALTER TABLE orders MODIFY note VARCHAR(40) NULL;"
            ),
        );
        assert_eq!(run.answers()[1], INSTANT, "{add}");
        assert_eq!(run.answers()[2], COPY, "{add}");
    }
}

#[test]
fn forms_the_server_accepts_are_not_refused_for_a_name() {
    // By hand: each of these is accepted, as kilit's tables say it should
    // be. kilit must not call them a duplicate or a missing name (1060,
    // 1061, 1050, 1054); where it cannot follow one it says something
    // else, or answers.
    for statement in [
        "ALTER TABLE orders CHANGE amount amount BIGINT NOT NULL",
        "ALTER TABLE orders RENAME COLUMN note TO note",
        "ALTER TABLE orders RENAME COLUMN note TO NOTE",
        "ALTER TABLE orders RENAME INDEX ix_amount TO ix_amount",
        "ALTER TABLE orders RENAME INDEX ix_amount TO IX_AMOUNT",
        "ALTER TABLE orders RENAME TO orders",
        // A foreign key may take the name of an index of another table.
        "ALTER TABLE orders ADD CONSTRAINT ix_other FOREIGN KEY (user_id) REFERENCES users (id)",
        // A CHECK may take the name of a foreign key, and a foreign key the
        // name of a CHECK.
        "ALTER TABLE orders ADD CONSTRAINT orders_user_id_foreign CHECK (amount < 100000)",
        "ALTER TABLE orders ADD CONSTRAINT orders_amount_check FOREIGN KEY (user_id) REFERENCES users (id)",
    ] {
        let run = run_on_dump(TABLES_WITH_NAMES_DUMP, &format!("{statement};"));
        if run.answers()[0] == "unknown" {
            let reason = run.reason(0);
            for refused in ["1060", "1061", "1050", "1054", "1826", "3822"] {
                assert!(!reason.contains(refused), "{statement}: {reason}");
            }
        }
    }
}

#[test]
fn a_foreign_key_that_takes_the_name_of_an_index_of_its_own_table_is_refused_with_1061() {
    // By hand: ADD CONSTRAINT ix_amount FOREIGN KEY ... where ix_amount is an
    // index of orders is error 1061 and the table is unchanged.
    let run = run_on_dump(
        TABLES_WITH_NAMES_DUMP,
        "ALTER TABLE orders ADD CONSTRAINT ix_amount FOREIGN KEY (user_id) REFERENCES users (id);",
    );
    assert_ne!(run.answers()[0], COPY);
    assert!(
        !matches!(
            run.impacts()[0],
            Impact::WritesWait | Impact::WritesContinue | Impact::NewTable
        ),
        "{:?}",
        run.answers()
    );
}

#[test]
fn a_chain_of_renames_in_one_alter_names_a_column_the_table_lacks_and_loses_the_table() {
    // By hand: RENAME COLUMN amount TO total, RENAME COLUMN total TO sum
    // names total, which the table does not have before the statement:
    // 1054, for CHANGE the same. The table cannot be trusted afterwards.
    for chain in [
        "ALTER TABLE orders RENAME COLUMN note TO total, RENAME COLUMN total TO sum",
        "ALTER TABLE orders CHANGE note total VARCHAR(50) NULL, CHANGE total sum VARCHAR(50) NULL",
    ] {
        let run = run_on_dump(
            USERS_AND_ORDERS_DUMP,
            &format!("{chain};\n{}", add_probe_column("orders")),
        );
        assert_eq!(run.answers()[0], "unknown", "{chain}");
        assert_eq!(run.answers()[1], "unknown", "{chain}");
    }
}

/// The ways to give a column the table lacks, and to take a name the table
/// has, for each clause kilit reads.
fn missing_and_taken_names() -> Vec<(String, bool, &'static str)> {
    // (statement, kilit keeps the table, the error)
    let mut sweep = Vec::new();
    for missing in ["zzz", "no_such", "ZZZ", "Note2"] {
        for clause in [
            format!("MODIFY {missing} INT NULL"),
            format!("CHANGE {missing} q INT NULL"),
            format!("RENAME COLUMN {missing} TO q"),
            format!("ALTER COLUMN {missing} SET DEFAULT 1"),
            format!("ALTER COLUMN {missing} SET VISIBLE"),
            format!("ALTER COLUMN {missing} SET INVISIBLE"),
        ] {
            sweep.push((format!("ALTER TABLE orders {clause}"), false, "1054"));
        }
        sweep.push((
            format!("ALTER TABLE orders DROP COLUMN {missing}"),
            true,
            "1091",
        ));
        sweep.push((
            format!("ALTER TABLE orders ALTER COLUMN {missing} DROP DEFAULT"),
            true,
            "1054",
        ));
    }
    for taken in ["id", "user_id", "amount", "email", "note"] {
        sweep.push((
            format!("ALTER TABLE orders ADD COLUMN {taken} INT NULL"),
            true,
            "1060",
        ));
    }
    for taken in ["id", "user_id", "amount", "note"] {
        sweep.push((
            format!("ALTER TABLE orders CHANGE email {taken} VARCHAR(255) NULL"),
            false,
            "1060",
        ));
        sweep.push((
            format!("ALTER TABLE orders RENAME COLUMN email TO {taken}"),
            false,
            "1060",
        ));
    }
    for taken in ["orders_email_unique", "ix_amount"] {
        sweep.push((
            format!("ALTER TABLE orders ADD INDEX {taken} (note)"),
            true,
            "1061",
        ));
        sweep.push((
            format!("ALTER TABLE orders ADD UNIQUE INDEX {taken} (note)"),
            true,
            "1061",
        ));
        sweep.push((
            format!("CREATE INDEX {taken} ON orders (note)"),
            true,
            "1061",
        ));
    }
    sweep
}

#[test]
fn no_clause_with_a_name_the_table_lacks_or_has_gets_a_measured_runs_answer_or_a_failure() {
    // A property over every operation kilit reads: with a missing or a taken
    // name the statement is never writes_continue, writes_wait, new_table or
    // a measured failure, and the next ADD COLUMN is measured exactly when
    // the table is kept.
    let sweep = missing_and_taken_names();
    assert!(sweep.len() >= 50, "{}", sweep.len());
    let mut problems = Vec::new();
    for (statement, kept, _code) in sweep {
        let sql = format!("{statement};\n{}", add_probe_column("orders"));
        let run = run_on_dump(TABLES_WITH_NAMES_DUMP, &sql);
        if run.answers()[0] != "unknown"
            || matches!(
                run.impacts()[0],
                Impact::WritesContinue | Impact::WritesWait | Impact::NewTable | Impact::Fails
            )
        {
            problems.push(format!(
                "{statement}: {} / {:?}",
                run.answers()[0],
                run.impacts()[0]
            ));
        }
        let probe = run.last_answer();
        if kept && probe != INSTANT {
            problems.push(format!("{statement} should keep the table, got {probe}"));
        }
        if !kept && probe != "unknown" {
            problems.push(format!("{statement} should lose the table, got {probe}"));
        }
    }
    assert!(problems.is_empty(), "{problems:#?}");
}

#[test]
fn the_same_clauses_with_a_name_the_table_has_not_on_a_table_made_here_are_refused_too() {
    // The table is made in the input: it is kilit's only picture of it.
    let create = "CREATE TABLE orders (id BIGINT UNSIGNED AUTO_INCREMENT PRIMARY KEY, \
                  amount INT NOT NULL, note VARCHAR(50) NULL, KEY ix_amount (amount)) \
                  DEFAULT CHARSET=utf8mb4;";
    for statement in [
        "ALTER TABLE orders ADD COLUMN note INT NULL",
        "ALTER TABLE orders MODIFY zzz INT NULL",
        "ALTER TABLE orders RENAME COLUMN zzz TO yyy",
        "ALTER TABLE orders ADD INDEX ix_amount (note)",
    ] {
        let run = run_without_dump(&format!("{create}\n{statement};"));
        assert_eq!(run.answers()[1], "unknown", "{statement}");
        assert_ne!(run.impacts()[1], Impact::NewTable, "{statement}");
    }
}

// Two clauses of one ALTER that name one column. By hand on MySQL
// 8.0.46 and 8.4.13 (the same on both): every clause is taken against the
// names the table had before the statement.

const INPLACE_REBUILD_CONTINUE: &str = "INPLACE, rebuilds the table, writes continue";

/// `orders` of USERS_AND_ORDERS_DUMP with the index `ix_amount` on `amount`.
fn orders_with_amount_index_dump() -> String {
    format!(
        "{}ALTER TABLE orders ADD INDEX ix_amount (amount);\n",
        USERS_AND_ORDERS_DUMP
    )
}

#[test]
fn swapping_two_column_names_in_one_alter_is_followed() {
    // After the swap note is the old amount (INT NOT NULL) and amount the old
    // note (VARCHAR(50) NULL), whichever way it is written.
    for swap in [
        "ALTER TABLE orders CHANGE amount note INT NOT NULL, CHANGE note amount VARCHAR(50) NULL",
        "ALTER TABLE orders CHANGE note amount VARCHAR(50) NULL, CHANGE amount note INT NOT NULL",
        "ALTER TABLE orders RENAME COLUMN amount TO note, RENAME COLUMN note TO amount",
    ] {
        for (probe, answer) in [
            ("MODIFY note INT NOT NULL", INSTANT),
            ("MODIFY amount VARCHAR(50) NULL", INSTANT),
            ("MODIFY amount VARCHAR(40) NULL", COPY),
            ("ADD COLUMN probe_col INT NULL", INSTANT),
        ] {
            let run = run_on_dump(
                USERS_AND_ORDERS_DUMP,
                &format!("{swap};\nALTER TABLE orders {probe};"),
            );
            assert_eq!(run.last_answer(), answer, "{swap}: {probe}");
        }
    }
}

#[test]
fn a_swap_moves_the_index_with_the_column() {
    // By hand: the index on amount ends up on the column that was amount,
    // now named note. Dropping an indexed column rebuilds the table
    // (drop-column-indexed), dropping one without an index is INSTANT.
    let dump = orders_with_amount_index_dump();
    let swap =
        "ALTER TABLE orders CHANGE amount note INT NOT NULL, CHANGE note amount VARCHAR(50) NULL;";
    let indexed = run_on_dump(
        &dump,
        &format!("{swap}\nALTER TABLE orders DROP COLUMN note;"),
    );
    assert_eq!(indexed.last_answer(), INPLACE_REBUILD_CONTINUE);
    let plain = run_on_dump(
        &dump,
        &format!("{swap}\nALTER TABLE orders DROP COLUMN amount;"),
    );
    assert_eq!(plain.last_answer(), INSTANT);
}

#[test]
fn a_chain_of_renames_in_one_alter_names_a_column_that_is_not_there_yet_and_is_refused() {
    // By hand: RENAME COLUMN amount TO total, RENAME COLUMN total TO sum and
    // the CHANGE form are error 1054.
    for chain in [
        "ALTER TABLE orders RENAME COLUMN amount TO total, RENAME COLUMN total TO sum",
        "ALTER TABLE orders CHANGE amount total INT NOT NULL, CHANGE total sum INT NOT NULL",
    ] {
        let run = run_on_dump(
            USERS_AND_ORDERS_DUMP,
            &format!("{chain};\n{}", add_probe_column("orders")),
        );
        assert_eq!(run.answers()[0], "unknown", "{chain}");
        assert_eq!(run.last_answer(), "unknown", "{chain}");
    }
}

#[test]
fn dropping_and_adding_a_column_of_one_name_in_one_alter_is_followed() {
    // DROP COLUMN note, ADD COLUMN note INT NULL: note is INT NULL.
    let run = run_on_dump(
        USERS_AND_ORDERS_DUMP,
        "ALTER TABLE orders DROP COLUMN note, ADD COLUMN note INT NULL;
         ALTER TABLE orders MODIFY note INT NULL;
         ALTER TABLE orders ADD COLUMN note INT NULL;",
    );
    assert_eq!(run.answers()[1], INSTANT);
    assert_eq!(run.answers()[2], "unknown");
    assert!(run.reason(2).contains("1060"), "{}", run.reason(2));
    // The other order is accepted by MySQL too (ADD COLUMN note INT NULL,
    // DROP COLUMN note): note is INT NULL. kilit follows it or loses the
    // table; it never keeps the old VARCHAR(50).
    let run = run_on_dump(
        USERS_AND_ORDERS_DUMP,
        "ALTER TABLE orders ADD COLUMN note INT NULL, DROP COLUMN note;
         ALTER TABLE orders MODIFY note INT NULL;",
    );
    assert!(
        [INSTANT, "unknown"].contains(&run.last_answer().as_str()),
        "{}",
        run.last_answer()
    );
}

#[test]
fn adding_and_dropping_a_new_column_in_one_alter_is_refused_with_1091() {
    // By hand: ADD COLUMN extra INT NULL, DROP COLUMN extra is error 1091
    // (extra is not a column before the statement). Whether kilit keeps the
    // table (the refused statement and the statement as written leave the
    // same names) or loses it (it names extra in two clauses), a later ADD
    // COLUMN extra is INSTANT or unknown, never 1060.
    let run = run_on_dump(
        USERS_AND_ORDERS_DUMP,
        "ALTER TABLE orders ADD COLUMN extra INT NULL, DROP COLUMN extra;
         ALTER TABLE orders ADD COLUMN extra INT NULL;",
    );
    assert_eq!(run.answers()[0], "unknown");
    assert_eq!(run.impacts()[0], Impact::Unknown);
    assert!(
        [INSTANT, "unknown"].contains(&run.last_answer().as_str()),
        "{}",
        run.last_answer()
    );
    if run.last_answer() == "unknown" {
        assert!(!run.reason(1).contains("1060"), "{}", run.reason(1));
    }
}

#[test]
fn a_new_or_renamed_column_named_in_a_new_index_of_the_same_alter_is_followed() {
    for change in [
        "ADD COLUMN c INT NULL, ADD INDEX ix_c (c)",
        "CHANGE amount c INT NOT NULL, ADD INDEX ix_c (c)",
        "RENAME COLUMN amount TO c, ADD INDEX ix_c (c)",
    ] {
        let run = run_on_dump(
            USERS_AND_ORDERS_DUMP,
            &format!(
                "ALTER TABLE orders {change};
                 ALTER TABLE orders DROP INDEX ix_c;"
            ),
        );
        // The index ix_c exists: a measured drop-index.
        assert_eq!(
            run.last_answer(),
            "INPLACE, no rebuild, writes continue",
            "{change}"
        );
        // c is a column; adding it again is 1060.
        let run = run_on_dump(
            USERS_AND_ORDERS_DUMP,
            &format!("ALTER TABLE orders {change};\nALTER TABLE orders ADD COLUMN c INT NULL;"),
        );
        assert_eq!(run.last_answer(), "unknown", "{change}");
        assert!(
            run.reason(1).contains("1060"),
            "{change}: {}",
            run.reason(1)
        );
    }
    // After a CHANGE or RENAME of amount, amount is a free name.
    for change in [
        "CHANGE amount c INT NOT NULL, ADD INDEX ix_c (c)",
        "RENAME COLUMN amount TO c, ADD INDEX ix_c (c)",
    ] {
        let run = run_on_dump(
            USERS_AND_ORDERS_DUMP,
            &format!(
                "ALTER TABLE orders {change};\nALTER TABLE orders ADD COLUMN amount INT NULL;"
            ),
        );
        assert_eq!(run.last_answer(), INSTANT, "{change}");
    }
}

#[test]
fn an_index_on_the_old_name_of_a_renamed_column_is_unknown_naming_1072_and_loses_the_table() {
    // By hand: CHANGE amount c ..., ADD INDEX ix_a (amount) is error 1072.
    // The statement names amount in two clauses and is not one of the shapes
    // the step follows or refuses, so kilit loses the table: nothing is
    // measured on it afterwards.
    let run = run_on_dump(
        USERS_AND_ORDERS_DUMP,
        "ALTER TABLE orders CHANGE amount c INT NOT NULL, ADD INDEX ix_a (amount);
         ALTER TABLE orders ADD COLUMN c INT NULL;",
    );
    assert_eq!(run.answers()[0], "unknown");
    assert!(run.reason(0).contains("1072"), "{}", run.reason(0));
    assert_eq!(run.last_answer(), "unknown");
    assert_eq!(run.last_impact(), Impact::Unknown);
}

#[test]
fn modify_beside_a_rename_of_the_same_column_is_refused_with_1054_and_the_table_stays() {
    // By hand: MODIFY note ..., RENAME COLUMN note TO memo in either order,
    // and RENAME COLUMN note TO memo, MODIFY memo ...: error 1054.
    for statement in [
        "ALTER TABLE orders MODIFY note VARCHAR(100) NULL, RENAME COLUMN note TO memo",
        "ALTER TABLE orders RENAME COLUMN note TO memo, MODIFY note VARCHAR(100) NULL",
        "ALTER TABLE orders RENAME COLUMN note TO memo, MODIFY memo VARCHAR(100) NULL",
    ] {
        let run = run_on_dump(
            USERS_AND_ORDERS_DUMP,
            &format!(
                "{statement};
                 ALTER TABLE orders ADD COLUMN memo INT NULL;
                 ALTER TABLE orders MODIFY note VARCHAR(50) NULL;"
            ),
        );
        assert_eq!(run.answers()[0], "unknown", "{statement}");
        assert!(
            run.reason(0).contains("1054"),
            "{statement}: {}",
            run.reason(0)
        );
        // memo was not created by the refused statement, note is as it was.
        assert_eq!(run.answers()[1], INSTANT, "{statement}");
        assert_eq!(run.answers()[2], INSTANT, "{statement}");
    }
}

#[test]
fn dropping_a_foreign_key_beside_adding_one_of_the_same_name_is_refused_with_1826() {
    // By hand: DROP FOREIGN KEY k, ADD CONSTRAINT k FOREIGN KEY ... in either
    // order is error 1826 and the key stays: dropping its column still
    // fails with 1828.
    for statement in [
        "ALTER TABLE orders DROP FOREIGN KEY orders_user_id_foreign, ADD CONSTRAINT orders_user_id_foreign FOREIGN KEY (user_id) REFERENCES users (id)",
        "ALTER TABLE orders ADD CONSTRAINT orders_user_id_foreign FOREIGN KEY (user_id) REFERENCES users (id), DROP FOREIGN KEY orders_user_id_foreign",
    ] {
        let run = run_on_dump(
            USERS_AND_ORDERS_WITH_FK_DUMP,
            &format!("{statement};\nALTER TABLE orders DROP COLUMN user_id;"),
        );
        assert_eq!(run.answers()[0], "unknown", "{statement}");
        assert!(
            run.reason(0).contains("1826"),
            "{statement}: {}",
            run.reason(0)
        );
        assert_eq!(run.last_answer(), FAILS_1828, "{statement}");
    }
}

#[test]
fn dropping_a_foreign_key_column_beside_other_clauses_is_refused_with_1828_and_the_table_stays() {
    // By hand: DROP COLUMN user_id with ADD COLUMN or MODIFY, in either
    // order, is error 1828 and nothing changes.
    for statement in [
        "ALTER TABLE orders DROP COLUMN user_id, ADD COLUMN extra INT NULL",
        "ALTER TABLE orders DROP COLUMN user_id, MODIFY amount BIGINT NOT NULL",
        "ALTER TABLE orders ADD COLUMN extra INT NULL, DROP COLUMN user_id",
    ] {
        let run = run_on_dump(
            USERS_AND_ORDERS_WITH_FK_DUMP,
            &format!(
                "{statement};
                 ALTER TABLE orders DROP COLUMN user_id;
                 ALTER TABLE orders ADD COLUMN extra INT NULL;
                 ALTER TABLE orders MODIFY amount INT NOT NULL;"
            ),
        );
        assert_eq!(run.answers()[0], "unknown", "{statement}");
        assert!(
            run.reason(0).contains("1828"),
            "{statement}: {}",
            run.reason(0)
        );
        assert_eq!(run.answers()[1], FAILS_1828, "{statement}");
        assert_eq!(run.answers()[2], INSTANT, "{statement}");
        assert_eq!(run.answers()[3], INSTANT, "{statement}");
    }
}

#[test]
fn dropping_a_foreign_key_and_its_column_in_one_alter_is_followed() {
    // By hand: DROP FOREIGN KEY k, DROP COLUMN user_id in either order is
    // accepted: the column and the key are gone.
    for statement in [
        "ALTER TABLE orders DROP FOREIGN KEY orders_user_id_foreign, DROP COLUMN user_id",
        "ALTER TABLE orders DROP COLUMN user_id, DROP FOREIGN KEY orders_user_id_foreign",
    ] {
        let run = run_on_dump(
            USERS_AND_ORDERS_WITH_FK_DUMP,
            &format!("{statement};\nALTER TABLE orders ADD COLUMN user_id BIGINT UNSIGNED NULL;"),
        );
        assert_eq!(run.last_answer(), INSTANT, "{statement}");
    }
}

#[test]
fn other_shapes_that_name_one_column_in_two_clauses_lose_the_table() {
    // Not hand-tested shapes: kilit cannot tell what MySQL does, so it loses
    // the table, whatever the final state is.
    for statement in [
        "ALTER TABLE orders MODIFY note VARCHAR(60) NULL, ALTER COLUMN note SET DEFAULT 'x'",
        "ALTER TABLE orders MODIFY note VARCHAR(60) NULL, MODIFY note VARCHAR(70) NULL",
    ] {
        let run = run_on_dump(
            USERS_AND_ORDERS_DUMP,
            &format!("{statement};\n{}", add_probe_column("orders")),
        );
        assert_eq!(run.last_answer(), "unknown", "{statement}");
        assert_eq!(run.last_impact(), Impact::Unknown, "{statement}");
    }
}

// An inline PRIMARY KEY in MODIFY or CHANGE, and SERIAL. By hand
// on MySQL 8.0.46 and 8.4.13 (the same on both).

const SERIAL_TABLES_DUMP: &str = "CREATE TABLE `users` (
  `id` bigint unsigned NOT NULL AUTO_INCREMENT,
  `name` varchar(100) NOT NULL,
  PRIMARY KEY (`id`)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_0900_ai_ci;
CREATE TABLE `orders` (
  `id` bigint unsigned NOT NULL AUTO_INCREMENT,
  `user_id` bigint unsigned NOT NULL,
  `amount` int NOT NULL,
  PRIMARY KEY (`id`)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_0900_ai_ci;
CREATE TABLE `np` (
  `c` int DEFAULT NULL,
  `d` int DEFAULT NULL
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_0900_ai_ci;
CREATE TABLE `sn` (
  `id` bigint unsigned NOT NULL,
  `uid` bigint unsigned DEFAULT NULL,
  PRIMARY KEY (`id`),
  KEY `sn_uid_foreign` (`uid`),
  CONSTRAINT `sn_uid_foreign` FOREIGN KEY (`uid`) REFERENCES `users` (`id`) ON DELETE SET NULL
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_0900_ai_ci;
CREATE TABLE `sc` (
  `id` bigint unsigned NOT NULL,
  `uid` bigint unsigned DEFAULT NULL,
  PRIMARY KEY (`id`),
  KEY `sc_uid_foreign` (`uid`),
  CONSTRAINT `sc_uid_foreign` FOREIGN KEY (`uid`) REFERENCES `users` (`id`) ON DELETE CASCADE
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_0900_ai_ci;
CREATE TABLE `pl` (
  `id` bigint unsigned NOT NULL,
  `uid` bigint unsigned DEFAULT NULL,
  PRIMARY KEY (`id`)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_0900_ai_ci;
";

#[test]
fn an_inline_primary_key_on_a_table_without_one_is_followed() {
    // MODIFY c INT PRIMARY KEY, CHANGE c c INT PRIMARY KEY and CHANGE c k INT
    // PRIMARY KEY make the column the primary key (and NOT NULL).
    for (statement, column) in [
        ("ALTER TABLE np MODIFY c INT PRIMARY KEY", "c"),
        ("ALTER TABLE np CHANGE c c INT PRIMARY KEY", "c"),
        ("ALTER TABLE np CHANGE c k INT PRIMARY KEY", "k"),
    ] {
        let run = run_on_dump(
            SERIAL_TABLES_DUMP,
            &format!(
                "{statement};
                 ALTER TABLE np ADD PRIMARY KEY (d);
                 ALTER TABLE np ADD COLUMN {column} INT NULL;
                 {}",
                add_probe_column("np")
            ),
        );
        // The table has a primary key now: a second one is error 1068.
        assert_eq!(run.answers()[1], "unknown", "{statement}");
        assert!(
            run.reason(1).contains("1068"),
            "{statement}: {}",
            run.reason(1)
        );
        // The column is there under its new name.
        assert_eq!(run.answers()[2], "unknown", "{statement}");
        assert!(
            run.reason(2).contains("1060"),
            "{statement}: {}",
            run.reason(2)
        );
        assert_eq!(run.last_answer(), INSTANT, "{statement}");
    }
    // After the rename the old name is free.
    let run = run_on_dump(
        SERIAL_TABLES_DUMP,
        "ALTER TABLE np CHANGE c k INT PRIMARY KEY;\nALTER TABLE np ADD COLUMN c INT NULL;",
    );
    assert_eq!(run.last_answer(), INSTANT);
}

#[test]
fn an_inline_primary_key_on_a_table_that_has_one_is_refused_with_1068_and_the_table_stays() {
    // MODIFY amount INT PRIMARY KEY on orders, which has a primary key.
    for statement in [
        "ALTER TABLE orders MODIFY amount INT PRIMARY KEY",
        "ALTER TABLE orders CHANGE amount amount INT PRIMARY KEY",
        "ALTER TABLE orders MODIFY amount INT NOT NULL PRIMARY KEY",
        "ALTER TABLE orders MODIFY id BIGINT UNSIGNED AUTO_INCREMENT PRIMARY KEY",
    ] {
        let run = run_on_dump(
            SERIAL_TABLES_DUMP,
            &format!(
                "{statement};
                 ALTER TABLE orders MODIFY amount INT NOT NULL;
                 {}",
                add_probe_column("orders")
            ),
        );
        assert_refused_by_hand(&run, 0, "1068", statement).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(run.answers()[1], INSTANT, "{statement}");
        assert_eq!(run.answers()[2], INSTANT, "{statement}");
    }
}

#[test]
fn serial_on_the_column_of_a_foreign_key_is_refused_and_the_key_stays() {
    // SET NULL key: error 1830; CASCADE key: error 1832. The key still stops
    // DROP COLUMN uid with 1828.
    for (table, code) in [("sn", "1830"), ("sc", "1832")] {
        let run = run_on_dump(
            SERIAL_TABLES_DUMP,
            &format!(
                "ALTER TABLE {table} MODIFY uid SERIAL;
                 ALTER TABLE {table} DROP COLUMN uid;
                 {}",
                add_probe_column(table)
            ),
        );
        assert_refused_by_hand(&run, 0, code, table).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(run.answers()[1], FAILS_1828, "{table}");
        assert_eq!(run.answers()[2], INSTANT, "{table}");
    }
}

#[test]
fn serial_beside_another_auto_increment_column_is_refused_with_1075() {
    let run = run_on_dump(
        SERIAL_TABLES_DUMP,
        &format!(
            "ALTER TABLE orders MODIFY user_id SERIAL;\n{}",
            add_probe_column("orders")
        ),
    );
    assert_refused_by_hand(&run, 0, "1075", "serial beside auto_increment")
        .unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(run.answers()[1], INSTANT);
}

#[test]
fn serial_on_a_plain_column_is_not_null_auto_increment_with_a_unique_key_named_after_it() {
    // MODIFY uid SERIAL: BIGINT UNSIGNED NOT NULL AUTO_INCREMENT and UNIQUE
    // KEY uid (uid).
    let run = run_on_dump(
        SERIAL_TABLES_DUMP,
        "ALTER TABLE pl MODIFY uid SERIAL;
         ALTER TABLE pl ADD INDEX uid (id);
         ALTER TABLE pl ADD COLUMN uid INT NULL;
         ALTER TABLE pl ADD COLUMN probe_col INT NULL;",
    );
    assert_ne!(run.answers()[0], "fails with error 1830");
    // The key named uid is there: a second index of that name is 1061.
    assert_eq!(run.answers()[1], "unknown");
    assert!(run.reason(1).contains("1061"), "{}", run.reason(1));
    assert_eq!(run.answers()[2], "unknown");
    assert!(run.reason(2).contains("1060"), "{}", run.reason(2));
    assert_eq!(run.answers()[3], INSTANT);
    // On a table without a primary key the unique key is named after the
    // column too.
    let run = run_on_dump(
        SERIAL_TABLES_DUMP,
        "ALTER TABLE np MODIFY c SERIAL;\nALTER TABLE np ADD INDEX c (d);",
    );
    assert_eq!(run.answers()[1], "unknown");
    assert!(run.reason(1).contains("1061"), "{}", run.reason(1));
}

// CREATE TABLE ... LIKE. By hand on MySQL 8.0.46 and 8.4.13 (the
// same on both): the copy has the columns, the indexes under their names
// (also the automatic index of a foreign key), the CHECK constraints under
// the names <copy>_chk_<n>, numbered in the order of the source's names, no
// foreign key, and the engine, row format, key block size, character set,
// collation and comment of the source; its AUTO_INCREMENT counter starts
// again.

const LIKE_SOURCES_DUMP: &str = "CREATE TABLE `users` (
  `id` bigint unsigned NOT NULL AUTO_INCREMENT,
  `name` varchar(100) NOT NULL,
  PRIMARY KEY (`id`)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_0900_ai_ci;
CREATE TABLE `a` (
  `id` bigint unsigned NOT NULL AUTO_INCREMENT,
  `uid` bigint unsigned NOT NULL,
  `title` varchar(100) CHARACTER SET latin1 COLLATE latin1_bin NOT NULL,
  `body` text COLLATE utf8mb4_unicode_ci,
  `amount` int NOT NULL DEFAULT '0',
  PRIMARY KEY (`id`),
  KEY `ix_amount` (`amount`),
  KEY `title` (`title`),
  KEY `a_uid_foreign` (`uid`),
  FULLTEXT KEY `ft_body` (`body`),
  CONSTRAINT `a_uid_foreign` FOREIGN KEY (`uid`) REFERENCES `users` (`id`),
  CONSTRAINT `a_chk_1` CHECK ((`amount` < 1000000)),
  CONSTRAINT `a_pos` CHECK ((`amount` >= 0))
) ENGINE=InnoDB AUTO_INCREMENT=100 DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_unicode_ci COMMENT='table a';
CREATE TABLE `ck` (
  `id` int NOT NULL,
  `a` int DEFAULT NULL,
  `b` int DEFAULT NULL,
  `c` int DEFAULT NULL,
  PRIMARY KEY (`id`),
  CONSTRAINT `a_second` CHECK ((`b` < 100)),
  CONSTRAINT `ck_chk_1` CHECK ((`c` <> 50)),
  CONSTRAINT `z_first` CHECK ((`a` > 0))
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_0900_ai_ci;
CREATE TABLE `am` (
  `id` int NOT NULL AUTO_INCREMENT,
  `uid` bigint unsigned NOT NULL,
  `body` text,
  PRIMARY KEY (`id`),
  KEY `ix_uid` (`uid`),
  FULLTEXT KEY `ft` (`body`),
  CONSTRAINT `am_chk` CHECK ((`uid` > 0))
) ENGINE=MyISAM DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_0900_ai_ci;
CREATE TABLE `my` (
  `id` int NOT NULL AUTO_INCREMENT,
  `uid` bigint unsigned NOT NULL,
  `amount` int NOT NULL,
  PRIMARY KEY (`id`),
  KEY `ix_uid` (`uid`)
) ENGINE=MyISAM DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_0900_ai_ci;
CREATE TABLE `cz` (
  `id` bigint unsigned NOT NULL AUTO_INCREMENT,
  `uid` bigint unsigned NOT NULL,
  `body` text COLLATE utf8mb4_unicode_ci,
  `amount` int NOT NULL DEFAULT '0',
  PRIMARY KEY (`id`),
  KEY `ix_amount` (`amount`)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_unicode_ci ROW_FORMAT=COMPRESSED KEY_BLOCK_SIZE=8;
CREATE TABLE `ac` (
  `id` int NOT NULL,
  `uid` bigint unsigned DEFAULT NULL,
  `note` varchar(20) DEFAULT NULL,
  PRIMARY KEY (`id`),
  KEY `uid` (`uid`)
) ENGINE=InnoDB DEFAULT CHARSET=latin1 ROW_FORMAT=COMPACT;
CREATE TABLE `fd` (
  `id` int NOT NULL,
  `body` text,
  PRIMARY KEY (`id`),
  FULLTEXT KEY `ft` (`body`)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_0900_ai_ci;
";

/// Every way to write the plain copy of `a` as `b`.
const PLAIN_LIKE_FORMS: [&str; 5] = [
    "CREATE TABLE b LIKE a;",
    "CREATE TABLE b (LIKE a);",
    "CREATE TABLE IF NOT EXISTS b LIKE a;",
    "CREATE TABLE IF NOT EXISTS b (LIKE a);",
    "create table `b` like `a`;",
];

#[test]
fn a_table_copied_with_like_has_what_the_source_has_and_no_foreign_key() {
    for form in PLAIN_LIKE_FORMS {
        let probe = |statement: &str| -> FilesRun {
            run_on_dump(LIKE_SOURCES_DUMP, &format!("{form}\n{statement};"))
        };
        // The copy is a table made here whose FULLTEXT index (copied) makes
        // ADD COLUMN a copy: the same as the source.
        let add = probe("ALTER TABLE b ADD COLUMN probe_col INT NULL");
        let source = run_on_dump(
            LIKE_SOURCES_DUMP,
            "ALTER TABLE a ADD COLUMN probe_col INT NULL;",
        );
        if form.contains("IF NOT EXISTS") {
            // CREATE TABLE IF NOT EXISTS never counts as a new table.
            assert_ne!(add.last_impact(), Impact::NewTable, "{form}");
            continue;
        }
        assert_eq!(add.last_answer(), COPY, "{form}");
        assert_eq!(add.last_answer(), source.last_answer(), "{form}");
        assert_eq!(add.last_impact(), Impact::NewTable, "{form}");
        // The columns of a, also the character set of a column.
        let same = probe(
            "ALTER TABLE b MODIFY title VARCHAR(100) CHARACTER SET latin1 COLLATE latin1_bin NOT NULL",
        );
        let same_on_a = run_on_dump(
            LIKE_SOURCES_DUMP,
            "ALTER TABLE a MODIFY title VARCHAR(100) CHARACTER SET latin1 COLLATE latin1_bin NOT NULL;",
        );
        assert_eq!(same.last_answer(), same_on_a.last_answer(), "{form}");
        assert_eq!(
            probe("ALTER TABLE b ADD COLUMN amount INT NULL").last_answer(),
            "unknown",
            "{form}"
        );
        // The indexes keep their names, the automatic index of the key too.
        for index in ["ix_amount", "title", "a_uid_foreign", "ft_body"] {
            let taken = probe(&format!("ALTER TABLE b ADD INDEX {index} (id)"));
            assert_eq!(taken.last_answer(), "unknown", "{form}: {index}");
            assert!(
                taken.reason(1).contains("1061"),
                "{form}: {index}: {}",
                taken.reason(1)
            );
        }
        // Dropping an index of the copy is measured, as on a.
        let dropped = probe("ALTER TABLE b DROP INDEX ix_amount");
        let dropped_on_a = run_on_dump(LIKE_SOURCES_DUMP, "ALTER TABLE a DROP INDEX ix_amount;");
        assert_ne!(dropped.last_answer(), "unknown", "{form}");
        assert_eq!(dropped.last_answer(), dropped_on_a.last_answer(), "{form}");
        // The copy has no foreign key: a has one (measured), b does not
        // (error 1091, unknown).
        let key_of_a = run_on_dump(
            LIKE_SOURCES_DUMP,
            "ALTER TABLE a DROP FOREIGN KEY a_uid_foreign;",
        );
        assert_ne!(key_of_a.last_answer(), "unknown");
        assert_eq!(
            probe("ALTER TABLE b DROP FOREIGN KEY a_uid_foreign").last_answer(),
            "unknown",
            "{form}"
        );
        assert_ne!(
            probe("ALTER TABLE b DROP COLUMN uid").last_answer(),
            FAILS_1828,
            "{form}"
        );
        // The CHECK constraints are renamed <copy>_chk_<n>, numbered in the
        // order of the source's names (a_chk_1, a_pos).
        for name in ["b_chk_1", "b_chk_2"] {
            let dropped = probe(&format!("ALTER TABLE b DROP CHECK {name}"));
            assert_eq!(dropped.last_answer(), INSTANT, "{form}: {name}");
        }
        for name in ["a_chk_1", "a_pos", "b_chk_3"] {
            let missing = probe(&format!("ALTER TABLE b DROP CHECK {name}"));
            assert_eq!(missing.last_answer(), "unknown", "{form}: {name}");
        }
    }
}

#[test]
fn the_checks_of_a_copy_are_numbered_in_the_order_of_the_source_s_names() {
    // ck has z_first (a), a_second (b) and the automatic ck_chk_1 (c). By
    // hand the copy has b_chk_1 for b, b_chk_2 for c, b_chk_3 for a. Renaming
    // a column that a CHECK uses is error 3959 until the CHECK is dropped.
    for (check, column) in [("b_chk_1", "b"), ("b_chk_2", "c"), ("b_chk_3", "a")] {
        let before = run_on_dump(
            LIKE_SOURCES_DUMP,
            &format!("CREATE TABLE b LIKE ck;\nALTER TABLE b RENAME COLUMN {column} TO renamed;"),
        );
        assert_eq!(before.last_answer(), "fails with error 3959", "{column}");
        let after = run_on_dump(
            LIKE_SOURCES_DUMP,
            &format!(
                "CREATE TABLE b LIKE ck;
                 ALTER TABLE b DROP CHECK {check};
                 ALTER TABLE b RENAME COLUMN {column} TO renamed;"
            ),
        );
        assert_eq!(after.answers()[1], INSTANT, "{check}");
        assert_eq!(after.last_answer(), INSTANT, "{check} frees {column}");
    }
}

#[test]
fn a_copy_of_a_myisam_compressed_or_compact_table_answers_as_its_source_does() {
    // The engine, the row format and the key block size are copied.
    for (source, copy_answer) in [("my", Some(COPY)), ("am", None), ("cz", None), ("ac", None)] {
        for probe in [
            "ADD COLUMN probe_col INT NULL",
            "ADD INDEX ix_p (id)",
            "DROP COLUMN id",
        ] {
            let on_source =
                run_on_dump(LIKE_SOURCES_DUMP, &format!("ALTER TABLE {source} {probe};"));
            let on_copy = run_on_dump(
                LIKE_SOURCES_DUMP,
                &format!("CREATE TABLE b LIKE {source};\nALTER TABLE b {probe};"),
            );
            assert_eq!(
                on_copy.last_answer(),
                on_source.last_answer(),
                "{source}: {probe}"
            );
            if probe == "ADD COLUMN probe_col INT NULL" && source != "am" {
                assert_ne!(on_copy.last_answer(), "unknown", "{source}");
                if let Some(answer) = copy_answer {
                    assert_eq!(on_copy.last_answer(), answer, "{source}");
                }
            }
        }
    }
}

#[test]
fn a_copy_has_the_columns_the_source_has_at_that_point_of_the_input() {
    let run = run_on_dump(
        LIKE_SOURCES_DUMP,
        "ALTER TABLE a ADD COLUMN extra INT NULL;
         CREATE TABLE b LIKE a;
         ALTER TABLE b ADD COLUMN extra INT NULL;",
    );
    assert_eq!(run.last_answer(), "unknown");
    assert!(run.reason(2).contains("1060"), "{}", run.reason(2));
}

#[test]
fn like_of_a_temporary_table_copies_the_temporary_table_that_hides_the_base_table() {
    // By hand: the copy has the columns of the temporary table that hides
    // orders, not those of the base orders.
    let run = run_on_dump(
        USERS_AND_ORDERS_DUMP,
        "CREATE TEMPORARY TABLE orders (id INT PRIMARY KEY, tmp_col INT NULL);
         CREATE TABLE b LIKE orders;
         ALTER TABLE b ADD COLUMN user_id INT NULL;
         ALTER TABLE b ADD COLUMN tmp_col INT NULL;",
    );
    assert_eq!(run.answers()[2], INSTANT);
    assert_eq!(run.answers()[3], "unknown");
    assert!(run.reason(3).contains("1060"), "{}", run.reason(3));
}

#[test]
fn like_of_a_table_kilit_does_not_know_loses_the_copy() {
    // No schema: the source is unseen. The dump lacks nosuch (error 1146 on
    // the server). A source kilit lost. In each case b is lost.
    let probe = "ALTER TABLE b ADD COLUMN probe_col INT NULL;";
    assert_eq!(
        run_without_dump(&format!("CREATE TABLE b LIKE a;\n{probe}")).last_answer(),
        "unknown"
    );
    assert_eq!(
        run_on_dump(
            LIKE_SOURCES_DUMP,
            &format!("CREATE TABLE b LIKE nosuch;\n{probe}")
        )
        .last_answer(),
        "unknown"
    );
    let run = run_on_dump(
        LIKE_SOURCES_DUMP,
        &format!("ALTER TABLE a FROBNICATE x;\nCREATE TABLE b LIKE a;\n{probe}"),
    );
    assert_eq!(run.last_answer(), "unknown");
    assert_eq!(run.last_impact(), Impact::Unknown);
}

#[test]
fn create_table_if_not_exists_like_over_a_table_made_in_the_input_changes_nothing() {
    // By hand: accepted with Note 1050, the first b stays.
    let run = run_on_dump(
        LIKE_SOURCES_DUMP,
        "CREATE TABLE b (id INT PRIMARY KEY, x INT NULL);
         CREATE TABLE IF NOT EXISTS b LIKE a;
         ALTER TABLE b ADD COLUMN x INT NULL;
         ALTER TABLE b ADD COLUMN amount INT NULL;",
    );
    assert_eq!(run.answers()[2], "unknown");
    assert!(run.reason(2).contains("1060"), "{}", run.reason(2));
    assert_eq!(run.answers()[3], INSTANT);
}

#[test]
fn a_temporary_copy_of_a_compressed_table_or_of_a_fulltext_innodb_table_is_refused() {
    // By hand: CREATE TEMPORARY TABLE b LIKE <ROW_FORMAT=COMPRESSED table> is
    // error 3500; LIKE an InnoDB table with a FULLTEXT index is 1796. No
    // table b exists afterwards.
    for (source, code) in [("cz", "3500"), ("fd", "1796")] {
        let run = run_on_dump(
            LIKE_SOURCES_DUMP,
            &format!(
                "CREATE TEMPORARY TABLE b LIKE {source};\n{}",
                add_probe_column("b")
            ),
        );
        assert_eq!(run.answers()[0], "unknown", "{source}");
        assert!(run.reason(0).contains(code), "{source}: {}", run.reason(0));
        assert_eq!(run.answers()[1], "unknown", "{source}");
        assert_eq!(run.last_impact(), Impact::Unknown, "{source}");
    }
    // The same copy as a base table is accepted, as are temporary copies of
    // a MyISAM table with a FULLTEXT index and of a COMPACT table.
    for statement in [
        "CREATE TABLE b LIKE cz;",
        "CREATE TABLE b LIKE fd;",
        "CREATE TEMPORARY TABLE b LIKE am;",
        "CREATE TEMPORARY TABLE b LIKE ac;",
    ] {
        let run = run_on_dump(LIKE_SOURCES_DUMP, statement);
        assert_eq!(run.answers(), ["not checked"], "{statement}");
    }
}

#[test]
fn like_with_more_after_it_or_a_table_option_is_refused_by_the_servers_and_creates_nothing() {
    // By hand: CREATE TABLE b (LIKE ac, extra INT) and CREATE TABLE b LIKE ac
    // ENGINE=MyISAM are error 1064 on both servers: unknown, with the reason
    // that names the servers, and no table b.
    let mut problems = Vec::new();
    for statement in [
        "CREATE TABLE b (LIKE ac, extra INT);",
        "CREATE TABLE b LIKE ac ENGINE=MyISAM;",
    ] {
        let run = run_on_dump(
            LIKE_SOURCES_DUMP,
            &format!("{statement}\n{}", add_probe_column("b")),
        );
        if run.answers()[0] != "unknown" {
            problems.push(format!("{statement}: answered {}", run.answers()[0]));
        } else if !run.reason(0).contains("8.0.46 and 8.4.13") {
            problems.push(format!(
                "{statement}: the reason does not name the servers: {}",
                run.reason(0)
            ));
        }
        if run.answers()[1] != "unknown" {
            problems.push(format!(
                "{statement}: b should not exist, but ADD COLUMN is {}",
                run.answers()[1]
            ));
        }
    }
    assert!(problems.is_empty(), "{problems:#?}");
}

// The three table states and forced variants for the refusals of names.

/// Every statement of the two tables above and of the sweep.
fn every_name_refusal_statement() -> Vec<String> {
    let mut statements: Vec<String> = REFUSALS_THAT_KEEP_THE_TABLE
        .iter()
        .map(|(_, statement, _)| (*statement).to_owned())
        .chain(
            REFUSALS_THAT_LOSE_THE_TABLE
                .iter()
                .map(|(_, statement, _)| (*statement).to_owned()),
        )
        .collect();
    statements.extend(
        missing_and_taken_names()
            .into_iter()
            .map(|(statement, _, _)| statement),
    );
    statements
}

#[test]
fn a_name_the_table_lacks_or_has_is_never_a_failure_on_a_table_kilit_does_not_know() {
    // Missing (no --schema) and Lost (a statement kilit cannot read changed
    // it): kilit does not know the names, so it cannot say 1060, 1054, 1061
    // or 1050. The answer may be unknown or what every table shares, never
    // a failure.
    let lose = "ALTER TABLE orders FROBNICATE x;\n";
    let mut problems = Vec::new();
    for statement in every_name_refusal_statement() {
        for (state, dump, prefix) in [
            ("missing", None, ""),
            ("lost", Some(TABLES_WITH_NAMES_DUMP), lose),
        ] {
            let run = run_files_on_both_servers(dump, &[&format!("{prefix}{statement};")]);
            let answer = run.last_answer();
            if answer.starts_with("fails") || run.last_impact() == Impact::Fails {
                problems.push(format!("{state}: {statement}: {answer}"));
            }
        }
    }
    assert!(problems.is_empty(), "{problems:#?}");
}

#[test]
fn a_forced_algorithm_or_lock_does_not_make_a_refused_name_a_runs_answer() {
    // Forced variants are answered from the lab's run of exactly that
    // variant, but only for a statement MySQL accepts; a refused name stays
    // unknown whatever is forced.
    let mut problems = Vec::new();
    let alters = REFUSALS_THAT_KEEP_THE_TABLE
        .iter()
        .map(|(_, statement, _)| *statement)
        .chain(
            REFUSALS_THAT_LOSE_THE_TABLE
                .iter()
                .map(|(_, statement, _)| *statement),
        )
        .filter(|statement| statement.starts_with("ALTER TABLE"));
    for statement in alters {
        for forced in [
            ", ALGORITHM=INSTANT",
            ", ALGORITHM=INPLACE",
            ", ALGORITHM=COPY",
            ", ALGORITHM=INPLACE, LOCK=NONE",
            ", LOCK=NONE",
        ] {
            let run = run_on_dump(TABLES_WITH_NAMES_DUMP, &format!("{statement}{forced};"));
            if run.answers()[0] != "unknown" || run.impacts()[0] != Impact::Unknown {
                problems.push(format!(
                    "{statement}{forced}: {} / {:?}",
                    run.answers()[0],
                    run.impacts()[0]
                ));
            }
        }
    }
    assert!(problems.is_empty(), "{problems:#?}");
}

/// A table as mysqldump writes it: an automatic foreign key name
/// (`ac_ibfk_1`), a CHECK on a string written `_utf8mb4''`, a capital
/// letter in a column's type, and a version comment on a NOT ENFORCED CHECK.
const MYSQLDUMP_STYLE_DUMP: &str = "CREATE TABLE `users` (
  `id` bigint unsigned NOT NULL AUTO_INCREMENT,
  PRIMARY KEY (`id`)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_0900_ai_ci;
CREATE TABLE `ac` (
  `id` int NOT NULL,
  `uid` bigint unsigned DEFAULT NULL,
  `note` varchar(20) DEFAULT NULL,
  PRIMARY KEY (`id`),
  KEY `uid` (`uid`),
  CONSTRAINT `ac_ibfk_1` FOREIGN KEY (`uid`) REFERENCES `users` (`id`),
  CONSTRAINT `ac_chk_1` CHECK ((`id` > 0)),
  CONSTRAINT `ac_note` CHECK ((`note` <> _utf8mb4'')) /*!80016 NOT ENFORCED */
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_0900_ai_ci;
";

#[test]
fn a_table_copied_with_like_from_a_mysqldump_table_has_no_foreign_key_and_renamed_checks() {
    // By hand: the copy of ac has KEY uid, no foreign key (ac_ibfk_1 is not
    // there), and CHECKs b_chk_1 (id > 0) and b_chk_2 (note <> '').
    let run = run_on_dump(
        MYSQLDUMP_STYLE_DUMP,
        "CREATE TABLE `b` LIKE `ac`;
         ALTER TABLE `b` DROP FOREIGN KEY `ac_ibfk_1`;
         ALTER TABLE `b` ADD INDEX `uid` (`id`);
         ALTER TABLE `b` DROP CHECK `ac_note`;
         ALTER TABLE `b` DROP CHECK `b_chk_2`;",
    );
    assert_eq!(run.answers()[1], "unknown");
    assert!(run.reason(1).contains("1091"), "{}", run.reason(1));
    assert_eq!(run.answers()[2], "unknown");
    assert!(run.reason(2).contains("1061"), "{}", run.reason(2));
    assert_eq!(run.answers()[3], "unknown");
    assert_eq!(run.answers()[4], INSTANT);
    // The source still has its foreign key under the automatic name.
    let source = run_on_dump(
        MYSQLDUMP_STYLE_DUMP,
        "ALTER TABLE `ac` DROP FOREIGN KEY `ac_ibfk_1`;",
    );
    assert_ne!(source.last_answer(), "unknown");
}

#[test]
fn a_mysqldump_statement_in_capitals_with_quoted_names_gets_the_same_refusals() {
    // The statements as a tool writes them: capital letters, backticks.
    for (statement, code) in [
        (
            "ALTER TABLE `orders` ADD COLUMN `note` VARCHAR(50) NULL;",
            "1060",
        ),
        ("ALTER TABLE `orders` MODIFY COLUMN `zzz` INT NULL;", "1054"),
        (
            "ALTER TABLE `orders` ADD INDEX `ix_amount` (`note`);",
            "1061",
        ),
        (
            "ALTER TABLE `orders` ADD CONSTRAINT `orders_user_id_foreign` FOREIGN KEY (`user_id`) REFERENCES `users` (`id`);",
            "1826",
        ),
        ("ALTER TABLE `orders` ADD PRIMARY KEY (`amount`);", "1068"),
    ] {
        let run = run_on_dump(TABLES_WITH_NAMES_DUMP, statement);
        assert_eq!(run.answers()[0], "unknown", "{statement}");
        assert!(
            run.reason(0).contains(code),
            "{statement}: {}",
            run.reason(0)
        );
    }
}

// Inputs kilit once followed wrongly. Facts: by hand on MySQL 8.0.46 and
// 8.4.13 (the same on both).

/// The statement at `at` is not a measured answer that lets writes continue
/// or wait, not a table created here and not a measured failure.
fn assert_not_a_measured_answer(run: &FilesRun, at: usize, what: &str) {
    assert!(
        !matches!(
            run.impacts()[at],
            Impact::WritesContinue | Impact::WritesWait | Impact::NewTable | Impact::Fails
        ),
        "{what}: {} / {:?}",
        run.answers()[at],
        run.impacts()[at]
    );
}

/// No unknown statement of `run` names error `code` in its reason.
fn assert_no_reason_names(run: &FilesRun, code: &str, what: &str) {
    for (at, finding) in run.findings.iter().enumerate() {
        if let Prediction::Unknown { reason, .. } = &finding.prediction {
            assert!(
                !reason.contains(code),
                "{what}: statement {at} names {code}: {reason}"
            );
        }
    }
}

#[test]
fn a_temporary_table_with_the_name_a_rename_goes_to_is_no_clash_and_the_old_name_is_gone() {
    // CREATE TEMPORARY TABLE x; RENAME TABLE users TO x is accepted (RENAME
    // TABLE does not see the temporary x), and a later ALTER TABLE users is
    // error 1146: never a measured answer.
    for rename in ["RENAME TABLE users TO x;", "ALTER TABLE users RENAME TO x;"] {
        let run = run_on_dump(
            USERS_AND_ORDERS_DUMP,
            &format!(
                "CREATE TEMPORARY TABLE x (id INT PRIMARY KEY);
                 {rename}
                 ALTER TABLE users ADD COLUMN c INT NULL;"
            ),
        );
        assert_no_reason_names(&run, "1050", rename);
        assert_eq!(run.last_answer(), "unknown", "{rename}");
        assert_not_a_measured_answer(&run, 2, rename);
    }
}

#[test]
fn a_temporary_table_moved_onto_a_base_name_or_a_copy_beside_a_temporary_name_is_no_clash() {
    // ALTER TABLE t3 RENAME TO users with a temporary t3 and a base users is
    // accepted; CREATE TABLE b LIKE orders with only a temporary b is
    // accepted. Neither is error 1050.
    let run = run_on_dump(
        USERS_AND_ORDERS_DUMP,
        "CREATE TEMPORARY TABLE t3 (id INT PRIMARY KEY);
         ALTER TABLE t3 RENAME TO users;",
    );
    assert_no_reason_names(&run, "1050", "t3 to users");
    let run = run_on_dump(
        USERS_AND_ORDERS_DUMP,
        "CREATE TEMPORARY TABLE b (id INT PRIMARY KEY);
         CREATE TABLE b LIKE orders;",
    );
    assert_no_reason_names(&run, "1050", "LIKE beside a temporary b");
    assert_eq!(run.answers()[1], "not checked");
}

#[test]
fn a_foreign_key_and_its_column_dropped_in_one_alter_leave_no_column_for_an_index() {
    // By hand: DROP CONSTRAINT orders_user_id_foreign, DROP COLUMN user_id is
    // accepted, so ADD INDEX ix_u (user_id) is error 1072. Not writes
    // continue, and not the 1828 of a key that is gone.
    let run = run_on_dump(
        USERS_AND_ORDERS_WITH_FK_DUMP,
        "ALTER TABLE orders DROP CONSTRAINT orders_user_id_foreign, DROP COLUMN user_id;
         ALTER TABLE orders ADD INDEX ix_u (user_id);",
    );
    assert_no_reason_names(&run, "1828", "named key");
    assert_not_a_measured_answer(&run, 1, "ADD INDEX on the dropped column");
    assert_ne!(run.answers()[1], FAILS_1828);
}

#[test]
fn an_unnamed_key_dropped_by_its_automatic_name_with_its_column_gives_no_measured_index() {
    // By hand: DROP FOREIGN KEY o2_ibfk_1, DROP COLUMN user_id is accepted;
    // the later ADD INDEX ix_u (user_id) is error 1072. kilit may lose the
    // table; it never gives a measured runs answer.
    let run = run_on_dump(
        USERS_AND_ORDERS_DUMP,
        "CREATE TABLE o2 (id BIGINT UNSIGNED AUTO_INCREMENT PRIMARY KEY, user_id BIGINT UNSIGNED NOT NULL, \
         amount INT NOT NULL, FOREIGN KEY (user_id) REFERENCES users (id)) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4;
         ALTER TABLE o2 DROP FOREIGN KEY o2_ibfk_1, DROP COLUMN user_id;
         ALTER TABLE o2 ADD INDEX ix_u (user_id);",
    );
    assert_not_a_measured_answer(&run, 2, "ADD INDEX on the dropped column");
    assert_ne!(run.answers()[2], FAILS_1828);
}

#[test]
fn a_swap_of_a_column_a_check_uses_is_refused_with_3959_and_the_table_is_unchanged() {
    // By hand: with CHECK (amount > 0), both swaps fail with 3959 and change
    // nothing, so RENAME COLUMN amount TO total still fails with 3959.
    let dump = "CREATE TABLE `orders` (
  `id` bigint unsigned NOT NULL AUTO_INCREMENT,
  `amount` int NOT NULL,
  `note` varchar(50) DEFAULT NULL,
  `extra` int DEFAULT NULL,
  PRIMARY KEY (`id`),
  KEY `ix_amount` (`amount`),
  CONSTRAINT `orders_amount_check` CHECK ((`amount` > 0))
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_0900_ai_ci;
";
    for swap in [
        "ALTER TABLE orders CHANGE amount note INT NOT NULL, CHANGE note amount VARCHAR(50) NULL;",
        "ALTER TABLE orders RENAME COLUMN amount TO note, RENAME COLUMN note TO amount;",
    ] {
        let run = run_on_dump(
            dump,
            &format!("{swap}\nALTER TABLE orders RENAME COLUMN amount TO total;"),
        );
        assert_not_a_measured_answer(&run, 0, swap);
        assert_eq!(run.last_answer(), "fails with error 3959", "{swap}");
    }
}

#[test]
fn a_swap_of_a_column_a_generated_column_uses_is_never_followed() {
    // By hand: RENAME COLUMN a TO c, RENAME COLUMN b TO a with g AS (a + 1)
    // is error 3108. The table is as it was: renaming a is still refused (or
    // unknown), never a measured INSTANT as if a were a free column.
    let dump = "CREATE TABLE `o` (
  `id` int NOT NULL,
  `a` int DEFAULT NULL,
  `b` int DEFAULT NULL,
  `g` int GENERATED ALWAYS AS ((`a` + 1)) VIRTUAL,
  PRIMARY KEY (`id`),
  KEY `ix_a` (`a`)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_0900_ai_ci;
";
    let run = run_on_dump(
        dump,
        "ALTER TABLE o RENAME COLUMN a TO c, RENAME COLUMN b TO a;
         ALTER TABLE o DROP COLUMN a;
         ALTER TABLE o RENAME COLUMN a TO c;",
    );
    assert_not_a_measured_answer(&run, 0, "the swap");
    assert_ne!(run.answers()[2], INSTANT);
    assert_not_a_measured_answer(&run, 2, "rename of the generated column's source");
    assert_not_a_measured_answer(&run, 1, "drop of the generated column's source");
}

#[test]
fn clauses_are_resolved_against_the_names_the_statement_leaves_so_these_are_unknown_and_not_followed()
 {
    // By hand: each is refused (1072, 1072, 1054, 1054), so the table is
    // unchanged and ADD COLUMN of the column that the "followed" reading
    // would have removed or renamed is not a free name.
    for (statement, column) in [
        (
            "ALTER TABLE orders ADD INDEX ix (note), DROP COLUMN note",
            "note",
        ),
        (
            "ALTER TABLE orders CHANGE user_id uid BIGINT UNSIGNED NOT NULL, \
             ADD CONSTRAINT fk_u FOREIGN KEY (user_id) REFERENCES users (id)",
            "user_id",
        ),
        (
            "ALTER TABLE orders DROP COLUMN note, ADD CONSTRAINT ck CHECK (note <> '')",
            "note",
        ),
        (
            "ALTER TABLE orders ADD COLUMN x INT AFTER note, DROP COLUMN note",
            "note",
        ),
    ] {
        let run = run_on_dump(
            USERS_AND_ORDERS_WITH_FK_DUMP,
            &format!("{statement};\nALTER TABLE orders ADD COLUMN {column} INT NULL;"),
        );
        assert_eq!(run.answers()[0], "unknown", "{statement}");
        assert_ne!(run.answers()[1], INSTANT, "{statement}");
        assert_not_a_measured_answer(&run, 1, statement);
    }
}

#[test]
fn a_rename_beside_a_column_placed_after_the_new_name_is_accepted_and_not_refused_with_1054() {
    // By hand: RENAME COLUMN note TO memo, ADD COLUMN y INT AFTER memo is
    // accepted on both servers.
    let run = run_on_dump(
        USERS_AND_ORDERS_WITH_FK_DUMP,
        "ALTER TABLE orders RENAME COLUMN note TO memo, ADD COLUMN y INT AFTER memo;",
    );
    assert_no_reason_names(&run, "1054", "rename and AFTER the new name");
}

#[test]
fn a_create_table_after_a_rename_away_and_back_is_unknown_naming_1050_and_the_alter_is_on_the_live_table()
 {
    // The dump has orders. It is renamed away and back, so the name is the
    // dump's table again; CREATE TABLE orders is error 1050; the ALTER is the
    // measured copy of the live table.
    let run = run_on_dump(
        USERS_AND_ORDERS_DUMP,
        "RENAME TABLE orders TO orders_tmp;
         RENAME TABLE orders_tmp TO orders;
         CREATE TABLE orders (id BIGINT UNSIGNED NOT NULL PRIMARY KEY, amount INT NOT NULL) DEFAULT CHARSET=utf8mb4;
         ALTER TABLE orders MODIFY amount BIGINT NOT NULL;",
    );
    assert_eq!(run.answers()[2], "unknown");
    assert!(run.reason(2).contains("1050"), "{}", run.reason(2));
    assert_eq!(run.last_answer(), COPY);
    assert_eq!(run.last_impact(), Impact::WritesWait);
}

#[test]
fn a_create_table_if_not_exists_like_of_a_name_the_dump_lacks_is_not_a_new_table() {
    // Like the plain CREATE TABLE IF NOT EXISTS: MySQL does nothing if the
    // table is there, so the ALTER may be on a live table.
    let plain = run_on_dump(
        USERS_AND_ORDERS_DUMP,
        "CREATE TABLE IF NOT EXISTS x1 (id BIGINT UNSIGNED NOT NULL PRIMARY KEY, amount INT NOT NULL) DEFAULT CHARSET=utf8mb4;
         ALTER TABLE x1 MODIFY amount BIGINT NOT NULL;",
    );
    let like = run_on_dump(
        USERS_AND_ORDERS_DUMP,
        "CREATE TABLE IF NOT EXISTS x2 LIKE orders;
         ALTER TABLE x2 MODIFY amount BIGINT NOT NULL;",
    );
    assert_eq!(like.last_answer(), plain.last_answer());
    assert_eq!(like.last_impact(), plain.last_impact());
    assert_ne!(like.last_impact(), Impact::NewTable);
}

#[test]
fn dropping_a_temporary_table_made_with_like_of_an_unknown_source_does_not_say_1051() {
    // CREATE TEMPORARY TABLE scratch LIKE missing_src may fail or not, but a
    // DROP TEMPORARY TABLE scratch after it is not error 1051 on kilit's say.
    for dump in [Some(USERS_AND_ORDERS_DUMP), None] {
        let run = run_files_on_both_servers(
            dump,
            &["CREATE TEMPORARY TABLE scratch LIKE missing_src;\nDROP TEMPORARY TABLE scratch;"],
        );
        assert_no_reason_names(&run, "1051", "drop of a temporary copy");
    }
}

#[test]
fn an_index_named_primary_is_unknown_naming_1280_and_the_table_stays() {
    // By hand: ADD INDEX `PRIMARY` (amount) is error 1280 on both servers.
    let run = run_on_dump(
        USERS_AND_ORDERS_DUMP,
        "ALTER TABLE orders ADD INDEX `PRIMARY` (amount);
         ALTER TABLE orders ADD COLUMN probe_col INT NULL;",
    );
    assert_eq!(run.answers()[0], "unknown");
    assert!(run.reason(0).contains("1280"), "{}", run.reason(0));
    assert_eq!(run.answers()[1], INSTANT);
}

#[test]
fn serial_in_a_new_column_is_not_followed_and_the_table_is_lost() {
    // No hand test shows what ADD COLUMN s SERIAL adds, so a later statement
    // on the table is unknown, never measured.
    let run = run_on_dump(
        USERS_AND_ORDERS_DUMP,
        "ALTER TABLE orders ADD COLUMN s SERIAL;
         ALTER TABLE orders ADD COLUMN probe_col INT NULL;",
    );
    assert_not_a_measured_answer(&run, 0, "ADD COLUMN s SERIAL");
    assert_eq!(run.last_answer(), "unknown");
    assert_eq!(run.last_impact(), Impact::Unknown);
}

/// `orders` with CHECK ck2 on amount and note, CHECK orders_amount_check on
/// amount and a generated column g that uses extra.
const CHECKS_AND_GENERATED_DUMP: &str = "CREATE TABLE `users` (
  `id` bigint unsigned NOT NULL AUTO_INCREMENT,
  PRIMARY KEY (`id`)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_0900_ai_ci;
CREATE TABLE `orders` (
  `id` bigint unsigned NOT NULL AUTO_INCREMENT,
  `user_id` bigint unsigned NOT NULL,
  `amount` int NOT NULL,
  `note` varchar(50) DEFAULT NULL,
  `extra` int DEFAULT NULL,
  `g` int GENERATED ALWAYS AS ((`extra` + 1)) VIRTUAL,
  PRIMARY KEY (`id`),
  KEY `ix_amount` (`amount`),
  KEY `orders_user_id_foreign` (`user_id`),
  CONSTRAINT `orders_user_id_foreign` FOREIGN KEY (`user_id`) REFERENCES `users` (`id`),
  CONSTRAINT `ck2` CHECK ((`amount` < `note`)),
  CONSTRAINT `orders_amount_check` CHECK ((`amount` > 0))
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_0900_ai_ci;
";

#[test]
fn dropping_or_renaming_a_column_a_check_uses_beside_another_clause_is_refused_and_the_table_stays()
{
    // By hand on MySQL 8.0.46 and 8.4.13: each statement is error 3959 (a
    // CHECK uses the column) and changes nothing, so adding the same name
    // afterwards is error 1060, never a measured answer.
    for (statement, column) in [
        (
            "ALTER TABLE orders DROP COLUMN note, ADD COLUMN y INT NULL",
            "note",
        ),
        (
            "ALTER TABLE orders RENAME COLUMN amount TO total, ADD COLUMN y3 INT NULL",
            "amount",
        ),
        (
            "ALTER TABLE orders CHANGE amount total INT NOT NULL, ADD COLUMN y INT NULL",
            "amount",
        ),
    ] {
        let run = run_on_dump(
            CHECKS_AND_GENERATED_DUMP,
            &format!("{statement};\nALTER TABLE orders ADD COLUMN {column} INT NULL;"),
        );
        assert_not_a_measured_answer(&run, 0, statement);
        assert_eq!(run.answers()[1], "unknown", "{statement}");
        assert!(
            run.reason(1).contains("1060"),
            "{statement}: {}",
            run.reason(1)
        );
    }
}

#[test]
fn dropping_or_renaming_the_source_of_a_generated_column_beside_another_clause_is_never_followed() {
    // By hand: error 3108 and nothing changes. The later ADD COLUMN extra is
    // never a measured runs answer.
    for statement in [
        "ALTER TABLE orders DROP COLUMN extra, ADD COLUMN y2 INT NULL",
        "ALTER TABLE orders RENAME COLUMN extra TO e2, ADD COLUMN y INT NULL",
    ] {
        let run = run_on_dump(
            CHECKS_AND_GENERATED_DUMP,
            &format!("{statement};\nALTER TABLE orders ADD COLUMN extra INT NULL;"),
        );
        assert_not_a_measured_answer(&run, 0, statement);
        assert_not_a_measured_answer(&run, 1, statement);
        assert_ne!(run.answers()[1], INSTANT, "{statement}");
    }
}

#[test]
fn dropping_and_adding_back_a_column_a_check_uses_is_never_followed() {
    // By hand: DROP COLUMN amount, ADD COLUMN amount INT NULL with a CHECK
    // on amount is refused (3959); the table is as it was.
    let run = run_on_dump(
        CHECKS_AND_GENERATED_DUMP,
        "ALTER TABLE orders DROP COLUMN amount, ADD COLUMN amount INT NULL;
         ALTER TABLE orders ADD COLUMN amount INT NULL;",
    );
    assert_not_a_measured_answer(&run, 0, "drop and add of amount");
    assert_not_a_measured_answer(&run, 1, "a later ADD COLUMN amount");
}

const TWO_COLUMN_CHECK_DUMP: &str = "CREATE TABLE `orders` (
  `id` int NOT NULL,
  `amount` int NOT NULL,
  `note` varchar(10) DEFAULT NULL,
  `extra` int DEFAULT NULL,
  `g` int GENERATED ALWAYS AS ((`extra` + 1)) VIRTUAL,
  PRIMARY KEY (`id`),
  CONSTRAINT `ck2` CHECK (((`amount` > 0) or (`note` is null)))
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_0900_ai_ci;
";

#[test]
fn a_check_the_same_alter_drops_does_not_refuse_the_drop_or_rename_of_its_columns() {
    // By hand on MySQL 8.0.46 and 8.4.13: DROP CONSTRAINT ck2, DROP COLUMN
    // note and DROP CONSTRAINT ck2, RENAME COLUMN amount TO total are
    // accepted. Afterwards the other column is free: DROP COLUMN runs, so it
    // is not the 3959 of a CHECK that is gone, and nothing fails or waits.
    for (first, later) in [
        (
            "ALTER TABLE orders DROP CONSTRAINT ck2, DROP COLUMN note;",
            "ALTER TABLE orders DROP COLUMN amount;",
        ),
        (
            "ALTER TABLE orders DROP CONSTRAINT ck2, RENAME COLUMN amount TO total;",
            "ALTER TABLE orders DROP COLUMN note;",
        ),
        // DROP CHECK stays unreadable beside another clause: only never a
        // measured failure.
        (
            "ALTER TABLE orders DROP CHECK ck2, RENAME COLUMN amount TO total;",
            "ALTER TABLE orders DROP COLUMN note;",
        ),
    ] {
        let run = run_on_dump(TWO_COLUMN_CHECK_DUMP, &format!("{first}\n{later}"));
        for at in 0..2 {
            assert_ne!(run.impacts()[at], Impact::Fails, "{first}: {at}");
            assert_ne!(run.answers()[at], "fails with error 3959", "{first}: {at}");
        }
        if first.contains("DROP CONSTRAINT") {
            assert_ne!(run.impacts()[1], Impact::WritesWait, "{first}");
            assert_ne!(run.impacts()[0], Impact::WritesWait, "{first}");
        }
    }
}

#[test]
fn a_check_on_one_column_goes_with_its_column_beside_other_drops_and_the_table_stays_known() {
    // By hand: ALTER TABLE bots DROP info, DROP seedbonus, DROP invites with
    // CHECK (seedbonus >= 0) is accepted. The next statement is measured.
    let dump = "CREATE TABLE `bots` (
  `id` int NOT NULL,
  `info` varchar(10) DEFAULT NULL,
  `seedbonus` decimal(12,2) NOT NULL DEFAULT '0.00',
  `invites` int NOT NULL DEFAULT '0',
  PRIMARY KEY (`id`),
  CONSTRAINT `check_bots_seedbonus` CHECK ((`seedbonus` >= 0))
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_0900_ai_ci;
";
    let run = run_on_dump(
        dump,
        "ALTER TABLE bots DROP info, DROP seedbonus, DROP invites;
         ALTER TABLE bots ADD COLUMN probe_col INT NULL;
         ALTER TABLE bots ADD COLUMN seedbonus INT NULL;",
    );
    assert_ne!(run.answers()[0], "fails with error 3959");
    assert_ne!(run.impacts()[0], Impact::Fails);
    assert_eq!(run.answers()[1], INSTANT);
    // The column is gone, so its name is free.
    assert_eq!(run.answers()[2], INSTANT);
}

#[test]
fn dropping_every_column_of_a_two_column_check_in_one_alter_is_refused_with_3959() {
    // By hand: CHECK (a < b) with DROP a, DROP b in one ALTER is error 3959
    // on both servers and the table is unchanged.
    let dump = "CREATE TABLE `b2` (
  `id` int NOT NULL,
  `a` int DEFAULT NULL,
  `b` int DEFAULT NULL,
  PRIMARY KEY (`id`),
  CONSTRAINT `ckab` CHECK ((`a` < `b`))
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_0900_ai_ci;
";
    let run = run_on_dump(
        dump,
        "ALTER TABLE b2 DROP a, DROP b;\nALTER TABLE b2 ADD COLUMN a INT NULL;",
    );
    assert_not_a_measured_answer(&run, 0, "drop a and b");
    assert_eq!(run.answers()[1], "unknown");
    assert!(run.reason(1).contains("1060"), "{}", run.reason(1));
}
