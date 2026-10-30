//! Decides what MySQL will do with each statement of a migration, from
//! what the lab measured.
//!
//! Every change belongs to a [`Case`]: a kind of change together with the
//! lab scenarios that measured it. kilit answers only from those
//! measurements. When it cannot place a statement in a case, it says
//! "unknown" and why; it never guesses that something is safe.
//!
//! kilit follows the tables through the migration (see [`Schema`]), so it
//! can tell what a `MODIFY` changes, or whether a table has a FULLTEXT
//! index. Without the tables it still answers what does not depend on them.

use std::fmt::Write as _;

use crate::knowledge::ServerResults;
use crate::modify::{Change, changes};
use crate::results::{Variant, Verdict};
use crate::schema::{Schema, TableState};
use crate::statement::{
    Alter, Column, DataType, IndexKind, Lock, Operation, Position, Statement, StatementKind, Table,
    Variable,
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
        /// What kilit worked out on the way, such as what a MODIFY changes;
        /// the assumptions it had to make; and measured situations that
        /// change the answer but that kilit cannot see, such as the rows.
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
    /// The same change measured in a situation where the answer differs.
    pub exceptions: &'static [Exception],
}

#[derive(Debug)]
pub struct Exception {
    pub scenario: &'static str,
    /// When this scenario's result applies, e.g. "if the table has a FULLTEXT index".
    pub when: &'static str,
    pub condition: Condition,
}

/// What an exception depends on. When kilit can see it in the tables, the
/// exception's result is the answer, or does not apply at all; when it
/// cannot, the exception becomes a note.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Condition {
    /// The table has a FULLTEXT index.
    FulltextIndex,
    /// The changed column is in an index, the primary key included.
    IndexedColumn,
    /// The changed column is in one of the table's foreign keys.
    ForeignKeyColumn,
    /// Something no schema shows: the rows, or the table's history.
    Hidden,
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
            condition: Condition::FulltextIndex,
        },
        Exception {
            scenario: "add-column-after-64-changes",
            when: "if the table has had 64 instant column changes since it was last rebuilt",
            condition: Condition::Hidden,
        },
    ],
};

const DROP_COLUMN: Case = Case {
    id: "drop-column",
    scenarios: &["drop-column"],
    exceptions: &[Exception {
        scenario: "drop-column-with-fulltext",
        when: "if the table has a FULLTEXT index",
        condition: Condition::FulltextIndex,
    }],
};

const RENAME_COLUMN: Case = Case {
    id: "rename-column",
    scenarios: &["rename-column", "change-column-rename"],
    exceptions: &[],
};

const SET_COLUMN_DEFAULT: Case = Case {
    id: "set-column-default",
    scenarios: &["set-column-default"],
    exceptions: &[],
};

const CHANGE_INTEGER_TYPE: Case = Case {
    id: "change-integer-type",
    scenarios: &["int-to-bigint", "int-to-smallint", "int-to-unsigned"],
    exceptions: &[
        Exception {
            scenario: "modify-foreign-key-column",
            when: "if the column is in a foreign key",
            condition: Condition::ForeignKeyColumn,
        },
        Exception {
            scenario: "int-to-tinyint-out-of-range",
            when: "if a value does not fit the new type (in strict SQL mode, the default)",
            condition: Condition::Hidden,
        },
    ],
};

const LENGTHEN_VARCHAR: Case = Case {
    id: "lengthen-varchar",
    scenarios: &[
        "varchar-50-to-60",
        "varchar-50-to-63",
        "varchar-255-to-300",
        "varchar-latin1-200-to-255",
        "varchar-50-to-60-indexed",
    ],
    exceptions: &[],
};

const LENGTHEN_VARCHAR_PAST_255_BYTES: Case = Case {
    id: "lengthen-varchar-past-255-bytes",
    scenarios: &[
        "varchar-50-to-64",
        "varchar-50-to-100",
        "varchar-latin1-200-to-256",
    ],
    exceptions: &[],
};

const SHORTEN_VARCHAR: Case = Case {
    id: "shorten-varchar",
    scenarios: &["varchar-50-to-40"],
    exceptions: &[Exception {
        scenario: "varchar-50-to-5-too-long",
        when: "if a value is longer than the new length (in strict SQL mode, the default)",
        condition: Condition::Hidden,
    }],
};

const CHANGE_COLUMN_CHARSET: Case = Case {
    id: "change-column-charset",
    scenarios: &["change-column-charset", "modify-latin1-without-charset"],
    exceptions: &[],
};

const CHANGE_COLUMN_COLLATION: Case = Case {
    id: "change-column-collation",
    scenarios: &["change-column-collation"],
    exceptions: &[Exception {
        scenario: "change-column-collation-indexed",
        when: "if the column is in an index",
        condition: Condition::IndexedColumn,
    }],
};

const MAKE_COLUMN_NOT_NULL: Case = Case {
    id: "make-column-not-null",
    scenarios: &["not-null-no-nulls"],
    exceptions: &[Exception {
        scenario: "not-null-with-nulls",
        when: "if a row holds NULL",
        condition: Condition::Hidden,
    }],
};

const MAKE_COLUMN_NULLABLE: Case = Case {
    id: "make-column-nullable",
    scenarios: &["make-column-nullable"],
    exceptions: &[],
};

const MODIFY_COLUMN_DEFAULT: Case = Case {
    id: "modify-column-default",
    scenarios: &["modify-column-default", "modify-drops-default"],
    exceptions: &[],
};

const MOVE_COLUMN: Case = Case {
    id: "move-column",
    scenarios: &["move-column", "move-column-first"],
    exceptions: &[],
};

const MODIFY_SAME_DEFINITION: Case = Case {
    id: "modify-same-definition",
    scenarios: &["modify-same-definition", "modify-same-place"],
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
        condition: Condition::Hidden,
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
        condition: Condition::Hidden,
    }],
};

const ADD_FOREIGN_KEY_CHECKS_OFF: Case = Case {
    id: "add-foreign-key-checks-off",
    scenarios: &["add-foreign-key-checks-off"],
    exceptions: &[Exception {
        scenario: "add-foreign-key-checks-off-orphans",
        when: "if a row points to a parent row that does not exist (the row is kept unchecked)",
        condition: Condition::Hidden,
    }],
};

const DROP_FOREIGN_KEY: Case = Case {
    id: "drop-foreign-key",
    scenarios: &["drop-foreign-key"],
    exceptions: &[],
};

/// Every case kilit knows.
pub const CASES: [&Case; 22] = [
    &ADD_COLUMN,
    &DROP_COLUMN,
    &RENAME_COLUMN,
    &SET_COLUMN_DEFAULT,
    &CHANGE_INTEGER_TYPE,
    &LENGTHEN_VARCHAR,
    &LENGTHEN_VARCHAR_PAST_255_BYTES,
    &SHORTEN_VARCHAR,
    &CHANGE_COLUMN_CHARSET,
    &CHANGE_COLUMN_COLLATION,
    &MAKE_COLUMN_NOT_NULL,
    &MAKE_COLUMN_NULLABLE,
    &MODIFY_COLUMN_DEFAULT,
    &MOVE_COLUMN,
    &MODIFY_SAME_DEFINITION,
    &ADD_INDEX,
    &ADD_UNIQUE_INDEX,
    &ADD_FULLTEXT_INDEX,
    &DROP_INDEX,
    &ADD_FOREIGN_KEY,
    &ADD_FOREIGN_KEY_CHECKS_OFF,
    &DROP_FOREIGN_KEY,
];

/// The cases a MODIFY or CHANGE can fall into, listed when kilit does not
/// know the table.
const MODIFY_CASES: [&Case; 12] = [
    &CHANGE_INTEGER_TYPE,
    &LENGTHEN_VARCHAR,
    &LENGTHEN_VARCHAR_PAST_255_BYTES,
    &SHORTEN_VARCHAR,
    &CHANGE_COLUMN_CHARSET,
    &CHANGE_COLUMN_COLLATION,
    &MAKE_COLUMN_NOT_NULL,
    &MAKE_COLUMN_NULLABLE,
    &MODIFY_COLUMN_DEFAULT,
    &MOVE_COLUMN,
    &MODIFY_SAME_DEFINITION,
    &RENAME_COLUMN,
];

/// Checks every statement, in order, against what the lab measured on one
/// server. `schema` holds the tables as they are before the first
/// statement; pass an empty one when they are not known. There is one
/// finding per statement.
pub fn check(statements: &[Statement], schema: &Schema, results: &ServerResults) -> Vec<Finding> {
    let mut schema = schema.clone();
    let mut session = Session::default();
    let mut findings = Vec::with_capacity(statements.len());
    for statement in statements {
        let prediction = match &statement.kind {
            StatementKind::Alter(alter) => predict(alter, &session, &schema, results),
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
            StatementKind::Unparsed { error, .. } => {
                unknown(format!("kilit could not read this statement: {error}"))
            }
        };
        // The next statement sees the tables as this one leaves them.
        schema.apply(statement);
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

const ASSUMES_FOREIGN_KEY_CHECKS: &str =
    "assumes foreign_key_checks is ON, the MySQL default: nothing before this statement sets it";

fn predict(
    alter: &Alter,
    session: &Session,
    schema: &Schema,
    results: &ServerResults,
) -> Prediction {
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
    let table = schema.table(&alter.table);
    let Classified {
        case,
        mut notes,
        column,
    } = match classify(operation, session, table, results) {
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

    // A situation kilit can see in the table decides the answer; one it
    // cannot see becomes a note.
    let mut unseen = Vec::new();
    for exception in case.exceptions {
        match applies(
            exception.condition,
            table,
            column.as_deref(),
            session,
            schema,
        ) {
            Applies::Yes(why) => {
                let Some(other) = results
                    .scenario(exception.scenario)
                    .and_then(|scenario| scenario.verdict_for(variant))
                else {
                    return unknown(format!(
                        "{why}, and the lab did not run {} as {variant}",
                        exception.scenario
                    ));
                };
                notes.push(why);
                return Prediction::Measured {
                    case: case.id,
                    verdict: other,
                    evidence: vec![exception.scenario],
                    notes,
                };
            }
            Applies::Unmeasured(reason) => return unknown(reason),
            Applies::No => {}
            Applies::Maybe => unseen.push(exception),
        }
    }
    for exception in unseen {
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

/// Whether an exception's situation holds.
#[derive(Debug, PartialEq, Eq)]
enum Applies {
    /// It holds, for this reason.
    Yes(String),
    No,
    /// kilit cannot tell: it does not know the table, or no schema shows it.
    Maybe,
    /// It holds, but not the way the lab measured it.
    Unmeasured(String),
}

fn applies(
    condition: Condition,
    table: TableState<'_>,
    column: Option<&str>,
    session: &Session,
    schema: &Schema,
) -> Applies {
    let TableState::Known(table) = table else {
        return Applies::Maybe;
    };
    match (condition, column) {
        (Condition::Hidden, _) | (Condition::IndexedColumn | Condition::ForeignKeyColumn, None) => {
            Applies::Maybe
        }
        (Condition::FulltextIndex, _) => match table.fulltext_index() {
            Some(index) => Applies::Yes(format!(
                "{} has the FULLTEXT index {}",
                table.name,
                index.name.as_deref().unwrap_or_default()
            )),
            None => Applies::No,
        },
        (Condition::IndexedColumn, Some(column)) => match table.index_on(column) {
            Some(index) => Applies::Yes(format!("{column} is in the index {index}")),
            None => Applies::No,
        },
        (Condition::ForeignKeyColumn, Some(column)) => {
            if let Some(key) = table.foreign_key_on(column) {
                let key = key.name.as_deref().unwrap_or_default();
                return match session.foreign_key_checks {
                    Setting::On => Applies::Yes(format!("{column} is in the foreign key {key}")),
                    Setting::ServerDefault => Applies::Yes(format!(
                        "{column} is in the foreign key {key}; {ASSUMES_FOREIGN_KEY_CHECKS}"
                    )),
                    Setting::Off | Setting::Unclear => Applies::Unmeasured(format!(
                        "{column} is in the foreign key {key}, and the lab has not measured \
                         changing such a column with foreign_key_checks off"
                    )),
                };
            }
            match referenced_by(schema, table, column) {
                Some((other, key)) => Applies::Unmeasured(format!(
                    "{column} is referenced by the foreign key {key} on {other}, and the lab \
                     has not measured changing a referenced column"
                )),
                None => Applies::No,
            }
        }
    }
}

/// A foreign key on another table that points at `table.column`.
fn referenced_by(schema: &Schema, table: &Table, column: &str) -> Option<(String, String)> {
    schema.tables().find_map(|other| {
        other
            .foreign_keys
            .iter()
            .find(|key| {
                key.referenced_table.eq_ignore_ascii_case(&table.name)
                    && key
                        .referenced_columns
                        .iter()
                        .any(|c| c.eq_ignore_ascii_case(column))
            })
            .map(|key| (other.name.clone(), key.name.clone().unwrap_or_default()))
    })
}

/// The case an operation belongs to, with what kilit worked out on the way.
struct Classified {
    case: &'static Case,
    notes: Vec<String>,
    /// The column the change is about, as the table has it now.
    column: Option<String>,
}

/// Finds the case an operation belongs to, or says why it has none.
fn classify(
    operation: &Operation,
    session: &Session,
    table: TableState<'_>,
    results: &ServerResults,
) -> Result<Classified, String> {
    let mut notes = Vec::new();
    let mut subject = None;
    let case = match operation {
        Operation::AddColumn { column, .. } => {
            check_new_column(column)?;
            &ADD_COLUMN
        }
        Operation::DropColumn { name } => {
            subject = Some(name.clone());
            &DROP_COLUMN
        }
        Operation::RenameColumn { from, .. } => {
            subject = Some(from.clone());
            &RENAME_COLUMN
        }
        Operation::SetColumnDefault { column, .. } => {
            subject = Some(column.clone());
            &SET_COLUMN_DEFAULT
        }
        Operation::ModifyColumn { column, position } => {
            subject = Some(column.name.clone());
            modified(
                table,
                &column.name,
                column,
                position.as_ref(),
                results,
                &mut notes,
            )?
        }
        Operation::ChangeColumn {
            old_name,
            column,
            position,
        } => {
            subject = Some(old_name.clone());
            modified(
                table,
                old_name,
                column,
                position.as_ref(),
                results,
                &mut notes,
            )?
        }
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
                notes.push(ASSUMES_FOREIGN_KEY_CHECKS.to_owned());
                &ADD_FOREIGN_KEY
            }
            Setting::Unclear => {
                return Err(
                    "foreign_key_checks is set to a value kilit does not understand".into(),
                );
            }
        },
        Operation::DropForeignKey { .. } => &DROP_FOREIGN_KEY,
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
    Ok(Classified {
        case,
        notes,
        column: subject,
    })
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

/// The case of a MODIFY or CHANGE, from what it changes in the column. The
/// change is noted, so the reader sees what kilit compared.
fn modified(
    table: TableState<'_>,
    old_name: &str,
    new: &Column,
    position: Option<&Position>,
    results: &ServerResults,
    notes: &mut Vec<String>,
) -> Result<&'static Case, String> {
    let table = match table {
        TableState::Known(table) => table,
        TableState::Lost { reason } => {
            return Err(format!(
                "kilit lost track of this table ({reason}), and a MODIFY or CHANGE depends \
                 on the column's current definition"
            ));
        }
        TableState::Missing => return Err(modify_hint(results)),
    };
    let old = table.column(old_name).ok_or_else(|| {
        format!(
            "{} has no column {old_name}, as far as kilit can tell",
            table.name
        )
    })?;
    match changes(table, old, new, position)?.as_slice() {
        [] => Ok(&MODIFY_SAME_DEFINITION),
        [change] => {
            let case = case_of(change)?;
            notes.push(change.to_string());
            Ok(case)
        }
        many => {
            let mut reason = format!(
                "{} changes in one statement. kilit has measured them one at a time, and MySQL \
                 picks one algorithm for the whole statement, which can be heavier than each \
                 change alone:",
                many.len()
            );
            for change in many {
                let _ = write!(reason, "\n- {change}");
            }
            Err(reason)
        }
    }
}

/// The case of a single change to a column.
fn case_of(change: &Change) -> Result<&'static Case, String> {
    Ok(match change {
        Change::Name { .. } => &RENAME_COLUMN,
        Change::Type {
            from: DataType::Integer { .. },
            to: DataType::Integer { .. },
        } => &CHANGE_INTEGER_TYPE,
        Change::Type { .. } => {
            return Err(format!(
                "the statement {change}; the lab has measured changes between integer types only"
            ));
        }
        Change::Length { from, to, charset } => {
            // Up to 255 bytes, MySQL stores a value's length in 1 byte.
            let one_byte = |length: u64| length * charset.max_bytes <= 255;
            if to < from {
                &SHORTEN_VARCHAR
            } else if one_byte(*from) == one_byte(*to) {
                &LENGTHEN_VARCHAR
            } else {
                &LENGTHEN_VARCHAR_PAST_255_BYTES
            }
        }
        Change::Charset { .. } => &CHANGE_COLUMN_CHARSET,
        Change::Collation { .. } => &CHANGE_COLUMN_COLLATION,
        Change::Nullable { to: false } => &MAKE_COLUMN_NOT_NULL,
        Change::Nullable { to: true } => &MAKE_COLUMN_NULLABLE,
        Change::Default { .. } => &MODIFY_COLUMN_DEFAULT,
        Change::Position => &MOVE_COLUMN,
        Change::Comment { .. } => {
            return Err(
                "the statement only changes the comment, which has not been measured yet".into(),
            );
        }
        Change::Other(what) => {
            return Err(format!(
                "the statement {what}, which has not been measured yet"
            ));
        }
    })
}

/// Without the table, kilit cannot tell what a MODIFY or CHANGE changes.
/// It can still say what the lab measured for each kind of change.
fn modify_hint(results: &ServerResults) -> String {
    let mut hint = format!(
        "MODIFY and CHANGE depend on the column's current definition, and kilit does not know \
         this table. Give it the schema, such as the file `php artisan schema:dump` writes. \
         Measured on {}:",
        results.server.name()
    );
    for case in MODIFY_CASES {
        let first = case.scenarios.first().and_then(|id| results.scenario(id));
        if let Some(scenario) = first {
            let _ = write!(hint, "\n- {}: {}", case.id, brief(&scenario.verdict));
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

    /// Laravel's tables, as `php artisan schema:dump` writes them.
    const LARAVEL_TABLES: &str = include_str!("../examples/mysql-schema.sql");

    fn results() -> &'static ServerResults {
        Knowledge::built_in().server("mysql-8.4.13").unwrap()
    }

    /// The prediction for each statement of `sql` on MySQL 8.4.13, with
    /// the tables of `schema`.
    fn predictions_with(schema: &str, sql: &str) -> Vec<Prediction> {
        check(&parse(sql), &Schema::from_sql(schema), results())
            .into_iter()
            .map(|finding| finding.prediction)
            .collect()
    }

    /// Without knowing any table.
    fn predictions(sql: &str) -> Vec<Prediction> {
        predictions_with("", sql)
    }

    fn only(sql: &str) -> Prediction {
        let mut predictions = predictions(sql);
        assert_eq!(predictions.len(), 1, "{sql}");
        predictions.remove(0)
    }

    /// The prediction for the only statement of `sql`, on Laravel's tables.
    fn on_laravel_tables(sql: &str) -> Prediction {
        let mut predictions = predictions_with(LARAVEL_TABLES, sql);
        assert_eq!(predictions.len(), 1, "{sql}");
        predictions.remove(0)
    }

    fn reason(prediction: Prediction) -> String {
        match prediction {
            Prediction::Unknown { reason } => reason,
            other => panic!("expected unknown, got {other:?}"),
        }
    }

    /// The case, verdict and notes of a measured prediction.
    fn measured(prediction: Prediction) -> (&'static str, String, Vec<String>) {
        match prediction {
            Prediction::Measured {
                case,
                verdict,
                notes,
                ..
            } => (case, verdict.to_string(), notes),
            other => panic!("expected a measured prediction, got {other:?}"),
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
        // kilit does not know the table, so both exceptions are notes.
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
    fn without_the_table_modify_lists_what_was_measured() {
        let reason = reason(only("ALTER TABLE orders MODIFY amount BIGINT NOT NULL"));
        assert!(reason.contains("current definition"));
        assert!(reason.contains("schema:dump"));
        assert!(reason.contains("- change-integer-type: COPY, copies every row, writes wait"));
        assert!(reason.contains("- lengthen-varchar: INPLACE, no rebuild, writes continue"));
    }

    #[test]
    fn a_modify_is_judged_from_the_column_it_changes() {
        for (sql, case, verdict) in [
            (
                "alter table `orders` modify `amount` bigint not null",
                "change-integer-type",
                "COPY, copies every row, writes wait",
            ),
            (
                "alter table `orders` modify `note` varchar(60) null",
                "lengthen-varchar",
                "INPLACE, no rebuild, writes continue",
            ),
            (
                "alter table `orders` modify `note` varchar(100) null",
                "lengthen-varchar-past-255-bytes",
                "COPY, copies every row, writes wait",
            ),
            (
                "alter table `orders` modify `status` varchar(20) null default 'new'",
                "make-column-nullable",
                "INPLACE, rebuilds the table, writes continue",
            ),
            (
                "alter table `orders` modify `legacy_flag` tinyint(1) not null",
                "modify-column-default",
                "INSTANT, no rebuild, writes continue",
            ),
            (
                "alter table `orders` modify `note` varchar(50) null",
                "modify-same-definition",
                "INSTANT, no rebuild, writes continue",
            ),
            (
                "alter table `orders` change `note` `memo` varchar(50) null",
                "rename-column",
                "INSTANT, no rebuild, writes continue",
            ),
        ] {
            let (found, found_verdict, _) = measured(on_laravel_tables(sql));
            assert_eq!((found, found_verdict.as_str()), (case, verdict), "{sql}");
        }
        let (_, _, notes) = measured(on_laravel_tables(
            "alter table `orders` modify `legacy_flag` tinyint(1) not null",
        ));
        assert_eq!(notes, ["drops the default '0'"]);
    }

    #[test]
    fn several_changes_to_a_column_are_listed() {
        // Laravel 11 and later: $table->string('status', 30)->change()
        // drops the default the column had.
        let reason = reason(on_laravel_tables(
            "alter table `orders` modify `status` varchar(30) not null",
        ));
        assert!(reason.starts_with("2 changes in one statement"), "{reason}");
        assert!(reason.ends_with("\n- drops the default 'new'"), "{reason}");
    }

    #[test]
    fn a_fulltext_index_in_the_table_decides_the_answer() {
        let predictions = predictions_with(
            LARAVEL_TABLES,
            "alter table `orders` add `coupon` varchar(255) null;
             alter table `orders` add fulltext `orders_note_fulltext`(`note`);
             alter table `orders` add `discount` int null;",
        );
        // Before the FULLTEXT index: INSTANT, and only the note kilit cannot check.
        let (_, verdict, notes) = measured(predictions[0].clone());
        assert_eq!(verdict, "INSTANT, no rebuild, writes continue");
        assert_eq!(notes.len(), 1);
        assert!(notes[0].contains("64 instant column changes"));
        // After it: a copy, from the scenario that measured exactly that.
        let Prediction::Measured {
            verdict,
            evidence,
            notes,
            ..
        } = predictions[2].clone()
        else {
            panic!("{:?}", predictions[2]);
        };
        assert_eq!(verdict.to_string(), "COPY, copies every row, writes wait");
        assert_eq!(evidence, ["add-column-with-fulltext"]);
        assert_eq!(
            notes,
            ["orders has the FULLTEXT index orders_note_fulltext"]
        );
    }

    #[test]
    fn a_column_in_a_foreign_key_cannot_change_type() {
        let (case, verdict, notes) = measured(on_laravel_tables(
            "alter table `orders` modify `user_id` int unsigned not null",
        ));
        assert_eq!(case, "change-integer-type");
        assert!(verdict.starts_with("fails with error 3780"), "{verdict}");
        assert_eq!(notes.len(), 2);
        assert!(notes[1].starts_with("user_id is in the foreign key orders_user_id_foreign"));

        // With the checks off, the lab has not measured it.
        let predictions = predictions_with(
            LARAVEL_TABLES,
            "SET FOREIGN_KEY_CHECKS=0; alter table `orders` modify `user_id` int unsigned not null",
        );
        assert!(reason(predictions[1].clone()).contains("foreign_key_checks off"));

        // Nor changing the column a foreign key points at.
        let reason = reason(on_laravel_tables(
            "alter table `users` modify `id` int unsigned not null auto_increment",
        ));
        assert!(reason.contains("referenced by the foreign key orders_user_id_foreign"));
    }

    #[test]
    fn a_table_kilit_lost_track_of_is_unknown_for_modify() {
        let predictions = predictions_with(
            LARAVEL_TABLES,
            "ALTER TABLE orders CONVERT TO CHARACTER SET latin1;
             alter table `orders` modify `note` varchar(60) null;",
        );
        assert!(reason(predictions[1].clone()).contains("lost track of this table (line 1"));
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
            &Schema::new(),
            results(),
        );
        let lines: Vec<usize> = findings.iter().map(|finding| finding.line).collect();
        assert_eq!(lines, [1, 3, 4]);
    }
}
