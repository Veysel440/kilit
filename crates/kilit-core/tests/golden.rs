//! kilit's answers against the lab's own measurements.
//!
//! The built-in result files are the evidence. These tests make sure that
//! kilit uses every measurement, places every measured statement in the
//! right case, and gives back exactly what the lab measured, on the lab's
//! own tables.

use std::fs;
use std::path::Path;

use kilit_core::{
    CASES, Condition, Knowledge, Prediction, ScenarioResult, Schema, Statement, Variant, Verdict,
    check, parse, read_pretend,
};

/// The tables every lab scenario starts from.
const LAB_TABLES: &str = include_str!("../../../scenarios/tables.sql");

/// The lab's tables as a scenario measured them: after its setup.
fn lab_schema(scenario: &ScenarioResult) -> Schema {
    let mut sql = LAB_TABLES.to_owned();
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

#[test]
fn every_scenario_is_used() {
    for results in Knowledge::built_in().servers() {
        for scenario in results.scenarios() {
            let id = scenario.id.as_str();
            let used = CASES.iter().any(|case| {
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
        for case in CASES {
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
        for case in CASES {
            for id in case.scenarios {
                let scenario = results.scenario(id).unwrap();
                let schema = lab_schema(scenario);
                for variant in Variant::ALL {
                    match predict(scenario, variant, &server, &schema) {
                        Prediction::Measured {
                            case: found,
                            verdict,
                            ..
                        } => {
                            assert_eq!(found, case.id, "{id} as {variant} on {server}");
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
