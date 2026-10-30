//! Decides what MySQL will do with each statement of a migration, from
//! what the lab measured.
//!
//! Every change belongs to a [`Case`]: a kind of change together with the
//! lab scenarios that measured it. kilit answers only from those
//! measurements. When it cannot place a statement in a case, it says
//! "unknown" and why; it never guesses that something is safe.

use std::fmt::Write as _;

use crate::knowledge::ServerResults;
use crate::results::{Variant, Verdict};
use crate::statement::{
    Alter, Column, IndexKind, Lock, Operation, Statement, StatementKind, Variable,
};

/// What kilit says about one statement of the input.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Finding {
    pub line: usize,
    pub sql: String,
    pub prediction: Prediction,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Prediction {
    /// The lab measured this kind of change on the target server.
    Measured {
        /// The kind of change, e.g. `add-column`.
        case: &'static str,
        /// What the server does with the statement as written.
        verdict: Verdict,
        /// The lab scenarios behind the verdict. They all agree.
        evidence: Vec<&'static str>,
        /// What changes the answer and kilit cannot check yet, each measured
        /// by its own scenario, plus assumptions kilit had to make.
        notes: Vec<String>,
    },
    /// kilit cannot tell, and says why. Never treat this as safe.
    Unknown { reason: String },
    /// Not something kilit checks: a new table, a data change, a setting.
    NotChecked { reason: String },
}

/// A kind of change, with the lab scenarios that measured it.
#[derive(Debug)]
pub struct Case {
    pub id: &'static str,
    /// Scenarios that measure this change. They must give the same result
    /// on every server, or kilit refuses to answer; a test checks it.
    pub scenarios: &'static [&'static str],
    /// The same change measured on a table or on data where the answer is
    /// different. kilit cannot see the table or the data yet, so it reports
    /// these as notes.
    pub exceptions: &'static [Exception],
}

#[derive(Debug)]
pub struct Exception {
    pub scenario: &'static str,
    /// When this scenario's result applies, e.g. "if the table has a FULLTEXT index".
    pub when: &'static str,
}

const ADD_COLUMN: Case = Case {
    id: "add-column",
    scenarios: &[
        "add-nullable-column",
        "add-column-after",
        "add-column-first",
        "add-column-not-null",
        "add-column-not-null-default",
        "add-column-options",
        "add-column-current-timestamp",
    ],
    exceptions: &[
        Exception {
            scenario: "add-column-with-fulltext",
            when: "if the table has a FULLTEXT index",
        },
        Exception {
            scenario: "add-column-after-64-changes",
            when: "if the table has had 64 instant column changes since it was last rebuilt",
        },
    ],
};

const DROP_COLUMN: Case = Case {
    id: "drop-column",
    scenarios: &["drop-column"],
    exceptions: &[Exception {
        scenario: "drop-column-with-fulltext",
        when: "if the table has a FULLTEXT index",
    }],
};

const RENAME_COLUMN: Case = Case {
    id: "rename-column",
    scenarios: &["rename-column"],
    exceptions: &[],
};

const SET_COLUMN_DEFAULT: Case = Case {
    id: "set-column-default",
    scenarios: &["set-column-default"],
    exceptions: &[],
};

const ADD_INDEX: Case = Case {
    id: "add-index",
    scenarios: &["add-index"],
    exceptions: &[],
};

const ADD_UNIQUE_INDEX: Case = Case {
    id: "add-unique-index",
    scenarios: &["add-unique-index"],
    exceptions: &[Exception {
        scenario: "add-unique-index-duplicates",
        when: "if the columns hold duplicate values",
    }],
};

const ADD_FULLTEXT_INDEX: Case = Case {
    id: "add-fulltext-index",
    scenarios: &["add-fulltext-index"],
    exceptions: &[],
};

const DROP_INDEX: Case = Case {
    id: "drop-index",
    scenarios: &["drop-index"],
    exceptions: &[],
};

const ADD_FOREIGN_KEY: Case = Case {
    id: "add-foreign-key",
    scenarios: &["add-foreign-key"],
    exceptions: &[Exception {
        scenario: "add-foreign-key-orphans",
        when: "if a row points to a parent row that does not exist",
    }],
};

const ADD_FOREIGN_KEY_CHECKS_OFF: Case = Case {
    id: "add-foreign-key-checks-off",
    scenarios: &["add-foreign-key-checks-off"],
    exceptions: &[Exception {
        scenario: "add-foreign-key-checks-off-orphans",
        when: "if a row points to a parent row that does not exist (the row is kept unchecked)",
    }],
};

const DROP_FOREIGN_KEY: Case = Case {
    id: "drop-foreign-key",
    scenarios: &["drop-foreign-key"],
    exceptions: &[],
};

/// Every case kilit knows.
pub const CASES: [&Case; 11] = [
    &ADD_COLUMN,
    &DROP_COLUMN,
    &RENAME_COLUMN,
    &SET_COLUMN_DEFAULT,
    &ADD_INDEX,
    &ADD_UNIQUE_INDEX,
    &ADD_FULLTEXT_INDEX,
    &DROP_INDEX,
    &ADD_FOREIGN_KEY,
    &ADD_FOREIGN_KEY_CHECKS_OFF,
    &DROP_FOREIGN_KEY,
];

/// Checks every statement, in order, against what the lab measured on one
/// server. There is one finding per statement.
pub fn check(statements: &[Statement], results: &ServerResults) -> Vec<Finding> {
    let mut session = Session::default();
    let mut findings = Vec::with_capacity(statements.len());
    for statement in statements {
        let prediction = match &statement.kind {
            StatementKind::Alter(alter) => predict(alter, &session, results),
            StatementKind::Set { variables } => {
                session.apply(variables);
                not_checked(format!(
                    "sets {} for the statements after it",
                    variables
                        .iter()
                        .map(|variable| format!("{} = {}", variable.name, variable.value))
                        .collect::<Vec<_>>()
                        .join(", ")
                ))
            }
            StatementKind::CreateTable { .. } => {
                not_checked("creates a new table; no existing rows are touched")
            }
            StatementKind::DropTable { .. } => not_checked(
                "drops tables with all their rows; kilit checks locks and copies, not lost data",
            ),
            StatementKind::RenameTable { .. } => {
                unknown("renaming tables has not been measured yet")
            }
            StatementKind::Other => not_checked("not a schema change"),
            StatementKind::Unparsed { error } => {
                unknown(format!("kilit could not read this statement: {error}"))
            }
        };
        findings.push(Finding {
            line: statement.line,
            sql: statement.sql.clone(),
            prediction,
        });
    }
    findings
}

fn not_checked(reason: impl Into<String>) -> Prediction {
    Prediction::NotChecked {
        reason: reason.into(),
    }
}

fn unknown(reason: impl Into<String>) -> Prediction {
    Prediction::Unknown {
        reason: reason.into(),
    }
}

/// Session settings that change how an ALTER runs.
#[derive(Default)]
struct Session {
    foreign_key_checks: Setting,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum Setting {
    /// Nothing in the input set it: the server's value is used.
    #[default]
    ServerDefault,
    On,
    Off,
    /// Set to something kilit does not understand.
    Unclear,
}

impl Session {
    fn apply(&mut self, variables: &[Variable]) {
        for variable in variables {
            if variable.name == "foreign_key_checks" {
                self.foreign_key_checks = Setting::parse(&variable.value);
            }
        }
    }
}

impl Setting {
    fn parse(value: &str) -> Setting {
        match value.trim_matches('\'').to_ascii_uppercase().as_str() {
            "1" | "ON" | "TRUE" => Setting::On,
            "0" | "OFF" | "FALSE" => Setting::Off,
            "DEFAULT" => Setting::ServerDefault,
            _ => Setting::Unclear,
        }
    }
}

fn predict(alter: &Alter, session: &Session, results: &ServerResults) -> Prediction {
    let operation = match alter.operations.as_slice() {
        [operation] => operation,
        [] => return unknown("the statement has no change kilit understands"),
        many => {
            return unknown(format!(
                "{} changes in one statement. kilit has measured single changes only, and \
                 MySQL picks one algorithm for the whole statement, which can be heavier \
                 than each change alone.",
                many.len()
            ));
        }
    };
    let (case, mut notes) = match classify(operation, session, results) {
        Ok(classified) => classified,
        Err(reason) => return unknown(reason),
    };
    let Some(variant) = variant_of(alter) else {
        return unknown(format!(
            "the statement asks for {}, which the lab has not measured",
            clauses(alter)
        ));
    };

    // All of the case's scenarios must agree on this server.
    let mut verdict: Option<Verdict> = None;
    for id in case.scenarios {
        let Some(scenario) = results.scenario(id) else {
            return unknown(format!(
                "the lab has no result for {id} on {}",
                results.server.name()
            ));
        };
        if matches!(
            scenario.verdict,
            Verdict::Runs {
                consistent: false,
                ..
            }
        ) {
            return unknown(format!(
                "the lab saw something unexpected in {id}; run kilit-lab with --details"
            ));
        }
        let Some(this) = scenario.verdict_for(variant) else {
            return unknown(format!("the lab did not run {id} as {variant}"));
        };
        match &verdict {
            None => verdict = Some(this),
            Some(first) if *first == this => {}
            Some(_) => {
                return unknown(format!(
                    "the lab's scenarios for {} disagree on {}",
                    case.id,
                    results.server.name()
                ));
            }
        }
    }
    let Some(verdict) = verdict else {
        return unknown(format!("{} has no lab scenarios", case.id));
    };

    for exception in case.exceptions {
        let Some(other) = results
            .scenario(exception.scenario)
            .and_then(|scenario| scenario.verdict_for(variant))
        else {
            continue;
        };
        if other == verdict {
            notes.push(format!(
                "{}: same result ({})",
                exception.when, exception.scenario
            ));
        } else {
            notes.push(format!(
                "{}: {} ({})",
                exception.when,
                brief(&other),
                exception.scenario
            ));
        }
    }

    Prediction::Measured {
        case: case.id,
        verdict,
        evidence: case.scenarios.to_vec(),
        notes,
    }
}

/// Finds the case an operation belongs to, with any assumption made on
/// the way, or says why it has none.
fn classify(
    operation: &Operation,
    session: &Session,
    results: &ServerResults,
) -> Result<(&'static Case, Vec<String>), String> {
    let mut notes = Vec::new();
    let case = match operation {
        Operation::AddColumn { column, .. } => {
            check_new_column(column)?;
            &ADD_COLUMN
        }
        Operation::DropColumn { .. } => &DROP_COLUMN,
        Operation::RenameColumn { .. } => &RENAME_COLUMN,
        Operation::SetColumnDefault { .. } => &SET_COLUMN_DEFAULT,
        Operation::AddIndex(index) => match index.kind {
            IndexKind::Plain => &ADD_INDEX,
            IndexKind::Unique => &ADD_UNIQUE_INDEX,
            IndexKind::Fulltext => &ADD_FULLTEXT_INDEX,
            IndexKind::Spatial => return Err("spatial indexes have not been measured yet".into()),
        },
        Operation::DropIndex { .. } => &DROP_INDEX,
        Operation::AddForeignKey(_) => match session.foreign_key_checks {
            Setting::On => &ADD_FOREIGN_KEY,
            Setting::Off => &ADD_FOREIGN_KEY_CHECKS_OFF,
            Setting::ServerDefault => {
                notes.push(
                    "assumes foreign_key_checks is ON, the MySQL default: nothing before \
                     this statement sets it"
                        .to_owned(),
                );
                &ADD_FOREIGN_KEY
            }
            Setting::Unclear => {
                return Err(
                    "foreign_key_checks is set to a value kilit does not understand".into(),
                );
            }
        },
        Operation::DropForeignKey { .. } => &DROP_FOREIGN_KEY,
        Operation::ModifyColumn { .. } | Operation::ChangeColumn { .. } => {
            return Err(modify_hint(results));
        }
        Operation::AddPrimaryKey { .. } | Operation::DropPrimaryKey => {
            return Err("changing the primary key has not been measured yet".into());
        }
        Operation::RenameTable { .. } => {
            return Err("renaming tables has not been measured yet".into());
        }
        Operation::Other { sql } => {
            return Err(format!("kilit does not understand this change: {sql}"));
        }
    };
    Ok((case, notes))
}

/// The lab measured plain new columns: with or without NULL, a default, a
/// position, a character set, a comment, or a CURRENT_TIMESTAMP default.
fn check_new_column(column: &Column) -> Result<(), String> {
    if column.auto_increment {
        return Err("adding an AUTO_INCREMENT column has not been measured yet".into());
    }
    if column.generated.is_some() {
        return Err("adding a generated column has not been measured yet".into());
    }
    if !column.other_options.is_empty() {
        return Err(format!(
            "the new column has options kilit does not understand yet: {}",
            column.other_options.join(" ")
        ));
    }
    Ok(())
}

/// For MODIFY and CHANGE kilit needs the column's current definition,
/// which it does not read yet. It can still say what the lab measured.
fn modify_hint(results: &ServerResults) -> String {
    let mut hint = format!(
        "MODIFY and CHANGE depend on the column's current definition, which kilit does \
         not read yet. Measured on {}:",
        results.server.name()
    );
    let measured = [
        ("int-to-bigint", "a type change such as INT to BIGINT"),
        (
            "varchar-50-to-60",
            "a longer VARCHAR that stays within 255 bytes",
        ),
        (
            "varchar-50-to-100",
            "a longer VARCHAR that goes past 255 bytes",
        ),
        ("not-null-no-nulls", "NULL to NOT NULL"),
        ("not-null-with-nulls", "NULL to NOT NULL when a row is NULL"),
    ];
    for (id, change) in measured {
        if let Some(scenario) = results.scenario(id) {
            let _ = write!(hint, "\n- {change}: {} ({id})", brief(&scenario.verdict));
        }
    }
    hint
}

/// The variant the lab ran that matches the statement's clauses, if any.
fn variant_of(alter: &Alter) -> Option<Variant> {
    let lock_none = match alter.lock {
        None => false,
        Some(Lock::None) => true,
        Some(Lock::Shared | Lock::Exclusive) => return None,
    };
    let variant = Variant {
        algorithm: alter.algorithm,
        lock_none,
    };
    Variant::ALL.contains(&variant).then_some(variant)
}

/// A verdict for a note. An error message from the lab names the lab's
/// own tables and keys, which would only confuse next to the user's
/// statement; the note's condition already says why it fails.
fn brief(verdict: &Verdict) -> String {
    match verdict {
        Verdict::Fails { code, .. } => format!("fails with error {code}"),
        runs => runs.to_string(),
    }
}

/// `ALGORITHM=INSTANT, LOCK=NONE`
fn clauses(alter: &Alter) -> String {
    let mut clauses = Vec::new();
    if let Some(algorithm) = alter.algorithm {
        clauses.push(format!("ALGORITHM={algorithm}"));
    }
    if let Some(lock) = alter.lock {
        clauses.push(format!("LOCK={lock}"));
    }
    clauses.join(", ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::knowledge::Knowledge;
    use crate::parse::parse;
    use crate::results::Writes;
    use crate::statement::Algorithm;

    fn results() -> &'static ServerResults {
        Knowledge::built_in().server("mysql-8.4.13").unwrap()
    }

    /// The prediction for each statement of `sql` on MySQL 8.4.13.
    fn predictions(sql: &str) -> Vec<Prediction> {
        check(&parse(sql), results())
            .into_iter()
            .map(|finding| finding.prediction)
            .collect()
    }

    fn only(sql: &str) -> Prediction {
        let mut predictions = predictions(sql);
        assert_eq!(predictions.len(), 1, "{sql}");
        predictions.remove(0)
    }

    fn reason(prediction: Prediction) -> String {
        match prediction {
            Prediction::Unknown { reason } => reason,
            other => panic!("expected unknown, got {other:?}"),
        }
    }

    #[test]
    fn reads_foreign_key_checks_values() {
        for (value, setting) in [
            ("0", Setting::Off),
            ("OFF", Setting::Off),
            ("off", Setting::Off),
            ("'OFF'", Setting::Off),
            ("1", Setting::On),
            ("ON", Setting::On),
            ("DEFAULT", Setting::ServerDefault),
            ("2", Setting::Unclear),
        ] {
            assert_eq!(Setting::parse(value), setting, "{value}");
        }
    }

    #[test]
    fn a_new_column_is_instant() {
        let Prediction::Measured {
            case,
            verdict,
            evidence,
            notes,
        } = only("ALTER TABLE orders ADD coupon VARCHAR(255) NULL AFTER amount")
        else {
            panic!("expected a measured prediction");
        };
        assert_eq!(case, "add-column");
        assert_eq!(verdict.to_string(), "INSTANT, no rebuild, writes continue");
        assert_eq!(evidence.len(), ADD_COLUMN.scenarios.len());
        // Both exceptions differ from the answer, so both are reported.
        assert_eq!(notes.len(), 2);
        assert!(notes[0].starts_with("if the table has a FULLTEXT index: COPY"));
    }

    #[test]
    fn foreign_key_checks_follow_the_session() {
        let predictions = predictions(
            "ALTER TABLE orders ADD CONSTRAINT fk FOREIGN KEY (user_id) REFERENCES users (id);
             SET FOREIGN_KEY_CHECKS=0;
             ALTER TABLE orders ADD CONSTRAINT fk FOREIGN KEY (user_id) REFERENCES users (id);
             SET FOREIGN_KEY_CHECKS=1;
             ALTER TABLE orders ADD CONSTRAINT fk FOREIGN KEY (user_id) REFERENCES users (id);",
        );
        let cases: Vec<&str> = predictions
            .iter()
            .map(|prediction| match prediction {
                Prediction::Measured { case, .. } => case,
                Prediction::NotChecked { .. } => "not checked",
                Prediction::Unknown { .. } => "unknown",
            })
            .collect();
        assert_eq!(
            cases,
            [
                "add-foreign-key",
                "not checked",
                "add-foreign-key-checks-off",
                "not checked",
                "add-foreign-key"
            ]
        );
        // Only the first one had to assume the server's default.
        let assumes = |prediction: &Prediction| {
            matches!(prediction, Prediction::Measured { notes, .. }
                if notes.iter().any(|note| note.starts_with("assumes foreign_key_checks")))
        };
        assert!(assumes(&predictions[0]));
        assert!(!assumes(&predictions[4]));
    }

    #[test]
    fn an_unclear_setting_makes_foreign_keys_unknown() {
        // mysqldump turns the checks off, then restores the saved value.
        let mut predictions = predictions(
            "SET @OLD_FOREIGN_KEY_CHECKS=@@FOREIGN_KEY_CHECKS, FOREIGN_KEY_CHECKS=0;
             ALTER TABLE orders ADD CONSTRAINT fk FOREIGN KEY (user_id) REFERENCES users (id);
             SET FOREIGN_KEY_CHECKS=@OLD_FOREIGN_KEY_CHECKS;
             ALTER TABLE orders ADD CONSTRAINT fk FOREIGN KEY (user_id) REFERENCES users (id);",
        );
        assert!(matches!(
            predictions[1],
            Prediction::Measured {
                case: "add-foreign-key-checks-off",
                ..
            }
        ));
        assert!(reason(predictions.remove(3)).contains("does not understand"));
    }

    #[test]
    fn forced_variants_use_the_lab_run_for_that_variant() {
        // Forcing INPLACE on a new column: accepted, but the table is rebuilt.
        let Prediction::Measured { verdict, .. } =
            only("ALTER TABLE orders ADD coupon VARCHAR(255) NULL, ALGORITHM=INPLACE")
        else {
            panic!("expected a measured prediction");
        };
        assert_eq!(
            verdict,
            Verdict::Runs {
                algorithm: Some(Algorithm::Inplace),
                rebuilds_table: true,
                copied_rows: Some(0),
                writes: Writes::Allowed,
                consistent: true,
            }
        );
    }

    #[test]
    fn unmeasured_clauses_are_unknown() {
        for sql in [
            "ALTER TABLE orders ADD coupon INT NULL, ALGORITHM=INSTANT, LOCK=NONE",
            "ALTER TABLE orders ADD coupon INT NULL, LOCK=SHARED",
        ] {
            assert!(reason(only(sql)).contains("has not measured"), "{sql}");
        }
    }

    #[test]
    fn several_changes_in_one_statement_are_unknown() {
        assert!(reason(only("ALTER TABLE orders DROP a, DROP b")).starts_with("2 changes"));
    }

    #[test]
    fn modify_explains_what_was_measured() {
        let reason = reason(only("ALTER TABLE orders MODIFY amount BIGINT NOT NULL"));
        assert!(reason.contains("current definition"));
        assert!(reason.contains("COPY, copies every row, writes wait (int-to-bigint)"));
    }

    #[test]
    fn new_columns_kilit_has_not_measured_are_unknown() {
        for (sql, expected) in [
            (
                "ALTER TABLE orders ADD id2 INT NOT NULL AUTO_INCREMENT",
                "AUTO_INCREMENT",
            ),
            ("ALTER TABLE orders ADD g INT AS (amount + 1)", "generated"),
            ("ALTER TABLE orders ADD c INT UNIQUE", "UNIQUE"),
        ] {
            assert!(reason(only(sql)).contains(expected), "{sql}");
        }
    }

    #[test]
    fn statements_that_are_not_schema_changes_are_not_checked() {
        for sql in [
            "CREATE TABLE shops (id INT)",
            "DROP TABLE shops",
            "INSERT INTO shops VALUES (1)",
            "SET foreign_key_checks = 0",
        ] {
            assert!(matches!(only(sql), Prediction::NotChecked { .. }), "{sql}");
        }
    }

    #[test]
    fn unreadable_statements_are_unknown() {
        let reason = reason(only("ALTER TABLE orders CONVERT TO CHARACTER SET utf8mb4"));
        assert!(reason.starts_with("kilit could not read this statement"));
    }

    #[test]
    fn there_is_one_finding_per_statement_with_its_line() {
        let findings = check(
            &parse("SELECT 1;\n\nALTER TABLE orders DROP note;\nFOO BAR;"),
            results(),
        );
        let lines: Vec<usize> = findings.iter().map(|finding| finding.line).collect();
        assert_eq!(lines, [1, 3, 4]);
    }
}
