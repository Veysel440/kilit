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
    Alter, Column, DataType, Index, IndexKind, Lock, Operation, Position, Statement, StatementKind,
    Table, Variable,
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
    /// Scenarios that measure this situation, in each shape the lab tried.
    /// Like a case's, they must give the same result on every server.
    pub scenarios: &'static [&'static str],
    /// When this result applies, e.g. "if the table has a FULLTEXT index".
    pub when: &'static str,
    /// The situation: all of these hold. Two situations measured together
    /// are an exception of their own.
    pub conditions: &'static [Condition],
}

/// What an exception depends on. When kilit can see it in the tables, the
/// exception's result is the answer, or does not apply at all; when it
/// cannot, the exception becomes a note.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Condition {
    /// The table has a FULLTEXT index, and keeps it: dropping the only
    /// column of a FULLTEXT index drops the index too.
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
            scenarios: &["add-column-with-fulltext"],
            when: "if the table has a FULLTEXT index",
            conditions: &[Condition::FulltextIndex],
        },
        Exception {
            scenarios: &["add-column-after-64-changes"],
            when: "if the table has had 64 instant column changes since it was last rebuilt",
            conditions: &[Condition::Hidden],
        },
    ],
};

/// A new column whose default is one of the expressions in
/// [`EXPRESSION_DEFAULTS`] that MySQL accepts, by copying the table.
const ADD_COLUMN_EXPRESSION_DEFAULT: Case = Case {
    id: "add-column-expression-default",
    scenarios: &[
        "add-column-default-now",
        "add-column-default-current-timestamp-in-parentheses",
        "add-column-default-curdate",
        "add-column-default-date-in-a-year",
        "add-column-default-json-array",
    ],
    exceptions: &[],
};

/// A new column whose default MySQL calls unsafe for the binary log
/// (error 1674): a replica could compute another value. Only the defaults
/// in [`EXPRESSION_DEFAULTS`] belong here, not every such function.
const ADD_COLUMN_BINLOG_UNSAFE_DEFAULT: Case = Case {
    id: "add-column-binlog-unsafe-default",
    scenarios: &["add-column-default-uuid", "add-column-default-rand"],
    exceptions: &[],
};

/// The expression defaults the lab measured, as kilit reads them, in
/// uppercase, with the case of each. Any other expression default is
/// unknown: a function the lab has not run may behave either way.
const EXPRESSION_DEFAULTS: [(&str, &Case); 7] = [
    ("(NOW())", &ADD_COLUMN_EXPRESSION_DEFAULT),
    ("(CURRENT_TIMESTAMP)", &ADD_COLUMN_EXPRESSION_DEFAULT),
    ("(CURDATE())", &ADD_COLUMN_EXPRESSION_DEFAULT),
    (
        "(CURRENT_DATE + INTERVAL 1 YEAR)",
        &ADD_COLUMN_EXPRESSION_DEFAULT,
    ),
    ("(JSON_ARRAY())", &ADD_COLUMN_EXPRESSION_DEFAULT),
    ("(UUID())", &ADD_COLUMN_BINLOG_UNSAFE_DEFAULT),
    ("(RAND())", &ADD_COLUMN_BINLOG_UNSAFE_DEFAULT),
];

/// Shapes the lab has not measured, such as a column in a foreign key or
/// the primary key, are unknown: see [`check_dropped_column`].
const DROP_COLUMN: Case = Case {
    id: "drop-column",
    scenarios: &["drop-column"],
    exceptions: &[
        Exception {
            scenarios: &["drop-column-with-fulltext"],
            when: "if the table keeps a FULLTEXT index",
            conditions: &[Condition::FulltextIndex],
        },
        Exception {
            scenarios: &[
                "drop-column-indexed",
                "drop-column-indexed-first-of-two",
                "drop-column-indexed-second-of-two",
                "drop-column-indexed-unique",
                "drop-column-indexed-prefix",
                "drop-column-indexed-fulltext",
            ],
            when: "if the column is in a plain or FULLTEXT index, or has a UNIQUE index of its own",
            conditions: &[Condition::IndexedColumn],
        },
        Exception {
            scenarios: &[
                "drop-column-indexed-with-fulltext",
                "drop-column-indexed-first-of-two-with-fulltext",
                "drop-column-indexed-second-of-two-with-fulltext",
                "drop-column-indexed-unique-with-fulltext",
                "drop-column-indexed-prefix-with-fulltext",
                "drop-column-indexed-fulltext-with-fulltext",
                "drop-column-indexed-fulltext-two-columns",
            ],
            when: "if the column is in an index and the table keeps a FULLTEXT index",
            conditions: &[Condition::FulltextIndex, Condition::IndexedColumn],
        },
    ],
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
            scenarios: &["modify-foreign-key-column"],
            when: "if the column is in a foreign key",
            conditions: &[Condition::ForeignKeyColumn],
        },
        Exception {
            scenarios: &["int-to-tinyint-out-of-range"],
            when: "if a value does not fit the new type (in strict SQL mode, the default)",
            conditions: &[Condition::Hidden],
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
        scenarios: &["varchar-50-to-5-too-long"],
        when: "if a value is longer than the new length (in strict SQL mode, the default)",
        conditions: &[Condition::Hidden],
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
        scenarios: &["change-column-collation-indexed"],
        when: "if the column is in an index",
        conditions: &[Condition::IndexedColumn],
    }],
};

const MAKE_COLUMN_NOT_NULL: Case = Case {
    id: "make-column-not-null",
    scenarios: &["not-null-no-nulls"],
    exceptions: &[Exception {
        scenarios: &["not-null-with-nulls"],
        when: "if a row holds NULL",
        conditions: &[Condition::Hidden],
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

/// The one functional index the lab measured, on `LOWER()` of a VARCHAR
/// column: see [`check_functional_index`].
const ADD_FUNCTIONAL_INDEX: Case = Case {
    id: "add-functional-index",
    scenarios: &["add-functional-index"],
    exceptions: &[],
};

const ADD_UNIQUE_INDEX: Case = Case {
    id: "add-unique-index",
    scenarios: &["add-unique-index"],
    exceptions: &[Exception {
        scenarios: &["add-unique-index-duplicates"],
        when: "if the columns hold duplicate values",
        conditions: &[Condition::Hidden],
    }],
};

const ADD_FULLTEXT_INDEX: Case = Case {
    id: "add-fulltext-index",
    scenarios: &["add-fulltext-index"],
    exceptions: &[Exception {
        scenarios: &["add-second-fulltext-index"],
        when: "if the table already has a FULLTEXT index",
        conditions: &[Condition::FulltextIndex],
    }],
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
        scenarios: &["add-foreign-key-orphans"],
        when: "if a row points to a parent row that does not exist",
        conditions: &[Condition::Hidden],
    }],
};

const ADD_FOREIGN_KEY_CHECKS_OFF: Case = Case {
    id: "add-foreign-key-checks-off",
    scenarios: &["add-foreign-key-checks-off"],
    exceptions: &[Exception {
        scenarios: &["add-foreign-key-checks-off-orphans"],
        when: "if a row points to a parent row that does not exist (the row is kept unchecked)",
        conditions: &[Condition::Hidden],
    }],
};

const DROP_FOREIGN_KEY: Case = Case {
    id: "drop-foreign-key",
    scenarios: &["drop-foreign-key"],
    exceptions: &[],
};

/// Every case kilit knows.
pub const CASES: [&Case; 25] = [
    &ADD_COLUMN,
    &ADD_COLUMN_EXPRESSION_DEFAULT,
    &ADD_COLUMN_BINLOG_UNSAFE_DEFAULT,
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
    &ADD_FUNCTIONAL_INDEX,
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
                        .map(ToString::to_string)
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
            StatementKind::Unparsed { error, .. } => unknown(error.as_str()),
        };
        // The next statement sees the tables as this one leaves them.
        follow(&mut schema, statement, &prediction);
        findings.push(Finding {
            line: statement.line,
            sql: statement.sql.clone(),
            prediction,
        });
    }
    findings
}

/// Moves `schema` past `statement`. MySQL runs an ALTER TABLE whole or not
/// at all, so one that kilit answers "fails" for leaves the tables as they
/// were, with none of its changes. Any other answer applies the statement:
/// a failure only in a note may not happen, and an unknown statement is
/// followed as if it ran.
fn follow(schema: &mut Schema, statement: &Statement, prediction: &Prediction) {
    let fails = matches!(
        prediction,
        Prediction::Measured {
            verdict: Verdict::Fails { .. },
            ..
        }
    );
    if !fails {
        schema.apply(statement);
    }
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
    /// `sql_log_bin`, which decides whether MySQL refuses some expression
    /// defaults.
    binary_log: BinaryLog,
}

/// Whether the session's statements go to the binary log, as far as the
/// input says. The lab measured expression defaults with them going there.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
enum BinaryLog {
    /// Nothing in the input sets `sql_log_bin`.
    #[default]
    NotSet,
    /// `sql_log_bin = 1` or `ON`, as in the lab.
    On,
    /// Set any other way, as written: `sql_log_bin = 0`, `sql_log_bin =
    /// DEFAULT`, or `GLOBAL sql_log_bin = 1`, which MySQL refuses.
    Other(String),
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
            match variable.name.as_str() {
                // A global value does not change this session's.
                "foreign_key_checks" if !variable.global => {
                    self.foreign_key_checks = Setting::parse(&variable.value);
                }
                "sql_log_bin" => {
                    let on = !variable.global && Setting::parse(&variable.value) == Setting::On;
                    self.binary_log = if on {
                        BinaryLog::On
                    } else {
                        BinaryLog::Other(variable.to_string())
                    };
                }
                _ => {}
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

const ASSUMES_BINARY_LOG: &str = "assumes the binary log is on, the MySQL default";

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
    } = match classify(operation, session, table, schema, results) {
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
    let verdict = match agreed(case.scenarios, case.id, variant, results) {
        Ok(verdict) => verdict,
        Err(reason) => return unknown(reason),
    };

    // A situation kilit can see in the table decides the answer; one it
    // cannot see becomes a note. When several hold, the exception measured
    // with all of their conditions decides, never the order of the list.
    let situation = Situation {
        table,
        column: column.as_deref(),
        dropped: match operation {
            Operation::DropColumn { name } => Some(name),
            _ => None,
        },
        session,
        schema,
    };
    let mut holding = Vec::new();
    let mut unseen = Vec::new();
    for exception in case.exceptions {
        match holds(exception, &situation) {
            Applies::Yes(why) => holding.push((exception, why)),
            Applies::Unmeasured(reason) => return unknown(reason),
            Applies::No => {}
            Applies::Maybe => unseen.push(exception),
        }
    }
    if !holding.is_empty() {
        let (exception, why) = match deciding(holding) {
            Ok(decides) => decides,
            Err(reason) => return unknown(reason),
        };
        let other = match agreed(exception.scenarios, case.id, variant, results) {
            Ok(other) => other,
            Err(reason) => return unknown(format!("{}, and {reason}", why.join(", and "))),
        };
        notes.extend(why);
        return Prediction::Measured {
            case: case.id,
            verdict: other,
            evidence: exception.scenarios.to_vec(),
            notes,
        };
    }
    for exception in unseen {
        let Ok(other) = agreed(exception.scenarios, case.id, variant, results) else {
            continue;
        };
        let scenarios = scenario_list(exception.scenarios);
        if other == verdict {
            notes.push(format!("{}: same result ({scenarios})", exception.when));
        } else {
            notes.push(format!(
                "{}: {} ({scenarios})",
                exception.when,
                brief(&other)
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

/// The verdict that all of `scenarios` give for `variant`, or why there is
/// none. `case` names them in the reason.
fn agreed(
    scenarios: &[&str],
    case: &str,
    variant: Variant,
    results: &ServerResults,
) -> Result<Verdict, String> {
    let mut verdict: Option<Verdict> = None;
    for id in scenarios {
        let Some(scenario) = results.scenario(id) else {
            return Err(format!(
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
            return Err(format!(
                "the lab saw something unexpected in {id}; run kilit-lab with --details"
            ));
        }
        let Some(this) = scenario.verdict_for(variant) else {
            return Err(format!("the lab did not run {id} as {variant}"));
        };
        match &verdict {
            None => verdict = Some(this),
            Some(first) if *first == this => {}
            Some(_) => {
                return Err(format!(
                    "the lab's scenarios for {case} disagree on {}",
                    results.server.name()
                ));
            }
        }
    }
    verdict.ok_or_else(|| format!("{case} has no lab scenarios"))
}

/// The scenarios behind a note: `drop-column-with-fulltext`, or
/// `drop-column-indexed and 5 more lab scenarios`.
fn scenario_list(scenarios: &[&str]) -> String {
    match scenarios {
        [] => String::new(),
        [one] => (*one).to_owned(),
        [first, more] => format!("{first} and {more}"),
        [first, rest @ ..] => format!("{first} and {} more lab scenarios", rest.len()),
    }
}

/// When several exceptions hold, the one whose conditions include all of
/// theirs decides: the lab measured that very combination. Without one,
/// kilit does not pick.
fn deciding(
    mut holding: Vec<(&'static Exception, Vec<String>)>,
) -> Result<(&'static Exception, Vec<String>), String> {
    let covers_all = |exception: &Exception| {
        holding.iter().all(|(other, _)| {
            other
                .conditions
                .iter()
                .all(|condition| exception.conditions.contains(condition))
        })
    };
    if let Some(at) = holding
        .iter()
        .position(|(exception, _)| covers_all(exception))
    {
        return Ok(holding.swap_remove(at));
    }
    let mut reasons: Vec<&str> = Vec::new();
    for (_, why) in &holding {
        for reason in why {
            if !reasons.contains(&reason.as_str()) {
                reasons.push(reason);
            }
        }
    }
    let count = match holding.len() {
        2 => "two".to_owned(),
        many => many.to_string(),
    };
    Err(format!(
        "{}: {count} measured situations apply, and the lab has not measured them together",
        reasons.join(", and ")
    ))
}

/// What the conditions of an exception are checked against.
struct Situation<'a> {
    table: TableState<'a>,
    /// The column the change is about, as the table has it now.
    column: Option<&'a str>,
    /// The column the statement drops, which takes its indexes with it.
    dropped: Option<&'a str>,
    session: &'a Session,
    schema: &'a Schema,
}

/// Whether an exception's situation holds.
#[derive(Debug, PartialEq, Eq)]
enum Applies {
    /// It holds, with a reason for each condition.
    Yes(Vec<String>),
    No,
    /// kilit cannot tell: it does not know the table, or no schema shows it.
    Maybe,
    /// It holds, but not the way the lab measured it.
    Unmeasured(String),
}

/// All of the exception's conditions must hold. One that does not decides
/// first, then one the lab has not measured, then one kilit cannot see.
fn holds(exception: &Exception, situation: &Situation<'_>) -> Applies {
    let mut reasons = Vec::new();
    let mut unmeasured = None;
    let mut maybe = false;
    for &condition in exception.conditions {
        match applies(condition, situation) {
            Applies::No => return Applies::No,
            Applies::Yes(why) => reasons.extend(why),
            Applies::Unmeasured(reason) => {
                unmeasured.get_or_insert(reason);
            }
            Applies::Maybe => maybe = true,
        }
    }
    match unmeasured {
        Some(reason) => Applies::Unmeasured(reason),
        None if maybe => Applies::Maybe,
        None => Applies::Yes(reasons),
    }
}

fn applies(condition: Condition, situation: &Situation<'_>) -> Applies {
    let TableState::Known(table) = situation.table else {
        return Applies::Maybe;
    };
    match (condition, situation.column) {
        (Condition::Hidden, _) | (Condition::IndexedColumn | Condition::ForeignKeyColumn, None) => {
            Applies::Maybe
        }
        (Condition::FulltextIndex, _) => match kept_fulltext_index(table, situation.dropped) {
            Some(index) => Applies::Yes(vec![format!(
                "{} has the FULLTEXT index {}",
                table.name,
                index.name.as_deref().unwrap_or_default()
            )]),
            None => Applies::No,
        },
        (Condition::IndexedColumn, Some(column)) => match table.index_on(column) {
            Some(index) => Applies::Yes(vec![format!("{column} is in the index {index}")]),
            None => Applies::No,
        },
        (Condition::ForeignKeyColumn, Some(column)) => {
            if let Some(key) = table.foreign_key_on(column) {
                let key = key.name.as_deref().unwrap_or_default();
                return match situation.session.foreign_key_checks {
                    Setting::On => {
                        Applies::Yes(vec![format!("{column} is in the foreign key {key}")])
                    }
                    Setting::ServerDefault => Applies::Yes(vec![format!(
                        "{column} is in the foreign key {key}; {ASSUMES_FOREIGN_KEY_CHECKS}"
                    )]),
                    Setting::Off | Setting::Unclear => Applies::Unmeasured(format!(
                        "{column} is in the foreign key {key}, and the lab has not measured \
                         changing such a column with foreign_key_checks off"
                    )),
                };
            }
            match referenced_by(situation.schema, table, column) {
                Some((other, key)) => Applies::Unmeasured(format!(
                    "{column} is referenced by the foreign key {key} on {other}, and the lab \
                     has not measured changing a referenced column"
                )),
                None => Applies::No,
            }
        }
    }
}

/// A FULLTEXT index that the table keeps after the statement. Dropping a
/// column drops the indexes it is the only column of.
fn kept_fulltext_index<'a>(table: &'a Table, dropped: Option<&str>) -> Option<&'a Index> {
    table.indexes.iter().find(|index| {
        index.kind == IndexKind::Fulltext
            && index.columns.iter().any(|entry| {
                dropped.is_none_or(|dropped| {
                    entry_column(entry).is_none_or(|column| !column.eq_ignore_ascii_case(dropped))
                })
            })
    })
}

/// The column an index entry names: `email` in `email` or `email(10)`.
/// An expression such as `(lower(email))` names none.
fn entry_column(entry: &str) -> Option<&str> {
    if is_expression(entry) {
        return None;
    }
    entry.split('(').next()
}

/// Whether an expression, such as `(amount * 2)`, names the column. A word
/// inside a string counts too, which only makes kilit more careful.
fn uses_column(expression: &str, column: &str) -> bool {
    expression
        .split(|c: char| !(c.is_alphanumeric() || c == '_' || c == '$'))
        .any(|word| word.eq_ignore_ascii_case(column))
        || expression
            .to_lowercase()
            .contains(&format!("`{}`", column.to_lowercase()))
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
    schema: &Schema,
    results: &ServerResults,
) -> Result<Classified, String> {
    let mut notes = Vec::new();
    let mut subject = None;
    let case = match operation {
        Operation::AddColumn { column, .. } => {
            check_new_column(column)?;
            match expression_default(column.default.as_deref()) {
                None => &ADD_COLUMN,
                Some(default) => {
                    let case = measured_default(default)?;
                    if let BinaryLog::Other(setting) = &session.binary_log {
                        return Err(format!(
                            "a statement before this one sets {setting}, and the lab has \
                             measured expression defaults only with the binary log on"
                        ));
                    }
                    notes.push(ASSUMES_BINARY_LOG.to_owned());
                    case
                }
            }
        }
        Operation::DropColumn { name } => {
            check_dropped_column(table, name, schema)?;
            subject = Some(name.clone());
            &DROP_COLUMN
        }
        Operation::RenameColumn { from, .. } => {
            subject = Some(from.clone());
            &RENAME_COLUMN
        }
        Operation::SetColumnDefault { column, default } => {
            // Measured with a literal, on a column that had no default.
            if let Some(default) = expression_default(Some(default)) {
                return Err(format!(
                    "setting the expression default {default} has not been measured yet"
                ));
            }
            if let TableState::Known(table) = table
                && let Some(old) = table.column(column)
                && let Some(old_default) = expression_default(old.default.as_deref())
            {
                return Err(format!(
                    "{column} has the expression default {old_default}, and changing it \
                     has not been measured yet"
                ));
            }
            subject = Some(column.clone());
            &SET_COLUMN_DEFAULT
        }
        Operation::ModifyColumn { column, position } => {
            check_new_definition(column)?;
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
            check_new_definition(column)?;
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
        Operation::AddIndex(index) if index.columns.iter().any(|entry| is_expression(entry)) => {
            check_functional_index(index, table)?;
            &ADD_FUNCTIONAL_INDEX
        }
        Operation::AddIndex(index) => match index.kind {
            IndexKind::Plain => &ADD_INDEX,
            IndexKind::Unique => &ADD_UNIQUE_INDEX,
            IndexKind::Fulltext => &ADD_FULLTEXT_INDEX,
            IndexKind::Spatial => return Err(SPATIAL_NOT_MEASURED.into()),
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

/// The default, if it is an expression. MySQL takes an expression only in
/// parentheses: `(NOW())`. `CURRENT_TIMESTAMP` and its synonyms without
/// them are not expressions here, and keep the answers measured for them.
fn expression_default(default: Option<&str>) -> Option<&str> {
    default.filter(|default| is_expression(default))
}

/// Whether a default or an index entry is an expression: it starts with a
/// parenthesis, as MySQL requires. `note(10)` is a prefix of `note`.
fn is_expression(text: &str) -> bool {
    text.trim_start().starts_with('(')
}

/// The case of an expression default the lab measured, matched in
/// uppercase: Laravel writes `(CURDATE())`, others `(now())`.
fn measured_default(default: &str) -> Result<&'static Case, String> {
    let written = default.to_ascii_uppercase();
    EXPRESSION_DEFAULTS
        .iter()
        .find(|(measured, _)| *measured == written)
        .map(|(_, case)| *case)
        .ok_or_else(|| format!("the expression default {default} has not been measured yet"))
}

/// The lab measured MODIFY and CHANGE with literal defaults only.
fn check_new_definition(column: &Column) -> Result<(), String> {
    match expression_default(column.default.as_deref()) {
        Some(default) => Err(format!(
            "the new definition has the expression default {default}, and a MODIFY or \
             CHANGE with an expression default has not been measured yet"
        )),
        None => Ok(()),
    }
}

const SPATIAL_NOT_MEASURED: &str = "spatial indexes have not been measured yet";

/// The one functional index the lab measured is a plain index on `LOWER()`
/// of a VARCHAR column. MySQL refuses others that look alike, such as
/// `LOWER()` of a TEXT column (error 3757), so kilit needs the table to
/// answer, and every other shape is unknown.
fn check_functional_index(index: &Index, table: TableState<'_>) -> Result<(), String> {
    match index.kind {
        IndexKind::Plain => {}
        IndexKind::Unique => {
            return Err("a UNIQUE index on an expression has not been measured yet".into());
        }
        IndexKind::Fulltext => {
            return Err("a FULLTEXT index on an expression has not been measured yet".into());
        }
        IndexKind::Spatial => return Err(SPATIAL_NOT_MEASURED.into()),
    }
    let [entry] = index.columns.as_slice() else {
        let what = if index.columns.iter().all(|entry| is_expression(entry)) {
            "several expressions"
        } else {
            "both columns and expressions"
        };
        return Err(format!("an index on {what} has not been measured yet"));
    };
    let Some(column) = lowered_column(entry) else {
        return Err(format!(
            "a functional index on {entry} has not been measured yet; the lab measured one \
             on LOWER() of a VARCHAR column"
        ));
    };
    let table = match table {
        TableState::Known(table) => table,
        TableState::Lost { reason } => {
            return Err(format!(
                "kilit lost track of this table ({reason}), and a functional index was \
                 measured only on LOWER() of a VARCHAR column"
            ));
        }
        TableState::Missing => {
            return Err(format!(
                "kilit does not know this table, so it cannot tell whether {column} is a \
                 VARCHAR column, the only kind the lab measured a functional index on"
            ));
        }
    };
    let Some(found) = table.column(column) else {
        return Err(format!(
            "{} has no column {column}, as far as kilit can tell",
            table.name
        ));
    };
    if !matches!(found.data_type, DataType::VarChar { .. }) {
        return Err(format!(
            "{column} is {}, and the lab measured a functional index on LOWER() of a VARCHAR \
             column only",
            found.data_type
        ));
    }
    Ok(())
}

/// The column in `(LOWER(email))` or ``(lower(`email`))``, in any case.
fn lowered_column(entry: &str) -> Option<&str> {
    let call = entry.strip_prefix('(')?.strip_suffix(')')?;
    let (function, rest) = call.split_once('(')?;
    if !function.eq_ignore_ascii_case("LOWER") {
        return None;
    }
    let argument = rest.strip_suffix(')')?;
    let column = argument
        .strip_prefix('`')
        .and_then(|quoted| quoted.strip_suffix('`'))
        .unwrap_or(argument);
    let plain = |c: char| c.is_alphanumeric() || c == '_' || c == '$';
    (!column.is_empty() && column.chars().all(plain)).then_some(column)
}

const DROPPING_NOT_MEASURED: &str = "dropping such a column has not been measured yet";

/// The lab measured dropping a column that is in no index, or in plain or
/// FULLTEXT indexes, or that has a UNIQUE index of its own. Other shapes
/// it has not measured, so the answer the lab gave for indexed columns
/// must not reach them. Without the table kilit cannot see the shape; the
/// notes say what the lab measured.
fn check_dropped_column(
    table: TableState<'_>,
    column: &str,
    schema: &Schema,
) -> Result<(), String> {
    let TableState::Known(table) = table else {
        return Ok(());
    };
    if let Some(key) = table.foreign_key_on(column) {
        return Err(format!(
            "{column} is in the foreign key {}, and {DROPPING_NOT_MEASURED}",
            key.name.as_deref().unwrap_or_default()
        ));
    }
    // One look at each index: the first that uses the column in an
    // expression, and the first of a shape the lab has not measured.
    let mut functional = None;
    let mut unmeasured = None;
    let mut indexed = table.in_primary_key(column);
    for index in &table.indexes {
        let index_name = || index.name.as_deref().unwrap_or_default();
        let mut has_column = false;
        let mut has_expression = false;
        for entry in &index.columns {
            match entry_column(entry) {
                Some(entry) => has_column |= entry.eq_ignore_ascii_case(column),
                None => {
                    has_expression = true;
                    if functional.is_none() && uses_column(entry, column) {
                        functional = Some(format!(
                            "{column} is used by the functional index {}, and \
                             {DROPPING_NOT_MEASURED}",
                            index_name()
                        ));
                    }
                }
            }
        }
        if !has_column {
            continue;
        }
        indexed = true;
        if unmeasured.is_some() {
            continue;
        }
        unmeasured = match index.kind {
            _ if has_expression => Some(format!(
                "{column} is in the index {}, which also indexes an expression, and \
                 {DROPPING_NOT_MEASURED}",
                index_name()
            )),
            IndexKind::Spatial => Some(format!(
                "{column} is in the spatial index {}, and {DROPPING_NOT_MEASURED}",
                index_name()
            )),
            // The other columns must stay unique without it.
            IndexKind::Unique if index.columns.len() > 1 => Some(format!(
                "{column} is in the unique index {} with other columns, which must stay \
                 unique without it; that depends on the rows, and {DROPPING_NOT_MEASURED}",
                index_name()
            )),
            IndexKind::Plain | IndexKind::Unique | IndexKind::Fulltext => None,
        };
    }
    // MySQL keeps an index on the columns a foreign key points at, so only
    // an indexed column needs the look through every table.
    if let Some((other, key)) = indexed
        .then(|| referenced_by(schema, table, column))
        .flatten()
    {
        return Err(format!(
            "{column} is referenced by the foreign key {key} on {other}, and \
             {DROPPING_NOT_MEASURED}"
        ));
    }
    if let Some(reason) = functional.or(unmeasured) {
        return Err(reason);
    }
    if table.in_primary_key(column) {
        return Err(if table.primary_key.len() > 1 {
            format!(
                "{column} is in the primary key with other columns, which must stay unique \
                 without it; that depends on the rows, and {DROPPING_NOT_MEASURED}"
            )
        } else {
            format!("{column} is the primary key, and {DROPPING_NOT_MEASURED}")
        });
    }
    // Generated columns are measured neither as the column dropped nor as
    // what uses it. Only the indexed shapes are kept away here, from the
    // answer the lab gave for indexed columns.
    if indexed {
        for other in &table.columns {
            let Some(generated) = &other.generated else {
                continue;
            };
            if other.name.eq_ignore_ascii_case(column) {
                return Err(format!(
                    "{column} is a generated column, and {DROPPING_NOT_MEASURED}"
                ));
            }
            if uses_column(&generated.expression, column) {
                return Err(format!(
                    "the generated column {} uses {column}, and {DROPPING_NOT_MEASURED}",
                    other.name
                ));
            }
        }
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
        // Measured between literal defaults.
        Change::Default { from, .. } if expression_default(from.as_deref()).is_some() => {
            return Err(format!(
                "the statement {change}, and changing an expression default has not been \
                 measured yet"
            ));
        }
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
    fn statements_mysql_does_not_accept_are_unknown_with_the_words() {
        // Measured cases until now: each would have had a lab answer.
        for (sql, words) in [
            (
                "ALTER TABLE orders DROP COLUMN IF EXISTS note",
                "DROP COLUMN IF EXISTS",
            ),
            (
                "ALTER TABLE IF EXISTS orders ADD COLUMN coupon INT",
                "ALTER TABLE IF EXISTS",
            ),
            (
                "ALTER TABLE ONLY orders ADD COLUMN coupon INT NULL",
                "ALTER TABLE ONLY",
            ),
            (
                "CREATE INDEX IF NOT EXISTS i ON orders (amount)",
                "CREATE INDEX IF NOT EXISTS",
            ),
            (
                "CREATE INDEX CONCURRENTLY IF NOT EXISTS i ON orders (amount)",
                "CREATE INDEX CONCURRENTLY or CREATE INDEX IF NOT EXISTS",
            ),
            (
                "CREATE INDEX i ON orders (amount) WHERE amount > 0",
                "WHERE in CREATE INDEX",
            ),
            (
                "DROP INDEX IF EXISTS orders_status_index ON orders",
                "DROP INDEX IF EXISTS",
            ),
            (
                "ALTER TABLE orders ADD CONSTRAINT fk FOREIGN KEY (user_id) \
                 REFERENCES users (id) NOT VALID",
                "NOT VALID",
            ),
        ] {
            assert_eq!(
                reason(on_laravel_tables(sql)),
                format!("MySQL 8.0.46 and 8.4.13 do not accept {words}"),
                "{sql}"
            );
        }
        assert_eq!(
            reason(on_laravel_tables(
                r#"ALTER TABLE "orders" ADD COLUMN coupon INT"#
            )),
            "a double-quoted table name works only when sql_mode has ANSI_QUOTES, \
             and kilit does not know the server's sql_mode"
        );
    }

    #[test]
    fn truncate_lock_tables_and_drop_index_without_on_are_unknown() {
        for sql in [
            "TRUNCATE TABLE orders",
            "LOCK TABLES orders WRITE",
            "DROP INDEX orders_status_index",
        ] {
            assert!(
                matches!(on_laravel_tables(sql), Prediction::Unknown { .. }),
                "{sql}"
            );
        }
        for sql in [
            "INSERT INTO shops (name) VALUES ('a')",
            "UPDATE orders SET amount = 1",
            "DELETE FROM orders",
            "SELECT * FROM orders",
            "SET foreign_key_checks = 0",
            "UNLOCK TABLES",
        ] {
            assert!(
                matches!(on_laravel_tables(sql), Prediction::NotChecked { .. }),
                "{sql}"
            );
        }
    }

    #[test]
    fn a_statement_mysql_rejects_leaves_the_tables_as_they_were() {
        // 8.0.46 and 8.4.13 fail the first statement, so note is still there
        // and orders is still known.
        let predictions = predictions_with(
            LARAVEL_TABLES,
            "ALTER TABLE orders DROP COLUMN IF EXISTS note;
             ALTER TABLE orders MODIFY note VARCHAR(100) NULL;
             ALTER TABLE orders MODIFY amount BIGINT NOT NULL;",
        );
        assert!(matches!(predictions[0], Prediction::Unknown { .. }));
        assert_eq!(
            measured(predictions[1].clone()).0,
            "lengthen-varchar-past-255-bytes"
        );
        assert_eq!(measured(predictions[2].clone()).0, "change-integer-type");
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

    /// Two hand-written tables, shaped like the lab's.
    const SHOP: &str = "
        CREATE TABLE users (
            id BIGINT UNSIGNED AUTO_INCREMENT PRIMARY KEY,
            name VARCHAR(100) NOT NULL
        ) DEFAULT CHARSET=utf8mb4;
        CREATE TABLE orders (
            id BIGINT UNSIGNED AUTO_INCREMENT PRIMARY KEY,
            user_id BIGINT UNSIGNED NOT NULL,
            amount INT NOT NULL,
            email VARCHAR(255) NULL,
            note VARCHAR(50) NULL
        ) DEFAULT CHARSET=utf8mb4;";

    /// The prediction for the last statement of `sql`, on [`SHOP`] as
    /// `setup` leaves it.
    fn on_shop(setup: &str, sql: &str) -> Prediction {
        predictions_with(&format!("{SHOP}\n{setup}"), sql)
            .pop()
            .unwrap()
    }

    /// The verdict of a measured prediction, or what else it is.
    fn said(prediction: &Prediction) -> String {
        match prediction {
            Prediction::Measured { verdict, .. } => verdict.to_string(),
            Prediction::Unknown { reason } => format!("unknown: {reason}"),
            Prediction::NotChecked { .. } => "not checked".to_owned(),
        }
    }

    #[test]
    fn a_second_fulltext_index_does_not_rebuild_the_table() {
        // The first FULLTEXT index is in the schema dump.
        let dump = "CREATE TABLE posts (id INT PRIMARY KEY, title VARCHAR(100), body TEXT,
                    FULLTEXT KEY posts_title_fulltext (title));";
        let Prediction::Measured {
            case,
            verdict,
            evidence,
            notes,
        } = predictions_with(
            dump,
            "ALTER TABLE posts ADD FULLTEXT INDEX posts_body_fulltext (body)",
        )
        .remove(0)
        else {
            panic!("expected a measured prediction");
        };
        assert_eq!(case, "add-fulltext-index");
        assert_eq!(verdict.to_string(), "INPLACE, no rebuild, writes wait");
        assert_eq!(evidence, ["add-second-fulltext-index"]);
        assert_eq!(notes, ["posts has the FULLTEXT index posts_title_fulltext"]);

        // The first one comes earlier in the same migration.
        let predictions = predictions_with(
            SHOP,
            "ALTER TABLE orders ADD FULLTEXT INDEX ft_note (note);
             ALTER TABLE orders ADD FULLTEXT INDEX ft_email (email);",
        );
        let verdicts: Vec<String> = predictions.iter().map(said).collect();
        assert_eq!(
            verdicts,
            [
                "INPLACE, rebuilds the table, writes wait",
                "INPLACE, no rebuild, writes wait"
            ]
        );
    }

    #[test]
    fn a_fulltext_index_dropped_earlier_no_longer_counts() {
        // InnoDB keeps a hidden column for FULLTEXT indexes after the last
        // one is dropped, and MySQL then adds the next one without a
        // rebuild. No schema shows that column, so kilit warns of a
        // rebuild: the safe side.
        let predictions = predictions_with(
            SHOP,
            "ALTER TABLE orders ADD FULLTEXT INDEX ft_note (note);
             ALTER TABLE orders DROP INDEX ft_note;
             ALTER TABLE orders ADD FULLTEXT INDEX ft_email (email);",
        );
        assert_eq!(
            said(&predictions[2]),
            "INPLACE, rebuilds the table, writes wait"
        );
    }

    #[test]
    fn without_the_table_a_second_fulltext_index_is_a_note() {
        let note = "if the table already has a FULLTEXT index: INPLACE, no rebuild, writes wait \
                    (add-second-fulltext-index)";
        let (_, verdict, notes) = measured(only("ALTER TABLE posts ADD FULLTEXT INDEX ft (body)"));
        assert_eq!(verdict, "INPLACE, rebuilds the table, writes wait");
        assert_eq!(notes, [note]);
        // Nor on a table kilit lost track of.
        let predictions = predictions_with(
            SHOP,
            "ALTER TABLE orders CONVERT TO CHARACTER SET latin1;
             ALTER TABLE orders ADD FULLTEXT INDEX ft_note (note);",
        );
        let (_, verdict, notes) = measured(predictions[1].clone());
        assert_eq!(verdict, "INPLACE, rebuilds the table, writes wait");
        assert_eq!(notes, [note]);
    }

    #[test]
    fn dropping_a_column_with_an_index_rebuilds_the_table() {
        let setup = "ALTER TABLE orders ADD INDEX idx_amount (amount);";
        let Prediction::Measured {
            case,
            verdict,
            evidence,
            notes,
        } = on_shop(setup, "ALTER TABLE orders DROP COLUMN amount")
        else {
            panic!("expected a measured prediction");
        };
        assert_eq!(case, "drop-column");
        assert_eq!(
            verdict.to_string(),
            "INPLACE, rebuilds the table, writes continue"
        );
        assert_eq!(evidence, DROP_COLUMN.exceptions[1].scenarios);
        assert_eq!(notes, ["amount is in the index idx_amount"]);
        // MySQL refuses to do it instantly.
        let forced = said(&on_shop(
            setup,
            "ALTER TABLE orders DROP COLUMN amount, ALGORITHM=INSTANT",
        ));
        assert!(forced.starts_with("fails with error 1845"), "{forced}");
    }

    #[test]
    fn every_index_shape_the_lab_measured_gets_its_answer() {
        for (setup, column) in [
            ("ALTER TABLE orders ADD INDEX i (amount, user_id)", "amount"),
            ("ALTER TABLE orders ADD INDEX i (user_id, amount)", "amount"),
            ("ALTER TABLE orders ADD UNIQUE INDEX i (amount)", "amount"),
            ("ALTER TABLE orders ADD INDEX i (note(10))", "note"),
            // The index goes with its only column.
            ("ALTER TABLE orders ADD FULLTEXT INDEX i (note)", "note"),
        ] {
            let prediction = on_shop(
                &format!("{setup};"),
                &format!("ALTER TABLE orders DROP COLUMN {column}"),
            );
            assert_eq!(
                said(&prediction),
                "INPLACE, rebuilds the table, writes continue",
                "{setup}"
            );
        }
    }

    #[test]
    fn index_shapes_the_lab_has_not_measured_are_unknown() {
        let places = "CREATE TABLE places (id INT PRIMARY KEY, spot POINT NOT NULL,
                      SPATIAL INDEX places_spot (spot));";
        for (setup, sql, expected) in [
            (
                "ALTER TABLE orders DROP PRIMARY KEY, ADD PRIMARY KEY (id, amount);",
                "ALTER TABLE orders DROP COLUMN amount",
                "amount is in the primary key with other columns, which must stay unique \
                 without it; that depends on the rows, and dropping such a column has not \
                 been measured yet",
            ),
            (
                "",
                "ALTER TABLE orders DROP COLUMN id",
                "id is the primary key, and dropping such a column has not been measured yet",
            ),
            (
                "ALTER TABLE orders ADD UNIQUE INDEX uq (user_id, amount);",
                "ALTER TABLE orders DROP COLUMN amount",
                "amount is in the unique index uq with other columns, which must stay unique \
                 without it; that depends on the rows, and dropping such a column has not \
                 been measured yet",
            ),
            (
                places,
                "ALTER TABLE places DROP COLUMN spot",
                "spot is in the spatial index places_spot, and dropping such a column has not \
                 been measured yet",
            ),
            (
                "ALTER TABLE orders ADD INDEX mixed (amount, (note + 1));",
                "ALTER TABLE orders DROP COLUMN amount",
                "amount is in the index mixed, which also indexes an expression, and dropping \
                 such a column has not been measured yet",
            ),
        ] {
            assert_eq!(reason(on_shop(setup, sql)), expected, "{sql}");
        }
    }

    #[test]
    fn dropping_a_column_mysql_may_refuse_to_drop_is_unknown() {
        let foreign_key = "ALTER TABLE orders ADD CONSTRAINT orders_user \
                           FOREIGN KEY (user_id) REFERENCES users (id);";
        assert_eq!(
            reason(on_shop(
                foreign_key,
                "ALTER TABLE orders DROP COLUMN user_id"
            )),
            "user_id is in the foreign key orders_user, and dropping such a column has not \
             been measured yet"
        );
        assert_eq!(
            reason(on_shop(foreign_key, "ALTER TABLE users DROP COLUMN id")),
            "id is referenced by the foreign key orders_user on orders, and dropping such a \
             column has not been measured yet"
        );
        // A functional index uses the column, which is in no other index.
        for index in ["(amount * 2)", "(`amount` * 2)"] {
            assert_eq!(
                reason(on_shop(
                    &format!("ALTER TABLE orders ADD INDEX twice ({index});"),
                    "ALTER TABLE orders DROP COLUMN amount"
                )),
                "amount is used by the functional index twice, and dropping such a column \
                 has not been measured yet",
                "{index}"
            );
        }
    }

    #[test]
    fn generated_columns_do_not_get_the_answer_measured_for_indexed_columns() {
        let generated = "ALTER TABLE orders ADD COLUMN amount2 INT AS (amount * 2) VIRTUAL;";
        // Not indexed: the answer kilit gave before, which this change
        // leaves alone. MySQL refuses to drop a column a generated column
        // uses (error 3108); the lab has not measured it yet.
        assert_eq!(
            said(&on_shop(generated, "ALTER TABLE orders DROP COLUMN amount")),
            "INSTANT, no rebuild, writes continue"
        );
        assert_eq!(
            reason(on_shop(
                &format!("{generated} ALTER TABLE orders ADD INDEX i (amount);"),
                "ALTER TABLE orders DROP COLUMN amount"
            )),
            "the generated column amount2 uses amount, and dropping such a column has not \
             been measured yet"
        );
        assert_eq!(
            reason(on_shop(
                &format!("{generated} ALTER TABLE orders ADD INDEX i (amount2);"),
                "ALTER TABLE orders DROP COLUMN amount2"
            )),
            "amount2 is a generated column, and dropping such a column has not been \
             measured yet"
        );
    }

    #[test]
    fn a_fulltext_index_that_stays_decides_over_the_index() {
        let fulltext = "ALTER TABLE orders ADD FULLTEXT INDEX ft_note (note);";
        // Both situations hold, and the lab measured them together.
        let Prediction::Measured {
            verdict,
            evidence,
            notes,
            ..
        } = on_shop(
            &format!("{fulltext} ALTER TABLE orders ADD INDEX idx_amount (amount);"),
            "ALTER TABLE orders DROP COLUMN amount",
        )
        else {
            panic!("expected a measured prediction");
        };
        assert_eq!(verdict.to_string(), "COPY, copies every row, writes wait");
        assert_eq!(evidence, DROP_COLUMN.exceptions[2].scenarios);
        assert_eq!(
            notes,
            [
                "orders has the FULLTEXT index ft_note",
                "amount is in the index idx_amount"
            ]
        );
        // The only column of the only FULLTEXT index: the index goes with
        // it, the table keeps none, and MySQL rebuilds instead of copying.
        assert_eq!(
            said(&on_shop(fulltext, "ALTER TABLE orders DROP COLUMN note")),
            "INPLACE, rebuilds the table, writes continue"
        );
        // Another FULLTEXT index stays, or this one keeps another column.
        for setup in [
            format!("{fulltext} ALTER TABLE orders ADD FULLTEXT INDEX ft_email (email);"),
            "ALTER TABLE orders ADD FULLTEXT INDEX ft (note, email);".to_owned(),
        ] {
            assert_eq!(
                said(&on_shop(&setup, "ALTER TABLE orders DROP COLUMN note")),
                "COPY, copies every row, writes wait",
                "{setup}"
            );
        }
    }

    #[test]
    fn a_column_in_no_index_keeps_its_answer() {
        // Instant, even when other columns are indexed.
        let Prediction::Measured {
            verdict,
            evidence,
            notes,
            ..
        } = on_shop(
            "ALTER TABLE orders ADD INDEX idx_amount (amount);",
            "ALTER TABLE orders DROP COLUMN note",
        )
        else {
            panic!("expected a measured prediction");
        };
        assert_eq!(verdict.to_string(), "INSTANT, no rebuild, writes continue");
        assert_eq!(evidence, ["drop-column"]);
        assert!(notes.is_empty(), "{notes:?}");
        // A copy when the table has a FULLTEXT index.
        let Prediction::Measured {
            verdict,
            evidence,
            notes,
            ..
        } = on_shop(
            "ALTER TABLE orders ADD FULLTEXT INDEX ft_note (note);",
            "ALTER TABLE orders DROP COLUMN amount",
        )
        else {
            panic!("expected a measured prediction");
        };
        assert_eq!(verdict.to_string(), "COPY, copies every row, writes wait");
        assert_eq!(evidence, ["drop-column-with-fulltext"]);
        assert_eq!(notes, ["orders has the FULLTEXT index ft_note"]);
    }

    #[test]
    fn without_the_table_dropping_a_column_lists_what_was_measured() {
        let (case, verdict, notes) = measured(only("ALTER TABLE orders DROP COLUMN amount"));
        assert_eq!(case, "drop-column");
        assert_eq!(verdict, "INSTANT, no rebuild, writes continue");
        assert_eq!(
            notes,
            [
                "if the table keeps a FULLTEXT index: COPY, copies every row, writes wait \
                 (drop-column-with-fulltext)",
                "if the column is in a plain or FULLTEXT index, or has a UNIQUE index of its \
                 own: INPLACE, rebuilds the table, writes continue (drop-column-indexed and 5 \
                 more lab scenarios)",
                "if the column is in an index and the table keeps a FULLTEXT index: COPY, \
                 copies every row, writes wait (drop-column-indexed-with-fulltext and 6 more \
                 lab scenarios)",
            ]
        );
    }

    #[test]
    fn situations_measured_apart_are_not_combined_by_their_order() {
        static FULLTEXT: Exception = Exception {
            scenarios: &["a"],
            when: "a",
            conditions: &[Condition::FulltextIndex],
        };
        static INDEXED: Exception = Exception {
            scenarios: &["b"],
            when: "b",
            conditions: &[Condition::IndexedColumn],
        };
        static BOTH: Exception = Exception {
            scenarios: &["c"],
            when: "c",
            conditions: &[Condition::FulltextIndex, Condition::IndexedColumn],
        };
        let holding = |exceptions: &[&'static Exception]| {
            exceptions
                .iter()
                .map(|exception| (*exception, vec![format!("{} holds", exception.when)]))
                .collect::<Vec<_>>()
        };
        for exceptions in [[&FULLTEXT, &INDEXED], [&INDEXED, &FULLTEXT]] {
            let reason = deciding(holding(&exceptions)).unwrap_err();
            assert!(
                reason.ends_with(
                    ": two measured situations apply, and the lab has not measured them together"
                ),
                "{reason}"
            );
        }
        for exceptions in [[&FULLTEXT, &INDEXED, &BOTH], [&BOTH, &INDEXED, &FULLTEXT]] {
            let (decides, why) = deciding(holding(&exceptions)).unwrap();
            assert_eq!(decides.scenarios, ["c"]);
            assert_eq!(why, ["c holds"]);
        }
    }

    #[test]
    fn every_expression_default_the_lab_measured_gets_its_answer() {
        for (column, case, verdict) in [
            (
                "token CHAR(36) NOT NULL DEFAULT (UUID())",
                "add-column-binlog-unsafe-default",
                "fails with error 1674",
            ),
            (
                "weight DOUBLE NOT NULL DEFAULT (rand())",
                "add-column-binlog-unsafe-default",
                "fails with error 1674",
            ),
            (
                "placed_at DATETIME NOT NULL DEFAULT (NOW())",
                "add-column-expression-default",
                "COPY, copies every row, writes wait",
            ),
            (
                "seen_at TIMESTAMP NULL DEFAULT (CURRENT_TIMESTAMP)",
                "add-column-expression-default",
                "COPY, copies every row, writes wait",
            ),
            (
                // $table->date('due_on')->useCurrent()
                "`due_on` date not null default (CURDATE())",
                "add-column-expression-default",
                "COPY, copies every row, writes wait",
            ),
            (
                "expires_on DATE NOT NULL DEFAULT (current_date + interval 1 year)",
                "add-column-expression-default",
                "COPY, copies every row, writes wait",
            ),
            (
                "tags JSON NOT NULL DEFAULT (JSON_ARRAY())",
                "add-column-expression-default",
                "COPY, copies every row, writes wait",
            ),
        ] {
            let sql = format!("ALTER TABLE orders ADD COLUMN {column}");
            // The same with and without the table: no exception to look for.
            for prediction in [only(&sql), on_shop("", &sql)] {
                let (found, said, notes) = measured(prediction);
                assert_eq!(found, case, "{sql}");
                assert!(said.starts_with(verdict), "{sql}: {said}");
                assert_eq!(notes, [ASSUMES_BINARY_LOG], "{sql}");
            }
        }
    }

    #[test]
    fn every_expression_default_kilit_answers_for_was_measured() {
        for (default, case) in EXPRESSION_DEFAULTS {
            let measured = case.scenarios.iter().any(|id| {
                let scenario = results().scenario(id).unwrap();
                parse(&scenario.statement).iter().any(|statement| {
                    matches!(&statement.kind, StatementKind::Alter(alter)
                        if matches!(alter.operations.as_slice(),
                            [Operation::AddColumn { column, .. }]
                                if column.default.as_deref().map(str::to_ascii_uppercase)
                                    .as_deref() == Some(default)))
                })
            });
            assert!(measured, "{default} is in no lab scenario of {}", case.id);
        }
    }

    #[test]
    fn expression_defaults_the_lab_has_not_measured_are_unknown() {
        // As kilit prints them back.
        for default in [
            "(CONCAT('a', 'b'))",
            "(CURRENT_DATE)",
            "(CURRENT_TIMESTAMP())",
            "(CURDATE() + INTERVAL 1 DAY)",
            "(YEAR(CURDATE()))",
            "(UUID_TO_BIN(UUID()))",
            "(0)",
            "('x')",
        ] {
            let sql = format!("ALTER TABLE orders ADD COLUMN c VARCHAR(40) NULL DEFAULT {default}");
            assert_eq!(
                reason(only(&sql)),
                format!("the expression default {default} has not been measured yet"),
                "{sql}"
            );
        }
        // With another change it was unknown already.
        assert!(
            reason(only(
                "ALTER TABLE orders ADD COLUMN token CHAR(36) NOT NULL DEFAULT (UUID()), \
                 ADD INDEX i (amount)"
            ))
            .starts_with("2 changes in one statement")
        );
    }

    #[test]
    fn the_binary_log_follows_the_session() {
        let uuid = "ALTER TABLE orders ADD COLUMN token CHAR(36) NOT NULL DEFAULT (UUID());";
        let now = "ALTER TABLE orders ADD COLUMN placed_at DATETIME NOT NULL DEFAULT (NOW());";
        let told: Vec<String> = predictions(&format!(
            "{uuid} SET sql_log_bin = 0; {uuid} {now} ALTER TABLE orders ADD coupon INT NULL;
             SET sql_log_bin = 1; {uuid} {now}"
        ))
        .iter()
        .map(said)
        .collect();
        let off = "unknown: a statement before this one sets sql_log_bin = 0, and the lab has \
                   measured expression defaults only with the binary log on";
        assert!(told[0].starts_with("fails with error 1674"), "{told:?}");
        assert_eq!(told[1], "not checked");
        assert_eq!(told[2], off);
        assert_eq!(told[3], off);
        // A column without an expression default does not depend on it.
        assert_eq!(told[4], "INSTANT, no rebuild, writes continue");
        assert_eq!(told[5], "not checked");
        assert!(told[6].starts_with("fails with error 1674"), "{told:?}");
        assert_eq!(told[7], "COPY, copies every row, writes wait");

        for setting in [
            "sql_log_bin = ON",
            "SESSION sql_log_bin = 1",
            "@@sql_log_bin = TRUE",
        ] {
            let (_, verdict, notes) =
                measured(predictions(&format!("SET {setting}; {uuid}"))[1].clone());
            assert!(verdict.starts_with("fails with error 1674"), "{setting}");
            assert_eq!(notes, [ASSUMES_BINARY_LOG], "{setting}");
        }
        // Off, back to the server's value, a value kilit cannot read, or
        // GLOBAL, which MySQL refuses: the lab has not measured these.
        for (setting, written) in [
            ("SET sql_log_bin = OFF", "sql_log_bin = OFF"),
            ("SET sql_log_bin = DEFAULT", "sql_log_bin = DEFAULT"),
            ("SET sql_log_bin = @saved", "sql_log_bin = @saved"),
            ("SET GLOBAL sql_log_bin = 1", "GLOBAL sql_log_bin = 1"),
            ("SET @@global.sql_log_bin = 0", "GLOBAL sql_log_bin = 0"),
        ] {
            let answers = predictions(&format!("{setting}; {uuid}"));
            assert!(
                matches!(&answers[0], Prediction::NotChecked { reason }
                    if *reason == format!("sets {written} for the statements after it")),
                "{setting}: {:?}",
                answers[0]
            );
            assert!(
                reason(answers[1].clone()).contains(&format!("sets {written}, and")),
                "{setting}"
            );
        }
    }

    #[test]
    fn on_a_new_table_a_measured_failure_still_fails() {
        // The empty table fails too (checked by hand on both servers).
        let statements = parse(
            "CREATE TABLE tokens (id INT PRIMARY KEY);
             ALTER TABLE tokens ADD COLUMN token CHAR(36) NOT NULL DEFAULT (UUID());
             ALTER TABLE tokens ADD COLUMN placed_at DATETIME NOT NULL DEFAULT (NOW());",
        );
        let findings = check(&statements, &Schema::new(), results());
        assert_eq!(
            crate::impact::impacts(&statements, &findings),
            [
                crate::impact::Impact::NotChecked,
                crate::impact::Impact::Fails,
                crate::impact::Impact::NewTable
            ]
        );
    }

    #[test]
    fn literal_and_current_timestamp_defaults_keep_their_answer() {
        for column in [
            "seen_at TIMESTAMP NULL DEFAULT CURRENT_TIMESTAMP ON UPDATE CURRENT_TIMESTAMP",
            // useCurrent(), useCurrentOnUpdate(), and with a precision.
            "`created_at` timestamp not null default CURRENT_TIMESTAMP",
            "`updated_at` timestamp not null default CURRENT_TIMESTAMP on update CURRENT_TIMESTAMP",
            "`created_at` timestamp(6) not null default CURRENT_TIMESTAMP(6)",
            "placed_at DATETIME NULL DEFAULT NOW()",
            "touched_at TIMESTAMP NULL ON UPDATE CURRENT_TIMESTAMP",
            "c INT NOT NULL DEFAULT 0",
            "s VARCHAR(20) NOT NULL DEFAULT 'x'",
            "n INT NULL DEFAULT NULL",
        ] {
            let sql = format!("ALTER TABLE orders ADD COLUMN {column}");
            let (case, said, notes) = measured(only(&sql));
            assert_eq!(
                (case, said.as_str()),
                ("add-column", "INSTANT, no rebuild, writes continue"),
                "{sql}"
            );
            // The two exceptions kilit cannot check without the table.
            assert_eq!(notes.len(), 2, "{sql}: {notes:?}");
        }
        assert_eq!(
            measured(on_shop(
                "",
                "ALTER TABLE orders MODIFY amount INT NOT NULL DEFAULT 0"
            ))
            .0,
            "modify-column-default"
        );
        assert_eq!(
            measured(on_shop(
                "",
                "ALTER TABLE orders ALTER COLUMN amount SET DEFAULT 0"
            ))
            .0,
            "set-column-default"
        );
    }

    #[test]
    fn a_changed_default_measured_with_literals_is_unknown_with_an_expression() {
        for (sql, expected) in [
            (
                "ALTER TABLE orders MODIFY note VARCHAR(50) NULL DEFAULT (UUID())",
                "the new definition has the expression default (UUID()), and a MODIFY or \
                 CHANGE with an expression default has not been measured yet",
            ),
            (
                "ALTER TABLE orders CHANGE note memo VARCHAR(50) NULL DEFAULT (NOW())",
                "the new definition has the expression default (NOW()), and a MODIFY or \
                 CHANGE with an expression default has not been measured yet",
            ),
            (
                "ALTER TABLE orders ALTER COLUMN note SET DEFAULT (CONCAT('a', 'b'))",
                "setting the expression default (CONCAT('a', 'b')) has not been measured yet",
            ),
        ] {
            assert_eq!(reason(on_shop("", sql)), expected, "{sql}");
        }
        // A column whose default is an expression already.
        let setup = "ALTER TABLE orders ADD COLUMN due_on DATE NULL DEFAULT (CURDATE());";
        for (sql, expected) in [
            (
                "ALTER TABLE orders MODIFY due_on DATE NULL",
                "the statement drops the default (CURDATE()), and changing an expression \
                 default has not been measured yet",
            ),
            (
                "ALTER TABLE orders MODIFY due_on DATE NULL DEFAULT '2030-01-01'",
                "the statement changes the default from (CURDATE()) to '2030-01-01', and \
                 changing an expression default has not been measured yet",
            ),
            (
                "ALTER TABLE orders ALTER COLUMN due_on SET DEFAULT '2030-01-01'",
                "due_on has the expression default (CURDATE()), and changing it has not been \
                 measured yet",
            ),
            (
                "ALTER TABLE orders MODIFY due_on DATE NOT NULL DEFAULT (CURDATE())",
                "the new definition has the expression default (CURDATE()), and a MODIFY or \
                 CHANGE with an expression default has not been measured yet",
            ),
        ] {
            assert_eq!(reason(on_shop(setup, sql)), expected, "{sql}");
        }
    }

    #[test]
    fn a_functional_index_on_lower_of_a_varchar_column_makes_writes_wait() {
        for sql in [
            "ALTER TABLE orders ADD INDEX idx_lower_email ((LOWER(email)))",
            // $table->rawIndex('(lower(email))', 'orders_lower_email_index')
            "alter table `orders` add index `orders_lower_email_index`((lower(email)))",
            "alter table `orders` add index `orders_lower_email_index`((lower(`email`)))",
            "CREATE INDEX idx_lower_email ON orders ((LOWER(email)))",
        ] {
            let Prediction::Measured {
                case,
                verdict,
                evidence,
                notes,
            } = on_shop("", sql)
            else {
                panic!("expected a measured prediction: {sql}");
            };
            assert_eq!(case, "add-functional-index", "{sql}");
            assert_eq!(
                verdict.to_string(),
                "INPLACE, no rebuild, writes wait",
                "{sql}"
            );
            assert_eq!(evidence, ["add-functional-index"], "{sql}");
            assert!(notes.is_empty(), "{sql}: {notes:?}");
        }
        let forced = said(&on_shop(
            "",
            "ALTER TABLE orders ADD INDEX i ((LOWER(email))), LOCK=NONE",
        ));
        assert!(forced.starts_with("fails with error 1846"), "{forced}");
    }

    #[test]
    fn functional_index_shapes_the_lab_has_not_measured_are_unknown() {
        let lowered_only = "has not been measured yet; the lab measured one on LOWER() of a \
                            VARCHAR column";
        for (setup, sql, expected) in [
            (
                "",
                "ALTER TABLE orders ADD UNIQUE INDEX u ((LOWER(email)))",
                "a UNIQUE index on an expression has not been measured yet".to_owned(),
            ),
            (
                "",
                "ALTER TABLE orders ADD FULLTEXT INDEX f ((LOWER(email)))",
                "a FULLTEXT index on an expression has not been measured yet".to_owned(),
            ),
            (
                "",
                "ALTER TABLE orders ADD INDEX i (user_id, (LOWER(email)))",
                "an index on both columns and expressions has not been measured yet".to_owned(),
            ),
            (
                "",
                "ALTER TABLE orders ADD INDEX i ((LOWER(email)), (amount * 2))",
                "an index on several expressions has not been measured yet".to_owned(),
            ),
            (
                "",
                "ALTER TABLE orders ADD INDEX i ((UPPER(email)))",
                format!("a functional index on (UPPER(email)) {lowered_only}"),
            ),
            (
                "",
                "ALTER TABLE orders ADD INDEX i ((LOWER(CONCAT(email, note))))",
                format!("a functional index on (LOWER(CONCAT(email, note))) {lowered_only}"),
            ),
            // Shapes MySQL refuses, by hand on both servers: error 3762, 3758, 3757.
            (
                "",
                "ALTER TABLE orders ADD INDEX i ((note))",
                format!("a functional index on (note) {lowered_only}"),
            ),
            (
                "",
                "ALTER TABLE orders ADD INDEX i ((RAND()))",
                format!("a functional index on (RAND()) {lowered_only}"),
            ),
            (
                "ALTER TABLE orders ADD COLUMN body TEXT NULL;",
                "ALTER TABLE orders ADD INDEX i ((LOWER(body)))",
                "body is TEXT, and the lab measured a functional index on LOWER() of a VARCHAR \
                 column only"
                    .to_owned(),
            ),
            (
                "",
                "ALTER TABLE orders ADD INDEX i ((LOWER(missing)))",
                "orders has no column missing, as far as kilit can tell".to_owned(),
            ),
        ] {
            assert_eq!(reason(on_shop(setup, sql)), expected, "{sql}");
        }
        // Without the table kilit cannot see the column's type.
        assert_eq!(
            reason(only("ALTER TABLE orders ADD INDEX i ((LOWER(email)))")),
            "kilit does not know this table, so it cannot tell whether email is a VARCHAR \
             column, the only kind the lab measured a functional index on"
        );
        let predictions = predictions_with(
            SHOP,
            "ALTER TABLE orders CONVERT TO CHARACTER SET latin1;
             ALTER TABLE orders ADD INDEX i ((LOWER(email)));",
        );
        assert!(
            reason(predictions[1].clone()).starts_with("kilit lost track of this table (line 1")
        );
    }

    #[test]
    fn indexes_on_columns_keep_their_answers() {
        let continues = "INPLACE, no rebuild, writes continue";
        for (sql, case, verdict) in [
            (
                "ALTER TABLE orders ADD INDEX i (note(10))",
                "add-index",
                continues,
            ),
            (
                "ALTER TABLE orders ADD INDEX i (`note`(10))",
                "add-index",
                continues,
            ),
            (
                "CREATE INDEX i ON orders (note(10))",
                "add-index",
                continues,
            ),
            (
                "ALTER TABLE orders ADD INDEX i (amount)",
                "add-index",
                continues,
            ),
            (
                "ALTER TABLE orders ADD INDEX i (user_id, amount)",
                "add-index",
                continues,
            ),
            (
                "ALTER TABLE orders ADD UNIQUE INDEX u (email)",
                "add-unique-index",
                continues,
            ),
            (
                "ALTER TABLE orders ADD FULLTEXT INDEX f (note)",
                "add-fulltext-index",
                "INPLACE, rebuilds the table, writes wait",
            ),
        ] {
            for prediction in [only(sql), on_shop("", sql)] {
                let (found, said, _) = measured(prediction);
                assert_eq!((found, said.as_str()), (case, verdict), "{sql}");
            }
        }
    }

    #[test]
    fn a_failing_statement_leaves_the_tables_as_they_were() {
        // MySQL refuses to drop the FULLTEXT index instantly, so it stays,
        // and the next new column copies the table.
        let fulltext = "ALTER TABLE orders ADD FULLTEXT INDEX ft_note (note);";
        let predictions = predictions_with(
            &format!("{SHOP} {fulltext}"),
            "ALTER TABLE orders DROP INDEX ft_note, ALGORITHM=INSTANT;
             ALTER TABLE orders ADD COLUMN coupon VARCHAR(255) NULL;",
        );
        assert!(said(&predictions[0]).starts_with("fails with error 1845"));
        let Prediction::Measured {
            verdict,
            evidence,
            notes,
            ..
        } = predictions[1].clone()
        else {
            panic!("expected a measured prediction: {:?}", predictions[1]);
        };
        assert_eq!(verdict.to_string(), "COPY, copies every row, writes wait");
        assert_eq!(evidence, ["add-column-with-fulltext"]);
        assert_eq!(notes, ["orders has the FULLTEXT index ft_note"]);
    }

    #[test]
    fn a_column_change_that_fails_leaves_the_old_definition() {
        // user_id stays BIGINT UNSIGNED, in the foreign key: the same change
        // fails again, and the old definition changes nothing.
        let predictions = predictions_with(
            LARAVEL_TABLES,
            "alter table `orders` modify `user_id` int unsigned not null;
             alter table `orders` modify `user_id` int unsigned not null;
             alter table `orders` modify `user_id` bigint unsigned not null;",
        );
        assert!(said(&predictions[0]).starts_with("fails with error 3780"));
        assert!(said(&predictions[1]).starts_with("fails with error 3780"));
        let (case, verdict, _) = measured(predictions[2].clone());
        assert_eq!(
            (case, verdict.as_str()),
            (
                "modify-same-definition",
                "INSTANT, no rebuild, writes continue"
            )
        );
    }

    #[test]
    fn an_index_mysql_refuses_to_add_is_not_there_afterwards() {
        // An index cannot be added instantly, nor a FULLTEXT index while
        // writes continue.
        let told: Vec<String> = predictions_with(
            SHOP,
            "ALTER TABLE orders ADD INDEX idx_amount (amount), ALGORITHM=INSTANT;
             ALTER TABLE orders DROP COLUMN amount;
             ALTER TABLE orders ADD FULLTEXT INDEX ft_note (note), LOCK=NONE;
             ALTER TABLE orders ADD COLUMN coupon VARCHAR(255) NULL;",
        )
        .iter()
        .map(said)
        .collect();
        assert!(told[0].starts_with("fails with error 1845"), "{told:?}");
        // No index on amount: dropping it stays instant.
        assert_eq!(told[1], "INSTANT, no rebuild, writes continue");
        assert!(told[2].starts_with("fails with error 1846"), "{told:?}");
        // No FULLTEXT index: the new column stays instant.
        assert_eq!(told[3], "INSTANT, no rebuild, writes continue");
    }

    #[test]
    fn a_failing_statement_keeps_none_of_its_changes() {
        // kilit answers "unknown" for several changes in one statement, so
        // no such statement gets a measured failure today; when one does,
        // none of its changes are kept. Any other answer keeps them all.
        let statements =
            parse("ALTER TABLE orders ADD INDEX idx_amount (amount), DROP COLUMN note");
        let before = Schema::from_sql(SHOP);
        let fails = Prediction::Measured {
            case: "add-index",
            verdict: Verdict::Fails {
                code: 1845,
                message: String::new(),
            },
            evidence: Vec::new(),
            notes: Vec::new(),
        };
        let mut schema = before.clone();
        follow(&mut schema, &statements[0], &fails);
        assert_eq!(schema.table("orders"), before.table("orders"));

        follow(&mut schema, &statements[0], &unknown("2 changes"));
        let TableState::Known(orders) = schema.table("orders") else {
            panic!("orders should be known");
        };
        assert!(orders.column("note").is_none());
        assert_eq!(orders.index_on("amount").as_deref(), Some("idx_amount"));
    }

    #[test]
    fn a_failure_on_a_table_created_here_leaves_it_as_created() {
        let statements = parse(
            "CREATE TABLE tokens (id INT PRIMARY KEY, label VARCHAR(20) NULL);
             ALTER TABLE tokens ADD COLUMN token CHAR(36) NOT NULL DEFAULT (UUID());
             ALTER TABLE tokens MODIFY token CHAR(36) NULL;",
        );
        let findings = check(&statements, &Schema::new(), results());
        assert_eq!(
            crate::impact::impacts(&statements, &findings),
            [
                crate::impact::Impact::NotChecked,
                crate::impact::Impact::Fails,
                crate::impact::Impact::Unknown
            ]
        );
        // The table never got the column, in MySQL either.
        assert_eq!(
            reason(findings[2].prediction.clone()),
            "tokens has no column token, as far as kilit can tell"
        );
    }

    #[test]
    fn statements_that_do_not_fail_still_change_the_tables() {
        // A measured answer that runs.
        let told: Vec<String> = predictions_with(
            SHOP,
            "ALTER TABLE orders ADD INDEX idx_amount (amount);
             ALTER TABLE orders DROP COLUMN amount;",
        )
        .iter()
        .map(said)
        .collect();
        assert_eq!(told[1], "INPLACE, rebuilds the table, writes continue");

        // A failure in a note only: a row may point to a missing parent,
        // or not. The key is kept.
        let predictions = predictions_with(
            SHOP,
            "ALTER TABLE orders ADD CONSTRAINT fk_orders_user \
             FOREIGN KEY (user_id) REFERENCES users (id);
             ALTER TABLE orders MODIFY user_id INT UNSIGNED NOT NULL;",
        );
        let (_, verdict, notes) = measured(predictions[0].clone());
        assert_eq!(verdict, "COPY, copies every row, writes wait");
        assert!(
            notes
                .iter()
                .any(|note| note.contains("fails with error 1452")),
            "{notes:?}"
        );
        assert!(said(&predictions[1]).starts_with("fails with error 3780"));

        // An unknown statement is followed as if it ran, as before.
        let predictions = predictions_with(
            SHOP,
            "ALTER TABLE orders ADD INDEX idx_amount (amount), ADD COLUMN c INT NULL;
             ALTER TABLE orders DROP COLUMN amount;",
        );
        assert!(reason(predictions[0].clone()).starts_with("2 changes in one statement"));
        assert_eq!(
            said(&predictions[1]),
            "INPLACE, rebuilds the table, writes continue"
        );
    }

    #[test]
    fn the_answer_of_the_selected_server_decides() {
        let schema = Schema::from_sql(&format!(
            "{SHOP} ALTER TABLE orders ADD FULLTEXT INDEX ft_note (note);"
        ));
        let sql = "ALTER TABLE orders DROP INDEX ft_note, ALGORITHM=INSTANT;
                   ALTER TABLE orders ADD COLUMN coupon VARCHAR(255) NULL;";
        for server in ["mysql-8.0.46", "mysql-8.4.13"] {
            let results = Knowledge::built_in().server(server).unwrap();
            let told: Vec<String> = check(&parse(sql), &schema, results)
                .iter()
                .map(|finding| said(&finding.prediction))
                .collect();
            assert!(told[0].starts_with("fails with error 1845"), "{server}");
            assert_eq!(told[1], "COPY, copies every row, writes wait", "{server}");
        }
    }

    #[test]
    fn no_two_exceptions_of_a_case_hold_in_the_same_situation() {
        // Otherwise the order of the list would pick between them.
        for case in CASES {
            let seen: Vec<&[Condition]> = case
                .exceptions
                .iter()
                .map(|exception| exception.conditions)
                .filter(|conditions| !conditions.contains(&Condition::Hidden))
                .collect();
            for (at, conditions) in seen.iter().enumerate() {
                for other in &seen[at + 1..] {
                    let same = conditions.len() == other.len()
                        && conditions.iter().all(|condition| other.contains(condition));
                    assert!(!same, "{}: {conditions:?} twice", case.id);
                }
            }
        }
    }
}
