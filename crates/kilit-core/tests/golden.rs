//! kilit's answers against the lab's own measurements.
//!
//! The built-in result files are the evidence. These tests make sure that
//! kilit uses every measurement, places every measured statement in the
//! right case, and gives back exactly what the lab measured, on the lab's
//! own tables.

use std::fs;
use std::path::Path;

use kilit_core::{
    CASES, Condition, Knowledge, Prediction, ScenarioResult, Schema, Variant, Verdict, check, parse,
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
    let results = Knowledge::built_in().server("mysql-8.4.13").unwrap();
    check(&parse(sql), &Schema::from_sql(schema), results)
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
fn dropping_a_foreign_key_column_is_not_answered_yet() {
    // $table->dropColumn('user_id'); without dropForeign first
    assert_eq!(
        on_mysql_84(LARAVEL_TABLES, "alter table `orders` drop `user_id`"),
        ["unknown"]
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
