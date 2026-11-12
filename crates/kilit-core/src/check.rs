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

use std::collections::HashMap;
use std::fmt::Write as _;

use std::borrow::Cow;

use crate::knowledge::ServerResults;
use crate::modify::{Change, changes};
use crate::parse::REJECTED_BY;
use crate::results::{Server, Variant, Verdict, Writes};
use crate::row;
use crate::schema::{
    CheckTarget, ConstraintKind, Gone, GoneHow, InAnotherCase, Role, Schema, TableState,
    creates_temporary, has_descending, names_foreign_key_index, same_in_any_case,
    with_table_renamed,
};
use crate::statement::{
    Alter, Check, CheckColumns, Column, DataType, Engine, ForeignKey, ForeignKeyAction, Index,
    IndexKind, IntegerSize, Lock, Operation, Position, RowFormat, Statement, StatementKind, Table,
    TableOptions, Variable,
};

/// What kilit says about one statement of the input.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Finding {
    pub line: usize,
    pub sql: String,
    pub prediction: Prediction,
    /// Answering the statement used a lab result that another measured
    /// server gives differently, or a rule of kilit's that depends on the
    /// server. When no statement of the input did, every measured server
    /// answers every statement the same way.
    pub server_dependent: bool,
    /// The statement names a table in another letter case than a table
    /// kilit knows, or than another name of the statement: with
    /// lower_case_table_names = 0 it is another table, with 1 or 2 the same
    /// one. The statement is on no new table; how kilit follows it says
    /// which setting the migration goes on with.
    pub letter_case: Option<LetterCase>,
    /// What kilit knew of the table the statement is on (see [`table_of`])
    /// right before the statement.
    pub table_state: Option<TableSeen>,
}

/// What kilit knows of a table at a statement.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TableSeen {
    /// From the `--schema` dump, or from a statement before.
    Known,
    /// kilit lost track of it at a statement it could not follow.
    Lost,
    /// No `--schema` and no statement before shows it.
    Unseen,
}

/// The one table `statement` is on, as written; `None` for a statement on
/// no table or on several.
pub fn table_of(statement: &Statement) -> Option<&str> {
    match &statement.kind {
        StatementKind::Alter(alter) => Some(&alter.table),
        StatementKind::CreateTable { table, .. } => Some(&table.name),
        StatementKind::DropTable { tables } => match tables.as_slice() {
            [table] => Some(table),
            _ => None,
        },
        StatementKind::RenameTable { renames } => match renames.as_slice() {
            [rename] => Some(&rename.from),
            _ => None,
        },
        StatementKind::Unparsed { table, .. } => table.as_deref(),
        StatementKind::Set { .. } | StatementKind::Other => None,
    }
}

/// How kilit follows a statement that names a table in another letter case:
/// as the setting of lower_case_table_names under which the migration goes
/// on. By hand on 8.0.46 and 8.4.13 (0 and 1), each form fails under one
/// setting, which stops the migration there, or runs under both.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LetterCase {
    /// It fails with 0 (1146, 1051, 1049, or 1824 for a foreign key's
    /// parent with the checks on), so the migration goes on only with 1 or
    /// 2, where `written` is the table `known`.
    SameTable { written: String, known: String },
    /// It fails with 1 (1050: CREATE TABLE, or RENAME TABLE to a name in
    /// another case), so the migration goes on only with 0, where the name
    /// as written is a table of its own; or it runs with both and leaves
    /// the same tables (a foreign key whose checks may be off).
    AsWritten,
    /// It runs with both and leaves different tables, or the form has no
    /// hand test under both: kilit stops trusting what it may change.
    Unclear,
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
    Unknown {
        reason: String,
        /// What kilit knows that may help: on a table it does not know,
        /// each measured situation that would change the likely answer.
        notes: Vec<String>,
    },
    /// Not something kilit checks: a new table, a data change, a setting.
    NotChecked { reason: String },
    /// On a table kilit does not know, every answer the lab measured for
    /// the tables it may be makes writes wait, or every one fails: kilit says
    /// that, and the algorithm and the rebuild depend on the table. See
    /// [`Shared`].
    DependsOnTable {
        /// The case of the change on a plain InnoDB table.
        case: &'static str,
        /// What every measured answer shares.
        shared: Shared,
        /// The scenarios of that case.
        evidence: Vec<&'static str>,
        /// Why kilit does not know the table, and the plain table's answer.
        reason: String,
        /// Each answer the table could give, and what kilit cannot see.
        notes: Vec<String>,
    },
}

/// What every answer the lab measured for a table kilit does not know
/// shares. Writes continuing is never one: a safe answer needs every engine
/// and row format measured, and such a table may have one the lab never
/// measured (MEMORY, KEY_BLOCK_SIZE=4, ...).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Shared {
    /// No answer lets writes continue: each one waits, or fails.
    WritesWait,
    /// Every answer fails, with an error that depends on the table.
    Fails,
}

/// A kind of change, with the lab scenarios that measured it.
#[derive(Debug)]
pub struct Case {
    pub id: &'static str,
    /// Scenarios that measure this change. On each server they must give
    /// the same result as each other, or kilit refuses to answer; a test
    /// checks it. Two servers may give different results.
    pub scenarios: &'static [&'static str],
    /// The same change measured in a situation where the answer differs.
    pub exceptions: &'static [Exception],
}

#[derive(Debug)]
pub struct Exception {
    /// Scenarios that measure this situation, in each shape the lab tried.
    /// Like a case's, they must give the same result on each server.
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
    /// The new index takes a TEXT or BLOB column whole, without a prefix
    /// length. Only those types count: CHAR, ENUM and SET are not TEXT,
    /// and JSON is not BLOB.
    BlobOrTextWithoutPrefix,
    /// The dropped index is the only one that serves a foreign key of the
    /// table: the key's columns are the first columns of no other index,
    /// and not of the primary key.
    OnlyIndexOfForeignKey,
    /// A generated column of the table uses the changed column, in an
    /// expression of column names, numbers and arithmetic operators.
    GeneratedColumnSource,
    /// A CHECK constraint of the table uses the changed column and another
    /// column, in a plain expression: see [`CheckColumns::Plain`].
    CheckWithAnotherColumn,
    /// A CHECK constraint of the table uses the changed column, with other
    /// columns or alone, in a plain expression.
    CheckUsesColumn,
    /// What the schema shows about the foreign key the statement adds, when
    /// both tables are known: see [`KeyProblem`].
    NewForeignKey(KeyProblem),
    /// The dropped column is the table's whole primary key.
    PrimaryKeyAlone,
    /// The dropped column is the second column of a primary key of two.
    PrimaryKeySecondOfTwo,
    /// The table has no column, index or foreign key of the name the
    /// statement drops (error 1091). kilit never answers from it: its tables
    /// may be out of date, so on a known table the drop is unknown, with the
    /// measurement in the reason, and the table stays known.
    NoSuchName,
    /// The statement names the CHECK constraint with `DROP CONSTRAINT` or
    /// `ALTER CONSTRAINT`, not `DROP CHECK` or `ALTER CHECK`: MySQL refuses
    /// a name the table does not have with another error then. The
    /// statement shows it, whatever kilit knows of the table.
    ConstraintClause,
    /// The column whose default the statement sets is TEXT, BLOB or JSON,
    /// and the default a quoted literal, which MySQL refuses (error 1101).
    BlobDefault,
    /// The new column, or the longer one, makes the row longer than the
    /// 65,535 bytes MySQL allows, as kilit counts it from the table.
    RowTooLarge,
    /// The foreign key the statement adds sets its one column to NULL on
    /// delete, and the column is NOT NULL (error 1830).
    SetNullOnNotNull,
    /// The new FULLTEXT index takes one TEXT column in the binary character
    /// set, the column's or the table's, which MySQL makes a BLOB (error
    /// 1283).
    BinaryText,
    /// Something no schema shows: the rows, or the table's history.
    Hidden,
}

/// Why MySQL refuses a new foreign key, for a reason the schema shows. Each
/// counts only for a key of one column on another table, in the exact shape
/// the lab measured. Any other difference gets no answer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KeyProblem {
    /// The column is INT, INT UNSIGNED, BIGINT, or a SMALLINT, TINYINT or
    /// MEDIUMINT that is unsigned, and the column it points at is BIGINT
    /// UNSIGNED.
    IntegerMismatch,
    /// The column is BIGINT UNSIGNED and the column it points at INT UNSIGNED.
    NarrowerParent,
    /// Two VARCHAR columns of one length: the column it points at is utf8mb4
    /// with the default collation, and the other is latin1 with the default
    /// collation or utf8mb4_bin.
    TextMismatch,
    /// No index of the parent table starts with the column, or only an index
    /// on a part of it, such as `name(10)`. A column second in an index does
    /// not count.
    NoIndex,
    /// The one index that starts with the column is a plain index of it, or
    /// an index of it and one more column.
    NonUniqueIndex,
}

const VALUE_OUT_OF_RANGE: &str =
    "if a value does not fit the new type (in strict SQL mode, the default)";
const VALUE_TOO_LONG: &str =
    "if a value is longer than the new length (in strict SQL mode, the default)";
const NULL_IN_A_ROW: &str = "if a row holds NULL (in strict SQL mode, the default)";
const DUPLICATE_VALUES: &str = "if the columns hold duplicate values";
const ORPHAN_ROW: &str = "if a row points to a parent row that does not exist";
const ORPHAN_ROW_KEPT: &str =
    "if a row points to a parent row that does not exist (the row is kept unchecked)";
const ROW_BREAKS_CHECK: &str = "if a row breaks the constraint";
const PRIMARY_KEY_DUPLICATES: &str =
    "if the column is the second of a primary key of two, and the first holds duplicate values";

/// The measured situations that depend on the rows a table holds. A table
/// with no rows is in none of them.
const ROW_SITUATIONS: [&str; 8] = [
    VALUE_OUT_OF_RANGE,
    VALUE_TOO_LONG,
    NULL_IN_A_ROW,
    DUPLICATE_VALUES,
    ORPHAN_ROW,
    ORPHAN_ROW_KEPT,
    ROW_BREAKS_CHECK,
    PRIMARY_KEY_DUPLICATES,
];

/// Whether `note`, one of the notes of a [`Prediction::Measured`], tells
/// what happens if the table holds certain rows. Such a note says nothing
/// about a table that has no rows.
pub fn depends_on_rows(note: &str) -> bool {
    ROW_SITUATIONS.iter().any(|situation| {
        note.strip_prefix(situation)
            .is_some_and(|rest| rest.starts_with(": "))
    })
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
        // The same change on a table whose foreign key cascades or sets NULL
        // on delete, or that another table's key points at with ON DELETE
        // CASCADE. MySQL 8.0.46 and 8.4.13 take each one as without the key.
        "on-delete-cascade-add-nullable-column",
        "on-delete-set-null-add-nullable-column",
        "cascade-parent-add-nullable-column",
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
        Exception {
            scenarios: &["add-column-over-row-size-limit"],
            when: ROW_TOO_LARGE,
            conditions: &[Condition::RowTooLarge],
        },
    ],
};

const ROW_TOO_LARGE: &str = "if the row grows past the 65,535 bytes MySQL allows";

/// A new TEXT, BLOB, JSON or GEOMETRY column with a quoted default: MySQL
/// refuses it (error 1101) in strict SQL mode, and refuses all but `''` on
/// TEXT and BLOB in the others. See [`refused_default`].
const ADD_COLUMN_BLOB_LITERAL_DEFAULT: Case = Case {
    id: "add-column-blob-literal-default",
    scenarios: &[
        "add-column-text-default-literal",
        "add-column-text-nullable-default-literal",
        "add-column-blob-default-literal",
        "add-column-json-default-literal",
        "add-column-geometry-default-literal",
        "add-column-text-nullable-default-literal-sql-mode-empty",
        "add-column-text-nullable-default-literal-sql-mode-not-strict",
        "add-column-json-default-literal-sql-mode-empty",
        "add-column-json-default-literal-sql-mode-not-strict",
        "add-column-geometry-default-literal-sql-mode-empty",
        "add-column-geometry-default-literal-sql-mode-not-strict",
    ],
    exceptions: &[],
};

/// A new TEXT or BLOB column with `DEFAULT ''` when `sql_mode` is not
/// strict: MySQL drops the default with a warning and adds the column. The
/// lab measured it on a plain table only; see [`refused_default`].
const ADD_COLUMN_BLOB_EMPTY_DEFAULT_NOT_STRICT: Case = Case {
    id: "add-column-blob-empty-default-not-strict",
    scenarios: &[
        "add-column-text-default-literal-sql-mode-empty",
        "add-column-text-default-literal-sql-mode-not-strict",
        "add-column-blob-default-literal-sql-mode-empty",
        "add-column-blob-default-literal-sql-mode-not-strict",
    ],
    exceptions: &[],
};

/// A new TEXT column with `DEFAULT ('')`, or a JSON one with `DEFAULT
/// ('[]')`: an expression, which MySQL takes on these types, by copying the
/// table, whatever the SQL mode.
const ADD_COLUMN_BLOB_EXPRESSION_DEFAULT: Case = Case {
    id: "add-column-blob-expression-default",
    scenarios: &[
        "add-column-text-default-expression",
        "add-column-text-default-expression-sql-mode-empty",
        "add-column-text-default-expression-sql-mode-not-strict",
        "add-column-json-default-expression",
        "add-column-json-default-expression-sql-mode-empty",
        "add-column-json-default-expression-sql-mode-not-strict",
    ],
    exceptions: &[],
};

/// A MODIFY or CHANGE whose new definition is a TEXT column with a quoted
/// default, in strict SQL mode (error 1101). See [`refused_default`].
const MODIFY_COLUMN_BLOB_LITERAL_DEFAULT: Case = Case {
    id: "modify-column-blob-literal-default",
    scenarios: &[
        "modify-column-text-default-literal",
        "modify-text-column-default-literal",
    ],
    exceptions: &[],
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
    scenarios: &[
        "add-column-default-uuid",
        "add-column-default-rand",
        "add-column-default-uuid-short",
        "add-column-default-uuid-short-nullable",
        "add-column-default-sysdate",
        "add-column-default-sysdate-nullable",
        "add-column-default-user",
        "add-column-default-user-nullable",
    ],
    exceptions: &[],
};

/// The expression defaults the lab measured, as kilit reads them, in
/// uppercase, with the case of each. Any other expression default is
/// unknown: a function the lab has not run may behave either way.
const EXPRESSION_DEFAULTS: [(&str, &Case); 12] = [
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
    ("(UUID_SHORT())", &ADD_COLUMN_BINLOG_UNSAFE_DEFAULT),
    ("(SYSDATE())", &ADD_COLUMN_BINLOG_UNSAFE_DEFAULT),
    ("(USER())", &ADD_COLUMN_BINLOG_UNSAFE_DEFAULT),
    // Measured on a TEXT and on a JSON column only: see [`check_default_type`].
    ("('')", &ADD_COLUMN_BLOB_EXPRESSION_DEFAULT),
    ("('[]')", &ADD_COLUMN_BLOB_EXPRESSION_DEFAULT),
];

/// Shapes the lab has not measured, such as a column in a foreign key of
/// several columns or in the primary key, are unknown: see
/// [`check_dropped_column`], [`generated_column_source`] and [`check_use`].
const DROP_COLUMN: Case = Case {
    id: "drop-column",
    // A CHECK constraint that uses only the column goes with it, and changes
    // nothing. Neither does one that does not use the column.
    scenarios: &[
        "drop-column",
        "drop-column-in-check-alone",
        "drop-column-in-not-enforced-check-alone",
        "drop-column-next-to-check",
        "drop-column-named-in-check-string",
        // The same change on a table whose foreign key cascades or sets NULL
        // on delete.
        "on-delete-cascade-drop-column",
        "on-delete-set-null-drop-column",
    ],
    exceptions: &[
        Exception {
            scenarios: &["drop-column-with-fulltext"],
            when: "if the table keeps a FULLTEXT index",
            conditions: &[Condition::FulltextIndex],
        },
        Exception {
            // The last one: the index a dropped foreign key left behind.
            scenarios: &[
                "drop-column-indexed",
                "drop-column-indexed-first-of-two",
                "drop-column-indexed-second-of-two",
                "drop-column-indexed-unique",
                "drop-column-indexed-prefix",
                "drop-column-indexed-fulltext",
                "drop-column-after-dropping-its-foreign-key",
                "drop-column-indexed-in-check-alone",
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
        // A foreign key's columns are always in an index, so the exception
        // for an indexed column holds too. This one was measured with both
        // conditions, so it decides: see [`deciding`].
        Exception {
            scenarios: &["drop-column-in-foreign-key"],
            when: "if the column is in a foreign key",
            conditions: &[Condition::IndexedColumn, Condition::ForeignKeyColumn],
        },
        Exception {
            scenarios: &[
                "drop-column-generated-source",
                "drop-column-stored-generated-source",
            ],
            when: GENERATED_COLUMN_USES_IT,
            conditions: &[Condition::GeneratedColumnSource],
        },
        Exception {
            scenarios: &[
                "drop-column-in-two-column-check",
                "drop-column-in-not-enforced-two-column-check",
            ],
            when: "if a CHECK constraint uses the column and another column",
            conditions: &[Condition::CheckWithAnotherColumn],
        },
        Exception {
            scenarios: &["drop-column-indexed-in-two-column-check"],
            when: "if the column is in an index and a CHECK constraint uses it and another column",
            conditions: &[Condition::IndexedColumn, Condition::CheckWithAnotherColumn],
        },
        // The primary key is an index, so the exception for an indexed
        // column holds too; these were measured with both conditions.
        Exception {
            scenarios: &["drop-primary-key-column"],
            when: "if the column is the primary key",
            conditions: &[Condition::IndexedColumn, Condition::PrimaryKeyAlone],
        },
        Exception {
            scenarios: &["drop-column-in-two-column-primary-key"],
            when: "if the column is the second of a primary key of two columns",
            conditions: &[Condition::IndexedColumn, Condition::PrimaryKeySecondOfTwo],
        },
        Exception {
            scenarios: &["drop-column-in-two-column-primary-key-duplicates"],
            when: PRIMARY_KEY_DUPLICATES,
            conditions: &[
                Condition::IndexedColumn,
                Condition::PrimaryKeySecondOfTwo,
                Condition::Hidden,
            ],
        },
        Exception {
            scenarios: &["drop-column-missing"],
            when: "if the table has no such column",
            conditions: &[Condition::NoSuchName],
        },
    ],
};

const GENERATED_COLUMN_USES_IT: &str = "if a generated column uses the column";

/// A `CHANGE` that renames a column a generated column or a CHECK constraint
/// uses has not been measured, and is unknown: see [`classify`]. So are the
/// shapes in [`check_use`].
const RENAME_COLUMN: Case = Case {
    id: "rename-column",
    scenarios: &[
        "rename-column",
        "change-column-rename",
        "rename-column-next-to-check",
        "rename-column-named-in-check-string",
        // The same change on a table whose foreign key cascades or sets NULL
        // on delete.
        "on-delete-cascade-rename-column",
        "on-delete-set-null-rename-column",
    ],
    exceptions: &[
        Exception {
            scenarios: &[
                "rename-column-generated-source",
                "rename-column-stored-generated-source",
            ],
            when: GENERATED_COLUMN_USES_IT,
            conditions: &[Condition::GeneratedColumnSource],
        },
        Exception {
            scenarios: &[
                "rename-column-in-two-column-check",
                "rename-column-in-check-alone",
                "rename-column-in-not-enforced-two-column-check",
                "rename-column-in-not-enforced-check-alone",
                "rename-column-indexed-in-two-column-check",
                "rename-column-indexed-in-check-alone",
            ],
            when: "if a CHECK constraint uses the column",
            conditions: &[Condition::CheckUsesColumn],
        },
    ],
};

const SET_COLUMN_DEFAULT: Case = Case {
    id: "set-column-default",
    scenarios: &[
        "set-column-default",
        // The same change on a table whose foreign key cascades or sets NULL
        // on delete.
        "on-delete-cascade-set-column-default",
        "on-delete-set-null-set-column-default",
    ],
    exceptions: &[Exception {
        scenarios: &[
            "set-column-default-text-literal",
            "set-column-default-text-literal-sql-mode-empty",
            "set-column-default-text-literal-sql-mode-not-strict",
            "set-column-default-json-literal",
        ],
        when: "if the column is TEXT, BLOB or JSON",
        conditions: &[Condition::BlobDefault],
    }],
};

const CHANGE_INTEGER_TYPE: Case = Case {
    id: "change-integer-type",
    scenarios: &[
        "int-to-bigint",
        "int-to-smallint",
        "int-to-unsigned",
        // The same change on a table whose foreign key cascades or sets NULL
        // on delete.
        "on-delete-cascade-int-to-bigint",
        "on-delete-set-null-int-to-bigint",
    ],
    exceptions: &[
        Exception {
            scenarios: &["modify-foreign-key-column"],
            when: "if the column is in a foreign key",
            conditions: &[Condition::ForeignKeyColumn],
        },
        Exception {
            scenarios: &["int-to-tinyint-out-of-range"],
            when: VALUE_OUT_OF_RANGE,
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
        // The same change on a table whose foreign key cascades or sets NULL
        // on delete.
        "on-delete-cascade-varchar-50-to-60",
        "on-delete-set-null-varchar-50-to-60",
    ],
    exceptions: &[Exception {
        scenarios: &["lengthen-varchar-over-row-size-limit"],
        when: ROW_TOO_LARGE,
        conditions: &[Condition::RowTooLarge],
    }],
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
        when: VALUE_TOO_LONG,
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
        when: NULL_IN_A_ROW,
        conditions: &[Condition::Hidden],
    }],
};

const MAKE_COLUMN_NULLABLE: Case = Case {
    id: "make-column-nullable",
    scenarios: &[
        "make-column-nullable",
        // The same change on a table with a foreign key: ON DELETE or ON UPDATE
        // CASCADE or SET NULL, none, RESTRICT or NO ACTION on its own key, or
        // CASCADE or SET NULL on a key of another table that points at it.
        // MySQL 8.0.46 and 8.4.13 take each one as without the key.
        "on-delete-cascade-make-column-nullable",
        "on-delete-set-null-make-column-nullable",
        "on-update-cascade-make-column-nullable",
        "on-update-set-null-make-column-nullable",
        "on-delete-and-update-cascade-make-column-nullable",
        "foreign-key-make-column-nullable",
        "on-delete-restrict-make-column-nullable",
        "on-delete-no-action-make-column-nullable",
        "cascade-parent-make-column-nullable",
    ],
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

/// An index on a JSON column is unknown: see [`check_json_columns`].
const ADD_INDEX: Case = Case {
    id: "add-index",
    scenarios: &[
        "add-index",
        // The same change on a table with a foreign key: ON DELETE or ON UPDATE
        // CASCADE or SET NULL, none, RESTRICT or NO ACTION on its own key, or
        // CASCADE or SET NULL on a key of another table that points at it.
        // MySQL 8.0.46 and 8.4.13 take each one as without the key.
        "on-delete-cascade-add-index",
        "on-delete-set-null-add-index",
        "on-update-cascade-add-index",
        "on-update-set-null-add-index",
        "on-delete-and-update-cascade-add-index",
        "foreign-key-add-index",
        "on-delete-restrict-add-index",
        "on-delete-no-action-add-index",
        "cascade-parent-add-index",
        "set-null-parent-add-index",
    ],
    exceptions: &[Exception {
        scenarios: &[
            "add-index-on-text",
            "add-index-on-tinytext",
            "add-index-on-mediumtext",
            "add-index-on-longtext",
            "add-index-on-blob",
            "add-index-on-text-second-of-two",
        ],
        when: BLOB_OR_TEXT_WITHOUT_PREFIX,
        conditions: &[Condition::BlobOrTextWithoutPrefix],
    }],
};

const BLOB_OR_TEXT_WITHOUT_PREFIX: &str =
    "if the index takes a TEXT or BLOB column without a prefix length";

/// The one functional index the lab measured, on `LOWER()` of a VARCHAR
/// column: see [`check_functional_index`].
const ADD_FUNCTIONAL_INDEX: Case = Case {
    id: "add-functional-index",
    scenarios: &["add-functional-index"],
    exceptions: &[],
};

const ADD_UNIQUE_INDEX: Case = Case {
    id: "add-unique-index",
    scenarios: &[
        "add-unique-index",
        // The same change on a table whose foreign key cascades or sets NULL
        // on delete.
        "on-delete-cascade-add-unique-index",
        "on-delete-set-null-add-unique-index",
    ],
    exceptions: &[
        Exception {
            scenarios: &["add-unique-index-duplicates"],
            when: DUPLICATE_VALUES,
            conditions: &[Condition::Hidden],
        },
        Exception {
            scenarios: &[
                "add-unique-index-on-text",
                "add-unique-index-on-tinytext",
                "add-unique-index-on-mediumtext",
                "add-unique-index-on-longtext",
                "add-unique-index-on-blob",
            ],
            when: BLOB_OR_TEXT_WITHOUT_PREFIX,
            conditions: &[Condition::BlobOrTextWithoutPrefix],
        },
    ],
};

const ADD_FULLTEXT_INDEX: Case = Case {
    id: "add-fulltext-index",
    scenarios: &[
        "add-fulltext-index",
        // On one TEXT, MEDIUMTEXT or LONGTEXT column, or on a VARCHAR and a
        // TEXT one; and with a foreign key that cascades or sets NULL on
        // delete.
        "on-delete-cascade-add-fulltext-index",
        "on-delete-set-null-add-fulltext-index",
        "add-fulltext-index-text",
        "add-fulltext-index-mediumtext",
        "add-fulltext-index-longtext",
        "add-fulltext-index-two-columns",
    ],
    exceptions: &[
        Exception {
            scenarios: &["add-second-fulltext-index"],
            when: "if the table already has a FULLTEXT index",
            conditions: &[Condition::FulltextIndex],
        },
        Exception {
            scenarios: &["add-fulltext-index-binary-text"],
            when: BINARY_TEXT,
            conditions: &[Condition::BinaryText],
        },
    ],
};

const BINARY_TEXT: &str =
    "if the column is TEXT in the binary character set, which MySQL makes a BLOB";

/// A FULLTEXT index with ASC or DESC written on a key part: refused
/// whatever the table, as written and in every variant. By hand on MySQL
/// 8.0.46 and 8.4.13: so on MyISAM and COMPACT tables, and by CREATE
/// FULLTEXT INDEX and CREATE TABLE too.
const ADD_FULLTEXT_INDEX_WITH_ORDER: Case = Case {
    id: "add-fulltext-index-with-order",
    scenarios: &["add-fulltext-index-desc", "add-fulltext-index-asc"],
    exceptions: &[],
};

/// Shapes the lab has not measured, and an index MySQL may have replaced,
/// are unknown: see [`only_index_of_foreign_key`].
const DROP_INDEX: Case = Case {
    id: "drop-index",
    scenarios: &[
        "drop-index",
        "drop-fulltext-index",
        "drop-unique-index",
        // The index of a foreign key, after dropping the key.
        "drop-index-after-dropping-its-foreign-key",
        // An index of a foreign key, when something else serves the key.
        "drop-index-foreign-key-keeps-second-index",
        "drop-index-foreign-key-keeps-first-index",
        "drop-index-foreign-key-keeps-primary-key",
        // The same change on a table whose foreign key cascades or sets NULL
        // on delete.
        "on-delete-cascade-drop-index",
        "on-delete-set-null-drop-index",
    ],
    exceptions: &[
        Exception {
            scenarios: &[
                "drop-index-needed-by-foreign-key",
                "drop-index-needed-by-foreign-key-two-column-index",
                "drop-index-needed-by-foreign-key-automatic",
                "drop-index-needed-by-foreign-key-checks-off",
                "drop-index-needed-by-two-column-foreign-key",
            ],
            when: "if the index is the only one that serves a foreign key",
            conditions: &[Condition::OnlyIndexOfForeignKey],
        },
        Exception {
            scenarios: &["drop-index-missing"],
            when: "if the table has no such index",
            conditions: &[Condition::NoSuchName],
        },
    ],
};

const ADD_FOREIGN_KEY: Case = Case {
    id: "add-foreign-key",
    scenarios: &[
        "add-foreign-key",
        "add-foreign-key-new-column",
        "add-foreign-key-parent-unique-index",
        "add-foreign-key-string-shorter",
        "add-foreign-key-string-longer",
        "add-foreign-key-parent-unique-desc-index",
        "add-foreign-key-parent-nullable-unique-index",
        "add-foreign-key-parent-unique-and-plain-index",
        "add-foreign-key-parent-unique-and-longer-index",
        "add-foreign-key-parent-unique-and-second-column-index",
        "add-foreign-key-two-columns",
        "add-foreign-key-char-to-varchar",
        "add-foreign-key-shorter-char-to-varchar",
        "add-foreign-key-varchar-to-char",
        "add-foreign-key-char-to-char",
        "add-foreign-key-self-reference",
        // The key with ON DELETE CASCADE, SET NULL (on a nullable column) or
        // SET DEFAULT, which MySQL 8.0.46 and 8.4.13 take alike.
        "add-foreign-key-on-delete-cascade",
        "add-foreign-key-on-delete-set-null",
        "add-foreign-key-on-delete-set-default",
    ],
    exceptions: &[
        Exception {
            scenarios: &["add-foreign-key-orphans"],
            when: ORPHAN_ROW,
            conditions: &[Condition::Hidden],
        },
        Exception {
            scenarios: &["add-foreign-key-set-null-not-null"],
            when: SET_NULL_ON_NOT_NULL,
            conditions: &[Condition::SetNullOnNotNull],
        },
        Exception {
            scenarios: &[
                "add-foreign-key-type-int",
                "add-foreign-key-type-int-unsigned",
                "add-foreign-key-type-bigint",
                "add-foreign-key-type-smallint-unsigned",
                "add-foreign-key-type-tinyint-unsigned",
                "add-foreign-key-type-mediumint-unsigned",
            ],
            when: "if the parent column is BIGINT UNSIGNED and this one is INT, INT UNSIGNED, \
                   BIGINT or a smaller unsigned integer",
            conditions: &[Condition::NewForeignKey(KeyProblem::IntegerMismatch)],
        },
        Exception {
            scenarios: &["add-foreign-key-type-parent-int-unsigned"],
            when: "if the parent column is INT UNSIGNED and this one BIGINT UNSIGNED",
            conditions: &[Condition::NewForeignKey(KeyProblem::NarrowerParent)],
        },
        Exception {
            scenarios: &[
                "add-foreign-key-string-latin1",
                "add-foreign-key-string-collation-bin",
            ],
            when: "if the parent column is utf8mb4 with the default collation and this one \
                   latin1 or utf8mb4_bin",
            conditions: &[Condition::NewForeignKey(KeyProblem::TextMismatch)],
        },
        Exception {
            scenarios: &[
                "add-foreign-key-no-parent-index",
                "add-foreign-key-parent-second-column-index",
                "add-foreign-key-parent-column-prefix-index",
                "add-foreign-key-parent-unique-column-prefix-index",
            ],
            when: "if no index of the parent table starts with the whole parent column",
            conditions: &[Condition::NewForeignKey(KeyProblem::NoIndex)],
        },
        Exception {
            scenarios: &[
                "add-foreign-key-parent-plain-index",
                "add-foreign-key-parent-longer-index",
                "add-foreign-key-parent-longer-unique-index",
                "add-foreign-key-parent-plain-desc-index",
            ],
            when: "if the index that starts with the parent column is not unique on that \
                   column alone",
            conditions: &[Condition::NewForeignKey(KeyProblem::NonUniqueIndex)],
        },
    ],
};

const ADD_FOREIGN_KEY_CHECKS_OFF: Case = Case {
    id: "add-foreign-key-checks-off",
    scenarios: &[
        "add-foreign-key-checks-off",
        // The key with ON DELETE CASCADE or SET NULL (on a nullable column).
        "add-foreign-key-checks-off-on-delete-cascade",
        "add-foreign-key-checks-off-on-delete-set-null",
    ],
    exceptions: &[
        Exception {
            scenarios: &["add-foreign-key-checks-off-orphans"],
            when: ORPHAN_ROW_KEPT,
            conditions: &[Condition::Hidden],
        },
        Exception {
            scenarios: &["add-foreign-key-checks-off-set-null-not-null"],
            when: SET_NULL_ON_NOT_NULL,
            conditions: &[Condition::SetNullOnNotNull],
        },
    ],
};

const SET_NULL_ON_NOT_NULL: &str =
    "if the column is NOT NULL and the key sets it to NULL on delete";

const DROP_FOREIGN_KEY: Case = Case {
    id: "drop-foreign-key",
    scenarios: &[
        "drop-foreign-key",
        // The same change on a table whose foreign key cascades or sets NULL
        // on delete.
        "on-delete-cascade-drop-foreign-key",
        "on-delete-set-null-drop-foreign-key",
    ],
    exceptions: &[Exception {
        scenarios: &["drop-foreign-key-missing"],
        when: "if the table has no such foreign key",
        conditions: &[Condition::NoSuchName],
    }],
};

/// An enforced CHECK constraint with a plain expression over columns of the
/// table: see [`check_new_constraint`].
const ADD_CHECK_CONSTRAINT: Case = Case {
    id: "add-check-constraint",
    scenarios: &[
        "add-check-constraint",
        "add-check-constraint-enforced",
        "add-check-constraint-without-a-name",
        "add-check-constraint-two-columns",
    ],
    exceptions: &[Exception {
        scenarios: &["add-check-constraint-rows-break-it"],
        when: ROW_BREAKS_CHECK,
        conditions: &[Condition::Hidden],
    }],
};

/// A NOT ENFORCED CHECK constraint, under the conditions of
/// [`ADD_CHECK_CONSTRAINT`]. MySQL does not read the rows for it.
const ADD_NOT_ENFORCED_CHECK_CONSTRAINT: Case = Case {
    id: "add-not-enforced-check-constraint",
    scenarios: &[
        "add-check-constraint-not-enforced",
        "add-check-constraint-not-enforced-rows-break-it",
    ],
    exceptions: &[],
};

/// `DROP CONSTRAINT` and `DROP CHECK` of a CHECK constraint: see
/// [`dropped_constraint`] and [`named_check`].
const DROP_CHECK_CONSTRAINT: Case = Case {
    id: "drop-check-constraint",
    scenarios: &[
        "drop-constraint-check",
        "drop-constraint-two-column-check",
        "drop-constraint-not-enforced-check",
        "drop-check",
        "drop-check-two-column",
        "drop-check-not-enforced",
    ],
    exceptions: &[
        Exception {
            scenarios: &["drop-check-missing"],
            when: "if the table has no CHECK constraint of the name",
            conditions: &[Condition::NoSuchName],
        },
        Exception {
            scenarios: &["drop-constraint-missing"],
            when: "if DROP CONSTRAINT names no constraint of the table",
            conditions: &[Condition::NoSuchName, Condition::ConstraintClause],
        },
    ],
};

/// `ALTER CHECK` or `ALTER CONSTRAINT` of a CHECK constraint, `NOT
/// ENFORCED` whatever it was, or `ENFORCED` when it already is: see
/// [`named_check`].
const ALTER_CHECK_CONSTRAINT: Case = Case {
    id: "alter-check-constraint",
    scenarios: &[
        "alter-check-not-enforced",
        "alter-check-not-enforced-again",
        "alter-check-enforced-again",
        "alter-constraint-not-enforced",
        "alter-constraint-not-enforced-again",
        "alter-constraint-enforced-again",
    ],
    exceptions: &[
        Exception {
            scenarios: &["alter-check-missing"],
            when: "if the table has no CHECK constraint of the name",
            conditions: &[Condition::NoSuchName],
        },
        Exception {
            scenarios: &["alter-constraint-missing"],
            when: "if ALTER CONSTRAINT names no constraint of the table",
            conditions: &[Condition::NoSuchName, Condition::ConstraintClause],
        },
    ],
};

/// `ALTER CHECK` or `ALTER CONSTRAINT ... ENFORCED` of a NOT ENFORCED CHECK
/// constraint: MySQL reads every row, as for a new one.
const ENFORCE_CHECK_CONSTRAINT: Case = Case {
    id: "enforce-check-constraint",
    scenarios: &["alter-check-enforced", "alter-constraint-enforced"],
    exceptions: &[Exception {
        scenarios: &[
            "alter-check-enforced-rows-break-it",
            "alter-constraint-enforced-rows-break-it",
        ],
        when: ROW_BREAKS_CHECK,
        conditions: &[Condition::Hidden],
    }],
};

/// `COMMENT = '...'` as the statement's only change. A change of ENGINE,
/// ROW_FORMAT or KEY_BLOCK_SIZE has not been measured.
const SET_TABLE_COMMENT: Case = Case {
    id: "set-table-comment",
    scenarios: &["set-table-comment"],
    exceptions: &[],
};

/// The most characters a table comment holds in MySQL 8.0 and 8.4.
const MAX_TABLE_COMMENT: usize = 2048;

/// Every case kilit knows.
pub const CASES: [&Case; 36] = [
    &ADD_COLUMN,
    &ADD_COLUMN_EXPRESSION_DEFAULT,
    &ADD_COLUMN_BINLOG_UNSAFE_DEFAULT,
    &ADD_COLUMN_BLOB_LITERAL_DEFAULT,
    &ADD_COLUMN_BLOB_EMPTY_DEFAULT_NOT_STRICT,
    &ADD_COLUMN_BLOB_EXPRESSION_DEFAULT,
    &MODIFY_COLUMN_BLOB_LITERAL_DEFAULT,
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
    &ADD_FULLTEXT_INDEX_WITH_ORDER,
    &DROP_INDEX,
    &ADD_FOREIGN_KEY,
    &ADD_FOREIGN_KEY_CHECKS_OFF,
    &DROP_FOREIGN_KEY,
    &ADD_CHECK_CONSTRAINT,
    &ADD_NOT_ENFORCED_CHECK_CONSTRAINT,
    &DROP_CHECK_CONSTRAINT,
    &ALTER_CHECK_CONSTRAINT,
    &ENFORCE_CHECK_CONSTRAINT,
    &SET_TABLE_COMMENT,
];

/// MyISAM tables. MySQL can change only a MyISAM table's metadata in place,
/// so the lab measured each change on its own: no answer for InnoDB tables
/// is ever given to a MyISAM table, and one change measured on MyISAM says
/// nothing about another. See [`ON_MYISAM`] and [`myisam_case`].
const MYISAM_ADD_COLUMN: Case = Case {
    id: "myisam-add-column",
    scenarios: &[
        "myisam-add-nullable-column",
        "myisam-add-column-not-null-default",
    ],
    exceptions: &[],
};

/// A column in a plain index of its own, such as the one MySQL adds for a
/// foreign key it ignores, was measured too.
const MYISAM_DROP_COLUMN: Case = Case {
    id: "myisam-drop-column",
    scenarios: &["myisam-drop-column", "myisam-drop-column-in-foreign-key"],
    exceptions: &[],
};

const MYISAM_RENAME_COLUMN: Case = Case {
    id: "myisam-rename-column",
    scenarios: &["myisam-rename-column"],
    exceptions: &[],
};

const MYISAM_SET_COLUMN_DEFAULT: Case = Case {
    id: "myisam-set-column-default",
    scenarios: &["myisam-set-default"],
    exceptions: &[],
};

/// INT to BIGINT, both signed, the one pair measured.
const MYISAM_CHANGE_INTEGER_TYPE: Case = Case {
    id: "myisam-change-integer-type",
    scenarios: &["myisam-modify-int-to-bigint"],
    exceptions: &[],
};

/// A key of one integer column, or of one utf8mb4 VARCHAR column of at most
/// 1000 bytes: see [`myisam_key`].
const MYISAM_ADD_INDEX: Case = Case {
    id: "myisam-add-index",
    scenarios: &["myisam-add-index", "myisam-add-index-key-at-limit"],
    exceptions: &[],
};

const MYISAM_ADD_UNIQUE_INDEX: Case = Case {
    id: "myisam-add-unique-index",
    scenarios: &[
        "myisam-add-unique-index",
        "myisam-add-unique-index-key-at-limit",
    ],
    exceptions: &[Exception {
        scenarios: &["myisam-add-unique-index-duplicates"],
        when: DUPLICATE_VALUES,
        conditions: &[Condition::Hidden],
    }],
};

/// A MyISAM key holds at most 1000 bytes: one utf8mb4 VARCHAR column
/// longer than 250 characters.
const MYISAM_ADD_KEY_TOO_LONG: Case = Case {
    id: "myisam-add-key-too-long",
    scenarios: &[
        "myisam-add-index-key-over-limit",
        "myisam-add-unique-index-key-over-limit",
    ],
    exceptions: &[],
};

const MYISAM_ADD_FULLTEXT_INDEX: Case = Case {
    id: "myisam-add-fulltext-index",
    scenarios: &[
        "myisam-add-fulltext-index",
        "myisam-add-fulltext-index-text",
    ],
    exceptions: &[],
};

const MYISAM_DROP_INDEX: Case = Case {
    id: "myisam-drop-index",
    scenarios: &["myisam-drop-index"],
    exceptions: &[],
};

/// MySQL keeps no foreign key on a MyISAM table and checks nothing about
/// it: a type InnoDB refuses and a parent column without an index were
/// measured too. It adds the key's index: see [`myisam_case`].
const MYISAM_ADD_FOREIGN_KEY: Case = Case {
    id: "myisam-add-foreign-key",
    scenarios: &[
        "myisam-add-foreign-key",
        "myisam-add-foreign-key-type-mismatch",
        "myisam-add-foreign-key-no-parent-index",
        "myisam-add-foreign-key-indexed-column",
    ],
    exceptions: &[],
};

/// MySQL drops a foreign key of any name from a MyISAM table, as it keeps none.
const MYISAM_DROP_FOREIGN_KEY: Case = Case {
    id: "myisam-drop-foreign-key",
    scenarios: &["myisam-drop-foreign-key"],
    exceptions: &[],
};

/// Every case of a MyISAM table.
pub const MYISAM_CASES: [&Case; 12] = [
    &MYISAM_ADD_COLUMN,
    &MYISAM_DROP_COLUMN,
    &MYISAM_RENAME_COLUMN,
    &MYISAM_SET_COLUMN_DEFAULT,
    &MYISAM_CHANGE_INTEGER_TYPE,
    &MYISAM_ADD_INDEX,
    &MYISAM_ADD_UNIQUE_INDEX,
    &MYISAM_ADD_KEY_TOO_LONG,
    &MYISAM_ADD_FULLTEXT_INDEX,
    &MYISAM_DROP_INDEX,
    &MYISAM_ADD_FOREIGN_KEY,
    &MYISAM_DROP_FOREIGN_KEY,
];

/// For each InnoDB case measured on a MyISAM table too: its MyISAM case, and
/// the conditions of the InnoDB case's exceptions that the MyISAM scenarios
/// measured. Any other condition that holds, or may hold, is a situation
/// the lab has not measured on MyISAM, except the hidden ones, which are
/// InnoDB's: the MyISAM case lists its own.
const ON_MYISAM: [(&str, &Case, &[Condition]); 11] = [
    ("add-column", &MYISAM_ADD_COLUMN, &[]),
    (
        "drop-column",
        &MYISAM_DROP_COLUMN,
        &[Condition::IndexedColumn],
    ),
    ("rename-column", &MYISAM_RENAME_COLUMN, &[]),
    ("set-column-default", &MYISAM_SET_COLUMN_DEFAULT, &[]),
    ("change-integer-type", &MYISAM_CHANGE_INTEGER_TYPE, &[]),
    ("add-index", &MYISAM_ADD_INDEX, &[]),
    ("add-unique-index", &MYISAM_ADD_UNIQUE_INDEX, &[]),
    ("add-fulltext-index", &MYISAM_ADD_FULLTEXT_INDEX, &[]),
    ("drop-index", &MYISAM_DROP_INDEX, &[]),
    (
        "add-foreign-key",
        &MYISAM_ADD_FOREIGN_KEY,
        &[
            Condition::NewForeignKey(KeyProblem::IntegerMismatch),
            Condition::NewForeignKey(KeyProblem::NoIndex),
        ],
    ),
    ("drop-foreign-key", &MYISAM_DROP_FOREIGN_KEY, &[]),
];

/// Tables with a row format other than DYNAMIC, the server's default, and
/// changes run with `old_alter_table` on. The lab measured each change on
/// its own: no answer measured on DYNAMIC, or with old_alter_table off, is
/// given to them, and one change measured there says nothing about another.
/// See [`Measured`] and [`measured_case`].
///
/// MySQL refuses INSTANT ADD and DROP COLUMN on a COMPRESSED table, and
/// rebuilds it in place instead. `KEY_BLOCK_SIZE=8` without `ROW_FORMAT`
/// makes a table COMPRESSED too.
const COMPRESSED_ADD_COLUMN: Case = Case {
    id: "compressed-add-column",
    scenarios: &[
        "compressed-add-nullable-column",
        "key-block-size-add-nullable-column",
    ],
    exceptions: &[],
};

const COMPRESSED_DROP_COLUMN: Case = Case {
    id: "compressed-drop-column",
    scenarios: &["compressed-drop-column"],
    exceptions: &[],
};

const COMPRESSED_RENAME_COLUMN: Case = Case {
    id: "compressed-rename-column",
    scenarios: &["compressed-rename-column"],
    exceptions: &[],
};

const COMPRESSED_SET_COLUMN_DEFAULT: Case = Case {
    id: "compressed-set-column-default",
    scenarios: &["compressed-set-column-default"],
    exceptions: &[],
};

const COMPRESSED_CHANGE_INTEGER_TYPE: Case = Case {
    id: "compressed-change-integer-type",
    scenarios: &["compressed-int-to-bigint"],
    exceptions: &[],
};

const COMPRESSED_ADD_INDEX: Case = Case {
    id: "compressed-add-index",
    scenarios: &["compressed-add-index"],
    exceptions: &[],
};

const COMPRESSED_ADD_UNIQUE_INDEX: Case = Case {
    id: "compressed-add-unique-index",
    scenarios: &["compressed-add-unique-index"],
    exceptions: &[],
};

const COMPRESSED_DROP_INDEX: Case = Case {
    id: "compressed-drop-index",
    scenarios: &["compressed-drop-index"],
    exceptions: &[],
};

/// A FULLTEXT index on one TEXT column: on COMPRESSED, COMPACT and
/// REDUNDANT tables alike, MySQL rebuilds the table in place and writes
/// wait, as on a DYNAMIC one.
const COMPRESSED_ADD_FULLTEXT_INDEX: Case = Case {
    id: "compressed-add-fulltext-index",
    scenarios: &["compressed-add-fulltext-index"],
    exceptions: &[],
};

const COMPACT_ADD_COLUMN: Case = Case {
    id: "compact-add-column",
    scenarios: &["compact-add-nullable-column"],
    exceptions: &[],
};

const COMPACT_DROP_COLUMN: Case = Case {
    id: "compact-drop-column",
    scenarios: &["compact-drop-column"],
    exceptions: &[],
};

const COMPACT_RENAME_COLUMN: Case = Case {
    id: "compact-rename-column",
    scenarios: &["compact-rename-column"],
    exceptions: &[],
};

const COMPACT_SET_COLUMN_DEFAULT: Case = Case {
    id: "compact-set-column-default",
    scenarios: &["compact-set-column-default"],
    exceptions: &[],
};

const COMPACT_CHANGE_INTEGER_TYPE: Case = Case {
    id: "compact-change-integer-type",
    scenarios: &["compact-int-to-bigint"],
    exceptions: &[],
};

/// A key of one integer column, or of one utf8mb4 VARCHAR column of at most
/// 767 bytes: see [`key_bytes`].
const COMPACT_ADD_INDEX: Case = Case {
    id: "compact-add-index",
    scenarios: &["compact-add-index", "compact-add-index-key-at-limit"],
    exceptions: &[],
};

const COMPACT_ADD_UNIQUE_INDEX: Case = Case {
    id: "compact-add-unique-index",
    scenarios: &["compact-add-unique-index-key-at-limit"],
    exceptions: &[],
};

const COMPACT_DROP_INDEX: Case = Case {
    id: "compact-drop-index",
    scenarios: &["compact-drop-index"],
    exceptions: &[],
};

/// A key of a COMPACT table holds at most 767 bytes: one utf8mb4 VARCHAR
/// column longer than 191 characters, such as `email` VARCHAR(255).
const COMPACT_ADD_KEY_TOO_LONG: Case = Case {
    id: "compact-add-key-too-long",
    scenarios: &[
        "compact-add-index-key-over-limit",
        "compact-add-unique-index-key-over-limit",
        "compact-add-unique-index",
    ],
    exceptions: &[],
};

const COMPACT_ADD_FULLTEXT_INDEX: Case = Case {
    id: "compact-add-fulltext-index",
    scenarios: &["compact-add-fulltext-index"],
    exceptions: &[],
};

const REDUNDANT_ADD_COLUMN: Case = Case {
    id: "redundant-add-column",
    scenarios: &["redundant-add-nullable-column"],
    exceptions: &[],
};

const REDUNDANT_DROP_COLUMN: Case = Case {
    id: "redundant-drop-column",
    scenarios: &["redundant-drop-column"],
    exceptions: &[],
};

const REDUNDANT_RENAME_COLUMN: Case = Case {
    id: "redundant-rename-column",
    scenarios: &["redundant-rename-column"],
    exceptions: &[],
};

const REDUNDANT_SET_COLUMN_DEFAULT: Case = Case {
    id: "redundant-set-column-default",
    scenarios: &["redundant-set-column-default"],
    exceptions: &[],
};

const REDUNDANT_CHANGE_INTEGER_TYPE: Case = Case {
    id: "redundant-change-integer-type",
    scenarios: &["redundant-int-to-bigint"],
    exceptions: &[],
};

/// A key of one integer column, or of one utf8mb4 VARCHAR column of at most
/// 767 bytes: see [`key_bytes`].
const REDUNDANT_ADD_INDEX: Case = Case {
    id: "redundant-add-index",
    scenarios: &["redundant-add-index", "redundant-add-index-key-at-limit"],
    exceptions: &[],
};

const REDUNDANT_ADD_UNIQUE_INDEX: Case = Case {
    id: "redundant-add-unique-index",
    scenarios: &["redundant-add-unique-index-key-at-limit"],
    exceptions: &[],
};

const REDUNDANT_DROP_INDEX: Case = Case {
    id: "redundant-drop-index",
    scenarios: &["redundant-drop-index"],
    exceptions: &[],
};

/// A key of a REDUNDANT table holds at most 767 bytes: one utf8mb4 VARCHAR
/// column longer than 191 characters, such as `email` VARCHAR(255).
const REDUNDANT_ADD_KEY_TOO_LONG: Case = Case {
    id: "redundant-add-key-too-long",
    scenarios: &[
        "redundant-add-index-key-over-limit",
        "redundant-add-unique-index-key-over-limit",
        "redundant-add-unique-index",
    ],
    exceptions: &[],
};

const REDUNDANT_ADD_FULLTEXT_INDEX: Case = Case {
    id: "redundant-add-fulltext-index",
    scenarios: &["redundant-add-fulltext-index"],
    exceptions: &[],
};

/// Every case of a table with a row format other than DYNAMIC.
pub const ROW_FORMAT_CASES: [&Case; 29] = [
    &COMPRESSED_ADD_COLUMN,
    &COMPRESSED_DROP_COLUMN,
    &COMPRESSED_RENAME_COLUMN,
    &COMPRESSED_SET_COLUMN_DEFAULT,
    &COMPRESSED_CHANGE_INTEGER_TYPE,
    &COMPRESSED_ADD_INDEX,
    &COMPRESSED_ADD_UNIQUE_INDEX,
    &COMPRESSED_DROP_INDEX,
    &COMPRESSED_ADD_FULLTEXT_INDEX,
    &COMPACT_ADD_COLUMN,
    &COMPACT_DROP_COLUMN,
    &COMPACT_RENAME_COLUMN,
    &COMPACT_SET_COLUMN_DEFAULT,
    &COMPACT_CHANGE_INTEGER_TYPE,
    &COMPACT_ADD_INDEX,
    &COMPACT_ADD_UNIQUE_INDEX,
    &COMPACT_DROP_INDEX,
    &COMPACT_ADD_KEY_TOO_LONG,
    &COMPACT_ADD_FULLTEXT_INDEX,
    &REDUNDANT_ADD_COLUMN,
    &REDUNDANT_DROP_COLUMN,
    &REDUNDANT_RENAME_COLUMN,
    &REDUNDANT_SET_COLUMN_DEFAULT,
    &REDUNDANT_CHANGE_INTEGER_TYPE,
    &REDUNDANT_ADD_INDEX,
    &REDUNDANT_ADD_UNIQUE_INDEX,
    &REDUNDANT_DROP_INDEX,
    &REDUNDANT_ADD_KEY_TOO_LONG,
    &REDUNDANT_ADD_FULLTEXT_INDEX,
];

/// Without an `ALGORITHM` clause, or with `ALGORITHM=DEFAULT`, a session
/// with `old_alter_table` on copies the table for every change.
const OLD_ALTER_TABLE_ADD_COLUMN: Case = Case {
    id: "old-alter-table-add-column",
    scenarios: &["old-alter-table-add-nullable-column"],
    exceptions: &[],
};

const OLD_ALTER_TABLE_DROP_COLUMN: Case = Case {
    id: "old-alter-table-drop-column",
    scenarios: &["old-alter-table-drop-column"],
    exceptions: &[],
};

const OLD_ALTER_TABLE_RENAME_COLUMN: Case = Case {
    id: "old-alter-table-rename-column",
    scenarios: &["old-alter-table-rename-column"],
    exceptions: &[],
};

const OLD_ALTER_TABLE_SET_COLUMN_DEFAULT: Case = Case {
    id: "old-alter-table-set-column-default",
    scenarios: &["old-alter-table-set-column-default"],
    exceptions: &[],
};

const OLD_ALTER_TABLE_CHANGE_INTEGER_TYPE: Case = Case {
    id: "old-alter-table-change-integer-type",
    scenarios: &["old-alter-table-int-to-bigint"],
    exceptions: &[],
};

const OLD_ALTER_TABLE_ADD_INDEX: Case = Case {
    id: "old-alter-table-add-index",
    scenarios: &["old-alter-table-add-index"],
    exceptions: &[],
};

const OLD_ALTER_TABLE_ADD_UNIQUE_INDEX: Case = Case {
    id: "old-alter-table-add-unique-index",
    scenarios: &["old-alter-table-add-unique-index"],
    exceptions: &[],
};

const OLD_ALTER_TABLE_DROP_INDEX: Case = Case {
    id: "old-alter-table-drop-index",
    scenarios: &["old-alter-table-drop-index"],
    exceptions: &[],
};

/// Every case of a change run with `old_alter_table` on.
pub const OLD_ALTER_TABLE_CASES: [&Case; 8] = [
    &OLD_ALTER_TABLE_ADD_COLUMN,
    &OLD_ALTER_TABLE_DROP_COLUMN,
    &OLD_ALTER_TABLE_RENAME_COLUMN,
    &OLD_ALTER_TABLE_SET_COLUMN_DEFAULT,
    &OLD_ALTER_TABLE_CHANGE_INTEGER_TYPE,
    &OLD_ALTER_TABLE_ADD_INDEX,
    &OLD_ALTER_TABLE_ADD_UNIQUE_INDEX,
    &OLD_ALTER_TABLE_DROP_INDEX,
];

/// Each scenario of [`OLD_ALTER_TABLE_CASES`] with the scenario that made
/// the same change with old_alter_table off. A written `ALGORITHM` other
/// than `DEFAULT` wins over old_alter_table: kilit gives the answer for the
/// change without it only where the lab saw the forced run of the one leave
/// exactly the traces of the other's.
pub const OLD_ALTER_TABLE_TWINS: [(&str, &str); 8] = [
    ("old-alter-table-add-nullable-column", "add-nullable-column"),
    ("old-alter-table-drop-column", "drop-column"),
    ("old-alter-table-rename-column", "rename-column"),
    ("old-alter-table-set-column-default", "set-column-default"),
    ("old-alter-table-add-index", "add-index"),
    ("old-alter-table-add-unique-index", "add-unique-index"),
    ("old-alter-table-drop-index", "drop-index"),
    ("old-alter-table-int-to-bigint", "int-to-bigint"),
];

/// A situation the lab measured apart: its cases, by the InnoDB case each
/// stands for, and what it measured about keys.
struct Measured {
    /// As kilit says it after "has not been measured": `on a COMPACT table`.
    name: &'static str,
    /// Where the InnoDB case's own measurements come from, for a note:
    /// `on a DYNAMIC table`.
    instead: &'static str,
    cases: &'static [(&'static str, &'static Case)],
    /// The most bytes an index key holds.
    key_bytes: u64,
    /// The case of a longer key, if the lab measured one.
    key_too_long: Option<&'static Case>,
    /// Whether it is old_alter_table, which a written `ALGORITHM` overrides.
    old_alter_table: bool,
}

const ON_COMPRESSED: Measured = Measured {
    name: "on a COMPRESSED table",
    instead: "on a DYNAMIC table",
    cases: &[
        ("add-column", &COMPRESSED_ADD_COLUMN),
        ("drop-column", &COMPRESSED_DROP_COLUMN),
        ("rename-column", &COMPRESSED_RENAME_COLUMN),
        ("set-column-default", &COMPRESSED_SET_COLUMN_DEFAULT),
        ("change-integer-type", &COMPRESSED_CHANGE_INTEGER_TYPE),
        ("add-index", &COMPRESSED_ADD_INDEX),
        ("add-unique-index", &COMPRESSED_ADD_UNIQUE_INDEX),
        ("drop-index", &COMPRESSED_DROP_INDEX),
        ("add-fulltext-index", &COMPRESSED_ADD_FULLTEXT_INDEX),
    ],
    key_bytes: MAX_INDEX_BYTES,
    key_too_long: None,
    old_alter_table: false,
};

const ON_COMPACT: Measured = Measured {
    name: "on a COMPACT table",
    instead: "on a DYNAMIC table",
    cases: &[
        ("add-column", &COMPACT_ADD_COLUMN),
        ("drop-column", &COMPACT_DROP_COLUMN),
        ("rename-column", &COMPACT_RENAME_COLUMN),
        ("set-column-default", &COMPACT_SET_COLUMN_DEFAULT),
        ("change-integer-type", &COMPACT_CHANGE_INTEGER_TYPE),
        ("add-index", &COMPACT_ADD_INDEX),
        ("add-unique-index", &COMPACT_ADD_UNIQUE_INDEX),
        ("drop-index", &COMPACT_DROP_INDEX),
        ("add-fulltext-index", &COMPACT_ADD_FULLTEXT_INDEX),
    ],
    key_bytes: 767,
    key_too_long: Some(&COMPACT_ADD_KEY_TOO_LONG),
    old_alter_table: false,
};

const ON_REDUNDANT: Measured = Measured {
    name: "on a REDUNDANT table",
    instead: "on a DYNAMIC table",
    cases: &[
        ("add-column", &REDUNDANT_ADD_COLUMN),
        ("drop-column", &REDUNDANT_DROP_COLUMN),
        ("rename-column", &REDUNDANT_RENAME_COLUMN),
        ("set-column-default", &REDUNDANT_SET_COLUMN_DEFAULT),
        ("change-integer-type", &REDUNDANT_CHANGE_INTEGER_TYPE),
        ("add-index", &REDUNDANT_ADD_INDEX),
        ("add-unique-index", &REDUNDANT_ADD_UNIQUE_INDEX),
        ("drop-index", &REDUNDANT_DROP_INDEX),
        ("add-fulltext-index", &REDUNDANT_ADD_FULLTEXT_INDEX),
    ],
    key_bytes: 767,
    key_too_long: Some(&REDUNDANT_ADD_KEY_TOO_LONG),
    old_alter_table: false,
};

const WITH_OLD_ALTER_TABLE: Measured = Measured {
    name: "with old_alter_table ON",
    instead: "with it OFF",
    cases: &[
        ("add-column", &OLD_ALTER_TABLE_ADD_COLUMN),
        ("drop-column", &OLD_ALTER_TABLE_DROP_COLUMN),
        ("rename-column", &OLD_ALTER_TABLE_RENAME_COLUMN),
        ("set-column-default", &OLD_ALTER_TABLE_SET_COLUMN_DEFAULT),
        ("change-integer-type", &OLD_ALTER_TABLE_CHANGE_INTEGER_TYPE),
        ("add-index", &OLD_ALTER_TABLE_ADD_INDEX),
        ("add-unique-index", &OLD_ALTER_TABLE_ADD_UNIQUE_INDEX),
        ("drop-index", &OLD_ALTER_TABLE_DROP_INDEX),
    ],
    key_bytes: MAX_INDEX_BYTES,
    key_too_long: None,
    old_alter_table: true,
};

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
/// finding per statement. The statements run in one session, as one SQL
/// file does: see [`check_sessions`] for several.
pub fn check(statements: &[Statement], schema: &Schema, results: &ServerResults) -> Vec<Finding> {
    check_sessions(&[statements], schema, results)
}

/// Like [`check`], for statements that run in several sessions one after
/// the other, such as several SQL files given to the `mysql` client one at
/// a time. Each session starts with the MySQL defaults of the settings kilit
/// follows, unless an earlier session set one globally; the tables carry
/// over. There is one finding per statement, in order.
pub fn check_sessions(
    sessions: &[&[Statement]],
    schema: &Schema,
    results: &ServerResults,
) -> Vec<Finding> {
    check_files(sessions, &[], schema, results)
}

/// Like [`check_sessions`], with the name of each session's input file as
/// given, which a reason uses for a statement of an earlier file.
pub fn check_files(
    sessions: &[&[Statement]],
    file_names: &[&str],
    schema: &Schema,
    results: &ServerResults,
) -> Vec<Finding> {
    let mut schema = schema.clone();
    let mut session = Session {
        file_names: file_names.iter().map(|name| (*name).to_owned()).collect(),
        ..Session::default()
    };
    let mut agreements = Agreements::default();
    let mut findings = Vec::with_capacity(sessions.iter().map(|statements| statements.len()).sum());
    for (at, statements) in sessions.iter().enumerate() {
        if at > 0 {
            session.starts_again();
            schema.session_ends();
        }
        for statement in *statements {
            agreements.server_dependent = false;
            let table_state = table_of(statement).map(|name| match schema.table(name) {
                TableState::Known(_) => TableSeen::Known,
                TableState::Lost { .. } => TableSeen::Lost,
                TableState::Missing => TableSeen::Unseen,
            });
            let in_another_case = schema.in_another_case(statement);
            let (prediction, letter_case) = check_one(
                statement,
                in_another_case,
                &mut session,
                &mut schema,
                results,
                &mut agreements,
            );
            findings.push(Finding {
                line: statement.line,
                sql: statement.sql.clone(),
                prediction,
                server_dependent: agreements.server_dependent,
                letter_case,
                table_state,
            });
        }
    }
    findings
}

/// Checks one statement and moves the session and the tables past it.
fn check_one<'r>(
    statement: &Statement,
    in_another_case: Option<InAnotherCase>,
    session: &mut Session,
    schema: &mut Schema,
    results: &'r ServerResults,
    agreements: &mut Agreements<'r>,
) -> (Prediction, Option<LetterCase>) {
    for (line, text) in &statement.run_comments {
        left_out_comment(*line, text, session, schema, agreements);
    }
    let mut unread_foreign_key_checks = false;
    // 8.0.46 has no restrict_fk_on_non_standard_key and 8.4.13 does: once
    // the input names it, every answer may depend on the server.
    if session.restrict_fk_set {
        agreements.server_dependent = true;
    }
    // MySQL refuses the whole SET when it assigns sql_log_bin globally
    // (error 1228), and 8.0.46 when it names restrict_fk_on_non_standard_key
    // (error 1193): the session keeps its values, or not.
    let mut may_be_refused = false;
    if matches!(
        statement.kind,
        StatementKind::Set { .. } | StatementKind::Other | StatementKind::Unparsed { .. }
    ) {
        let sql = statement.sql.to_ascii_lowercase();
        may_be_refused = sql.contains("restrict_fk_on_non_standard_key")
            || matches!(&statement.kind, StatementKind::Set { variables }
                if variables.iter().any(|variable| variable.name == "sql_log_bin" && variable.global));
        if sql.contains("restrict_fk_on_non_standard_key") {
            session.restrict_fk_set = true;
            agreements.server_dependent = true;
        }
        // Any scope, any value: the engine of a table created after it
        // without ENGINE is the server's to pick. `default_tmp_storage_engine`
        // does the same for temporary tables.
        if sql.contains("storage_engine") {
            schema.default_engine_changes();
        }
        if sql.contains("sql_mode") {
            session.sql_mode_set = true;
            session.sql_mode = SqlMode::read(&sql, &statement.kind, statement.line);
        }
        if sql.contains("binlog_format") {
            session.binlog_format_set = true;
        }
        // `SET GLOBAL` and `SET PERSIST` leave this session's value as it
        // was, but a session started after it begins with the global value.
        // A session value set in the same statement is read below.
        if sql.trim_start().starts_with("set") {
            let scopes = scopes_assigned(&sql, "foreign_key_checks");
            if !scopes.is_empty() && scopes.iter().all(|scope| scope.sets_global()) {
                session.foreign_key_checks_global = true;
            }
            if scopes.iter().any(|scope| scope.sets_global()) {
                session.later_sessions.foreign_key_checks_global = true;
            }
            // A session value kilit sees assigned but cannot read, such as
            // `foreign_key_checks := DEFAULT`, leaves the checks unknown.
            let assigned = scopes
                .iter()
                .filter(|scope| **scope == Scope::Session)
                .count();
            let read = match &statement.kind {
                StatementKind::Set { variables } => variables
                    .iter()
                    .filter(|variable| variable.name == "foreign_key_checks" && !variable.global)
                    .count(),
                _ => 0,
            };
            unread_foreign_key_checks = assigned > read;
            let sql_log_bin = scopes_assigned(&sql, "sql_log_bin");
            if sql_log_bin.iter().any(|scope| scope.sets_global()) {
                session.later_sessions.sql_log_bin_global = true;
            }
            // kilit reads no PERSIST: like a GLOBAL one, which MySQL
            // refuses, it leaves the binary log unclear.
            if sql_log_bin
                .iter()
                .any(|scope| matches!(scope, Scope::Persist | Scope::PersistOnly))
            {
                session.binary_log = BinaryLog::Other("PERSIST sql_log_bin".to_owned());
            }
        }
        // Any scope, any value: a table without `ROW_FORMAT` may get
        // another row format, and its keys another limit.
        if sql.contains("innodb_default_row_format") {
            session.default_row_format_set = true;
            schema.default_row_format_changes();
        }
        if sql.contains("innodb_strict_mode") {
            session.strict_mode_set = true;
            schema.default_row_format_changes();
        }
        // Only a session value kilit reads counts: a global one, a
        // PERSIST, or any form kilit cannot read leaves the session's
        // value unknown.
        let named = sql.matches("old_alter_table").count();
        let read = match &statement.kind {
            StatementKind::Set { variables } => variables
                .iter()
                .filter(|variable| variable.name == "old_alter_table")
                .count(),
            _ => 0,
        };
        // `SET GLOBAL a = 1, old_alter_table = ON` sets the global value
        // of both: a scope keyword carries to the assignments after it, and
        // kilit reads no global value of it, and no PERSIST.
        if named != read {
            session.old_alter_table = OldAlterTable::Unknown(statement.line);
            // It may be the global value a later session starts with.
            session.later_sessions.old_alter_table_unclear = true;
        }
    }
    let letter_case = in_another_case
        .as_ref()
        .map(|found| letter_case_of(found, statement, session, schema));
    // What the statement does under the setting the migration goes on with.
    let goes_on = match &letter_case {
        Some(LetterCase::SameTable { written, known }) => {
            let renamed = with_table_renamed(statement, written, known);
            let prediction = predict_kind(
                &renamed,
                session,
                schema,
                results,
                agreements,
                may_be_refused,
            );
            Some((Cow::Owned(renamed), prediction))
        }
        Some(LetterCase::AsWritten) => {
            let prediction = predict_kind(
                statement,
                session,
                schema,
                results,
                agreements,
                may_be_refused,
            );
            Some((Cow::Borrowed(statement), prediction))
        }
        Some(LetterCase::Unclear) | None => None,
    };
    let prediction = match (&in_another_case, &letter_case) {
        // A statement kilit could not read keeps its own reason.
        (Some(found), Some(letter_case)) if found.role != Role::Unread => {
            unknown(in_another_case_reason(
                found,
                letter_case,
                goes_on.as_ref().map(|(_, prediction)| prediction),
                session.foreign_key_checks,
            ))
        }
        _ => predict_kind(
            statement,
            session,
            schema,
            results,
            agreements,
            may_be_refused,
        ),
    };
    if unread_foreign_key_checks {
        session.foreign_key_checks = Setting::Unknown;
        session.foreign_key_checks_global = false;
        session.earlier_session.foreign_key_checks = false;
        session.foreign_key_checks_unknown = format!(
            "line {} sets foreign_key_checks in a form kilit cannot read",
            statement.line
        );
    }
    // The next statement sees the tables as this one leaves them. One that
    // names a table in another letter case is followed as it runs under
    // the setting the migration goes on with.
    match (&letter_case, goes_on) {
        (Some(LetterCase::Unclear), _) => schema.follow_in_another_case(statement),
        (_, Some((followed, goes_on))) => go_on(schema, &followed, &goes_on, session),
        (_, None) => go_on(schema, statement, &prediction, session),
    }
    for (line, text) in &statement.run_comments_after {
        left_out_comment(*line, text, session, schema, agreements);
    }
    (created_fulltext_index(statement, prediction), letter_case)
}

/// Why kilit does not know a setting after a `/*!` comment names it.
fn in_a_comment(line: usize, setting: &str) -> String {
    format!(
        "line {line} runs a /*! */ comment that names {setting}, and kilit does not read such \
         comments"
    )
}

/// A `/*!` comment that split.rs left out of every statement, on `line`:
/// MySQL runs its text, and kilit does not read it. Each setting kilit
/// follows that the text names, as a statement's text names it (in any
/// case, `@OLD_SQL_MODE` too), is from here on as after a SET of it that
/// kilit cannot read. With GLOBAL or PERSIST in the text, later input files
/// start with it unclear too.
fn left_out_comment(
    line: usize,
    text: &str,
    session: &mut Session,
    schema: &mut Schema,
    agreements: &mut Agreements<'_>,
) {
    let text = text.to_ascii_lowercase();
    let global = text.contains("global") || text.contains("persist");
    if text.contains("restrict_fk_on_non_standard_key") {
        session.restrict_fk_set = true;
        agreements.server_dependent = true;
    }
    if text.contains("storage_engine") {
        schema.default_engine_changes();
    }
    if text.contains("sql_mode") {
        session.sql_mode_set = true;
        session.sql_mode = SqlMode::Unclear(in_a_comment(line, "sql_mode"));
    }
    if text.contains("binlog_format") {
        session.binlog_format_set = true;
    }
    if text.contains("foreign_key_checks") {
        session.foreign_key_checks = Setting::Unknown;
        session.foreign_key_checks_global = false;
        session.earlier_session.foreign_key_checks = false;
        session.foreign_key_checks_unknown = in_a_comment(line, "foreign_key_checks");
        if global {
            session.later_sessions.foreign_key_checks_global = true;
        }
    }
    if text.contains("sql_log_bin") {
        session.binary_log = BinaryLog::Other(format!(
            "sql_log_bin in a /*! */ comment on line {line}, which kilit does not read"
        ));
        if global {
            session.later_sessions.sql_log_bin_global = true;
        }
    }
    if text.contains("innodb_default_row_format") {
        session.default_row_format_set = true;
        schema.default_row_format_changes();
    }
    if text.contains("innodb_strict_mode") {
        session.strict_mode_set = true;
        schema.default_row_format_changes();
    }
    if text.contains("old_alter_table") {
        session.old_alter_table = OldAlterTable::InComment(line);
        if global {
            session.later_sessions.old_alter_table_unclear = true;
        }
    }
}

/// A `CREATE FULLTEXT INDEX`, which kilit answers and follows as the `ALTER
/// TABLE ... ADD FULLTEXT` MySQL runs it as (by hand on MySQL 8.0.46 and
/// 8.4.13, the two leave the same table and are refused alike): the lab
/// has not measured the statement itself, so the answer becomes the
/// reason's, after the tables follow it, a refusal leaving them as they
/// were.
fn created_fulltext_index(statement: &Statement, prediction: Prediction) -> Prediction {
    if !crate::parse::creates_fulltext_index(&statement.sql) {
        return prediction;
    }
    match prediction {
        Prediction::Measured {
            case,
            verdict,
            notes,
            ..
        } => Prediction::Unknown {
            reason: format!(
                "CREATE FULLTEXT INDEX has not been measured itself; MySQL runs it as ALTER \
                 TABLE ... ADD FULLTEXT, by hand on MySQL 8.0.46 and 8.4.13, and the lab \
                 measured that: {} ({case})",
                brief(&verdict)
            ),
            notes,
        },
        Prediction::DependsOnTable { reason, notes, .. } => Prediction::Unknown {
            reason: format!(
                "{reason}; CREATE FULLTEXT INDEX itself has not been measured: MySQL runs it as \
                 ALTER TABLE ... ADD FULLTEXT, by hand on MySQL 8.0.46 and 8.4.13"
            ),
            notes,
        },
        other => other,
    }
}

/// Moves the tables past `statement`, which kilit answered `prediction`,
/// unless MySQL may refuse it for the table's keys.
fn go_on(schema: &mut Schema, statement: &Statement, prediction: &Prediction, session: &Session) {
    if let Some((table, reason, engine, unclear)) = options_kilit_cannot_follow(statement, schema) {
        schema.lose_table(&table, reason, engine, unclear);
    } else if let Some(rewritten) = rewritten(statement, schema, session) {
        match rewritten {
            // MySQL refuses the statement: the tables stay as they were.
            Rewritten::Refused => {}
            Rewritten::Lost {
                table,
                reason,
                engine,
            } => {
                schema.apply(statement);
                schema.lose_table(&table, reason, engine, false);
            }
        }
    } else {
        follow(schema, statement, prediction);
    }
}

/// Whether `column` may be longer than its type holds in some character
/// set: a VARCHAR of more than 65,535 / 4 characters, or a column too long
/// in any. A quick look before the table's character set is looked up.
fn may_be_too_long(column: &Column) -> bool {
    match column.data_type {
        DataType::VarChar { length } => length > row::MAX_ROW_BYTES / 4,
        _ => row::too_long_kind(None, column).is_some(),
    }
}

/// What MySQL makes of a statement kilit would otherwise follow as written.
enum Rewritten {
    /// MySQL refuses it.
    Refused,
    /// MySQL may run something else: kilit stops trusting the table.
    Lost {
        table: String,
        reason: String,
        engine: Option<Engine>,
    },
}

/// A column longer than its type holds, in ADD, MODIFY, CHANGE or CREATE
/// TABLE (see [`row::TooLong`]): MySQL refuses a CHAR or BINARY one in
/// every SQL mode, so the tables stay as they were. It refuses a VARCHAR or
/// VARBINARY one in strict mode, and in the others makes it a TEXT or BLOB
/// type, which kilit does not follow. kilit takes the mode as strict only
/// when the input sets a strict sql_mode earlier in the same session: the
/// server's own may not be, and a framework may set one the input does not
/// show (Laravel's `'strict' => false` sets a non-strict one, and `migrate
/// --pretend` does not print it). A refused CREATE TABLE is taken as lost
/// too: kilit gives that table no answer either way.
fn rewritten(statement: &Statement, schema: &Schema, session: &Session) -> Option<Rewritten> {
    if let Some(refused) = refused_by_mysql(statement, schema) {
        return Some(refused);
    }
    // Without a dump, a table no statement before shows may be there: MySQL
    // then keeps it as it is, and creates nothing. One a statement before
    // dropped or renamed is not there.
    if let StatementKind::CreateTable {
        table,
        if_not_exists: true,
    } = &statement.kind
        && !schema.is_from_dump()
        && schema.table(&table.name) == TableState::Missing
        && schema
            .gone(&table.name)
            .is_none_or(|gone| gone.how == GoneHow::MaybeDropped)
    {
        return Some(Rewritten::Lost {
            table: table.name.clone(),
            reason: format!(
                "line {}: CREATE TABLE IF NOT EXISTS keeps a table of the name that no --schema \
                 shows, if there is one",
                statement.line
            ),
            engine: None,
        });
    }
    let (name, table, columns): (&str, Option<&Table>, Vec<&Column>) = match &statement.kind {
        StatementKind::Alter(alter) => {
            let columns: Vec<&Column> = alter
                .operations
                .iter()
                .filter_map(|operation| match operation {
                    Operation::AddColumn { column, .. }
                    | Operation::ModifyColumn { column, .. }
                    | Operation::ChangeColumn { column, .. } => Some(column),
                    _ => None,
                })
                .filter(|column| may_be_too_long(column))
                .collect();
            if columns.is_empty() {
                return None;
            }
            let table = match schema.table(&alter.table) {
                TableState::Known(table) => Some(table),
                _ => None,
            };
            (&alter.table, table, columns)
        }
        StatementKind::CreateTable { table, .. } => {
            (&table.name, Some(table), table.columns.iter().collect())
        }
        _ => return None,
    };
    let (column, kind) = columns
        .into_iter()
        .find_map(|column| Some((column, row::too_long_kind(table, column)?)))?;
    let line = statement.line;
    let strict = session.sql_mode == SqlMode::Strict;
    let creates = matches!(statement.kind, StatementKind::CreateTable { .. });
    if !creates && (kind == row::TooLong::InEveryMode || strict) {
        return Some(Rewritten::Refused);
    }
    let reason = if kind == row::TooLong::InEveryMode || strict {
        format!(
            "line {line}: MySQL refuses to create it, as {} is longer than its type holds",
            column.name
        )
    } else {
        let mode = match &session.sql_mode {
            SqlMode::Unclear(why) => why.clone(),
            SqlMode::NotStrict => "the input sets a sql_mode that is not strict".to_owned(),
            SqlMode::ServerDefault | SqlMode::Strict => {
                "the input sets no strict sql_mode before it".to_owned()
            }
        };
        format!(
            "line {line}: {} is longer than its type holds, and unless the sql_mode is \
             strict MySQL makes it a TEXT or BLOB type; {mode}",
            column.name
        )
    };
    Some(Rewritten::Lost {
        table: name.to_owned(),
        reason,
        engine: table.and_then(|table| table.engine.clone()),
    })
}

/// Under which setting of lower_case_table_names the migration goes on
/// after `statement`, which names `found.written` in another letter case:
/// the hand tests of step 14 on 8.0.46 and 8.4.13, form by form. A form
/// they did not cover, or a name kilit is already unsure of, is unclear.
fn letter_case_of(
    found: &InAnotherCase,
    statement: &Statement,
    session: &Session,
    schema: &Schema,
) -> LetterCase {
    let InAnotherCase {
        written,
        known,
        role,
        alone,
        known_temporary,
    } = found;
    if !alone || !tested_letters(written, known) {
        return LetterCase::Unclear;
    }
    let same_table = || LetterCase::SameTable {
        written: written.clone(),
        known: known.clone(),
    };
    match (&statement.kind, role) {
        (StatementKind::Alter(alter), _)
            if alter
                .operations
                .iter()
                .any(|operation| matches!(operation, Operation::RenameTable { .. })) =>
        {
            LetterCase::Unclear
        }
        (StatementKind::Alter(_), Role::Changes) => same_table(),
        // A key to the table itself, written in another case, has no hand
        // test. With the checks on, 0 refuses the key (1824); otherwise
        // both settings add it to the same table.
        (StatementKind::Alter(alter), Role::PointsAt) if *known != alter.table => {
            match session.foreign_key_checks {
                Setting::On | Setting::ServerDefault => same_table(),
                Setting::Off | Setting::Unclear | Setting::Unknown => LetterCase::AsWritten,
            }
        }
        (StatementKind::DropTable { tables }, Role::Changes) if tables.len() == 1 => same_table(),
        // To a name no table kilit knows has, in any letter case: the hand
        // test renamed ORDERS to o2.
        (StatementKind::RenameTable { renames }, Role::Changes)
            if renames.len() == 1 && schema.no_table_in_any_case(&renames[0].to) =>
        {
            same_table()
        }
        // RENAME TABLE orders TO ORDERS: 1050 with 1. MySQL lets a base
        // table share a temporary table's name, so not for a temporary one.
        (StatementKind::RenameTable { renames }, Role::Names)
            if renames.len() == 1 && renames[0].from == *known && !known_temporary =>
        {
            LetterCase::AsWritten
        }
        // CREATE TABLE ORDERS while orders exists: 1050 with 1.
        (
            StatementKind::CreateTable {
                table,
                if_not_exists: false,
            },
            Role::Names,
        ) if table.foreign_keys.is_empty()
            && !creates_temporary(&statement.sql)
            && !known_temporary =>
        {
            LetterCase::AsWritten
        }
        (StatementKind::Unparsed { .. }, Role::Empties) => same_table(),
        _ => LetterCase::Unclear,
    }
}

/// Whether two names that match in another letter case differ only in
/// letters the hand tests changed: ASCII letters, and Ç, ç, İ against I or
/// i (with 1, ÇİZELGE, ÇIZELGE and Çizelge all reach çizelge).
fn tested_letters(a: &str, b: &str) -> bool {
    a.chars().count() == b.chars().count()
        && a.chars().zip(b.chars()).all(|(x, y)| {
            x == y
                || (x.is_ascii() && y.is_ascii() && x.eq_ignore_ascii_case(&y))
                || matches!(
                    (x, y),
                    ('Ç', 'ç') | ('ç', 'Ç') | ('İ', 'i' | 'I') | ('i' | 'I', 'İ')
                )
        })
}

/// Why kilit gives no answer for a statement that names a table in another
/// letter case: whether it is the table kilit knows depends on
/// lower_case_table_names, which kilit does not know. By hand on 8.0.46
/// and 8.4.13 with 0: 1146, 1051 or 1049 for a table
/// changed, dropped or renamed; 1824 for a foreign key's parent with the
/// checks on; a note for DROP TABLE IF EXISTS; a new table for CREATE TABLE
/// and RENAME. With 1 every one runs on the table kilit knows, or fails
/// with 1050 for a new name.
fn in_another_case_reason(
    found: &InAnotherCase,
    letter_case: &LetterCase,
    goes_on: Option<&Prediction>,
    foreign_key_checks: Setting,
) -> String {
    let InAnotherCase {
        written,
        known,
        role,
        ..
    } = found;
    let what = match role {
        Role::Changes | Role::Empties | Role::Unread => {
            "refuse the statement when no table has that exact name"
        }
        Role::DropsIfExists => "skip it with a note when no table has that exact name",
        Role::Names => "take it as a new name",
        Role::PointsAt => {
            "refuse the foreign key when foreign_key_checks is on and no table has that exact \
             name"
        }
    };
    let fails_too = matches!(
        goes_on,
        Some(Prediction::Measured {
            verdict: Verdict::Fails { .. },
            ..
        })
    );
    let after = match (letter_case, role) {
        (LetterCase::SameTable { .. }, _) if fails_too => format!(
            "With 0 the statement fails, and with 1 or 2, where {written} is {known}, the lab's \
             answer for it is a failure too, so the migration stops here and kilit leaves the \
             tables as they are."
        ),
        (LetterCase::SameTable { .. }, _) => format!(
            "With 0 the statement fails and the migration stops there, so kilit goes on as \
             with 1 or 2, where {written} is {known}."
        ),
        (LetterCase::AsWritten, Role::PointsAt) if foreign_key_checks == Setting::Off => {
            "With the checks off both settings add the foreign key, so kilit goes on with it \
             as written."
                .to_owned()
        }
        (LetterCase::AsWritten, Role::PointsAt) => {
            "The checks may be off, and then both settings add the foreign key, so kilit goes \
             on with it as written."
                .to_owned()
        }
        (LetterCase::AsWritten, _) => format!(
            "With 1 or 2 the statement fails and the migration stops there, so kilit goes on \
             as with 0, where {written} is a table of its own."
        ),
        (LetterCase::Unclear, _) => "kilit cannot tell which setting the migration goes on \
             with, so it stops trusting the tables the statement may change."
            .to_owned(),
    };
    format!(
        "kilit knows this table as {known}. With lower_case_table_names = 0, the default on \
         Linux, {written} is another table, and MySQL 8.0.46 and 8.4.13 {what}; with 1 or 2, \
         the defaults on Windows and macOS, it is the same table. {after}"
    )
}

/// What kilit says about a statement of `kind`, before it follows it.
fn predict_kind<'r>(
    statement: &Statement,
    session: &mut Session,
    schema: &Schema,
    results: &'r ServerResults,
    agreements: &mut Agreements<'r>,
    may_be_refused: bool,
) -> Prediction {
    match &statement.kind {
        StatementKind::Alter(alter) => {
            let mut prediction =
                predict(alter, &statement.sql, session, schema, results, agreements);
            // A written ALGORITHM wins over old_alter_table.
            if session.earlier_session.old_alter_table
                && alter.algorithm.is_none()
                && let Prediction::Measured { notes, .. } = &mut prediction
            {
                notes.push(ASSUMES_OLD_ALTER_TABLE_IN_A_NEW_SESSION.to_owned());
            }
            on_unknown_table(alter, session, schema, prediction)
        }
        StatementKind::Set { variables } => {
            session.apply(variables, statement.line, may_be_refused);
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
        StatementKind::RenameTable { .. } => unknown("renaming tables has not been measured yet"),
        StatementKind::Other => not_checked("not a schema change"),
        StatementKind::Unparsed { error, .. } => unknown(error.as_str()),
    }
}

/// The cases whose failure comes from the statement alone, whatever the
/// table: kilit gives it on a table it does not know too. Every other
/// failure, and every other answer, depends on the table somewhere.
/// - add-column-binlog-unsafe-default: a nondeterministic expression
///   default (error 1674);
/// - add-column-blob-literal-default and modify-column-blob-literal-default:
///   a quoted default on the TEXT, BLOB, JSON or GEOMETRY column the
///   statement writes (error 1101);
/// - add-fulltext-index-with-order: ASC or DESC on a key part of a FULLTEXT
///   index (error 1221; by hand on MySQL 8.0.46 and 8.4.13, on MyISAM and
///   COMPACT tables too).
const FAILS_FROM_THE_STATEMENT: [&Case; 4] = [
    &ADD_COLUMN_BINLOG_UNSAFE_DEFAULT,
    &ADD_COLUMN_BLOB_LITERAL_DEFAULT,
    &MODIFY_COLUMN_BLOB_LITERAL_DEFAULT,
    &ADD_FULLTEXT_INDEX_WITH_ORDER,
];

/// An answer on a table kilit does not know: one it lost track of, one no
/// `--schema` showed, or one an earlier statement dropped or renamed while
/// no dump was given, and the parent of a new foreign key in each of these.
/// The table may have what changes the answer (a FULLTEXT index, a foreign
/// key, another engine or row format, a longer row ...), so the measured
/// answer becomes the reason's likely answer and only a failure that comes
/// from the statement alone stays: see [`FAILS_FROM_THE_STATEMENT`]. With a
/// dump, a table it does not have is unknown before any case: see
/// [`not_in_dump`].
fn on_unknown_table(
    alter: &Alter,
    session: &Session,
    schema: &Schema,
    prediction: Prediction,
) -> Prediction {
    let Prediction::Measured {
        case,
        verdict,
        evidence,
        notes,
    } = prediction
    else {
        return prediction;
    };
    let Some(why) = unknown_table(alter, session, schema) else {
        return Prediction::Measured {
            case,
            verdict,
            evidence,
            notes,
        };
    };
    if matches!(verdict, Verdict::Fails { .. })
        && FAILS_FROM_THE_STATEMENT
            .iter()
            .any(|statement_alone| statement_alone.id == case)
    {
        return Prediction::Measured {
            case,
            verdict,
            evidence,
            notes,
        };
    }
    Prediction::Unknown {
        reason: format!(
            "{why}; on a plain InnoDB table the lab measured {} ({case})",
            brief(&verdict)
        ),
        notes,
    }
}

/// Why kilit does not know the table of `alter`, or the parent of the
/// foreign key it adds; `None` when it knows both.
fn unknown_table(alter: &Alter, session: &Session, schema: &Schema) -> Option<String> {
    let parents = alter
        .operations
        .iter()
        .filter_map(|operation| match operation {
            Operation::AddForeignKey(key) => Some(key.referenced_table.as_str()),
            _ => None,
        });
    let mut tables = std::iter::once(alter.table.as_str()).chain(parents);
    tables.find_map(|name| {
        let why = why_unknown(name, session, schema)?;
        Some(if name == alter.table {
            why
        } else {
            format!("the foreign key points at {name}: {why}")
        })
    })
}

/// Why kilit does not know the table `name`, as [`unknown_table`] says it;
/// `None` when it knows it, or when a dump decides (see [`not_in_dump`]).
fn why_unknown(name: &str, session: &Session, schema: &Schema) -> Option<String> {
    match schema.table(name) {
        TableState::Known(_) => None,
        TableState::Lost { reason, .. } => Some(format!("kilit lost track of {name} ({reason})")),
        TableState::Missing => match schema.gone(name) {
            Some(_) if schema.is_from_dump() => None,
            Some(gone) => Some(format!(
                "{name}, {}, and no statement after it creates it again",
                gone_by(gone, session)
            )),
            // With a dump, B1b's rule: see `not_in_dump`.
            None if schema.is_from_dump() => None,
            None => Some(format!(
                "no --schema was given, so kilit does not know {name}"
            )),
        },
    }
}

/// One word for what an answer means for the application.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Word {
    Fails,
    Waits,
    Continues,
    Unknown,
}

fn word(verdict: &Verdict) -> Word {
    match verdict {
        Verdict::Fails { .. } => Word::Fails,
        Verdict::Runs { writes, .. } => match writes {
            Writes::Blocked => Word::Waits,
            Writes::Allowed => Word::Continues,
            Writes::Unknown => Word::Unknown,
        },
    }
}

/// The engines and row formats the lab measured apart from InnoDB's
/// DYNAMIC, each with the case of `innodb` it measured there, if any.
fn measured_apart_from_dynamic(innodb: &str) -> [(&'static str, Option<&'static Case>); 4] {
    let row_format = |measured: &Measured| {
        measured
            .cases
            .iter()
            .find(|(id, _)| *id == innodb)
            .map(|(_, case)| *case)
    };
    [
        (
            "MyISAM",
            ON_MYISAM
                .iter()
                .find(|(id, ..)| *id == innodb)
                .map(|(_, case, _)| *case),
        ),
        (
            "COMPRESSED (ROW_FORMAT=COMPRESSED, or KEY_BLOCK_SIZE=8)",
            row_format(&ON_COMPRESSED),
        ),
        ("COMPACT", row_format(&ON_COMPACT)),
        ("REDUNDANT", row_format(&ON_REDUNDANT)),
    ]
}

/// Decision 1 on a table kilit does not know, after `answer` gave `base`
/// as if it were the lab's plain InnoDB table. The table could give each
/// of these answers: the base one; each exception of the case the table
/// decides; the case measured on MyISAM and on the COMPRESSED, COMPACT and
/// REDUNDANT row formats; for a new index, the refusals of a missing
/// column (1072) and a taken name (1061), by hand on MySQL 8.0.46 and
/// 8.4.13; and every path kilit cannot see (a note of `classified`'s
/// other than an assumption or a row situation, or a case the lab has not
/// measured on one of those engines and row formats), whose answer is
/// unknown. The first rule that matches decides:
/// 1. a failure that comes from the statement alone is the answer (see
///    [`FAILS_FROM_THE_STATEMENT`]);
/// 2. any answer is unknown: unknown;
/// 3. every answer has the same impact: that impact, but writes continuing
///    needs every engine and row format measured, which a table kilit does
///    not know may not have, so it is never kept;
/// 4. no answer lets writes continue: writes wait;
/// 5. else unknown.
///
/// Combinations the lab did not measure on another engine or row format
/// (a second FULLTEXT index on a COMPRESSED table, ...) and engines and
/// row formats it never measured count against writes continuing only.
/// A table a statement before dropped or renamed is not there, and MySQL
/// refuses the statement (1146, by hand on both servers): no answer of
/// these is the table's, so it stays unknown. So does a DROP INDEX, which
/// has shapes the lab has not measured on a table kilit knows (see
/// [`check_dropped_index`]). `base` comes back as it is for
/// [`on_unknown_table`] to keep (rule 1) or turn into an unknown one
/// (rules 2 and 5); `classified` is how many of its notes are classify's.
#[allow(clippy::too_many_arguments)]
fn on_a_table_kilit_does_not_know<'r>(
    alter: &Alter,
    case: &'static Case,
    operation: &Operation,
    situation: &Situation<'_>,
    variant: Variant,
    results: &'r ServerResults,
    agreements: &mut Agreements<'r>,
    classified: usize,
    base: Prediction,
) -> Prediction {
    let Prediction::Measured {
        case: id,
        verdict,
        evidence,
        notes: answered,
    } = &base
    else {
        return base;
    };
    let classified = &answered[..classified.min(answered.len())];
    let base_word = word(verdict);
    // Rule 1, and the answers no rule past 2 keeps: an exception that
    // decided on its own, or writes continuing.
    if matches!(base_word, Word::Continues | Word::Unknown)
        || evidence.as_slice() != case.scenarios
        || (base_word == Word::Fails
            && FAILS_FROM_THE_STATEMENT
                .iter()
                .any(|statement_alone| statement_alone.id == *id))
    {
        return base;
    }
    // A path kilit cannot see on such a table: rule 2.
    if matches!(operation, Operation::DropIndex { .. })
        || classified
            .iter()
            .any(|note| !note.starts_with("assumes ") && !depends_on_rows(note))
    {
        return base;
    }
    let Some(why) = why_unknown(&alter.table, situation.session, situation.schema) else {
        return base;
    };
    if situation.schema.gone(&alter.table).is_some() {
        return base;
    }
    let mut notes: Vec<String> = classified.to_vec();
    let mut answers = vec![base_word];
    let mut said = Vec::new();
    for (at, exception) in case.exceptions.iter().enumerate() {
        if !matches!(holds(exception, situation), Applies::Maybe) {
            continue;
        }
        let other = agreements.agreed(case, Group::Exception(at), variant, results);
        let scenarios = scenario_list(exception.scenarios);
        if exception.conditions.contains(&Condition::Hidden) {
            // The rows or the history: a note on every table.
            if let Ok(other) = other {
                notes.push(if other == *verdict {
                    format!("{}: same result ({scenarios})", exception.when)
                } else {
                    format!("{}: {} ({scenarios})", exception.when, brief(&other))
                });
            }
            continue;
        }
        let Ok(other) = other else {
            return base;
        };
        answers.push(word(&other));
        said.push(format!(
            "{}: {} ({scenarios})",
            exception.when,
            brief(&other)
        ));
    }
    for (name, measured) in measured_apart_from_dynamic(case.id) {
        let Some(measured) = measured else {
            return base;
        };
        let Ok(other) = agreements.agreed(measured, Group::Case, variant, results) else {
            return base;
        };
        answers.push(word(&other));
        said.push(format!(
            "if the table is {name}: {} ({})",
            brief(&other),
            scenario_list(measured.scenarios)
        ));
        for (at, exception) in measured.exceptions.iter().enumerate() {
            if exception.conditions.contains(&Condition::Hidden) {
                continue;
            }
            let Ok(other) = agreements.agreed(measured, Group::Exception(at), variant, results)
            else {
                return base;
            };
            answers.push(word(&other));
            said.push(format!(
                "if the table is {name} and {}: {} ({})",
                exception.when.trim_start_matches("if "),
                brief(&other),
                scenario_list(exception.scenarios)
            ));
        }
    }
    if let Operation::AddIndex(index) = operation {
        for column in index.columns.iter().filter_map(|entry| entry_column(entry)) {
            answers.push(Word::Fails);
            said.push(format!(
                "if the table has no column {column}: fails with error 1072 (by hand on MySQL \
                 8.0.46 and 8.4.13)"
            ));
        }
        if let Some(name) = &index.name {
            answers.push(Word::Fails);
            said.push(format!(
                "if the table has an index named {name} already: fails with error 1061 (by hand \
                 on MySQL 8.0.46 and 8.4.13)"
            ));
        }
    }
    if answers.contains(&Word::Unknown) {
        return base;
    }
    let (shared, every) = if answers.iter().all(|answer| *answer == Word::Fails) {
        (Shared::Fails, "fails")
    } else if answers.iter().all(|answer| *answer == Word::Waits) {
        (Shared::WritesWait, "makes writes wait")
    } else if !answers.contains(&Word::Continues) {
        (Shared::WritesWait, "makes writes wait or fails")
    } else {
        return base;
    };
    notes.extend(said);
    notes.push(
        "on another engine, row format or KEY_BLOCK_SIZE, or one of these in a shape the lab \
         did not measure there: not measured"
            .to_owned(),
    );
    Prediction::DependsOnTable {
        case: id,
        shared,
        evidence: evidence.clone(),
        reason: format!(
            "{why}; every answer the lab measured, or a hand test showed, for the tables it may \
             be {every}: on a plain InnoDB table {} ({id})",
            brief(verdict)
        ),
        notes,
    }
}

/// Which statement took a table away: "which the statement on line 4
/// drops".
fn gone_by(gone: &Gone, session: &Session) -> String {
    let statement = match (gone.earlier_file, session.file_of(gone)) {
        (true, Some(file)) => format!("the statement on line {} of {file}", gone.line),
        (true, None) => format!(
            "the statement on line {} of an earlier input file",
            gone.line
        ),
        (false, _) => format!("the statement on line {}", gone.line),
    };
    match &gone.how {
        GoneHow::Dropped => format!("which {statement} drops"),
        GoneHow::MaybeDropped => format!(
            "which kilit takes as dropped by {statement} (MySQL may have dropped only a \
             temporary table of that name)"
        ),
        GoneHow::Renamed { to } => format!("which {statement} renames to {to}"),
    }
}

/// A name with a database, `shop.orders`, where kilit has a table of the
/// name alone, `orders`: kilit does not know which database the statements
/// run in, so it cannot tell whether the two are one table. With a dump or
/// without one.
fn qualified_name(name: &str, table: TableState<'_>, schema: &Schema) -> Option<String> {
    if table != TableState::Missing {
        return None;
    }
    let (database, short) = name.rsplit_once('.')?;
    if schema.table(short) == TableState::Missing {
        return None;
    }
    let has = if schema.dump_has(short) {
        "the schema dump has a table"
    } else {
        "kilit has a table"
    };
    Some(format!(
        "{name} names the database {database}, and {has} {short}: kilit does not know the \
         database the statements run in, so it cannot tell whether {name} is {short}"
    ))
}

/// An `ALTER TABLE` of table options that MySQL may refuse for the keys of
/// a table kilit knows: kilit cannot tell whether it ran, so it stops
/// trusting the table. The table, why, the engine it may have, and whether
/// its row format is unclear.
fn options_kilit_cannot_follow(
    statement: &Statement,
    schema: &Schema,
) -> Option<(String, String, Option<Engine>, bool)> {
    let StatementKind::Alter(alter) = &statement.kind else {
        return None;
    };
    // CHECK constraint clauses beside the options change no key.
    let options = alter
        .operations
        .iter()
        .find_map(|operation| match operation {
            Operation::ChangeTableOptions(options) => Some(options),
            _ => None,
        })?;
    let TableState::Known(table) = schema.table(&alter.table) else {
        return None;
    };
    let reason = keys_fit(table, options).err()?;
    let engine = match &options.engine {
        Some(_) => Some(Engine::Unknown),
        None => table.engine.clone(),
    };
    Some((
        alter.table.clone(),
        format!("line {}: {reason}", statement.line),
        engine,
        true,
    ))
}

/// Whether the keys of `table` fit the engine and row format that `options`
/// give it, as far as kilit can tell: MySQL refuses the change with error
/// 1071 when a key is longer than the new limit, and a MyISAM table's
/// AUTO_INCREMENT column that InnoDB cannot take with error 1075.
fn keys_fit(table: &Table, options: &TableOptions) -> Result<(), String> {
    let engine = options.engine.as_ref().or(table.engine.as_ref());
    let was_myisam = table.engine == Some(Engine::MyIsam);
    let row_format = options.row_format.as_ref().or(table.row_format.as_ref());
    // (most bytes of one key column, most bytes of a key)
    let (column_limit, key_limit) = match engine {
        // kilit gives no answer for these tables anyway.
        Some(Engine::Other(_) | Engine::Unknown) => return Ok(()),
        Some(Engine::MyIsam) => (MYISAM_KEY_BYTES, MYISAM_KEY_BYTES),
        None | Some(Engine::InnoDb) => match row_format {
            Some(RowFormat::Compact | RowFormat::Redundant) => (767, 3072),
            Some(RowFormat::Other(_) | RowFormat::Unknown) => return Ok(()),
            // An InnoDB table keeps the limit it had, or gets a higher one.
            _ if !was_myisam => return Ok(()),
            _ => (3072, 3072),
        },
    };
    let cannot_tell = |key: &str| {
        Err(format!(
            "kilit cannot tell whether MySQL takes the key {key} with the new engine or row \
             format"
        ))
    };
    if was_myisam && engine != Some(&Engine::MyIsam) {
        for column in table.columns.iter().filter(|column| column.auto_increment) {
            let first = |columns: &[String]| {
                columns
                    .first()
                    .is_some_and(|first| first.eq_ignore_ascii_case(&column.name))
            };
            if !first(&table.primary_key)
                && !table.indexes.iter().any(|index| first(&index.columns))
            {
                return cannot_tell(&format!("on the AUTO_INCREMENT column {}", column.name));
            }
        }
    }
    let primary = (!table.primary_key.is_empty()).then_some(("PRIMARY", &table.primary_key));
    let mut indexes = Vec::new();
    for index in &table.indexes {
        let name = index.name.as_deref().unwrap_or("without a name");
        // A FULLTEXT or spatial index has limits of its own.
        if !matches!(index.kind, IndexKind::Plain | IndexKind::Unique) {
            return cannot_tell(name);
        }
        indexes.push((name, &index.columns));
    }
    for (name, columns) in primary.into_iter().chain(indexes) {
        let mut total = 0u64;
        for entry in columns {
            let Ok(bytes) = key_bytes(table, entry, "") else {
                return cannot_tell(name);
            };
            if bytes > column_limit {
                return cannot_tell(name);
            }
            total = total.saturating_add(bytes);
        }
        if total > key_limit {
            return cannot_tell(name);
        }
    }
    Ok(())
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
        } | Prediction::DependsOnTable {
            shared: Shared::Fails,
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
        notes: Vec::new(),
    }
}

/// Session settings that change how an ALTER runs.
#[derive(Default)]
struct Session {
    foreign_key_checks: Setting,
    /// A statement sets the global value of `foreign_key_checks`, and none
    /// sets this session's after it. The global value does not change the
    /// session's.
    foreign_key_checks_global: bool,
    /// Why `foreign_key_checks` is unknown, when it is [`Setting::Unknown`].
    foreign_key_checks_unknown: String,
    /// `sql_log_bin`, which decides whether MySQL refuses some expression
    /// defaults.
    binary_log: BinaryLog,
    /// The input names `restrict_fk_on_non_standard_key`, in any form kilit
    /// can see, read or not. Its value changes which foreign keys MySQL 8.4
    /// accepts and with which error, and the lab measured the default only.
    restrict_fk_set: bool,
    /// The input names `sql_mode`, in any form kilit can see. Whether MySQL
    /// refuses a key too long for a MyISAM index or shortens it depends on
    /// it, and the lab measured the default, which is strict.
    sql_mode_set: bool,
    /// The strictness of `sql_mode` in this session, which decides whether
    /// MySQL refuses some defaults or drops them with a warning.
    sql_mode: SqlMode,
    /// The input names `binlog_format`, in any form kilit can see. Whether
    /// MySQL refuses an expression default as unsafe for the binary log
    /// depends on it, and the lab measured it as `ROW`, the default.
    binlog_format_set: bool,
    /// `old_alter_table`, which makes MySQL copy the table for a change
    /// without an `ALGORITHM` clause.
    old_alter_table: OldAlterTable,
    /// The input names `innodb_default_row_format`, in any form kilit can
    /// see. A table without `ROW_FORMAT` may then have any row format, old
    /// tables too: MySQL gives a table it rebuilds the new default.
    default_row_format_set: bool,
    /// The input names `innodb_strict_mode`, in any form kilit can see. The
    /// lab measured the default, which is ON.
    strict_mode_set: bool,
    /// What this session's statements leave to the sessions after it.
    later_sessions: LaterSessions,
    /// A session before this one set these settings in its own scope; this
    /// one starts from the MySQL default again, and has not set them yet.
    earlier_session: EarlierSession,
    /// The name of each session's input file, as given, when known.
    file_names: Vec<String>,
}

/// What a session leaves to the sessions after it: a value set globally
/// is the one they start with.
#[derive(Clone, Copy, Default)]
struct LaterSessions {
    foreign_key_checks_global: bool,
    sql_log_bin_global: bool,
    /// `old_alter_table` set globally, or in a form kilit cannot read.
    old_alter_table_unclear: bool,
}

/// Settings an earlier session set for itself only.
#[derive(Clone, Copy, Default)]
struct EarlierSession {
    foreign_key_checks: bool,
    sql_log_bin: bool,
    old_alter_table: bool,
}

/// Whether `sql_mode` is strict in this session, as far as the input says.
/// The lab measured its own default, which is strict, and the session
/// values `''` and `'NO_ENGINE_SUBSTITUTION'`, which are not.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
enum SqlMode {
    /// Nothing in the input set it: the MySQL default, which is strict.
    #[default]
    ServerDefault,
    /// Set for the session to a list kilit reads, with
    /// `STRICT_TRANS_TABLES` or `STRICT_ALL_TABLES`, and other modes of the
    /// lab's default only.
    Strict,
    /// Set for the session to `''` or `'NO_ENGINE_SUBSTITUTION'`.
    NotStrict,
    /// Set in any other way, or globally: why kilit cannot tell. It stays
    /// so for the rest of the input.
    Unclear(String),
}

/// The modes of the lab's `sql_mode`, MySQL 8's default, in uppercase.
const LAB_SQL_MODES: [&str; 6] = [
    "ONLY_FULL_GROUP_BY",
    "STRICT_TRANS_TABLES",
    "NO_ZERO_IN_DATE",
    "NO_ZERO_DATE",
    "ERROR_FOR_DIVISION_BY_ZERO",
    "NO_ENGINE_SUBSTITUTION",
];

impl SqlMode {
    /// The mode after a statement that names `sql_mode`, written in
    /// lowercase as `sql`. Only a session assignment of a quoted list,
    /// alone in the statement, is read; any other mention, such as a
    /// global one, `CONCAT(@@sql_mode, ...)` or a SELECT, leaves it unclear.
    fn read(sql: &str, kind: &StatementKind, line: usize) -> SqlMode {
        let unclear = || {
            SqlMode::Unclear(format!(
                "line {line} sets sql_mode in a form kilit cannot read, or globally"
            ))
        };
        let StatementKind::Set { variables } = kind else {
            return unclear();
        };
        let assigned: Vec<&Variable> = variables
            .iter()
            .filter(|variable| variable.name == "sql_mode")
            .collect();
        let [variable] = assigned.as_slice() else {
            return unclear();
        };
        if sql.matches("sql_mode").count() != 1 || sql.contains("global") || sql.contains("persist")
        {
            return unclear();
        }
        let Some(list) = variable
            .value
            .strip_prefix('\'')
            .and_then(|rest| rest.strip_suffix('\''))
            .filter(|list| !list.contains(['\'', '\\']))
        else {
            return unclear();
        };
        let modes: Vec<String> = list
            .split(',')
            .map(|mode| mode.trim().to_ascii_uppercase())
            .filter(|mode| !mode.is_empty())
            .collect();
        if modes.iter().all(|mode| mode == "NO_ENGINE_SUBSTITUTION") {
            return SqlMode::NotStrict;
        }
        let strict = modes
            .iter()
            .any(|mode| mode == "STRICT_TRANS_TABLES" || mode == "STRICT_ALL_TABLES");
        let measured = modes
            .iter()
            .all(|mode| mode == "STRICT_ALL_TABLES" || LAB_SQL_MODES.contains(&mode.as_str()));
        if strict && measured {
            SqlMode::Strict
        } else {
            SqlMode::Unclear(format!(
                "line {line} sets sql_mode to '{list}', which the lab has not measured"
            ))
        }
    }
}

/// `old_alter_table` in this session, as far as the input says.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum OldAlterTable {
    /// OFF, the MySQL default, or set to OFF.
    #[default]
    Off,
    On,
    /// The statement on this line sets it globally, to `DEFAULT`, or in a
    /// form kilit cannot read. It stays unknown from there on.
    Unknown(usize),
    /// A `/*!` comment on this line, which MySQL runs and kilit does not
    /// read, names it. It stays unknown from there on.
    InComment(usize),
    /// An earlier session set it globally, or in a form kilit cannot read,
    /// and this one starts with the global value.
    UnknownFromEarlierSession,
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
    /// kilit cannot tell the value: [`Session::foreign_key_checks_unknown`]
    /// says why.
    Unknown,
}

impl Session {
    /// Follows the session variables of one SET. When MySQL `may_be_refused`
    /// it, the ones it sets become unknown.
    fn apply(&mut self, variables: &[Variable], line: usize, may_be_refused: bool) {
        for variable in variables {
            match variable.name.as_str() {
                "foreign_key_checks" if !variable.global && may_be_refused => {
                    self.foreign_key_checks = Setting::Unknown;
                    self.foreign_key_checks_global = false;
                    self.earlier_session.foreign_key_checks = false;
                    self.foreign_key_checks_unknown = format!(
                        "line {line} sets foreign_key_checks in a SET that MySQL may refuse as a \
                         whole, and the lab has not measured it"
                    );
                }
                // MySQL refuses 'DEFAULT' in quotes, and keeps the value.
                "foreign_key_checks"
                    if !variable.global
                        && variable.value.starts_with(['\'', '"'])
                        && variable.value[1..]
                            .trim_end_matches(['\'', '"'])
                            .eq_ignore_ascii_case("DEFAULT") => {}
                // A global value does not change this session's.
                "foreign_key_checks" if !variable.global => {
                    self.foreign_key_checks = Setting::parse(&variable.value);
                    self.foreign_key_checks_global = false;
                    self.earlier_session.foreign_key_checks = false;
                    // MySQL gives the session a value that depends on the
                    // global one, which kilit cannot see.
                    if self.foreign_key_checks == Setting::ServerDefault {
                        self.foreign_key_checks = Setting::Unknown;
                        self.foreign_key_checks_unknown = format!(
                            "line {line} sets foreign_key_checks to DEFAULT, which the lab has \
                             not measured"
                        );
                    }
                }
                "sql_log_bin" => {
                    let on = !variable.global && Setting::parse(&variable.value) == Setting::On;
                    self.binary_log = if on {
                        BinaryLog::On
                    } else {
                        BinaryLog::Other(variable.to_string())
                    };
                    self.earlier_session.sql_log_bin = false;
                }
                // `DEFAULT` takes the global value, which kilit cannot see.
                // MySQL takes ON and OFF in quotes, but refuses '1' or 'TRUE'
                // and keeps the value it had.
                "old_alter_table" if !self.old_alter_table.unknown() => {
                    let quoted = variable.value.starts_with(['\'', '"']);
                    let word = variable.value.trim_matches(['\'', '"']);
                    let refused = quoted
                        && !(word.eq_ignore_ascii_case("ON") || word.eq_ignore_ascii_case("OFF"));
                    self.old_alter_table = match Setting::parse(&variable.value) {
                        _ if variable.global || refused || may_be_refused => {
                            OldAlterTable::Unknown(line)
                        }
                        Setting::On => OldAlterTable::On,
                        Setting::Off => OldAlterTable::Off,
                        Setting::ServerDefault | Setting::Unclear | Setting::Unknown => {
                            OldAlterTable::Unknown(line)
                        }
                    };
                    self.earlier_session.old_alter_table = false;
                }
                _ => {}
            }
        }
    }

    /// A new session starts: each followed setting takes its MySQL default
    /// again, or is unknown when an earlier session set it globally. What
    /// kilit saw named anywhere earlier stays seen.
    /// The input file of the statement that took a table away, when the
    /// files have names.
    fn file_of(&self, gone: &Gone) -> Option<&str> {
        self.file_names.get(gone.session).map(String::as_str)
    }

    fn starts_again(&mut self) {
        let later = self.later_sessions;
        let earlier = EarlierSession {
            foreign_key_checks: self.earlier_session.foreign_key_checks
                || self.foreign_key_checks != Setting::ServerDefault
                || self.foreign_key_checks_global,
            sql_log_bin: self.earlier_session.sql_log_bin || self.binary_log != BinaryLog::NotSet,
            old_alter_table: self.earlier_session.old_alter_table
                || self.old_alter_table != OldAlterTable::Off,
        };
        self.foreign_key_checks_global = false;
        if later.foreign_key_checks_global {
            self.foreign_key_checks = Setting::Unknown;
            self.foreign_key_checks_unknown = "an earlier file sets foreign_key_checks \
                globally, and each file runs in a new session, which starts with the global \
                value"
                .to_owned();
        } else {
            self.foreign_key_checks = Setting::ServerDefault;
        }
        self.binary_log = if later.sql_log_bin_global {
            BinaryLog::Other("sql_log_bin globally in an earlier file".to_owned())
        } else {
            BinaryLog::NotSet
        };
        // A session value is gone with its session; an unclear one may
        // have been global.
        if matches!(self.sql_mode, SqlMode::Strict | SqlMode::NotStrict) {
            self.sql_mode = SqlMode::ServerDefault;
        }
        self.old_alter_table = if later.old_alter_table_unclear {
            OldAlterTable::UnknownFromEarlierSession
        } else {
            OldAlterTable::Off
        };
        self.earlier_session = EarlierSession {
            foreign_key_checks: earlier.foreign_key_checks && !later.foreign_key_checks_global,
            sql_log_bin: earlier.sql_log_bin && !later.sql_log_bin_global,
            old_alter_table: earlier.old_alter_table && !later.old_alter_table_unclear,
        };
    }
}

impl OldAlterTable {
    fn unknown(self) -> bool {
        matches!(
            self,
            OldAlterTable::Unknown(_)
                | OldAlterTable::InComment(_)
                | OldAlterTable::UnknownFromEarlierSession
        )
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

const ASSUMES_FOREIGN_KEY_CHECKS_AFTER_GLOBAL: &str = "assumes foreign_key_checks is ON in this \
     session, the MySQL default: a statement before this one changes its global value, which \
     does not change this session";

const ASSUMES_FOREIGN_KEY_CHECKS_IN_A_NEW_SESSION: &str = "assumes foreign_key_checks is ON, \
     the MySQL default: an earlier file sets it, but each file runs in a new session, and \
     nothing in this one sets it before this statement";

const ASSUMES_BINARY_LOG: &str =
    "assumes the binary log is on with binlog_format = ROW, the MySQL default";

const ASSUMES_BINARY_LOG_IN_A_NEW_SESSION: &str = "assumes the binary log is on with \
     binlog_format = ROW, the MySQL default: an earlier file sets sql_log_bin, but each file \
     runs in a new session, and nothing in this one sets it before this statement";

const ASSUMES_OLD_ALTER_TABLE_IN_A_NEW_SESSION: &str = "assumes old_alter_table is OFF, the \
     MySQL default: an earlier file sets it, but each file runs in a new session, and nothing \
     in this one sets it before this statement";

impl Session {
    /// What kilit assumes when no statement before sets this session's
    /// `foreign_key_checks`.
    fn assumes_foreign_key_checks(&self) -> &'static str {
        if self.foreign_key_checks_global {
            ASSUMES_FOREIGN_KEY_CHECKS_AFTER_GLOBAL
        } else if self.earlier_session.foreign_key_checks {
            ASSUMES_FOREIGN_KEY_CHECKS_IN_A_NEW_SESSION
        } else {
            ASSUMES_FOREIGN_KEY_CHECKS
        }
    }

    /// What kilit assumes when no statement before sets this session's
    /// `sql_log_bin`.
    fn assumes_binary_log(&self) -> &'static str {
        if self.earlier_session.sql_log_bin {
            ASSUMES_BINARY_LOG_IN_A_NEW_SESSION
        } else {
            ASSUMES_BINARY_LOG
        }
    }
}

/// Where a `SET` assigns a variable.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Scope {
    Session,
    Global,
    Persist,
    PersistOnly,
}

impl Scope {
    /// Whether the assignment changes the global value: a session started
    /// after it begins with that value.
    fn sets_global(self) -> bool {
        matches!(self, Scope::Global | Scope::Persist)
    }
}

/// The scope of each assignment of the variable `name` in `sql`, a SET in
/// lowercase, as MySQL takes it: `GLOBAL`, `PERSIST`, `PERSIST_ONLY`,
/// `SESSION` or `LOCAL` applies to the assignments after it that have no
/// scope of their own, and `@@global.` and the like to their variable only.
/// kilit reads none of `PERSIST`'s forms as a statement. Reading the global
/// value, as in `SET @saved = @@global.foreign_key_checks`, assigns nothing.
fn scopes_assigned(sql: &str, name: &str) -> Vec<Scope> {
    let sql = without_comments(sql);
    let Some(body) = sql.trim_start().strip_prefix("set") else {
        return Vec::new();
    };
    if !body.starts_with(char::is_whitespace) {
        return Vec::new();
    }
    let mut scopes = Vec::new();
    let mut carried = Scope::Session;
    for assignment in assignments(body) {
        let mut target = assignment.trim_start();
        for (keyword, scope) in [
            ("persist_only", Scope::PersistOnly),
            ("persist", Scope::Persist),
            ("global", Scope::Global),
            ("session", Scope::Session),
            ("local", Scope::Session),
        ] {
            if let Some(rest) = target.strip_prefix(keyword)
                && rest.starts_with(char::is_whitespace)
            {
                carried = scope;
                target = rest.trim_start();
                break;
            }
        }
        let Some((variable, _)) = target.split_once('=') else {
            continue;
        };
        let variable = variable.trim_end();
        let variable = variable.strip_suffix(':').unwrap_or(variable).trim_end();
        let (scope, variable) = [
            ("@@persist_only.", Scope::PersistOnly),
            ("@@persist.", Scope::Persist),
            ("@@global.", Scope::Global),
            ("@@session.", Scope::Session),
            ("@@local.", Scope::Session),
            ("@@", Scope::Session),
        ]
        .into_iter()
        .find_map(|(prefix, scope)| Some((scope, variable.strip_prefix(prefix)?)))
        .unwrap_or((carried, variable));
        if variable.trim_matches('`') == name {
            scopes.push(scope);
        }
    }
    scopes
}

/// `sql` with each comment outside quotes made one space, except a `/*!`
/// comment, which MySQL runs: its text stays.
fn without_comments(sql: &str) -> String {
    let mut out = String::with_capacity(sql.len());
    let mut rest = sql;
    let mut quote = None;
    while let Some(c) = rest.chars().next() {
        if let Some(open) = quote {
            if c == open {
                quote = None;
            }
        } else if c == '\'' || c == '"' || c == '`' {
            quote = Some(c);
        } else if rest.starts_with("/*") && !rest.starts_with("/*!") {
            let end = rest[2..].find("*/").map_or(rest.len(), |at| at + 4);
            out.push(' ');
            rest = &rest[end..];
            continue;
        } else if rest.starts_with("-- ") || rest.starts_with('#') {
            let end = rest.find('\n').unwrap_or(rest.len());
            out.push(' ');
            rest = &rest[end..];
            continue;
        }
        out.push(c);
        rest = &rest[c.len_utf8()..];
    }
    out
}

/// The assignments of a SET's body: its text between commas outside quotes
/// and parentheses.
fn assignments(body: &str) -> Vec<&str> {
    let mut pieces = Vec::new();
    let mut start = 0;
    let mut depth = 0usize;
    let mut quote = None;
    for (at, c) in body.char_indices() {
        match quote {
            Some(open) if c == open => quote = None,
            Some(_) => {}
            None => match c {
                '\'' | '"' | '`' => quote = Some(c),
                '(' => depth += 1,
                ')' => depth = depth.saturating_sub(1),
                ',' if depth == 0 => {
                    pieces.push(&body[start..at]);
                    start = at + 1;
                }
                _ => {}
            },
        }
    }
    pieces.push(&body[start..]);
    pieces
}

fn predict<'r>(
    alter: &Alter,
    sql: &str,
    session: &Session,
    schema: &Schema,
    results: &'r ServerResults,
    agreements: &mut Agreements<'r>,
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
    if let Some(reason) = qualified_name(&alter.table, table, schema) {
        return unknown(reason);
    }
    if let Some(reason) = not_in_dump(&alter.table, table, schema, session) {
        return unknown(format!("the statement is on {reason}"));
    }
    if let Operation::AddForeignKey(key) = operation {
        let parent = schema.table(&key.referenced_table);
        if let Some(reason) = qualified_name(&key.referenced_table, parent, schema) {
            return unknown(format!("the foreign key points at {reason}"));
        }
        if matches!(
            session.foreign_key_checks,
            Setting::On | Setting::ServerDefault
        ) && let Some(reason) = not_in_dump(&key.referenced_table, parent, schema, session)
        {
            return unknown(format!("the foreign key points at {reason}"));
        }
    }
    match session.old_alter_table {
        OldAlterTable::Unknown(line) => {
            return unknown(format!(
                "line {line} sets old_alter_table globally, to DEFAULT, or in a form kilit \
                 cannot read, and with it ON MySQL copies the table"
            ));
        }
        OldAlterTable::InComment(line) => {
            return unknown(format!(
                "{}, and with it ON MySQL copies the table",
                in_a_comment(line, "old_alter_table")
            ));
        }
        OldAlterTable::UnknownFromEarlierSession => {
            return unknown(
                "an earlier file sets old_alter_table globally or in a form kilit cannot read, \
                 and each file runs in a new session, which starts with the global value; with \
                 it ON MySQL copies the table",
            );
        }
        OldAlterTable::Off | OldAlterTable::On => {}
    }
    // The engine decides which measurements apply, before any case.
    let myisam = match measured_engine(table, &alter.table) {
        Ok(myisam) => myisam,
        Err(reason) => return unknown(reason),
    };
    if let Some(known) = myisam {
        if known.row_format.is_some() || known.key_block_size.is_some() {
            return unknown(format!(
                "{} is a MyISAM table with ROW_FORMAT or KEY_BLOCK_SIZE, which the lab has \
                 not measured",
                alter.table
            ));
        }
        if session.old_alter_table == OldAlterTable::On {
            return unknown(
                "the input sets old_alter_table ON, which the lab has not measured on a MyISAM \
                 table",
            );
        }
    }
    // So does the row format, and old_alter_table.
    let measured = if myisam.is_some() {
        None
    } else {
        match (
            measured_row_format(table, &alter.table, session),
            session.old_alter_table == OldAlterTable::On,
        ) {
            (Err(reason), _) => return unknown(reason),
            (Ok(Some(row_format)), true) => {
                return unknown(format!(
                    "the input sets old_alter_table ON, which the lab has not measured {}",
                    row_format.name
                ));
            }
            (Ok(Some(row_format)), false) => Some(row_format),
            (Ok(None), true) => Some(&WITH_OLD_ALTER_TABLE),
            (Ok(None), false) => None,
        }
    };
    if myisam.is_none()
        && let Operation::AddForeignKey(key) = operation
        && let Err(reason) = parent_engine(key, schema)
    {
        return unknown(reason);
    }
    let Classified {
        case,
        notes,
        unmeasured,
        column,
        row,
    } = match classify(operation, sql, session, table, schema, results) {
        Ok(classified) => classified,
        Err(reason) => return unknown(reason),
    };
    let Some(variant) = variant_of(alter) else {
        return unknown(format!(
            "the statement asks for {}, which the lab has not measured",
            clauses(alter)
        ));
    };
    let situation = Situation {
        table,
        column: column.as_deref(),
        dropped: match operation {
            Operation::DropColumn { name } => Some(name),
            _ => None,
        },
        index: match operation {
            Operation::AddIndex(index) => Some(index),
            _ => None,
        },
        dropped_index: match operation {
            Operation::DropIndex { name } => Some(name),
            _ => None,
        },
        foreign_key: match operation {
            Operation::AddForeignKey(key) => Some(key),
            _ => None,
        },
        dropped_foreign_key: match operation {
            Operation::DropForeignKey { name } => Some(name),
            _ => None,
        },
        default: match operation {
            Operation::SetColumnDefault { default, .. } => Some(default),
            _ => None,
        },
        constraint_clause: matches!(
            operation,
            Operation::DropConstraint { .. }
                | Operation::AlterCheck {
                    constraint: true,
                    ..
                }
        ),
        row: &row,
        session,
        schema,
    };
    let prediction = match (myisam, measured) {
        // The error comes from the statement alone, whatever the engine or
        // row format.
        (Some(_), _) | (None, Some(_)) if std::ptr::eq(case, &ADD_FULLTEXT_INDEX_WITH_ORDER) => {
            let mut notes = notes;
            notes.push(
                "the lab measured it on a DYNAMIC table; by hand on MySQL 8.0.46 and 8.4.13, \
                 MyISAM and COMPACT tables refuse it alike"
                    .to_owned(),
            );
            answer(case, &situation, variant, results, agreements, notes)
        }
        (Some(table), _) => on_myisam(
            operation, sql, case, table, &situation, variant, results, agreements, notes,
        ),
        (None, Some(measured)) => measured_apart(
            measured, operation, sql, case, &situation, variant, results, agreements, notes,
        ),
        (None, None) => {
            // The notes `answer` starts from are classify's.
            let classified = notes.len();
            let base = answer(case, &situation, variant, results, agreements, notes);
            if matches!(table, TableState::Known(_)) {
                base
            } else {
                on_a_table_kilit_does_not_know(
                    alter, case, operation, &situation, variant, results, agreements, classified,
                    base,
                )
            }
        }
    };
    match (unmeasured, &prediction) {
        (Some(reason), Prediction::Measured { .. } | Prediction::DependsOnTable { .. }) => {
            unknown(reason)
        }
        _ => prediction,
    }
}

/// Why kilit does not answer for the table `name`, when its tables come
/// from a dump and kilit has no table of that name at this statement.
fn not_in_dump(
    name: &str,
    table: TableState<'_>,
    schema: &Schema,
    session: &Session,
) -> Option<String> {
    if table != TableState::Missing || !schema.is_from_dump() {
        return None;
    }
    if let Some(gone) = schema.gone(name) {
        return Some(format!(
            "{name}, {}, and no statement after it creates it again: MySQL refuses the \
             statement if {name} does not exist",
            gone_by(gone, session)
        ));
    }
    Some(format!(
        "{name}, which kilit has neither in the schema dump nor from a statement before this \
         one: the dump may be out of date, or {name} does not exist and MySQL refuses the \
         statement"
    ))
}

/// The answer from `case`'s measurements on this server, with what the
/// situation shows.
fn answer<'r>(
    case: &'static Case,
    situation: &Situation<'_>,
    variant: Variant,
    results: &'r ServerResults,
    agreements: &mut Agreements<'r>,
    mut notes: Vec<String>,
) -> Prediction {
    // All of the case's scenarios must agree on this server.
    let verdict = match agreements.agreed(case, Group::Case, variant, results) {
        Ok(verdict) => verdict,
        Err(reason) => return unknown(reason),
    };

    // A situation kilit can see in the table decides the answer; one it
    // cannot see becomes a note. When several hold, the exception measured
    // with all of their conditions decides, never the order of the list.
    let mut holding = Vec::new();
    let mut unseen = Vec::new();
    for (at, exception) in case.exceptions.iter().enumerate() {
        match holds(exception, situation) {
            Applies::Yes(why) => holding.push((at, exception, why)),
            Applies::Unmeasured(reason) => return unknown(reason),
            Applies::No => {}
            Applies::Maybe => unseen.push((at, exception)),
        }
    }
    if !holding.is_empty() {
        let (at, exception, why) = match deciding(holding) {
            Ok(decides) => decides,
            Err(reason) => return unknown(reason),
        };
        let other = match agreements.agreed(case, Group::Exception(at), variant, results) {
            Ok(other) => other,
            Err(reason) => return unknown(format!("{}, and {reason}", why.join(", and "))),
        };
        notes.extend(why);
        // The same situation measured with rows kilit cannot see, such as
        // duplicates, still becomes a note.
        for (hidden_at, hidden) in &unseen {
            if !hidden.conditions.contains(&Condition::Hidden) || !includes(hidden, exception) {
                continue;
            }
            if let Ok(rows) =
                agreements.agreed(case, Group::Exception(*hidden_at), variant, results)
            {
                notes.push(format!(
                    "{}: {} ({})",
                    hidden.when,
                    brief(&rows),
                    scenario_list(hidden.scenarios)
                ));
            }
        }
        return Prediction::Measured {
            case: case.id,
            verdict: other,
            evidence: exception.scenarios.to_vec(),
            notes,
        };
    }
    for (at, exception) in unseen {
        let Ok(other) = agreements.agreed(case, Group::Exception(at), variant, results) else {
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

/// Which measurements a table gets: `None` for InnoDB's, the table for
/// MyISAM's, or why there are none. A table created without `ENGINE` is
/// InnoDB, the server's default, and so is one kilit does not know. A
/// table kilit lost track of keeps the InnoDB answer only when it was
/// InnoDB or no statement named its engine: MyISAM's are given to known
/// tables only.
fn measured_engine<'a>(table: TableState<'a>, name: &str) -> Result<Option<&'a Table>, String> {
    let engine = match table {
        TableState::Missing => return Ok(None),
        TableState::Known(known) => match &known.engine {
            None | Some(Engine::InnoDb) => return Ok(None),
            Some(Engine::MyIsam) => return Ok(Some(known)),
            Some(other) => other,
        },
        TableState::Lost { engine, .. } => match engine {
            None | Some(Engine::InnoDb) => return Ok(None),
            Some(Engine::MyIsam) => {
                return Err(format!(
                    "kilit lost track of {name}, a MyISAM table, and answers for MyISAM \
                     tables only when it knows them"
                ));
            }
            Some(other) => other,
        },
    };
    Err(match engine {
        Engine::Unknown => format!(
            "kilit cannot tell the engine of {name}: the input sets default_storage_engine \
             or default_tmp_storage_engine before creating it, or a statement names an engine \
             for it that kilit cannot follow"
        ),
        other => format!(
            "{name} uses the {other} engine; the lab has measured InnoDB tables, and MyISAM \
             tables for some changes"
        ),
    })
}

/// A foreign key from an InnoDB table to a table of another engine: MySQL
/// refuses it with the checks on, and the lab has not measured it.
fn parent_engine(key: &ForeignKey, schema: &Schema) -> Result<(), String> {
    let engine = match schema.table(&key.referenced_table) {
        TableState::Known(parent) => parent.engine.as_ref(),
        TableState::Lost { engine, .. } => engine,
        TableState::Missing => None,
    };
    match engine {
        None | Some(Engine::InnoDb) => Ok(()),
        Some(Engine::Unknown) => Err(format!(
            "kilit cannot tell the engine of {}, and the lab has measured foreign keys \
             between InnoDB tables only",
            key.referenced_table
        )),
        Some(other) => Err(format!(
            "{} uses the {other} engine, and the lab has measured foreign keys between \
             InnoDB tables only",
            key.referenced_table
        )),
    }
}

/// Which measurements an InnoDB table gets for its row format: `None` for
/// DYNAMIC's, the server's default, the row format's own, or why there are
/// none. A table without `ROW_FORMAT`, or with `ROW_FORMAT=DEFAULT`, is
/// DYNAMIC, unless the input names `innodb_default_row_format`, and so is
/// one kilit does not know or lost track of, unless something may have
/// given it another row format.
fn measured_row_format(
    table: TableState<'_>,
    name: &str,
    session: &Session,
) -> Result<Option<&'static Measured>, String> {
    let default_changed = || {
        Err(format!(
            "the input names innodb_default_row_format, so {name} may have any row format: \
             it writes none kilit knows"
        ))
    };
    let known = match table {
        TableState::Known(known) => known,
        TableState::Lost {
            row_format_unclear: true,
            ..
        } => {
            return Err(format!(
                "kilit lost track of {name}, which may have a row format other than DYNAMIC, \
                 and answers for other row formats only when it knows the table"
            ));
        }
        TableState::Lost { .. } | TableState::Missing if session.default_row_format_set => {
            return default_changed();
        }
        TableState::Lost { .. } | TableState::Missing => return Ok(None),
    };
    let row_format = known.row_format.as_ref();
    let key_block_size = known.key_block_size.as_deref();
    if let Some(size) = key_block_size
        && size.starts_with(['\'', '"', '`'])
    {
        return Err(format!(
            "{REJECTED_BY} a quoted KEY_BLOCK_SIZE, such as {name}'s {size}"
        ));
    }
    if let Some(size) = key_block_size
        && size != "0"
    {
        // Any valid KEY_BLOCK_SIZE compresses the table; the lab measured 8.
        return match row_format {
            None | Some(RowFormat::Default | RowFormat::Compressed) if size == "8" => {
                Ok(Some(&ON_COMPRESSED))
            }
            None | Some(RowFormat::Default | RowFormat::Compressed) => Err(format!(
                "{name} is COMPRESSED with KEY_BLOCK_SIZE={size}, and the lab has measured \
                 KEY_BLOCK_SIZE=8 only"
            )),
            Some(other @ (RowFormat::Dynamic | RowFormat::Compact | RowFormat::Redundant)) => {
                Err(format!(
                    "{name} writes KEY_BLOCK_SIZE={size} with ROW_FORMAT={other}, which MySQL \
                     8.0.46 and 8.4.13 refuse unless innodb_strict_mode is OFF"
                ))
            }
            Some(other) => Err(unmeasured_row_format(name, other)),
        };
    }
    match row_format {
        None | Some(RowFormat::Default) if session.default_row_format_set => default_changed(),
        None | Some(RowFormat::Default | RowFormat::Dynamic) => Ok(None),
        Some(_) if key_block_size.is_some() => Err(format!(
            "{name} writes KEY_BLOCK_SIZE=0 with ROW_FORMAT={}, which the lab has not measured",
            row_format.map(ToString::to_string).unwrap_or_default()
        )),
        Some(RowFormat::Compressed) => Ok(Some(&ON_COMPRESSED)),
        Some(RowFormat::Compact) => Ok(Some(&ON_COMPACT)),
        Some(RowFormat::Redundant) => Ok(Some(&ON_REDUNDANT)),
        Some(other) => Err(unmeasured_row_format(name, other)),
    }
}

/// Why a row format kilit does not know gets no answer.
fn unmeasured_row_format(name: &str, row_format: &RowFormat) -> String {
    match row_format {
        RowFormat::Unknown => format!(
            "kilit cannot tell the row format of {name}: the input names innodb_strict_mode or \
             innodb_default_row_format before creating it without ROW_FORMAT"
        ),
        RowFormat::Other(written) if written.starts_with(['\'', '"', '`']) => {
            format!("{REJECTED_BY} a quoted ROW_FORMAT, such as {name}'s {written}")
        }
        other => format!(
            "{name} has ROW_FORMAT={other}, which the lab has not measured; it measured \
             DYNAMIC, COMPRESSED, COMPACT and REDUNDANT"
        ),
    }
}

/// The answer in a situation the lab measured apart, a row format or
/// old_alter_table: its own case of the change, if the lab measured it in
/// this shape and situation, never the InnoDB case's answer. With
/// old_alter_table, a written `ALGORITHM` other than `DEFAULT` wins, where
/// the lab saw it win: see [`clause_wins`].
#[allow(clippy::too_many_arguments)]
fn measured_apart<'r>(
    measured: &Measured,
    operation: &Operation,
    sql: &str,
    innodb: &'static Case,
    situation: &Situation<'_>,
    variant: Variant,
    results: &'r ServerResults,
    agreements: &mut Agreements<'r>,
    mut notes: Vec<String>,
) -> Prediction {
    let case = match measured_case(measured, operation, sql, innodb, situation, &mut notes) {
        Ok(case) => case,
        Err(reason) => return unknown(reason),
    };
    // A situation of the InnoDB case's that holds, or may hold, has not
    // been measured here. The hidden ones become notes, from the InnoDB
    // case's measurements, saying where they come from.
    let mut hidden = Vec::new();
    for (at, exception) in innodb.exceptions.iter().enumerate() {
        if exception
            .conditions
            .iter()
            .all(|condition| *condition == Condition::Hidden)
        {
            hidden.push((at, exception));
            continue;
        }
        match holds(exception, situation) {
            Applies::No => {}
            Applies::Unmeasured(reason) => return unknown(reason),
            Applies::Yes(why) => {
                return unknown(format!(
                    "{}, and the lab has not measured that {}",
                    why.join(", and "),
                    measured.name
                ));
            }
            Applies::Maybe if exception.conditions == [Condition::RowTooLarge] => {
                notes.push(row_not_counted(situation.row, measured.name));
            }
            Applies::Maybe => {
                return unknown(format!(
                    "kilit cannot tell {}, and the lab has not measured that {}",
                    exception.when, measured.name
                ));
            }
        }
    }
    if measured.old_alter_table && variant.algorithm.is_some() {
        if let Err(reason) = clause_wins(case, variant, results, agreements) {
            return unknown(reason);
        }
        return answer(innodb, situation, variant, results, agreements, notes);
    }
    // A key too long fails whatever the rows hold.
    let too_long = measured
        .key_too_long
        .is_some_and(|too_long| too_long.id == case.id);
    for (at, exception) in hidden.into_iter().filter(|_| !too_long) {
        let Ok(other) = agreements.agreed(innodb, Group::Exception(at), variant, results) else {
            continue;
        };
        notes.push(format!(
            "{}: not measured {}; {}, {} ({})",
            exception.when,
            measured.name,
            measured.instead,
            brief(&other),
            scenario_list(exception.scenarios)
        ));
    }
    answer(case, situation, variant, results, agreements, notes)
}

/// The case of a change in the InnoDB case `innodb` in a situation measured
/// apart, if the lab measured that shape there: the shapes of its scenarios.
fn measured_case(
    measured: &Measured,
    operation: &Operation,
    sql: &str,
    innodb: &Case,
    situation: &Situation<'_>,
    notes: &mut Vec<String>,
) -> Result<&'static Case, String> {
    let not_measured = |what: &str| Err(format!("{what} has not been measured {}", measured.name));
    let Some(&(_, case)) = measured.cases.iter().find(|(id, _)| *id == innodb.id) else {
        return not_measured(innodb.id);
    };
    let table = || match situation.table {
        TableState::Known(table) => Ok(table),
        _ => Err(format!(
            "kilit does not know the table, and needs it for {}, which the lab measured in one \
             shape {}",
            innodb.id, measured.name
        )),
    };
    match (innodb.id, operation) {
        ("add-column", Operation::AddColumn { .. })
        | ("drop-column", Operation::DropColumn { .. })
        | ("rename-column", Operation::RenameColumn { .. })
        | ("set-column-default", Operation::SetColumnDefault { .. }) => Ok(case),
        ("change-integer-type", Operation::ModifyColumn { column, .. }) => {
            let int = DataType::Integer {
                size: IntegerSize::Int,
                unsigned: false,
            };
            let bigint = DataType::Integer {
                size: IntegerSize::Big,
                unsigned: false,
            };
            let old = table()?.column(&column.name).map(|old| &old.data_type);
            if old == Some(&int) && column.data_type == bigint {
                Ok(case)
            } else {
                not_measured("an integer change other than INT to BIGINT")
            }
        }
        ("add-index" | "add-unique-index", Operation::AddIndex(index)) => {
            let table = table()?;
            let [entry] = index.columns.as_slice() else {
                return not_measured("an index of several columns");
            };
            if has_descending(sql) {
                return not_measured("a descending index");
            }
            let session = situation.session;
            if session.default_row_format_set || session.strict_mode_set {
                return Err(
                    "the input names innodb_default_row_format or innodb_strict_mode, which the \
                     lab has not measured, and the most bytes a key holds may depend on them"
                        .to_owned(),
                );
            }
            let bytes = key_bytes(table, entry, measured.name)?;
            if bytes <= measured.key_bytes {
                return Ok(case);
            }
            let Some(too_long) = measured.key_too_long else {
                return not_measured(&format!("a key longer than {} bytes", measured.key_bytes));
            };
            if session.sql_mode_set {
                return not_measured(&format!(
                    "a key longer than {} bytes after the input sets sql_mode",
                    measured.key_bytes
                ));
            }
            notes.push(ASSUMES_STRICT_SQL_MODE.to_owned());
            Ok(too_long)
        }
        ("add-fulltext-index", Operation::AddIndex(index)) => {
            let table = table()?;
            let [entry] = index.columns.as_slice() else {
                return not_measured("a FULLTEXT index of several columns");
            };
            match table.column(entry) {
                Some(column) if is_plain_text(table, column) => Ok(case),
                _ => not_measured("a FULLTEXT index on anything but one TEXT column"),
            }
        }
        ("drop-index", Operation::DropIndex { name }) => {
            let plain = table()?.indexes.iter().any(|index| {
                index.kind == IndexKind::Plain
                    && index
                        .name
                        .as_deref()
                        .is_some_and(|n| n.eq_ignore_ascii_case(name))
            });
            if plain {
                Ok(case)
            } else {
                not_measured(&format!(
                    "dropping {name}, which is not a plain index kilit knows,"
                ))
            }
        }
        _ => not_measured(&format!("this form of {}", innodb.id)),
    }
}

/// Whether a written `ALGORITHM` wins over old_alter_table for every
/// scenario of `case` on this server: the lab's run of `variant` with
/// old_alter_table on left exactly the traces of its run with it off.
fn clause_wins(
    case: &Case,
    variant: Variant,
    results: &ServerResults,
    agreements: &mut Agreements<'_>,
) -> Result<(), String> {
    let observed = |id: &str| {
        results.scenario(id).and_then(|scenario| {
            scenario
                .observations
                .iter()
                .find(|observation| observation.variant == variant)
                .map(|observation| &observation.outcome)
        })
    };
    for id in case.scenarios {
        let twin = OLD_ALTER_TABLE_TWINS
            .iter()
            .find(|(scenario, _)| scenario == id)
            .map(|(_, twin)| *twin);
        agreements.server_dependent |=
            results.varies(id) || twin.is_some_and(|twin| results.varies(twin));
        let same = twin.is_some_and(|twin| {
            let (with, without) = (observed(id), observed(twin));
            with.is_some() && with == without
        });
        if !same {
            return Err(format!(
                "the lab has not seen {variant} win over old_alter_table for {} on {}",
                case.id,
                results.server.name()
            ));
        }
    }
    Ok(())
}

/// The answer for a MyISAM table: the MyISAM case of the change, if the lab
/// measured it in this shape and situation, never an InnoDB one.
#[allow(clippy::too_many_arguments)]
fn on_myisam<'r>(
    operation: &Operation,
    sql: &str,
    innodb: &'static Case,
    table: &Table,
    situation: &Situation<'_>,
    variant: Variant,
    results: &'r ServerResults,
    agreements: &mut Agreements<'r>,
    mut notes: Vec<String>,
) -> Prediction {
    let Some((_, _, measured)) = ON_MYISAM.iter().find(|(id, ..)| *id == innodb.id) else {
        return unknown(format!(
            "{} has not been measured on a MyISAM table",
            innodb.id
        ));
    };
    let case = match myisam_case(
        operation,
        sql,
        innodb.id,
        table,
        situation.session,
        &mut notes,
    ) {
        Ok(case) => case,
        Err(reason) => return unknown(reason),
    };
    // InnoDB's exceptions show the situations the MyISAM scenarios did not
    // measure.
    for exception in innodb.exceptions {
        let seen = exception
            .conditions
            .iter()
            .all(|condition| *condition == Condition::Hidden || measured.contains(condition));
        match holds(exception, situation) {
            Applies::No => {}
            Applies::Unmeasured(reason) => return unknown(reason),
            _ if seen => {}
            Applies::Yes(why) => {
                return unknown(format!(
                    "{}, and the lab has not measured that on a MyISAM table",
                    why.join(", and ")
                ));
            }
            Applies::Maybe if exception.conditions == [Condition::RowTooLarge] => {
                notes.push(row_not_counted(situation.row, "on a MyISAM table"));
            }
            Applies::Maybe => {
                return unknown(format!(
                    "kilit cannot tell {}, and the lab has not measured that on a MyISAM table",
                    exception.when
                ));
            }
        }
    }
    answer(case, situation, variant, results, agreements, notes)
}

/// The MyISAM case of a change in the InnoDB case `innodb`, if the lab
/// measured that shape on MyISAM: the shapes of the MyISAM scenarios.
fn myisam_case(
    operation: &Operation,
    sql: &str,
    innodb: &str,
    table: &Table,
    session: &Session,
    notes: &mut Vec<String>,
) -> Result<&'static Case, String> {
    let not_measured = |what: &str| Err(format!("{what} has not been measured on a MyISAM table"));
    match (innodb, operation) {
        ("add-column", Operation::AddColumn { .. }) => Ok(&MYISAM_ADD_COLUMN),
        ("drop-column", Operation::DropColumn { name }) => {
            let in_own_plain_index = |index: &Index| {
                index.kind == IndexKind::Plain
                    && matches!(index.columns.as_slice(), [only] if only.eq_ignore_ascii_case(name))
            };
            let in_other_key = table
                .primary_key
                .iter()
                .chain(
                    table
                        .indexes
                        .iter()
                        .filter(|index| !in_own_plain_index(index))
                        .flat_map(|index| &index.columns),
                )
                .any(|entry| entry_column(entry).is_some_and(|c| c.eq_ignore_ascii_case(name)));
            if in_other_key {
                return not_measured(&format!(
                    "dropping a column that is in a key other than a plain index of its own, \
                     such as {name},"
                ));
            }
            Ok(&MYISAM_DROP_COLUMN)
        }
        ("rename-column", Operation::RenameColumn { .. }) => Ok(&MYISAM_RENAME_COLUMN),
        ("set-column-default", Operation::SetColumnDefault { .. }) => {
            Ok(&MYISAM_SET_COLUMN_DEFAULT)
        }
        ("change-integer-type", Operation::ModifyColumn { column, .. }) => {
            let int = DataType::Integer {
                size: IntegerSize::Int,
                unsigned: false,
            };
            let bigint = DataType::Integer {
                size: IntegerSize::Big,
                unsigned: false,
            };
            let old = table.column(&column.name).map(|old| &old.data_type);
            if old == Some(&int) && column.data_type == bigint {
                Ok(&MYISAM_CHANGE_INTEGER_TYPE)
            } else {
                not_measured("an integer change other than INT to BIGINT")
            }
        }
        ("add-index" | "add-unique-index", Operation::AddIndex(index)) => {
            let [entry] = index.columns.as_slice() else {
                return not_measured("an index of several columns");
            };
            if has_descending(sql) {
                return not_measured("a descending index");
            }
            let bytes = myisam_key(table, entry)?;
            if bytes > MYISAM_KEY_BYTES {
                if session.sql_mode_set {
                    return not_measured(
                        "a key longer than 1000 bytes after the input sets sql_mode",
                    );
                }
                notes.push(ASSUMES_STRICT_SQL_MODE.to_owned());
                return Ok(&MYISAM_ADD_KEY_TOO_LONG);
            }
            Ok(if index.kind == IndexKind::Unique {
                &MYISAM_ADD_UNIQUE_INDEX
            } else {
                &MYISAM_ADD_INDEX
            })
        }
        ("add-fulltext-index", Operation::AddIndex(index)) => {
            let [entry] = index.columns.as_slice() else {
                return not_measured("a FULLTEXT index of several columns");
            };
            match table.column(entry) {
                Some(column) if matches!(column.data_type, DataType::VarChar { .. }) => {
                    Ok(&MYISAM_ADD_FULLTEXT_INDEX)
                }
                Some(column) if is_plain_text(table, column) => Ok(&MYISAM_ADD_FULLTEXT_INDEX),
                _ => not_measured("a FULLTEXT index on anything but one VARCHAR or TEXT column"),
            }
        }
        ("drop-index", Operation::DropIndex { name }) => {
            let plain = table.indexes.iter().any(|index| {
                index.kind == IndexKind::Plain
                    && index
                        .name
                        .as_deref()
                        .is_some_and(|n| n.eq_ignore_ascii_case(name))
            });
            if plain {
                Ok(&MYISAM_DROP_INDEX)
            } else {
                not_measured(&format!(
                    "dropping {name}, which is not a plain index kilit knows,"
                ))
            }
        }
        ("add-foreign-key", Operation::AddForeignKey(key)) => {
            let [column] = key.columns.as_slice() else {
                return not_measured("a foreign key of several columns");
            };
            if names_foreign_key_index(sql) {
                return not_measured("a foreign key that names its index");
            }
            if table.column(column).is_none() {
                return Err(format!(
                    "{} has no column {column} that kilit knows",
                    table.name
                ));
            }
            let own_index = table.indexes.iter().any(|index| {
                index.kind == IndexKind::Plain
                    && matches!(index.columns.as_slice(), [only] if only.eq_ignore_ascii_case(column))
            });
            if !own_index {
                // MySQL adds the key's index, which must fit: see `myisam_key`.
                let in_other_key = table
                    .primary_key
                    .iter()
                    .chain(table.indexes.iter().flat_map(|index| &index.columns))
                    .any(|entry| {
                        entry_column(entry).is_some_and(|c| c.eq_ignore_ascii_case(column))
                    });
                if in_other_key {
                    return not_measured(&format!(
                        "a foreign key on {column}, which is in a key of another shape,"
                    ));
                }
                let name = key.name.as_deref().unwrap_or(column);
                let taken = table.indexes.iter().any(|index| {
                    index
                        .name
                        .as_deref()
                        .is_some_and(|n| n.eq_ignore_ascii_case(name))
                });
                if taken {
                    return not_measured(&format!(
                        "a foreign key whose index would take the name {name}, which an index \
                         of the table has,"
                    ));
                }
                if myisam_key(table, column)? > MYISAM_KEY_BYTES {
                    return not_measured(&format!(
                        "a foreign key whose index on {column} would be longer than \
                         {MYISAM_KEY_BYTES} bytes"
                    ));
                }
            }
            Ok(&MYISAM_ADD_FOREIGN_KEY)
        }
        ("drop-foreign-key", Operation::DropForeignKey { .. }) => Ok(&MYISAM_DROP_FOREIGN_KEY),
        _ => not_measured(&format!("this form of {innodb}")),
    }
}

const ASSUMES_STRICT_SQL_MODE: &str = "assumes strict SQL mode, the MySQL default";

/// The most bytes a MyISAM key holds.
const MYISAM_KEY_BYTES: u64 = 1000;

/// The bytes a key of one column of a MyISAM table takes: see [`key_bytes`].
fn myisam_key(table: &Table, entry: &str) -> Result<u64, String> {
    key_bytes(table, entry, "on a MyISAM table")
}

/// The bytes a key of one column takes, for the types the lab measured in
/// a situation measured apart (`on a MyISAM table`): an integer, which is far
/// below every limit, and a utf8mb4 VARCHAR, at 4 bytes a character. A
/// prefix, any other type, and any other character set have not been
/// measured.
fn key_bytes(table: &Table, entry: &str, measured_on: &str) -> Result<u64, String> {
    let not_measured = |what: String| Err(format!("{what} has not been measured {measured_on}"));
    if is_expression(entry) || has_prefix(entry) {
        return not_measured(format!("a key on {entry}"));
    }
    let Some(column) = table.column(entry) else {
        return Err(format!(
            "{} has no column {entry} that kilit knows",
            table.name
        ));
    };
    match &column.data_type {
        DataType::Integer { .. } => Ok(8),
        DataType::VarChar { length } => {
            let settings = table.text_settings(column)?;
            if settings.charset.name == "utf8mb4" {
                Ok(length.saturating_mul(settings.charset.max_bytes))
            } else {
                not_measured(format!(
                    "a key on a {} VARCHAR column",
                    settings.charset.name
                ))
            }
        }
        other => not_measured(format!("a key on a column of type {}", type_name(other))),
    }
}

/// A type as kilit shows it in a reason.
fn type_name(data_type: &DataType) -> String {
    match data_type {
        DataType::Integer { .. } => "integer".to_owned(),
        DataType::VarChar { length } => format!("VARCHAR({length})"),
        DataType::Text(text) | DataType::Other(text) => text.clone(),
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

/// Which scenarios of a case [`agreed`] judges: the case's own, or those
/// of the exception at this position in the case's list.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Group {
    Case,
    Exception(usize),
}

/// What [`agreed`] said during one [`check`], so that each group of
/// scenarios is judged once per server and variant, not once per
/// statement. Found by the case id, then by the server, the group and the
/// variant. These name the scenarios: case ids are unique (a test checks),
/// and the group picks the case's own or one exception's.
#[derive(Default)]
struct Agreements<'r> {
    by_case: HashMap<&'static str, Vec<Agreement<'r>>>,
    /// The statement being checked has read a lab result that another
    /// measured server gives differently, or depends on the server in a
    /// way kilit knows: see [`Finding::server_dependent`].
    server_dependent: bool,
}

struct Agreement<'r> {
    server: &'r Server,
    group: Group,
    variant: Variant,
    said: Result<Verdict, String>,
    /// One of the scenarios differs on another measured server.
    varies: bool,
}

impl<'r> Agreements<'r> {
    fn agreed(
        &mut self,
        case: &'static Case,
        group: Group,
        variant: Variant,
        results: &'r ServerResults,
    ) -> Result<Verdict, String> {
        let server = &results.server;
        let seen = self.by_case.entry(case.id).or_default();
        if let Some(agreement) = seen.iter().find(|agreement| {
            agreement.group == group && agreement.variant == variant && agreement.server == server
        }) {
            self.server_dependent |= agreement.varies;
            return agreement.said.clone();
        }
        let scenarios = match group {
            Group::Case => case.scenarios,
            Group::Exception(at) => case.exceptions[at].scenarios,
        };
        let said = agreed(scenarios, case.id, variant, results);
        let varies = scenarios.iter().any(|id| results.varies(id));
        seen.push(Agreement {
            server,
            group,
            variant,
            said: said.clone(),
            varies,
        });
        self.server_dependent |= varies;
        said
    }
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

/// When several exceptions hold, the one whose conditions include the
/// conditions of every other one that holds decides: the lab measured that
/// very combination. A column in a foreign key, for one, is in an index
/// too, so dropping it makes the exceptions for an indexed column and for
/// a foreign key column hold; the second was measured with both
/// conditions, and decides. No two exceptions of a case have the same
/// conditions (a test checks), so at most one includes all the others,
/// and the order of the list never picks. Without one, kilit does not
/// pick: the answer is unknown. Each exception comes with its position in
/// the case's list, which goes back with the one that decides.
fn deciding(
    mut holding: Vec<(usize, &'static Exception, Vec<String>)>,
) -> Result<(usize, &'static Exception, Vec<String>), String> {
    let includes_every_other = |exception: &Exception| {
        holding
            .iter()
            .all(|(_, other, _)| includes(exception, other))
    };
    if let Some(found) = holding
        .iter()
        .position(|(_, exception, _)| includes_every_other(exception))
    {
        return Ok(holding.swap_remove(found));
    }
    let mut reasons: Vec<&str> = Vec::new();
    for (_, _, why) in &holding {
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

/// Whether `exception` was measured in `other`'s situation, and maybe in
/// more: its conditions include all of `other`'s.
fn includes(exception: &Exception, other: &Exception) -> bool {
    other
        .conditions
        .iter()
        .all(|condition| exception.conditions.contains(condition))
}

/// What the conditions of an exception are checked against.
struct Situation<'a> {
    table: TableState<'a>,
    /// The column the change is about, as the table has it now.
    column: Option<&'a str>,
    /// The column the statement drops, which takes its indexes with it.
    dropped: Option<&'a str>,
    /// The index the statement adds.
    index: Option<&'a Index>,
    /// The name of the index the statement drops.
    dropped_index: Option<&'a str>,
    /// The foreign key the statement adds.
    foreign_key: Option<&'a ForeignKey>,
    /// The name of the foreign key the statement drops.
    dropped_foreign_key: Option<&'a str>,
    /// The default the statement sets with `SET DEFAULT`.
    default: Option<&'a str>,
    /// The statement writes `DROP CONSTRAINT` or `ALTER CONSTRAINT`.
    constraint_clause: bool,
    /// What kilit counted of the row the change leaves.
    row: &'a RowSize,
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
    if condition == Condition::ConstraintClause {
        return if situation.constraint_clause {
            Applies::Yes(Vec::new())
        } else {
            Applies::No
        };
    }
    let TableState::Known(table) = situation.table else {
        return match condition {
            Condition::NewForeignKey(problem) => new_foreign_key(problem, situation),
            Condition::SetNullOnNotNull => match situation.foreign_key {
                Some(key) if sets_null_on_delete(key) && key.columns.len() == 1 => Applies::Maybe,
                _ => Applies::No,
            },
            Condition::BinaryText => match situation.index {
                Some(index) if index.kind == IndexKind::Fulltext && index.columns.len() == 1 => {
                    Applies::Maybe
                }
                _ => Applies::No,
            },
            _ => Applies::Maybe,
        };
    };
    match (condition, situation.column) {
        (Condition::SetNullOnNotNull, _) => set_null_on_not_null(table, situation),
        (Condition::BinaryText, _) => match situation.index {
            Some(index) if index.kind == IndexKind::Fulltext => match index.columns.as_slice() {
                [entry] => match table.column(crate::schema::indexed_column(entry)) {
                    Some(column) if is_binary_text(table, column) => Applies::Yes(vec![format!(
                        "{} is TEXT in the binary character set, which MySQL makes a BLOB",
                        column.name
                    )]),
                    _ => Applies::No,
                },
                _ => Applies::No,
            },
            _ => Applies::No,
        },
        (Condition::NewForeignKey(problem), _) => new_foreign_key(problem, situation),
        (Condition::NoSuchName, _) => no_such_name(table, situation),
        (Condition::RowTooLarge, _) => match situation.row {
            RowSize::Fits => Applies::No,
            RowSize::TooLarge(why) => Applies::Yes(vec![why.clone()]),
            RowSize::CannotCount(_) => Applies::Maybe,
        },
        (Condition::PrimaryKeyAlone, Some(column)) => match table.primary_key.as_slice() {
            [key] if same_column(key, column) => {
                Applies::Yes(vec![format!("{column} is the primary key")])
            }
            _ => Applies::No,
        },
        (Condition::PrimaryKeySecondOfTwo, Some(column)) => match table.primary_key.as_slice() {
            [first, second] if same_column(second, column) => Applies::Yes(vec![format!(
                "{column} is the second column of the primary key ({first}, {second})"
            )]),
            _ => Applies::No,
        },
        (Condition::BlobDefault, Some(column)) => blob_default(table, column, situation),
        (Condition::Hidden | Condition::ConstraintClause, _)
        | (
            Condition::PrimaryKeyAlone | Condition::PrimaryKeySecondOfTwo | Condition::BlobDefault,
            None,
        )
        | (
            Condition::IndexedColumn
            | Condition::ForeignKeyColumn
            | Condition::GeneratedColumnSource
            | Condition::CheckWithAnotherColumn
            | Condition::CheckUsesColumn,
            None,
        ) => Applies::Maybe,
        (Condition::OnlyIndexOfForeignKey, _) => {
            situation.dropped_index.map_or(Applies::No, |name| {
                only_index_of_foreign_key(table, name, situation)
            })
        }
        (Condition::GeneratedColumnSource, Some(column)) => generated_column_source(table, column),
        (Condition::CheckWithAnotherColumn, Some(column)) => {
            check_with_another_column(table, column, situation.schema)
        }
        (Condition::CheckUsesColumn, Some(column)) => {
            check_uses_column(table, column, situation.schema)
        }
        (Condition::BlobOrTextWithoutPrefix, _) => situation.index.map_or(Applies::No, |index| {
            blob_or_text_without_prefix(table, index)
        }),
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
                        "{column} is in the foreign key {key}; {}",
                        situation.session.assumes_foreign_key_checks()
                    )]),
                    Setting::Off | Setting::Unclear => Applies::Unmeasured(format!(
                        "{column} is in the foreign key {key}, and the lab has not measured \
                         changing such a column with foreign_key_checks off"
                    )),
                    Setting::Unknown => Applies::Unmeasured(format!(
                        "{column} is in the foreign key {key}, and {}",
                        situation.session.foreign_key_checks_unknown
                    )),
                };
            }
            match referenced(situation.schema, table, column) {
                Some(referenced) => Applies::Unmeasured(format!(
                    "{column} {referenced}, and the lab has not measured changing a referenced \
                     column"
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
/// Whether a column is TEXT itself, the type the lab measured FULLTEXT
/// indexes on apart from VARCHAR: not TINYTEXT, MEDIUMTEXT or LONGTEXT, and
/// not in the binary character set, which makes it a BLOB (MySQL refuses a
/// FULLTEXT index on it, error 1283), or one kilit does not know.
fn is_plain_text(table: &Table, column: &Column) -> bool {
    column.data_type.to_string().eq_ignore_ascii_case("TEXT")
        && table
            .text_settings(column)
            .is_ok_and(|settings| settings.charset.name != "binary")
}

/// The shapes of a FULLTEXT index the lab measured on a DYNAMIC table: one
/// or two columns, each VARCHAR, TEXT, MEDIUMTEXT or LONGTEXT in a
/// character set kilit can tell and that is not binary; and one TEXT
/// column in the binary character set, which fails (see
/// [`Condition::BinaryText`]). Any other is unknown.
fn fulltext_shape(table: &Table, index: &Index) -> Result<(), String> {
    const MEASURED: [&str; 3] = ["TEXT", "MEDIUMTEXT", "LONGTEXT"];
    let columns: Vec<&Column> = index
        .columns
        .iter()
        .filter_map(|entry| table.column(crate::schema::indexed_column(entry)))
        .collect();
    // A missing column is check_new_index's.
    if columns.len() != index.columns.len() {
        return Ok(());
    }
    if let Some(column) = columns.iter().find(|column| column.generated.is_some()) {
        return Err(format!(
            "{} is a generated column, and a FULLTEXT index on it has not been measured; MySQL \
             refuses one on a VIRTUAL column (error 3106, by hand on MySQL 8.0.46 and 8.4.13)",
            column.name
        ));
    }
    if let [column] = columns.as_slice()
        && is_binary_text(table, column)
    {
        return Ok(());
    }
    if columns.len() > 2 {
        return Err(format!(
            "a FULLTEXT index of {} columns has not been measured; the lab measured one or two",
            columns.len()
        ));
    }
    for column in columns {
        let name = column.data_type.to_string().to_ascii_uppercase();
        let text = matches!(column.data_type, DataType::VarChar { .. })
            || MEASURED.contains(&name.as_str());
        let charset = table
            .text_settings(column)
            .map(|settings| settings.charset.name);
        match (text, charset) {
            (true, Ok("binary")) => {
                return Err(format!(
                    "{} is {name} in the binary character set, and a FULLTEXT index on it has \
                     not been measured; the lab measured MySQL refusing one on a TEXT column in \
                     the binary character set (error 1283)",
                    column.name
                ));
            }
            (true, Ok(_)) => {}
            (true, Err(why)) => {
                return Err(format!(
                    "{why}, and a FULLTEXT index has been measured only in a character set \
                     kilit can tell"
                ));
            }
            (false, _) if name == "BLOB" || name.starts_with("VARBINARY") => {
                return Err(format!(
                    "{} is {name}, and a FULLTEXT index on it has not been measured; the lab \
                     measured MySQL refusing one on a TEXT column in the binary character set \
                     (error 1283)",
                    column.name
                ));
            }
            (false, _) => {
                return Err(format!(
                    "{} is {name}, and a FULLTEXT index has been measured only on VARCHAR, \
                     TEXT, MEDIUMTEXT and LONGTEXT columns",
                    column.name
                ));
            }
        }
    }
    Ok(())
}

/// The foreign key of `table` that sets the column `old_name` to NULL on
/// delete or update, when the new definition `column` makes it NOT NULL:
/// MySQL refuses that MODIFY or CHANGE (error 1830, by hand on MySQL 8.0.46
/// and 8.4.13), and the table stays as it was.
fn sets_to_null<'t>(table: &'t Table, old_name: &str, column: &Column) -> Option<&'t ForeignKey> {
    if column.nullable != Some(false) {
        return None;
    }
    table.foreign_keys.iter().find(|key| {
        (key.on_delete == Some(ForeignKeyAction::SetNull)
            || key.on_update == Some(ForeignKeyAction::SetNull))
            && key.columns.iter().any(|name| same_column(name, old_name))
    })
}

/// What MySQL refuses outright, by the lab or by hand on MySQL 8.0.46 and
/// 8.4.13, where kilit's answer is unknown or a failure: the tables stay
/// as they were, and a refused CREATE TABLE makes no table, which kilit
/// takes as lost (it gives that table no answer either way).
fn refused_by_mysql(statement: &Statement, schema: &Schema) -> Option<Rewritten> {
    match &statement.kind {
        StatementKind::Alter(alter) => {
            if crate::parse::ordered_index(&statement.sql).is_some() {
                return Some(Rewritten::Refused);
            }
            let TableState::Known(table) = schema.table(&alter.table) else {
                return None;
            };
            // Only a FULLTEXT index, a key that sets NULL, or a NOT NULL
            // column in such a key can be refused here: the rest needs no
            // copy of the table.
            let in_set_null_key = |name: &str| {
                table.foreign_keys.iter().any(|key| {
                    sets_null(key) && key.columns.iter().any(|column| same_column(column, name))
                })
            };
            // A column may become NOT NULL by a PRIMARY KEY too.
            let relevant = alter.operations.iter().any(|operation| match operation {
                Operation::AddIndex(index) => index.kind == IndexKind::Fulltext,
                Operation::AddForeignKey(key) => sets_null(key),
                Operation::AddColumn { column, .. } | Operation::ModifyColumn { column, .. } => {
                    in_set_null_key(&column.name)
                }
                Operation::ChangeColumn { old_name, .. } => in_set_null_key(old_name),
                Operation::AddPrimaryKey { columns } => columns
                    .iter()
                    .any(|entry| in_set_null_key(crate::schema::indexed_column(entry))),
                _ => false,
            });
            if !relevant {
                return None;
            }
            // MySQL judges the table as the whole statement leaves it; a new
            // index or key alone changes no column.
            let (after, added) = match alter.operations.as_slice() {
                [Operation::AddIndex(_)] => (Cow::Borrowed(table), None),
                [Operation::AddForeignKey(key)] => (Cow::Borrowed(table), Some(key)),
                _ => match crate::schema::table_after(table, alter) {
                    Ok(after) => (Cow::Owned(after), None),
                    // MySQL may refuse it, and kilit cannot tell how the
                    // clauses leave the table: it stops trusting the table.
                    Err(why) => {
                        return Some(Rewritten::Lost {
                            table: alter.table.clone(),
                            reason: format!(
                                "line {}: {why}, beside a change MySQL may refuse",
                                statement.line
                            ),
                            engine: table.engine.clone(),
                        });
                    }
                },
            };
            let after = after.as_ref();
            let written = |name: &str| {
                alter.operations.iter().any(|operation| match operation {
                    Operation::AddColumn { column, .. }
                    | Operation::ModifyColumn { column, .. } => same_column(&column.name, name),
                    Operation::ChangeColumn { column, .. } => same_column(&column.name, name),
                    Operation::AddPrimaryKey { columns } => columns
                        .iter()
                        .any(|entry| same_column(crate::schema::indexed_column(entry), name)),
                    _ => false,
                })
            };
            let new_fulltext = alter.operations.iter().any(|operation| match operation {
                Operation::AddIndex(index) if index.kind == IndexKind::Fulltext => index
                    .columns
                    .iter()
                    .filter_map(|entry| after.column(crate::schema::indexed_column(entry)))
                    .any(|column| refuses_fulltext(after, column)),
                _ => false,
            });
            let adds_key = alter
                .operations
                .iter()
                .any(|operation| matches!(operation, Operation::AddForeignKey(_)));
            let set_null_on_not_null = is_innodb(after)
                && after
                    .foreign_keys
                    .iter()
                    .chain(added)
                    .filter(|key| sets_null(key))
                    .any(|key| {
                        key.columns.iter().any(|name| {
                            after.column(name).is_some_and(|column| {
                                !after.is_nullable(column) || after.in_primary_key(&column.name)
                            }) && (adds_key || written(name))
                        })
                    });
            (new_fulltext || set_null_on_not_null).then_some(Rewritten::Refused)
        }
        StatementKind::CreateTable {
            table,
            if_not_exists,
        } => {
            let line = statement.line;
            let reason = if let Some(ordered) = crate::parse::ordered_index(&statement.sql) {
                format!(
                    "line {line}: MySQL refuses ASC or DESC on a key part of its {} index \
                     (error 1221)",
                    ordered.name()
                )
            } else if let Some(column) = table
                .indexes
                .iter()
                .filter(|index| index.kind == IndexKind::Fulltext)
                .flat_map(|index| &index.columns)
                .filter_map(|entry| table.column(crate::schema::indexed_column(entry)))
                .find(|column| refuses_fulltext(table, column))
            {
                format!(
                    "line {line}: MySQL refuses its FULLTEXT index on {} (error {})",
                    column.name,
                    if column.generated.is_some() {
                        3106
                    } else {
                        1283
                    }
                )
            } else if let Some(key) = table.foreign_keys.iter().find(|key| {
                is_innodb(table)
                    && sets_null(key)
                    && key.columns.iter().any(|name| {
                        table
                            .column(name)
                            .is_some_and(|column| !table.is_nullable(column))
                    })
            }) {
                format!(
                    "line {line}: MySQL refuses its foreign key {}, which sets a NOT NULL column \
                     to NULL (error 1830)",
                    key.name.as_deref().unwrap_or("without a name")
                )
            } else if table
                .columns
                .iter()
                .all(|column| row::too_long_kind(Some(table), column).is_none())
            {
                // By hand on MySQL 8.0.46 and 8.4.13, in strict mode and not:
                // latin1 VARCHAR(65535), VARBINARY(65535) alone, and utf8mb4
                // VARCHAR(16383) next to a BIGINT primary key. A column
                // longer than its type holds is `rewritten`'s.
                let bytes = row_past_the_limit(table)?;
                format!(
                    "line {line}: MySQL refuses it, as its row would take {}, past the {} it \
                     allows (error 1118)",
                    row::bytes(bytes),
                    row::MAX_ROW_BYTES
                )
            } else {
                return None;
            };
            // CREATE TABLE IF NOT EXISTS of a table that is there leaves it
            // as it is, refused or not (by hand on MySQL 8.0.46 and 8.4.13),
            // and a refused temporary table hides no table.
            if (*if_not_exists && schema.table(&table.name) != TableState::Missing)
                || crate::schema::creates_temporary(&statement.sql)
            {
                return Some(Rewritten::Refused);
            }
            Some(Rewritten::Lost {
                table: table.name.clone(),
                reason,
                engine: table.engine.clone(),
            })
        }
        _ => None,
    }
}

/// Whether a foreign key sets its columns to NULL, on delete or on update.
fn sets_null(key: &ForeignKey) -> bool {
    key.on_delete == Some(ForeignKeyAction::SetNull)
        || key.on_update == Some(ForeignKeyAction::SetNull)
}

/// Whether `table` is InnoDB: MySQL keeps foreign keys on InnoDB tables
/// only, and refuses a SET NULL one on a NOT NULL column there.
fn is_innodb(table: &Table) -> bool {
    matches!(table.engine, None | Some(Engine::InnoDb))
}

/// Whether MySQL refuses a FULLTEXT index on `column`, by hand on MySQL
/// 8.0.46 and 8.4.13: a VIRTUAL generated column (error 3106), or a
/// column in the binary character set, or in utf16, utf16le, utf32 or ucs2
/// (error 1283).
fn refuses_fulltext(table: &Table, column: &Column) -> bool {
    column
        .generated
        .as_ref()
        .is_some_and(|generated| !generated.stored)
        || in_binary(table, column)
        || column.data_type.is_text()
            && written_charset(table, column).is_some_and(|charset| {
                ["utf16", "utf16le", "utf32", "ucs2"]
                    .iter()
                    .any(|wide| charset.eq_ignore_ascii_case(wide))
            })
}

/// The character set written for `column`, or for its table, as written:
/// from the CHARACTER SET, or from the collation's first word.
fn written_charset<'t>(table: &'t Table, column: &'t Column) -> Option<&'t str> {
    let from_collation = |collation: &'t str| collation.split('_').next();
    column
        .charset
        .as_deref()
        .or_else(|| column.collation.as_deref().and_then(from_collation))
        .or(table.charset.as_deref())
        .or_else(|| table.collation.as_deref().and_then(from_collation))
}

/// The bytes of the row of a new table, when kilit counts it past the
/// 65,535 MySQL allows.
fn row_past_the_limit(table: &Table) -> Option<u64> {
    let columns: Vec<&Column> = table.columns.iter().collect();
    row::count(table, &columns, page_format(table))
        .ok()
        .map(|counts| counts.row)
        .filter(|bytes| *bytes > row::MAX_ROW_BYTES)
}

/// Whether a VARCHAR or TEXT column is in the binary character set (the
/// column's or the table's): MySQL makes it a VARBINARY or a BLOB, and
/// refuses a FULLTEXT index on it (error 1283, by hand on MySQL 8.0.46 and
/// 8.4.13).
fn in_binary(table: &Table, column: &Column) -> bool {
    column.data_type.is_text()
        && table
            .text_settings(column)
            .is_ok_and(|settings| settings.charset.name == "binary")
}

/// Whether a column is TEXT in the binary character set (the column's or
/// the table's), which MySQL makes a BLOB: by hand on MySQL 8.0.46 and
/// 8.4.13.
fn is_binary_text(table: &Table, column: &Column) -> bool {
    column.data_type.to_string().eq_ignore_ascii_case("TEXT")
        && table
            .text_settings(column)
            .is_ok_and(|settings| settings.charset.name == "binary")
}

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
/// Whether a foreign key of another table points at `column` of `table`,
/// in words: one of a table kilit holds, or, for an indexed column, one of
/// a table kilit does not know (a key needs an index on the columns it
/// points at).
fn referenced(schema: &Schema, table: &Table, column: &str) -> Option<String> {
    if let Some((other, key)) = referenced_by(schema, table, column) {
        return Some(format!("is referenced by the foreign key {key} on {other}"));
    }
    let indexed = table.in_primary_key(column)
        || table
            .indexes
            .iter()
            .any(|index| index.columns.iter().any(|entry| same_column(entry, column)));
    (indexed && schema.unseen_key_may_point_at(&table.name)).then(|| {
        "may be referenced by a foreign key of a table kilit does not know or lost track of"
            .to_owned()
    })
}

fn referenced_by(schema: &Schema, table: &Table, column: &str) -> Option<(String, String)> {
    schema.tables().find_map(|other| {
        other
            .foreign_keys
            .iter()
            .find(|key| {
                same_in_any_case(&key.referenced_table, &table.name)
                    && key
                        .referenced_columns
                        .iter()
                        .any(|c| c.eq_ignore_ascii_case(column))
            })
            .map(|key| (other.name.clone(), key.name.clone().unwrap_or_default()))
    })
}

/// Whether an index with these entries serves the foreign key: the key's
/// columns are its first columns, in order. A prefix such as `note(10)` does
/// not count, and an expression matches no column.
fn serves(entries: &[String], key: &ForeignKey) -> bool {
    entries.len() >= key.columns.len()
        && key
            .columns
            .iter()
            .zip(entries)
            .all(|(column, entry)| column.eq_ignore_ascii_case(entry))
}

/// What the lab measured against a BIGINT UNSIGNED column, with MySQL's
/// refusal: the size and whether it is unsigned.
const MEASURED_AGAINST_BIGINT_UNSIGNED: [(IntegerSize, bool); 6] = [
    (IntegerSize::Int, false),
    (IntegerSize::Int, true),
    (IntegerSize::Big, false),
    (IntegerSize::Small, true),
    (IntegerSize::Tiny, true),
    (IntegerSize::Medium, true),
];

/// The most bytes an InnoDB index can take from its columns in MySQL's default
/// row format. Past it, MySQL refuses the index a foreign key needs.
const MAX_INDEX_BYTES: u64 = 3072;

/// How the column of a new foreign key compares with the column it points at.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum KeyColumns {
    Fit,
    IntegerMismatch,
    NarrowerParent,
    TextMismatch,
}

/// What the parent table's indexes do for a new foreign key.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ParentIndex {
    Serves,
    Missing,
    NonUnique,
}

/// What the schema shows about a new foreign key, in shapes the lab measured.
struct KeySeen {
    columns: KeyColumns,
    why_columns: String,
    index: ParentIndex,
    why_index: String,
    self_reference: bool,
}

impl KeySeen {
    fn fits(self_reference: bool) -> KeySeen {
        KeySeen {
            columns: KeyColumns::Fit,
            why_columns: String::new(),
            index: ParentIndex::Serves,
            why_index: String::new(),
            self_reference,
        }
    }
}

/// Whether a problem of the new foreign key holds. A value of
/// `restrict_fk_on_non_standard_key` the lab did not measure leaves the index
/// rules without an answer, also when kilit cannot see the tables.
/// Whether a new foreign key sets its columns to NULL on delete, with no
/// ON UPDATE clause: the shape the lab measured with a NOT NULL column.
fn sets_null_on_delete(key: &ForeignKey) -> bool {
    key.on_delete == Some(ForeignKeyAction::SetNull) && key.on_update.is_none()
}

/// The new foreign key sets its one column, NOT NULL in `table`, to NULL on
/// delete: MySQL refuses it (error 1830, measured with foreign_key_checks on
/// and off). A key on its own table, or of several columns, has not been
/// measured; see [`foreign_key_seen`].
fn set_null_on_not_null(table: &Table, situation: &Situation<'_>) -> Applies {
    let Some(key) = situation.foreign_key else {
        return Applies::No;
    };
    if !sets_null_on_delete(key) || key.referenced_table.eq_ignore_ascii_case(&table.name) {
        return Applies::No;
    }
    let [column] = key.columns.as_slice() else {
        return Applies::No;
    };
    match table.column(column) {
        Some(column) if !table.is_nullable(column) => Applies::Yes(vec![format!(
            "{} is NOT NULL, and the foreign key sets it to NULL on delete",
            column.name
        )]),
        _ => Applies::No,
    }
}

fn new_foreign_key(problem: KeyProblem, situation: &Situation<'_>) -> Applies {
    let Some(key) = situation.foreign_key else {
        return Applies::No;
    };
    let about_index = matches!(problem, KeyProblem::NoIndex | KeyProblem::NonUniqueIndex);
    let restricted = about_index && situation.session.restrict_fk_set;
    let restrict_set = "restrict_fk_on_non_standard_key is set earlier in the input, and the lab \
                        measured MySQL's default only";
    let seen = match foreign_key_seen(key, situation.table, situation.schema) {
        Err(reason) => return Applies::Unmeasured(reason),
        Ok(None) if restricted => {
            return Applies::Unmeasured(format!(
                "kilit cannot see the indexes of both tables, and {restrict_set}"
            ));
        }
        Ok(None) => return Applies::Maybe,
        Ok(Some(seen)) => seen,
    };
    let holding = match problem {
        KeyProblem::IntegerMismatch => seen.columns == KeyColumns::IntegerMismatch,
        KeyProblem::NarrowerParent => seen.columns == KeyColumns::NarrowerParent,
        KeyProblem::TextMismatch => seen.columns == KeyColumns::TextMismatch,
        KeyProblem::NoIndex => seen.index == ParentIndex::Missing,
        KeyProblem::NonUniqueIndex => seen.index == ParentIndex::NonUnique,
    };
    if !holding {
        return Applies::No;
    }
    let why = if about_index {
        seen.why_index
    } else {
        seen.why_columns
    };
    if restricted {
        Applies::Unmeasured(format!("{why}, and {restrict_set}"))
    } else {
        Applies::Yes(vec![why])
    }
}

/// What the schema shows about the new foreign key: `None` when it does not
/// show both tables, an error when it shows a shape the lab has not measured.
fn foreign_key_seen(
    key: &ForeignKey,
    table: TableState<'_>,
    schema: &Schema,
) -> Result<Option<KeySeen>, String> {
    let TableState::Known(child) = table else {
        return Ok(None);
    };
    let TableState::Known(parent) = schema.table(&key.referenced_table) else {
        return Ok(None);
    };
    let name = key.name.as_deref().unwrap_or("without a name");
    if key.referenced_columns.is_empty() {
        return Err(format!(
            "the foreign key {name} names no column of {}, and the lab has not measured that",
            parent.name
        ));
    }
    if key.columns.len() != key.referenced_columns.len() {
        return Err(format!(
            "the foreign key {name} has {} columns and points at {}, and the lab has not \
             measured that",
            key.columns.len(),
            key.referenced_columns.len()
        ));
    }
    let self_reference = std::ptr::eq(child, parent);
    let mut pairs = Vec::with_capacity(key.columns.len());
    for (column, referenced) in key.columns.iter().zip(&key.referenced_columns) {
        pairs.push((key_column(child, column)?, key_column(parent, referenced)?));
    }
    // MySQL makes an index on the child's columns, which has its own limit
    // and comes before everything else below, as does this action.
    let mut bytes: u64 = 0;
    for &(column, _) in &pairs {
        if let Some(length) = declared_length(&column.data_type) {
            let per_character = if column.data_type.is_text() {
                child.text_settings(column)?.charset.max_bytes
            } else {
                1
            };
            bytes = bytes.saturating_add(length.saturating_mul(per_character));
        }
    }
    if bytes > MAX_INDEX_BYTES {
        return Err(format!(
            "the foreign key {name} takes up to {} in an index of {}, and the lab has not \
             measured a key past the index limit of {MAX_INDEX_BYTES}",
            row::bytes(bytes),
            child.name
        ));
    }
    let sets_null = key.on_delete == Some(ForeignKeyAction::SetNull)
        || key.on_update == Some(ForeignKeyAction::SetNull);
    // One column on another table, set to NULL on delete only, is measured:
    // see [`Condition::SetNullOnNotNull`].
    let measured = sets_null_on_delete(key) && pairs.len() == 1 && !self_reference;
    if let Some((column, _)) = pairs
        .iter()
        .find(|(column, _)| sets_null && !measured && !child.is_nullable(column))
    {
        return Err(format!(
            "the foreign key {name} sets {} to NULL, which is NOT NULL, and the lab has not \
             measured that",
            column.name
        ));
    }
    let &[(column, referenced)] = pairs.as_slice() else {
        if self_reference {
            return Err(format!(
                "the foreign key {name} has several columns and points at its own table, and \
                 the lab has not measured that"
            ));
        }
        for &(column, referenced) in &pairs {
            if !identical(child, column, parent, referenced)? {
                return Err(format!(
                    "{}, in a foreign key of several columns, and the lab measured identical \
                     columns only",
                    differ(child, column, parent, referenced)
                ));
            }
        }
        if !unique_on_exactly(parent, &key.referenced_columns) {
            return Err(format!(
                "no unique index of {} consists of exactly the columns of the foreign key \
                 {name}, and the lab has measured only that for several columns",
                parent.name
            ));
        }
        return Ok(Some(KeySeen::fits(false)));
    };
    if self_reference {
        let alone_in_primary_key = matches!(parent.primary_key.as_slice(), [only]
            if only.eq_ignore_ascii_case(&referenced.name));
        if column.name.eq_ignore_ascii_case(&referenced.name)
            || !alone_in_primary_key
            || !identical(child, column, parent, referenced)?
        {
            return Err(format!(
                "the foreign key {name} points at its own table, and the lab measured that only \
                 from a column of the same type to the primary key"
            ));
        }
        return Ok(Some(KeySeen::fits(true)));
    }
    let (columns, why_columns) = compare_key_columns(child, column, parent, referenced)?;
    let (index, why_index) = parent_index(parent, referenced)?;
    Ok(Some(KeySeen {
        columns,
        why_columns,
        index,
        why_index,
        self_reference,
    }))
}

/// A column of a foreign key, if the lab can have measured it: it exists and
/// is no generated, TEXT, BLOB, JSON or spatial column, no SERIAL, which comes
/// with a unique index no schema names, and kilit follows its options.
fn key_column<'a>(table: &'a Table, name: &str) -> Result<&'a Column, String> {
    let Some(column) = table.column(name) else {
        return Err(format!(
            "{}.{name} does not exist, and the lab has not measured a foreign key on a missing \
             column",
            table.name
        ));
    };
    let kind = if column.generated.is_some() {
        "a generated column"
    } else if is_blob_or_text(&column.data_type) {
        "a TEXT or BLOB column"
    } else if column.data_type.to_string().eq_ignore_ascii_case("JSON") {
        "a JSON column"
    } else if column.data_type.to_string().eq_ignore_ascii_case("SERIAL") {
        "a SERIAL column"
    } else if is_spatial(&column.data_type) {
        "a spatial column"
    } else if !column.other_options.is_empty() {
        "a column with options kilit does not follow"
    } else {
        return Ok(column);
    };
    Err(format!(
        "{}.{name} is {kind}, and the lab has not measured a foreign key on one",
        table.name
    ))
}

/// `POINT`, `GEOMETRY` and the other types of a spatial column.
fn is_spatial(data_type: &DataType) -> bool {
    let name = data_type.to_string();
    [
        "GEOMETRY",
        "POINT",
        "LINESTRING",
        "POLYGON",
        "MULTIPOINT",
        "MULTILINESTRING",
        "MULTIPOLYGON",
        "GEOMETRYCOLLECTION",
        "GEOMCOLLECTION",
    ]
    .iter()
    .any(|spatial| name.eq_ignore_ascii_case(spatial))
}

/// The length written on a type whose values can fill an index: characters of
/// `VARCHAR(10)` or `CHAR(10)`, bytes of `VARBINARY(10)` or `BINARY(10)`.
fn declared_length(data_type: &DataType) -> Option<u64> {
    let name = match data_type {
        DataType::VarChar { length } => return Some(*length),
        DataType::Text(name) if is_char_or_varchar(data_type) => name,
        DataType::Other(name) => name,
        _ => return None,
    };
    let (base, rest) = name.split_once('(')?;
    let base = base.trim();
    let counted = ["CHAR", "CHARACTER", "BINARY", "VARBINARY"]
        .iter()
        .any(|counted| base.eq_ignore_ascii_case(counted));
    if !counted {
        return None;
    }
    rest.split_once(')')?.0.trim().parse().ok()
}

fn differ(child: &Table, column: &Column, parent: &Table, referenced: &Column) -> String {
    format!(
        "{}.{} is {} and {}.{} is {}",
        child.name,
        column.name,
        column.data_type,
        parent.name,
        referenced.name,
        referenced.data_type
    )
}

/// The same type and, for text, the same character set and collation. An
/// unknown character set is an error, never a guess.
fn identical(
    child: &Table,
    column: &Column,
    parent: &Table,
    referenced: &Column,
) -> Result<bool, String> {
    if column.data_type != referenced.data_type {
        return Ok(false);
    }
    if !column.data_type.is_text() {
        return Ok(true);
    }
    let (ours, theirs) = (
        child.text_settings(column)?,
        parent.text_settings(referenced)?,
    );
    Ok(ours.charset.name == theirs.charset.name && ours.collation == theirs.collation)
}

/// How the two columns of a key of one column compare, or why the lab has not
/// measured it.
fn compare_key_columns(
    child: &Table,
    column: &Column,
    parent: &Table,
    referenced: &Column,
) -> Result<(KeyColumns, String), String> {
    let why = differ(child, column, parent, referenced);
    let (ours, theirs) = (&column.data_type, &referenced.data_type);
    let not_measured = || {
        Err(format!(
            "{why}, and the lab has not measured a foreign key between those types"
        ))
    };
    match (ours, theirs) {
        (
            DataType::Integer { size, unsigned },
            DataType::Integer {
                size: parent_size,
                unsigned: parent_unsigned,
            },
        ) => {
            let pair = (*parent_size, *parent_unsigned);
            if ours == theirs {
                Ok((KeyColumns::Fit, why))
            } else if pair == (IntegerSize::Big, true)
                && MEASURED_AGAINST_BIGINT_UNSIGNED.contains(&(*size, *unsigned))
            {
                Ok((KeyColumns::IntegerMismatch, why))
            } else if pair == (IntegerSize::Int, true)
                && (*size, *unsigned) == (IntegerSize::Big, true)
            {
                Ok((KeyColumns::NarrowerParent, why))
            } else {
                not_measured()
            }
        }
        _ if ours.is_text() && theirs.is_text() => {
            if ours != theirs && !(is_char_or_varchar(ours) && is_char_or_varchar(theirs)) {
                return not_measured();
            }
            let our_settings = child.text_settings(column)?;
            let their_settings = parent.text_settings(referenced)?;
            if our_settings.charset.name == their_settings.charset.name
                && our_settings.collation == their_settings.collation
            {
                return Ok((KeyColumns::Fit, why));
            }
            let why = format!(
                "{}.{} uses {} and {}.{} uses {}",
                child.name,
                column.name,
                our_settings.collation,
                parent.name,
                referenced.name,
                their_settings.collation
            );
            let measured = matches!(ours, DataType::VarChar { .. })
                && ours == theirs
                && their_settings.collation == "utf8mb4_0900_ai_ci"
                && (our_settings.collation == "utf8mb4_bin"
                    || our_settings.collation == "latin1_swedish_ci");
            if measured {
                Ok((KeyColumns::TextMismatch, why))
            } else {
                Err(format!(
                    "{why}, and the lab has not measured a foreign key between those two"
                ))
            }
        }
        _ if ours == theirs => Ok((KeyColumns::Fit, why)),
        _ => not_measured(),
    }
}

/// `CHAR(36)` or `VARCHAR(36)`: not TEXT, ENUM or SET.
fn is_char_or_varchar(data_type: &DataType) -> bool {
    match data_type {
        DataType::VarChar { .. } => true,
        DataType::Text(name) => {
            let base = name.split('(').next().unwrap_or_default().trim();
            base.eq_ignore_ascii_case("CHAR") || base.eq_ignore_ascii_case("CHARACTER")
        }
        _ => false,
    }
}

/// Whether the primary key or a unique index of the table consists of exactly
/// these columns, in this order.
fn unique_on_exactly(table: &Table, columns: &[String]) -> bool {
    let exactly = |entries: &[String]| {
        entries.len() == columns.len()
            && entries
                .iter()
                .zip(columns)
                .all(|(entry, column)| entry.eq_ignore_ascii_case(column))
    };
    exactly(&table.primary_key)
        || table
            .indexes
            .iter()
            .any(|index| index.kind == IndexKind::Unique && exactly(&index.columns))
}

/// What the indexes of `parent` do for a key of one column that points at
/// `column`. Index entries count by the column they name, whatever their
/// letter case or sort order. The lab measured a unique index or a primary key
/// of that column alone, with plain indexes beside it or none; and without
/// one, one plain index of the column, with or without one more column; one
/// unique index of the column and one more; one index of a shorter part of
/// the column; or none that starts with it. Anything else has no answer.
fn parent_index(parent: &Table, column: &Column) -> Result<(ParentIndex, String), String> {
    let name = column.name.as_str();
    let not_measured = |what: String| {
        Err(format!(
            "{what}, and the lab has not measured a foreign key pointing at {}.{name} in that \
             situation",
            parent.name
        ))
    };
    if parent.foreign_keys.iter().any(|key| {
        key.columns
            .first()
            .is_some_and(|first| first.eq_ignore_ascii_case(name))
    }) {
        return not_measured(format!(
            "{} has a foreign key on {name}, so MySQL may have made an index for it",
            parent.name
        ));
    }
    let primary_key_starts = parent.primary_key.first().is_some_and(|first| {
        entry_column(first).is_some_and(|entry| entry.eq_ignore_ascii_case(name))
    });
    if primary_key_starts
        && (parent.primary_key.len() > 1 || parent.primary_key.iter().any(|e| has_prefix(e)))
    {
        return not_measured(format!(
            "the primary key of {} has more than the whole of {name}",
            parent.name
        ));
    }
    let mut unique_alone = primary_key_starts;
    let mut whole = Vec::new();
    let mut parts = Vec::new();
    for index in &parent.indexes {
        let Some(first) = index.columns.first() else {
            continue;
        };
        let index_name = index.name.as_deref().unwrap_or("without a name");
        if is_expression(first) {
            if uses_column(first, name) {
                return not_measured(format!("the index {index_name} is on an expression"));
            }
            continue;
        }
        if !entry_column(first).is_some_and(|entry| entry.eq_ignore_ascii_case(name)) {
            continue;
        }
        if !can_serve_a_key(index) {
            return not_measured(format!(
                "the index {index_name} of {} is FULLTEXT or spatial",
                parent.name
            ));
        }
        if has_prefix(first) {
            if index.columns.len() != 1 || !shorter_than_column(first, column) {
                return not_measured(format!(
                    "the index {index_name} of {} takes a part of {name} of this length",
                    parent.name
                ));
            }
            parts.push(index);
        } else if index.columns.len() > 2
            || index
                .columns
                .iter()
                .any(|entry| is_expression(entry) || has_prefix(entry))
        {
            return not_measured(format!(
                "the index {index_name} of {} has this shape",
                parent.name
            ));
        } else if index.kind == IndexKind::Unique && index.columns.len() == 1 {
            unique_alone = true;
        } else {
            whole.push(index);
        }
    }
    if unique_alone {
        if let Some(other) = parts
            .iter()
            .chain(whole.iter().filter(|index| index.kind == IndexKind::Unique))
            .next()
        {
            return not_measured(format!(
                "the unique index of {name} has the index {} beside it",
                other.name.as_deref().unwrap_or("without a name")
            ));
        }
        return Ok((ParentIndex::Serves, String::new()));
    }
    match (whole.as_slice(), parts.as_slice()) {
        ([], [] | [_]) => Ok((
            ParentIndex::Missing,
            format!(
                "no index of {} starts with the whole of {name}",
                parent.name
            ),
        )),
        ([index], []) => Ok((
            ParentIndex::NonUnique,
            format!(
                "the index {} of {} starts with {name} but is not unique on it alone",
                index.name.as_deref().unwrap_or("without a name"),
                parent.name
            ),
        )),
        _ => not_measured(format!(
            "several indexes of {} start with {name}",
            parent.name
        )),
    }
}

/// Whether an entry such as `name(10)` takes less than the whole VARCHAR
/// column. The lab measured a part of a VARCHAR column only.
fn shorter_than_column(entry: &str, column: &Column) -> bool {
    let DataType::VarChar { length } = column.data_type else {
        return false;
    };
    entry
        .split_once('(')
        .and_then(|(_, part)| part.trim_end_matches(')').trim().parse::<u64>().ok())
        .is_some_and(|part| part < length)
}

/// The answer with foreign_key_checks off stands only for a key MySQL accepts:
/// the lab did not measure this step's refusals with the checks off.
fn foreign_key_with_checks_off(
    key: &ForeignKey,
    table: TableState<'_>,
    schema: &Schema,
) -> Result<(), String> {
    let off = "and the lab has not measured this with foreign_key_checks off";
    let Some(seen) = foreign_key_seen(key, table, schema)? else {
        return Ok(());
    };
    if seen.self_reference {
        return Err(format!("the foreign key points at its own table, {off}"));
    }
    if seen.columns != KeyColumns::Fit {
        return Err(format!("{}, {off}", seen.why_columns));
    }
    if seen.index != ParentIndex::Serves {
        return Err(format!("{}, {off}", seen.why_index));
    }
    Ok(())
}

/// Whether the index can serve a foreign key at all.
fn can_serve_a_key(index: &Index) -> bool {
    matches!(index.kind, IndexKind::Plain | IndexKind::Unique)
}

/// Whether MySQL may have made the index itself for the foreign key: it
/// names such an index after the key, or after the key's first column when
/// the key has no name of its own. No schema shows where an index came from.
/// A `RENAME TABLE` renames the key but not the index, which keeps the
/// key's old name: by hand on MySQL 8.0.46 and 8.4.13, MySQL still drops it
/// without a word when another index can serve the key.
fn may_be_automatic(index: &Index, key: &ForeignKey) -> bool {
    index.name.as_deref().is_some_and(|name| {
        key.name
            .iter()
            .chain(&key.former_names)
            .any(|key| key.eq_ignore_ascii_case(name))
            || key
                .columns
                .first()
                .is_some_and(|column| column.eq_ignore_ascii_case(name))
    })
}

/// Whether dropping the index leaves a foreign key of `table` without an
/// index, which MySQL refuses. The lab measured: a plain index of one or two
/// columns under a key of one column, also with the checks off when the index
/// has one column, and a plain index of exactly the two columns of a key. It
/// measured the drop with another plain index or the primary key serving a
/// key of one column, and that gets the answer for any index, unless one of
/// the indexes may be automatic: MySQL drops an index it made for a key,
/// without a word, when another index can serve the key. Then kilit's table
/// may have an index the server does not, and only the server knows.
/// What the row a change leaves comes to, against MySQL's limit of 65,535
/// bytes: see [`row_size`].
#[derive(Clone, Debug, PartialEq, Eq)]
enum RowSize {
    /// The change does not make the row longer, or the row fits.
    Fits,
    /// It does not fit, and why.
    TooLarge(String),
    /// kilit cannot count it, and why.
    CannotCount(String),
}

/// Whether a key or column name of the table is `column`, as MySQL
/// compares them.
fn same_column(name: &str, column: &str) -> bool {
    name.eq_ignore_ascii_case(column)
}

/// What the row of `table` comes to after the statement adds `column`, or
/// puts it in place of the column `replaced`. Only a known table is
/// counted. A row past InnoDB's limit for a record in its page is unknown:
/// see [`check_page_row`].
fn row_size(
    table: TableState<'_>,
    replaced: Option<&str>,
    column: &Column,
) -> Result<RowSize, String> {
    // MySQL refuses such a column (error 1074), or makes it a TEXT or BLOB
    // type: it is no row too long (see `row::TooLong`). Some are too long in
    // any character set.
    let TableState::Known(table) = table else {
        if let Some(reason) = row::too_long(None, column) {
            return Err(reason);
        }
        return Ok(RowSize::CannotCount("kilit does not know the table".into()));
    };
    if let Some(reason) = row::too_long(Some(table), column) {
        return Err(reason);
    }
    let mut columns: Vec<&Column> = table
        .columns
        .iter()
        .filter(|old| replaced.is_none_or(|name| !same_column(&old.name, name)))
        .collect();
    columns.push(column);
    let counts = match row::count(table, &columns, page_format(table)) {
        Ok(counts) => counts,
        Err(reason) => return Ok(RowSize::CannotCount(reason)),
    };
    if counts.row > row::MAX_ROW_BYTES {
        return Ok(RowSize::TooLarge(format!(
            "the row of {} would take {}, past the {} MySQL allows",
            table.name,
            row::bytes(counts.row),
            row::MAX_ROW_BYTES
        )));
    }
    check_page_row(table, counts)?;
    Ok(RowSize::Fits)
}

/// The note for a row kilit cannot count, on a table whose answers were
/// measured apart (`measured_on`), where the lab did not measure a row
/// too long.
fn row_not_counted(row: &RowSize, measured_on: &str) -> String {
    let why = match row {
        RowSize::CannotCount(why) => why.as_str(),
        RowSize::Fits | RowSize::TooLarge(_) => "",
    };
    format!(
        "{ROW_TOO_LARGE}: kilit cannot count the row ({why}); the lab measured error 1118 on \
         InnoDB's DYNAMIC row format, not {measured_on}"
    )
}

/// What kilit cannot tell when it cannot count the row: InnoDB's limit for
/// a record in a page, which the two servers count differently.
const PAGE_RECORD_UNSEEN: &str = "if the record grows past what InnoDB keeps in a page (8126 \
     bytes): MySQL fails with error 1118 (4157 with ALGORITHM=INSTANT), and 8.0.46 and 8.4.13 \
     count the record differently, so kilit cannot tell (it counts the record when it knows \
     every column of the table)";

/// A change that makes the row too long was measured for a plain new
/// column and for a longer VARCHAR only.
fn row_measured(case: &Case, row: &RowSize) -> Result<(), String> {
    match row {
        RowSize::TooLarge(why)
            if !std::ptr::eq(case, &ADD_COLUMN) && !std::ptr::eq(case, &LENGTHEN_VARCHAR) =>
        {
            Err(format!(
                "{why}, and the lab has measured that for a plain new column and a longer \
                 VARCHAR only, not for {}",
                case.id
            ))
        }
        _ => Ok(()),
    }
}

/// The row format whose record kilit counts in a page: `None` for a MyISAM
/// table, and for a format kilit cannot tell, which gets no answer anyway.
fn page_format(table: &Table) -> Option<row::Format> {
    if matches!(table.engine, Some(Engine::MyIsam)) {
        return None;
    }
    match (&table.row_format, &table.key_block_size) {
        (Some(RowFormat::Compressed), _) | (_, Some(_)) => Some(row::Format::Compressed),
        (None | Some(RowFormat::Default | RowFormat::Dynamic), None) => Some(row::Format::Dynamic),
        (Some(RowFormat::Compact), None) => Some(row::Format::Compact),
        (Some(RowFormat::Redundant), None) => Some(row::Format::Redundant),
        (Some(RowFormat::Other(_) | RowFormat::Unknown), None) => None,
    }
}

/// InnoDB's limit for a record in a page: the two servers count some
/// columns apart, and a column added to a table with rows may go in place
/// past the line, so a change past it gets no answer. kilit counts the
/// larger record of the two. A table kilit cannot count gets the note
/// [`PAGE_RECORD_UNSEEN`] instead.
fn check_page_row(table: &Table, counts: row::Counts) -> Result<(), String> {
    let Some((bytes, limit)) = counts.page else {
        return Ok(());
    };
    if bytes > limit {
        return Err(format!(
            "the record of {} would take {} in an InnoDB page, past the {limit} its \
             row format allows as the lab counted it, the larger count where MySQL 8.0.46 and \
             8.4.13 differ: MySQL fails with error 1118, or may add the column in place on a \
             table with rows",
            table.name,
            row::bytes(bytes)
        ));
    }
    Ok(())
}

/// The lab's scenarios for dropping what a table does not have. MySQL
/// refuses each with error 1091 on both servers.
const DROP_MISSING_COLUMN: &str = "drop-column-missing";
const DROP_MISSING_INDEX: &str = "drop-index-missing";
const DROP_MISSING_FOREIGN_KEY: &str = "drop-foreign-key-missing";

/// Whether the known table lacks the column, index or foreign key the
/// statement drops. kilit does not answer "fails" from its tables alone,
/// which may be out of date: such a drop is unknown, and says what the lab
/// measured.
fn no_such_name(table: &Table, situation: &Situation<'_>) -> Applies {
    let (what, name, scenario, found) = if let Some(name) = situation.dropped {
        (
            "column",
            name,
            DROP_MISSING_COLUMN,
            table.column(name).is_some(),
        )
    } else if let Some(name) = situation.dropped_index {
        if name.eq_ignore_ascii_case("PRIMARY") {
            return Applies::Unmeasured(
                "dropping the primary key with DROP INDEX has not been measured yet".into(),
            );
        }
        // An index kilit holds without a name has one MySQL picked, which
        // may be this one.
        let found = table
            .indexes
            .iter()
            .any(|index| index.name.as_deref().is_none_or(|n| same_column(n, name)));
        ("index", name, DROP_MISSING_INDEX, found)
    } else if let Some(name) = situation.dropped_foreign_key {
        // MySQL keeps no foreign key on a MyISAM table, and drops one of
        // any name without complaint.
        if table.engine == Some(Engine::MyIsam) {
            return Applies::No;
        }
        // So may a foreign key's, such as `orders_ibfk_1`.
        let found = table
            .foreign_keys
            .iter()
            .any(|key| key.name.as_deref().is_none_or(|n| same_column(n, name)));
        ("foreign key", name, DROP_MISSING_FOREIGN_KEY, found)
    } else {
        return Applies::No;
    };
    if found {
        return Applies::No;
    }
    Applies::Unmeasured(format!(
        "{} has no {what} {name} that kilit knows of. If it has none, MySQL fails with error \
         1091 ({scenario}); kilit does not answer that from its tables alone, which may be \
         out of date",
        table.name
    ))
}

/// Whether the column whose default the statement sets takes no literal
/// default, in a shape the lab measured: a quoted default on TEXT or BLOB
/// in its strict SQL mode and in the two non-strict ones, on JSON in the
/// strict one only.
fn blob_default(table: &Table, column: &str, situation: &Situation<'_>) -> Applies {
    let Some(found) = table.column(column) else {
        return Applies::Maybe;
    };
    let family = match no_literal_default(&found.data_type) {
        None => return Applies::No,
        Some(Err(reason)) => return Applies::Unmeasured(reason),
        Some(Ok(family)) => family,
    };
    let Some(default) = situation.default else {
        return Applies::No;
    };
    let type_name = type_name(&found.data_type);
    if default.eq_ignore_ascii_case("NULL") {
        return Applies::Unmeasured(format!(
            "setting the default of the {type_name} column {column} to NULL has not been \
             measured yet"
        ));
    }
    if !is_quoted(default) {
        return Applies::Unmeasured(format!(
            "the default {default} on the {type_name} column {column} has not been measured yet"
        ));
    }
    let why = || Applies::Yes(vec![format!("{column} is {type_name}")]);
    match (&situation.session.sql_mode, family) {
        (SqlMode::Unclear(reason), _) => Applies::Unmeasured(format!(
            "{reason}, and whether MySQL refuses the default {default} on the {type_name} \
             column {column} depends on it"
        )),
        (_, NoLiteralDefault::TextOrBlob) => why(),
        (SqlMode::NotStrict, NoLiteralDefault::Json) => Applies::Unmeasured(format!(
            "the input sets a non-strict sql_mode, and kilit has no lab scenario for setting \
             a default on the JSON column {column} in it"
        )),
        (_, NoLiteralDefault::Json) => why(),
        (_, NoLiteralDefault::Geometry) => Applies::Unmeasured(format!(
            "setting a default on the {type_name} column {column} has not been measured yet"
        )),
    }
}

fn only_index_of_foreign_key(table: &Table, name: &str, situation: &Situation<'_>) -> Applies {
    let Some(index) = table.indexes.iter().find(|index| {
        can_serve_a_key(index)
            && index
                .name
                .as_deref()
                .is_some_and(|n| n.eq_ignore_ascii_case(name))
    }) else {
        return Applies::No;
    };
    let not_measured = "dropping such an index has not been measured yet";
    // Most tables have no key pointing at them: no need to look through all.
    let pointing_here = situation
        .schema
        .may_be_pointed_at(&table.name)
        .then(|| {
            situation.schema.tables().find_map(|other| {
                other
                    .foreign_keys
                    .iter()
                    .find(|key| {
                        same_in_any_case(&key.referenced_table, &table.name)
                            && !key.referenced_columns.is_empty()
                            && key
                                .referenced_columns
                                .iter()
                                .zip(&index.columns)
                                .all(|(column, entry)| column.eq_ignore_ascii_case(entry))
                            && key.referenced_columns.len() <= index.columns.len()
                    })
                    .map(|key| (other.name.as_str(), key.name.as_deref().unwrap_or_default()))
            })
        })
        .flatten();
    if pointing_here.is_none() && situation.schema.unseen_key_may_point_at(&table.name) {
        return Applies::Unmeasured(format!(
            "a foreign key of a table kilit does not know or lost track of may point at {}, \
             and {not_measured}",
            table.name
        ));
    }
    if let Some((other, key)) = pointing_here {
        return Applies::Unmeasured(format!(
            "{name} serves the foreign key {key} on {other}, which points at this table, and \
             {not_measured}"
        ));
    }
    let keys: Vec<&ForeignKey> = table
        .foreign_keys
        .iter()
        .filter(|key| serves(&index.columns, key))
        .collect();
    let key = match keys.as_slice() {
        [] => return Applies::No,
        [key] => *key,
        [first, second, ..] => {
            return Applies::Unmeasured(format!(
                "the foreign keys {} and {} both use {name}, and {not_measured}",
                first.name.as_deref().unwrap_or("without a name"),
                second.name.as_deref().unwrap_or("without a name")
            ));
        }
    };
    let key_name = key.name.as_deref().unwrap_or("without a name");
    let checks_off = match situation.session.foreign_key_checks {
        Setting::Off => true,
        Setting::On | Setting::ServerDefault => false,
        Setting::Unclear => {
            return Applies::Unmeasured(format!(
                "{name} serves the foreign key {key_name}, and foreign_key_checks is set to a \
                 value kilit does not understand"
            ));
        }
        Setting::Unknown => {
            return Applies::Unmeasured(format!(
                "{name} serves the foreign key {key_name}, and {}",
                situation.session.foreign_key_checks_unknown
            ));
        }
    };
    if index.kind != IndexKind::Plain || index.columns.len() > 2 {
        return Applies::Unmeasured(format!(
            "{name} serves the foreign key {key_name}, and the lab measured plain indexes of \
             one or two columns only"
        ));
    }
    let others: Vec<&Index> = table
        .indexes
        .iter()
        .filter(|other| {
            !std::ptr::eq(*other, index) && can_serve_a_key(other) && serves(&other.columns, key)
        })
        .collect();
    if others.is_empty() && !serves(&table.primary_key, key) {
        let measured = match (key.columns.len(), index.columns.len()) {
            (1, 1) => true,
            (1, 2) | (2, 2) => !checks_off,
            _ => false,
        };
        if !measured {
            let setting = if checks_off {
                " with foreign_key_checks off"
            } else {
                ""
            };
            return Applies::Unmeasured(format!(
                "{name} is the only index that serves the foreign key {key_name}{setting}, and \
                 the lab has not measured this shape of index and key"
            ));
        }
        let assumes = if situation.session.foreign_key_checks == Setting::ServerDefault {
            format!("; {}", situation.session.assumes_foreign_key_checks())
        } else {
            String::new()
        };
        return Applies::Yes(vec![format!(
            "{name} is the only index that serves the foreign key {key_name}{assumes}"
        )]);
    }
    // Something else serves the key. The lab measured that for a key of one
    // column, with the checks on.
    if key.columns.len() > 1 || checks_off {
        return Applies::Unmeasured(format!(
            "another index also serves the foreign key {key_name}, and dropping {name} with \
             that key has not been measured yet"
        ));
    }
    if let Some(unique) = others.iter().find(|other| other.kind != IndexKind::Plain) {
        return Applies::Unmeasured(format!(
            "the unique index {} also serves the foreign key {key_name}, and {not_measured}",
            unique.name.as_deref().unwrap_or_default()
        ));
    }
    if let Some(automatic) = std::iter::once(&index)
        .chain(&others)
        .find(|index| may_be_automatic(index, key))
    {
        return Applies::Unmeasured(format!(
            "the index {} has the name MySQL gives the index it makes for the foreign key \
             {key_name}, and MySQL drops that index silently when another one can serve the key; \
             no schema shows which of the two this is",
            automatic.name.as_deref().unwrap_or_default()
        ));
    }
    Applies::No
}

/// The columns an expression names, when it is plain: column names of
/// `table`, numbers, arithmetic operators and parentheses, as in `(amount *
/// 2)`. A string, a function call, a word that is no column of the table or
/// two operands in a row (`amount DIV 2`) make it something else.
fn plain_expression_columns<'a>(expression: &'a str, table: &Table) -> Option<Vec<&'a str>> {
    let word = |c: char| c.is_alphanumeric() || c == '_' || c == '$';
    let mut columns = Vec::new();
    let mut after_operand = false;
    let mut rest = expression.trim_start();
    while let Some(first) = rest.chars().next() {
        if first.is_ascii_digit() {
            let end = rest
                .find(|c: char| !(c.is_ascii_digit() || c == '.'))
                .unwrap_or(rest.len());
            if after_operand || rest[end..].starts_with(word) {
                return None;
            }
            after_operand = true;
            rest = &rest[end..];
        } else if first == '`' || word(first) {
            let (name, after) = if first == '`' {
                rest[1..].split_once('`')?
            } else {
                rest.split_at(rest.find(|c: char| !word(c)).unwrap_or(rest.len()))
            };
            if after_operand || after.trim_start().starts_with('(') {
                return None;
            }
            table.column(name)?;
            columns.push(name);
            after_operand = true;
            rest = after;
        } else {
            match first {
                '+' | '-' | '*' | '/' | '%' | '(' => after_operand = false,
                ')' => after_operand = true,
                _ => return None,
            }
            rest = &rest[1..];
        }
        rest = rest.trim_start();
    }
    Some(columns)
}

/// The first generated column of `table`, other than `column` itself, whose
/// expression names `column`, plain or not.
fn generated_column_naming<'a>(table: &'a Table, column: &str) -> Option<&'a Column> {
    table.columns.iter().find(|other| {
        !other.name.eq_ignore_ascii_case(column)
            && other
                .generated
                .as_ref()
                .is_some_and(|generated| uses_column(&generated.expression, column))
    })
}

/// Whether a generated column of `table` uses `column`, so that MySQL refuses
/// to drop or rename it. The lab measured one shape: a plain expression over
/// a column in no index, on a table without a FULLTEXT index. Anything else
/// that names the column gets no answer: a word in a string or a function
/// name is no column at all, and MySQL accepts the change then.
fn generated_column_source(table: &Table, column: &str) -> Applies {
    let mut user = None;
    for other in &table.columns {
        let Some(generated) = &other.generated else {
            continue;
        };
        if other.name.eq_ignore_ascii_case(column) {
            continue;
        }
        match plain_expression_columns(&generated.expression, table) {
            Some(columns) => {
                if columns.iter().any(|c| c.eq_ignore_ascii_case(column)) {
                    user.get_or_insert(other);
                }
            }
            None if uses_column(&generated.expression, column) => {
                return Applies::Unmeasured(format!(
                    "the expression of the generated column {} names {column}, and the lab \
                     measured expressions of column names, numbers and arithmetic operators \
                     only, not strings or function calls",
                    other.name
                ));
            }
            None => {}
        }
    }
    let Some(user) = user else {
        return Applies::No;
    };
    let also = |what: String| {
        Applies::Unmeasured(format!(
            "the generated column {} uses {column}, and {what}, which has not been measured \
             together",
            user.name
        ))
    };
    if table.column(column).is_some_and(|c| c.generated.is_some()) {
        return also(format!("{column} is a generated column too"));
    }
    if let Some(next) = generated_column_naming(table, &user.name) {
        return also(format!(
            "the generated column {} uses {}",
            next.name, user.name
        ));
    }
    let uses_generated = table.columns.iter().any(|other| {
        other.generated.is_some()
            && !other.name.eq_ignore_ascii_case(&user.name)
            && user
                .generated
                .as_ref()
                .is_some_and(|generated| uses_column(&generated.expression, &other.name))
    });
    if uses_generated {
        return also(format!("{} uses another generated column", user.name));
    }
    if let Some(index) = table.index_on(column) {
        return also(format!("{column} is in the index {index}"));
    }
    if let Some(index) = table.index_on(&user.name) {
        return also(format!("{} is in the index {index}", user.name));
    }
    if let Some(index) = table.fulltext_index() {
        let name = index.name.as_deref().unwrap_or_default();
        return also(format!("{} has the FULLTEXT index {name}", table.name));
    }
    Applies::Yes(vec![format!(
        "the generated column {} uses {column}",
        user.name
    )])
}

/// How the CHECK constraints of a table use a column.
struct CheckUse<'a> {
    /// The first one that uses the column.
    user: &'a Check,
    /// The first one that also uses another column, with that column.
    with_another: Option<(&'a Check, &'a str)>,
}

/// The name of a CHECK constraint that kilit keeps: it names the ones left
/// without a name, as MySQL does.
fn check_name(check: &Check) -> &str {
    check.name.as_deref().unwrap_or_default()
}

/// How the CHECK constraints of `table` use `column`, or why kilit cannot say
/// what the lab measured about that. MySQL's rule also holds for the other
/// expressions that use a column (run by hand), but an expression that is not
/// plain may name it without using it: `lower(note)` does not use a column
/// `lower`. A word in a string is no use of a column at all. The lab measured
/// a column in no index or in a plain one, on a table without a FULLTEXT
/// index; the other situations are unknown, as it has not measured them
/// together with a CHECK constraint.
fn check_use<'a>(
    table: &'a Table,
    column: &str,
    schema: &Schema,
) -> Result<Option<CheckUse<'a>>, String> {
    let same = |name: &String| name.eq_ignore_ascii_case(column);
    let mut user = None;
    let mut with_another = None;
    for check in &table.checks {
        let columns = match &check.columns {
            CheckColumns::Plain(columns) => columns,
            CheckColumns::Other(names) if names.iter().any(same) => {
                return Err(format!(
                    "the expression of the CHECK constraint {} names {column}, and the lab \
                     measured expressions of column names, numbers and arithmetic, comparison \
                     and logical operators only, not strings, function calls, IN, BETWEEN or LIKE",
                    check_name(check)
                ));
            }
            CheckColumns::Other(_) => continue,
        };
        if !columns.iter().any(same) {
            continue;
        }
        if let Some(missing) = columns.iter().find(|name| table.column(name).is_none()) {
            return Err(format!(
                "the CHECK constraint {} uses {missing}, a column {} does not have, as far as \
                 kilit can tell",
                check_name(check),
                table.name
            ));
        }
        user.get_or_insert(check);
        if with_another.is_none() {
            with_another = columns
                .iter()
                .find(|name| !same(name))
                .map(|other| (check, other.as_str()));
        }
    }
    let Some(user) = user else {
        return Ok(None);
    };
    if let Some(what) = beside_a_check(table, column, schema) {
        return Err(format!(
            "the CHECK constraint {} uses {column}, and {what}, which has not been measured \
             together",
            check_name(user)
        ));
    }
    Ok(Some(CheckUse { user, with_another }))
}

/// What else holds for a column a CHECK constraint uses that the lab has not
/// measured next to one.
fn beside_a_check(table: &Table, column: &str, schema: &Schema) -> Option<String> {
    if table.column(column).is_some_and(|c| c.generated.is_some()) {
        return Some(format!("{column} is a generated column"));
    }
    if let Some(user) = generated_column_naming(table, column) {
        return Some(format!("the generated column {} uses it", user.name));
    }
    if table.in_primary_key(column) {
        return Some(format!("{column} is in the primary key"));
    }
    if let Some(key) = table.foreign_key_on(column) {
        let key = key.name.as_deref().unwrap_or_default();
        return Some(format!("{column} is in the foreign key {key}"));
    }
    if let Some(referenced) = referenced(schema, table, column) {
        return Some(format!("{column} {referenced}"));
    }
    if let Some(index) = table.fulltext_index() {
        let index = index.name.as_deref().unwrap_or_default();
        return Some(format!("{} has the FULLTEXT index {index}", table.name));
    }
    table
        .indexes
        .iter()
        .find(|index| {
            index.columns.iter().any(|entry| match entry_column(entry) {
                Some(entry) => index.kind != IndexKind::Plain && entry.eq_ignore_ascii_case(column),
                None => uses_column(entry, column),
            })
        })
        .map(|index| {
            let index = index.name.as_deref().unwrap_or_default();
            format!("{column} is in the index {index}, not a plain one")
        })
}

/// Whether a CHECK constraint uses `column` and another column of `table`, so
/// that MySQL refuses to drop `column`. A constraint that uses only `column`
/// goes with it, and the lab measured that it changes nothing in the answer.
fn check_with_another_column(table: &Table, column: &str, schema: &Schema) -> Applies {
    match check_use(table, column, schema) {
        Ok(Some(CheckUse {
            with_another: Some((check, other)),
            ..
        })) => Applies::Yes(vec![format!(
            "the CHECK constraint {} uses {column} and {other}",
            check_name(check)
        )]),
        Ok(_) => Applies::No,
        Err(reason) => Applies::Unmeasured(reason),
    }
}

/// Whether a CHECK constraint uses `column`, so that MySQL refuses to rename it.
fn check_uses_column(table: &Table, column: &str, schema: &Schema) -> Applies {
    match check_use(table, column, schema) {
        Ok(Some(CheckUse { user, .. })) => Applies::Yes(vec![format!(
            "the CHECK constraint {} uses {column}",
            check_name(user)
        )]),
        Ok(None) => Applies::No,
        Err(reason) => Applies::Unmeasured(reason),
    }
}

/// The first CHECK constraint of `table` that names `column`, plain or not.
fn check_naming<'a>(table: &'a Table, column: &str) -> Option<&'a Check> {
    table.checks.iter().find(|check| {
        let (CheckColumns::Plain(names) | CheckColumns::Other(names)) = &check.columns;
        names.iter().any(|name| name.eq_ignore_ascii_case(column))
    })
}

/// The lab measured adding a CHECK constraint, enforced or not, with a plain
/// expression over columns of the table. By hand, MySQL 8.0.46 and 8.4.13
/// refuse a name the database already has (error 3822), a column the table
/// does not have (1054), an AUTO_INCREMENT column (3818), a disallowed
/// function (3814) and a foreign key column with a referential action
/// (3823). None of that has a lab fact yet, so kilit gives no answer when it
/// can see one of them.
fn check_new_constraint(
    check: &Check,
    table: TableState<'_>,
    schema: &Schema,
    notes: &mut Vec<String>,
) -> Result<(), String> {
    let CheckColumns::Plain(columns) = &check.columns else {
        return Err(format!(
            "adding a CHECK constraint on ({}) has not been measured yet; the lab measured \
             expressions of column names, numbers and arithmetic, comparison and logical \
             operators only",
            check.expression
        ));
    };
    if columns.is_empty() {
        return Err(
            "adding a CHECK constraint that uses no column has not been measured yet".into(),
        );
    }
    let TableState::Known(table) = table else {
        notes.push(
            "if the constraint's name is taken, or a column it uses is missing, AUTO_INCREMENT, \
             generated or in a foreign key: not measured, so kilit cannot tell (it can on a \
             table it knows)"
                .to_owned(),
        );
        return Ok(());
    };
    if let Some(name) = &check.name {
        let holder = schema.tables().find(|other| {
            other
                .checks
                .iter()
                .any(|c| check_name(c).eq_ignore_ascii_case(name))
        });
        if let Some(holder) = holder {
            return Err(format!(
                "{} already has a CHECK constraint {name}, and adding one with a name the \
                 database already has has not been measured yet",
                holder.name
            ));
        }
    }
    for name in columns {
        let Some(column) = table.column(name) else {
            return Err(format!(
                "{} has no column {name}, as far as kilit can tell",
                table.name
            ));
        };
        let what = if column.auto_increment {
            "an AUTO_INCREMENT column"
        } else if column.generated.is_some() {
            "a generated column"
        } else if table.foreign_key_on(name).is_some() {
            "a foreign key's column"
        } else {
            continue;
        };
        return Err(format!(
            "{name} is {what}, and a CHECK constraint on such a column has not been measured yet"
        ));
    }
    Ok(())
}

/// The case of `DROP CONSTRAINT`, which depends on what the name is. The lab
/// measured it for a CHECK constraint only.
fn dropped_constraint(name: &str, table: TableState<'_>) -> Result<&'static Case, String> {
    let table = match table {
        TableState::Known(table) => table,
        TableState::Lost { reason, .. } => {
            return Err(format!(
                "kilit lost track of this table ({reason}), so it cannot tell what kind of \
                 constraint {name} is"
            ));
        }
        TableState::Missing => {
            return Err(format!(
                "kilit does not know this table, so it cannot tell what kind of constraint \
                 {name} is"
            ));
        }
    };
    let kind = match table.constraints_named(name).as_slice() {
        [ConstraintKind::Check] => return Ok(&DROP_CHECK_CONSTRAINT),
        [] if table.foreign_keys.iter().any(|key| key.name.is_none()) => {
            return Err(format!(
                "{} has a foreign key kilit holds without the name MySQL gave it, which may be \
                 {name}, and dropping a foreign key with DROP CONSTRAINT has not been measured \
                 yet",
                table.name
            ));
        }
        [ConstraintKind::ForeignKey] => "a foreign key",
        [ConstraintKind::UniqueIndex] => "a unique index",
        [ConstraintKind::PrimaryKey] => "the primary key",
        [] => {
            return Err(format!(
                "{} has no constraint {name} that kilit knows of (MySQL does not count a plain \
                 index as one). If it has none, MySQL fails with error 3940 \
                 (drop-constraint-missing); kilit does not answer that from its tables alone, \
                 which may be out of date",
                table.name
            ));
        }
        _ => {
            return Err(format!(
                "{} has several constraints named {name}, and MySQL 8.0.46 and 8.4.13 do not \
                 accept DROP CONSTRAINT then",
                table.name
            ));
        }
    };
    Err(format!(
        "{name} is {kind}, and dropping one with DROP CONSTRAINT has not been measured yet"
    ))
}

/// The CHECK constraint that `clause` (`DROP CHECK`, `ALTER CHECK` or `ALTER
/// CONSTRAINT`) names. MySQL takes these clauses for a CHECK constraint
/// only: by hand, MySQL 8.0.46 and 8.4.13 refuse each of them for a foreign
/// key, a unique index, the primary key or another index.
/// `None` on a table kilit does not know, which gets the case's answer as
/// the likely one: see [`on_unknown_table`].
fn named_check<'t>(
    clause: &str,
    name: &str,
    table: TableState<'t>,
) -> Result<Option<&'t Check>, String> {
    let TableState::Known(table) = table else {
        return Ok(None);
    };
    match table.check_target(name) {
        CheckTarget::Check(at) => Ok(Some(&table.checks[at])),
        CheckTarget::NotACheck(what) => Err(format!(
            "{name} is {} of {}, not a CHECK constraint, and {REJECTED_BY} {clause} for it",
            what.join(" and "),
            table.name
        )),
        CheckTarget::Nothing => {
            let (error, scenario) = match clause {
                "DROP CHECK" => (3821, "drop-check-missing"),
                "ALTER CHECK" => (3821, "alter-check-missing"),
                _ => (3940, "alter-constraint-missing"),
            };
            Err(format!(
                "{} has no CHECK constraint {name} that kilit knows of. If it has none, MySQL \
                 fails with error {error} ({scenario}); kilit does not answer that from its \
                 tables alone, which may be out of date",
                table.name
            ))
        }
        CheckTarget::Several => Err(format!(
            "{} has a CHECK constraint {name} and another constraint or index of that name, \
             and {clause} has not been measured then",
            table.name
        )),
    }
}

/// The TEXT and BLOB types the lab measured an index on, without a prefix
/// length: MySQL refuses each with error 1170, in a plain and in a unique
/// index. The names are kilit's: [`DataType`] as it prints.
const MEASURED_BLOB_OR_TEXT: [&str; 5] = ["TEXT", "TINYTEXT", "MEDIUMTEXT", "LONGTEXT", "BLOB"];

/// Whether a column of this type is TEXT or BLOB, by the name of the type:
/// `TEXT`, `TINYBLOB`, `TEXT(1000)`, or `LONG`, MySQL's other name for
/// MEDIUMTEXT. CHAR, ENUM and SET are kept with TEXT, and JSON with BLOB,
/// but none of them is one.
fn is_blob_or_text(data_type: &DataType) -> bool {
    let (DataType::Text(name) | DataType::Other(name)) = data_type else {
        return false;
    };
    let base = name.split('(').next().unwrap_or_default().trim();
    [
        "TEXT",
        "TINYTEXT",
        "MEDIUMTEXT",
        "LONGTEXT",
        "BLOB",
        "TINYBLOB",
        "MEDIUMBLOB",
        "LONGBLOB",
        "LONG",
    ]
    .iter()
    .any(|blob_or_text| base.eq_ignore_ascii_case(blob_or_text))
}

/// Whether an index entry takes a prefix of its column, as `note(10)`
/// does.
fn has_prefix(entry: &str) -> bool {
    !is_expression(entry) && entry.contains('(')
}

/// Whether the index takes a TEXT or BLOB column of `table` whole, in a
/// shape the lab measured: such a column of a type in
/// [`MEASURED_BLOB_OR_TEXT`] alone in a plain or a unique index, or a TEXT
/// column after one other column in a plain index. Every other index that
/// takes such a column whole is unmeasured, so it gets no answer rather
/// than the one for an index MySQL accepts, or an error by inference.
fn blob_or_text_without_prefix(table: &Table, index: &Index) -> Applies {
    // The entries that take a TEXT or BLOB column whole, with its type.
    let whole: Vec<(&str, &DataType)> = index
        .columns
        .iter()
        .filter(|entry| !has_prefix(entry))
        .filter_map(|entry| {
            let column = table.column(entry_column(entry)?)?;
            is_blob_or_text(&column.data_type).then_some((entry.as_str(), &column.data_type))
        })
        .collect();
    let Some(&(column, data_type)) = whole.first() else {
        return Applies::No;
    };
    let named = |data_type: &DataType, names: &[&str]| {
        let name = data_type.to_string();
        names
            .iter()
            .any(|measured| name.eq_ignore_ascii_case(measured))
    };
    let measured = match (index.kind, index.columns.as_slice()) {
        (IndexKind::Plain | IndexKind::Unique, [_]) => named(data_type, &MEASURED_BLOB_OR_TEXT),
        // The first column is one kilit knows and not TEXT or BLOB, so the
        // column taken whole is the second.
        (IndexKind::Plain, [first, _]) => {
            named(data_type, &["TEXT"])
                && entry_column(first)
                    .and_then(|first| table.column(first))
                    .is_some_and(|first| !is_blob_or_text(&first.data_type))
        }
        _ => false,
    };
    if measured {
        Applies::Yes(vec![format!(
            "{column} is {data_type}, and the index takes it without a prefix length"
        )])
    } else if index.columns.len() == 1 {
        Applies::Unmeasured(format!(
            "{column} is {data_type}, and an index on such a column without a prefix length has \
             not been measured yet"
        ))
    } else {
        Applies::Unmeasured(format!(
            "{column} is {data_type}, and an index of several columns that takes it without a \
             prefix length has not been measured yet; the lab measured a TEXT column after one \
             other column, in a plain index"
        ))
    }
}

const JSON_INDEX_UNSEEN: &str = "if a column of the index is JSON: not measured, so kilit cannot tell (it can on a table it \
     knows)";

/// The lab has not measured an index on a JSON column. MySQL's manual says
/// it refuses one with error 3152, not the error 1170 of TEXT and BLOB
/// columns, so on a table kilit knows such an index gets no answer.
fn check_json_columns(
    index: &Index,
    table: TableState<'_>,
    notes: &mut Vec<String>,
) -> Result<(), String> {
    let TableState::Known(table) = table else {
        notes.push(JSON_INDEX_UNSEEN.to_owned());
        return Ok(());
    };
    for entry in &index.columns {
        let Some(column) = entry_column(entry).and_then(|name| table.column(name)) else {
            continue;
        };
        if matches!(&column.data_type, DataType::Other(name) if name.eq_ignore_ascii_case("JSON")) {
            return Err(format!(
                "{} is a JSON column, and an index on a JSON column has not been measured yet",
                column.name
            ));
        }
    }
    Ok(())
}

/// The case an operation belongs to, with what kilit worked out on the way.
struct Classified {
    case: &'static Case,
    notes: Vec<String>,
    /// A shape the lab has not measured, which makes the answer unknown
    /// unless a more telling reason does first.
    unmeasured: Option<String>,
    /// The column the change is about, as the table has it now.
    column: Option<String>,
    /// The row the change leaves, as far as kilit counted it.
    row: RowSize,
}

/// Finds the case an operation belongs to, or says why it has none.
fn classify(
    operation: &Operation,
    sql: &str,
    session: &Session,
    table: TableState<'_>,
    schema: &Schema,
    results: &ServerResults,
) -> Result<Classified, String> {
    let mut notes = Vec::new();
    let mut subject = None;
    let mut row = RowSize::Fits;
    let mut unmeasured = None;
    let case = match operation {
        Operation::AddColumn { column, .. } => {
            check_new_column(column)?;
            check_date_without_default(column)?;
            row = row_size(table, None, column)?;
            match expression_default(column.default.as_deref()) {
                None => match refused_default(column, session, Refused::Add, &mut notes)? {
                    Some(case) if std::ptr::eq(case, &ADD_COLUMN_BLOB_EMPTY_DEFAULT_NOT_STRICT) => {
                        if let TableState::Known(table) = table
                            && let Some(index) = table.fulltext_index()
                        {
                            return Err(format!(
                                "{} has the FULLTEXT index {}, and a TEXT or BLOB column with \
                                 DEFAULT '' in a non-strict SQL mode has not been measured on \
                                 such a table",
                                table.name,
                                index.name.as_deref().unwrap_or_default()
                            ));
                        }
                        notes.push(PLAIN_TABLE_ONLY.to_owned());
                        case
                    }
                    Some(case) => case,
                    None => &ADD_COLUMN,
                },
                Some(default) => {
                    let case = measured_default(default)?;
                    check_default_type(case, column, default)?;
                    check_written_default(sql, default)?;
                    if let BinaryLog::Other(setting) = &session.binary_log {
                        return Err(format!(
                            "a statement before this one sets {setting}, and the lab has \
                             measured expression defaults only with the binary log on"
                        ));
                    }
                    if session.binlog_format_set {
                        return Err("a statement before this one names binlog_format, and the \
                                    lab has measured expression defaults only with \
                                    binlog_format = ROW, the MySQL default"
                            .into());
                    }
                    notes.push(session.assumes_binary_log().to_owned());
                    case
                }
            }
        }
        Operation::DropColumn { name } => {
            check_dropped_column(table, name, schema, &mut notes)?;
            subject = Some(name.clone());
            &DROP_COLUMN
        }
        Operation::RenameColumn { from, .. } => {
            subject = Some(from.clone());
            &RENAME_COLUMN
        }
        Operation::DropColumnDefault { column } => {
            if let TableState::Known(table) = table
                && table.column(column).is_none()
            {
                return Err(format!(
                    "{} has no column {column} that kilit knows, and MySQL refuses ALTER COLUMN \
                     ... DROP DEFAULT of such a column (error 1054, by hand on MySQL 8.0.46 and \
                     8.4.13)",
                    table.name
                ));
            }
            return Err("dropping a column's default has not been measured yet".into());
        }
        Operation::RenameIndex { from, to } => {
            if let TableState::Known(table) = table {
                check_renamed_index(table, from, to)?;
            }
            return Err("renaming an index has not been measured yet".into());
        }
        Operation::SetAutoIncrement { .. } => {
            return Err("setting AUTO_INCREMENT has not been measured yet".into());
        }
        Operation::SetColumnDefault { column, default } => {
            // Measured with a literal, on a column that had no default.
            if let Some(default) = expression_default(Some(default)) {
                return Err(format!(
                    "setting the expression default {default} has not been measured yet"
                ));
            }
            match table {
                TableState::Known(table) => {
                    if let Some(old) = table.column(column)
                        && let Some(old_default) = expression_default(old.default.as_deref())
                    {
                        return Err(format!(
                            "{column} has the expression default {old_default}, and changing \
                             it has not been measured yet"
                        ));
                    }
                }
                TableState::Lost { .. } | TableState::Missing => {
                    notes.push(format!(
                        "if {column} has an expression default: not measured, so kilit cannot \
                         tell (it can on a table it knows)"
                    ));
                }
            }
            subject = Some(column.clone());
            &SET_COLUMN_DEFAULT
        }
        Operation::ModifyColumn { column, position } => {
            check_new_definition(column)?;
            check_modified_shape(table, &column.name, column, position.as_ref())?;
            subject = Some(column.name.clone());
            if let Some(case) = refused_default(column, session, Refused::Modify, &mut notes)? {
                return Ok(Classified {
                    case,
                    notes,
                    unmeasured: None,
                    column: subject,
                    row: RowSize::Fits,
                });
            }
            let case = modified(
                table,
                &column.name,
                column,
                position.as_ref(),
                results,
                &mut notes,
            )?;
            row = row_size(table, Some(&column.name), column)?;
            case
        }
        Operation::ChangeColumn {
            old_name,
            column,
            position,
        } => {
            check_new_definition(column)?;
            check_modified_shape(table, old_name, column, position.as_ref())?;
            if !old_name.eq_ignore_ascii_case(&column.name)
                && let TableState::Known(table) = table
                && let Some(user) = generated_column_naming(table, old_name)
            {
                return Err(format!(
                    "the generated column {} uses {old_name}, and renaming such a column with \
                     CHANGE has not been measured yet",
                    user.name
                ));
            }
            if !old_name.eq_ignore_ascii_case(&column.name)
                && let TableState::Known(table) = table
                && let Some(user) = check_naming(table, old_name)
            {
                return Err(format!(
                    "the CHECK constraint {} names {old_name}, and renaming such a column with \
                     CHANGE has not been measured yet",
                    check_name(user)
                ));
            }
            subject = Some(old_name.clone());
            if let Some(case) = refused_default(column, session, Refused::Modify, &mut notes)? {
                return Ok(Classified {
                    case,
                    notes,
                    unmeasured: None,
                    column: subject,
                    row: RowSize::Fits,
                });
            }
            let case = modified(
                table,
                old_name,
                column,
                position.as_ref(),
                results,
                &mut notes,
            )?;
            row = row_size(table, Some(old_name), column)?;
            case
        }
        Operation::AddIndex(index) if index.columns.iter().any(|entry| is_expression(entry)) => {
            check_functional_index(index, table)?;
            &ADD_FUNCTIONAL_INDEX
        }
        Operation::AddIndex(index) => {
            if let TableState::Known(table) = table {
                unmeasured = check_new_index(table, index).err();
            }
            match crate::parse::ordered_index(sql) {
                Some(crate::parse::OrderedIndex::Fulltext) if index.kind == IndexKind::Fulltext => {
                    return Ok(Classified {
                        case: &ADD_FULLTEXT_INDEX_WITH_ORDER,
                        notes,
                        unmeasured,
                        column: None,
                        row,
                    });
                }
                Some(ordered) => {
                    return Err(format!(
                        "MySQL refuses ASC or DESC on a key part of a {} index (error 1221, by \
                         hand on MySQL 8.0.46 and 8.4.13)",
                        ordered.name()
                    ));
                }
                None => {}
            }
            if index.kind == IndexKind::Fulltext
                && let TableState::Known(table) = table
            {
                fulltext_shape(table, index)?;
            }
            match index.kind {
                IndexKind::Plain => {
                    check_json_columns(index, table, &mut notes)?;
                    &ADD_INDEX
                }
                IndexKind::Unique => {
                    check_json_columns(index, table, &mut notes)?;
                    &ADD_UNIQUE_INDEX
                }
                // MySQL refuses some on a table kilit knows: that reason first.
                // Otherwise it is answered as the ALTER TABLE it runs as, and
                // made unknown after the tables follow it: see
                // `created_fulltext_index`.
                IndexKind::Fulltext if crate::parse::creates_fulltext_index(sql) => {
                    if let Some(reason) = unmeasured.take() {
                        return Err(reason);
                    }
                    &ADD_FULLTEXT_INDEX
                }
                IndexKind::Fulltext => &ADD_FULLTEXT_INDEX,
                IndexKind::Spatial => return Err(SPATIAL_NOT_MEASURED.into()),
            }
        }
        Operation::DropIndex { name } => {
            if let TableState::Known(table) = table {
                unmeasured = check_dropped_index(table, name).err();
            }
            &DROP_INDEX
        }
        Operation::AddForeignKey(key) => {
            if !matches!(table, TableState::Known(_))
                || !matches!(schema.table(&key.referenced_table), TableState::Known(_))
            {
                notes.push(FOREIGN_KEY_UNSEEN.to_owned());
            }
            foreign_key_case(key, session, table, schema, &mut notes)?
        }
        Operation::DropForeignKey { .. } => &DROP_FOREIGN_KEY,
        Operation::ChangeTableOptions(options) => {
            let changed: Vec<&str> = [
                ("ENGINE", options.engine.is_some()),
                ("ROW_FORMAT", options.row_format.is_some()),
                ("KEY_BLOCK_SIZE", options.key_block_size.is_some()),
            ]
            .into_iter()
            .filter_map(|(option, written)| written.then_some(option))
            .collect();
            if !changed.is_empty() {
                return Err(format!(
                    "changing a table's {} has not been measured yet",
                    changed.join(" and ")
                ));
            }
            // MySQL refuses a longer one in strict SQL mode, and shortens it
            // in the others.
            if options
                .comment
                .as_ref()
                .is_some_and(|comment| comment.chars().count() > MAX_TABLE_COMMENT)
            {
                return Err(format!(
                    "a table comment longer than {MAX_TABLE_COMMENT} characters has not been \
                     measured"
                ));
            }
            &SET_TABLE_COMMENT
        }
        Operation::AddCheck(check) => {
            check_new_constraint(check, table, schema, &mut notes)?;
            if check.enforced {
                &ADD_CHECK_CONSTRAINT
            } else {
                &ADD_NOT_ENFORCED_CHECK_CONSTRAINT
            }
        }
        Operation::DropConstraint { name } => dropped_constraint(name, table)?,
        Operation::DropCheck { name } => {
            named_check("DROP CHECK", name, table)?;
            &DROP_CHECK_CONSTRAINT
        }
        Operation::AlterCheck {
            name,
            enforced,
            constraint,
        } => {
            let clause = if *constraint {
                "ALTER CONSTRAINT"
            } else {
                "ALTER CHECK"
            };
            match named_check(clause, name, table)? {
                Some(check) if *enforced && !check.enforced => &ENFORCE_CHECK_CONSTRAINT,
                Some(_) => &ALTER_CHECK_CONSTRAINT,
                // NOT ENFORCED is measured from either state.
                None if !*enforced => &ALTER_CHECK_CONSTRAINT,
                None => {
                    return Err(format!(
                        "kilit does not know the table, so it cannot tell whether the CHECK \
                         constraint {name} is enforced now: ENFORCED reads every row only if \
                         it is not"
                    ));
                }
            }
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
    row_measured(case, &row)?;
    // The plain new column, the shape the lab measured both limits on.
    if std::ptr::eq(case, &ADD_COLUMN) && matches!(row, RowSize::CannotCount(_)) {
        notes.push(PAGE_RECORD_UNSEEN.to_owned());
    }
    Ok(Classified {
        case,
        notes,
        unmeasured,
        column: subject,
        row,
    })
}

/// A new NOT NULL DATE, DATETIME or TIMESTAMP column without a default:
/// MySQL fills the rows with a zero date, which the default sql_mode
/// refuses, and the lab has not measured it.
fn check_date_without_default(column: &Column) -> Result<(), String> {
    let DataType::Other(written) = &column.data_type else {
        return Ok(());
    };
    let name = written
        .split('(')
        .next()
        .unwrap_or_default()
        .trim()
        .to_ascii_uppercase();
    if matches!(name.as_str(), "DATE" | "DATETIME" | "TIMESTAMP")
        && column.nullable == Some(false)
        && column.default.is_none()
        && column.generated.is_none()
    {
        return Err(format!(
            "a NOT NULL {name} column without a default has not been measured yet"
        ));
    }
    Ok(())
}

/// A MODIFY or CHANGE of a shape the lab has not measured: one that puts
/// the column after itself, or makes a primary key column nullable.
fn check_modified_shape(
    table: TableState<'_>,
    old_name: &str,
    column: &Column,
    position: Option<&Position>,
) -> Result<(), String> {
    if let Some(Position::After(after)) = position
        && (same_column(after, old_name) || same_column(after, &column.name))
    {
        return Err(format!(
            "putting {old_name} AFTER itself has not been measured yet"
        ));
    }
    if let TableState::Known(table) = table
        && let Some(key) = sets_to_null(table, old_name, column)
    {
        return Err(format!(
            "{old_name} is in the foreign key {}, which sets it to NULL, and MySQL refuses to \
             make it NOT NULL (error 1830, by hand on MySQL 8.0.46 and 8.4.13)",
            key.name.as_deref().unwrap_or("without a name")
        ));
    }
    if let TableState::Known(table) = table
        && column.nullable == Some(true)
        && table
            .primary_key
            .iter()
            .any(|entry| same_column(entry.split('(').next().unwrap_or(entry), old_name))
    {
        return Err(format!(
            "making the primary key column {old_name} nullable has not been measured yet"
        ));
    }
    Ok(())
}

/// A new index the lab has not measured on a table kilit knows: a name the
/// table has, a column it does not have (MySQL refuses both: error 1061
/// and 1072, by hand on MySQL 8.0.46 and 8.4.13), a prefix longer than its
/// VARCHAR column (error 1089 on both, by hand), and a key whose bytes
/// kilit counts past InnoDB's limit of 3072.
fn check_new_index(table: &Table, index: &Index) -> Result<(), String> {
    if let Some(name) = &index.name
        && let Some(other) = table
            .indexes
            .iter()
            .find(|other| other.name.as_deref().is_some_and(|n| same_column(n, name)))
    {
        return Err(format!(
            "{} has an index {} already, and MySQL refuses a second index of the name (error \
             1061, by hand on MySQL 8.0.46 and 8.4.13)",
            table.name,
            other.name.as_deref().unwrap_or_default()
        ));
    }
    let mut bytes: Option<u64> = Some(0);
    for entry in &index.columns {
        if is_expression(entry) {
            bytes = None;
            continue;
        }
        let name = entry.split('(').next().unwrap_or(entry).trim();
        // The parser cuts a name that starts with a digit, such as `2e`, at
        // the digits: such a key part says nothing kilit can check.
        if name.starts_with(|c: char| c.is_ascii_digit()) {
            bytes = None;
            continue;
        }
        let Some(found) = table.column(name) else {
            return Err(format!(
                "{} has no column {name} that kilit knows, and MySQL refuses an index on such \
                 a column (error 1072, by hand on MySQL 8.0.46 and 8.4.13)",
                table.name
            ));
        };
        if has_prefix(entry) {
            let more = prefix_bytes(table, found, entry)?;
            bytes = match (bytes, more) {
                (Some(total), Some(more)) => Some(total.saturating_add(more)),
                _ => None,
            };
            continue;
        }
        bytes = match (bytes, innodb_key_part_bytes(table, found)) {
            (Some(total), Some(more)) => Some(total.saturating_add(more)),
            _ => None,
        };
    }
    // A FULLTEXT or spatial index has no such limit.
    let innodb_dynamic = matches!(index.kind, IndexKind::Plain | IndexKind::Unique)
        && matches!(table.engine, None | Some(Engine::InnoDb))
        && matches!(
            table.row_format,
            None | Some(RowFormat::Default | RowFormat::Dynamic)
        )
        && table.key_block_size.is_none();
    if innodb_dynamic && let Some(total) = bytes.filter(|total| *total > INNODB_KEY_BYTES) {
        return Err(format!(
            "the key takes {}, past the {INNODB_KEY_BYTES} bytes of an InnoDB key, which the \
             lab has not measured",
            row::bytes(total)
        ));
    }
    Ok(())
}

/// The bytes a whole column takes in an InnoDB key: an integer its size, a
/// CHAR or VARCHAR its length in its character set's widest characters, a
/// BINARY or VARBINARY its length; `None` for any other type, or a
/// character set kilit cannot tell.
fn innodb_key_part_bytes(table: &Table, column: &Column) -> Option<u64> {
    let per_character = || {
        table
            .text_settings(column)
            .ok()
            .map(|settings| settings.charset.max_bytes)
    };
    match &column.data_type {
        DataType::Integer { size, .. } => Some(match size {
            IntegerSize::Tiny => 1,
            IntegerSize::Small => 2,
            IntegerSize::Medium => 3,
            IntegerSize::Int => 4,
            IntegerSize::Big => 8,
        }),
        DataType::VarChar { length } => Some(length.saturating_mul(per_character()?)),
        DataType::Text(written) | DataType::Other(written) => {
            let written = written.trim().to_ascii_uppercase();
            let (base, rest) = written.split_once('(')?;
            let length: u64 = rest.strip_suffix(')')?.trim().parse().ok()?;
            match base.trim() {
                "CHAR" | "CHARACTER" => Some(length.saturating_mul(per_character()?)),
                "BINARY" | "VARBINARY" => Some(length),
                _ => None,
            }
        }
    }
}

/// The bytes a key part on a prefix of `column` takes, such as `note(10)`;
/// `None` when kilit cannot count them. A prefix longer than a VARCHAR
/// column is refused (error 1089 on MySQL 8.0.46 and 8.4.13, by hand); one
/// on a column of another length, or of a type without one, has not been
/// measured.
fn prefix_bytes(table: &Table, column: &Column, entry: &str) -> Result<Option<u64>, String> {
    let Some(prefix) = entry
        .split_once('(')
        .and_then(|(_, rest)| rest.trim_end_matches(')').trim().parse::<u64>().ok())
    else {
        return Ok(None);
    };
    let name = &column.name;
    let per_character = || {
        table
            .text_settings(column)
            .ok()
            .map(|settings| prefix.saturating_mul(settings.charset.max_bytes))
    };
    let (base, length) = match &column.data_type {
        DataType::VarChar { length } if prefix > *length => {
            return Err(format!(
                "the prefix {entry} is longer than {name}, a VARCHAR({length}), and \
                 {REJECTED_BY} it (error 1089)"
            ));
        }
        DataType::VarChar { .. } => return Ok(per_character()),
        DataType::Integer { .. } => {
            return Err(format!(
                "a prefix of the integer column {name} has not been measured yet"
            ));
        }
        DataType::Text(written) | DataType::Other(written) => {
            let written = written.trim().to_ascii_uppercase();
            let (base, rest) = written.split_once('(').unwrap_or((written.as_str(), ""));
            let length = rest.trim_end_matches(')').trim().parse::<u64>().ok();
            (base.trim().to_owned(), length)
        }
    };
    let (text, has_length) = match base.as_str() {
        "CHAR" | "CHARACTER" => (true, true),
        "TINYTEXT" | "TEXT" | "MEDIUMTEXT" | "LONGTEXT" => (true, false),
        "BINARY" | "VARBINARY" => (false, true),
        "TINYBLOB" | "BLOB" | "MEDIUMBLOB" | "LONGBLOB" => (false, false),
        _ => {
            return Err(format!(
                "a prefix of {name}, a {base} column, has not been measured yet"
            ));
        }
    };
    if has_length && length.is_none_or(|length| prefix > length) {
        return Err(format!(
            "the prefix {entry} is longer than {name}, a {base} column, which has not been \
             measured yet"
        ));
    }
    Ok(if text { per_character() } else { Some(prefix) })
}

/// The most bytes of a key on an InnoDB table of the DYNAMIC row format.
const INNODB_KEY_BYTES: u64 = 3072;

/// A `RENAME INDEX` MySQL refuses on a table kilit knows, by hand on MySQL
/// 8.0.46 and 8.4.13: PRIMARY (error 1280), an index the table does not
/// have (1176), or a name another index has in any letter case (1061).
fn check_renamed_index(table: &Table, from: &str, to: &str) -> Result<(), String> {
    let by_hand = "by hand on MySQL 8.0.46 and 8.4.13";
    if same_column(from, "PRIMARY") || same_column(to, "PRIMARY") {
        return Err(format!(
            "MySQL refuses RENAME INDEX of or to PRIMARY (error 1280, {by_hand})"
        ));
    }
    let named = |name: &str| {
        table
            .indexes
            .iter()
            .position(|index| index.name.as_deref().is_some_and(|n| same_column(n, name)))
    };
    let Some(at) = named(from) else {
        if table.indexes.iter().any(|index| index.name.is_none()) {
            return Ok(());
        }
        return Err(format!(
            "{} has no index {from} that kilit knows, and MySQL refuses RENAME INDEX of such \
             an index (error 1176, {by_hand})",
            table.name
        ));
    };
    if named(to).is_some_and(|other| other != at) {
        return Err(format!(
            "{} has an index {to} already, and MySQL refuses RENAME INDEX to its name (error \
             1061, {by_hand})",
            table.name
        ));
    }
    Ok(())
}

/// A `DROP INDEX` of a shape the lab has not measured on a table kilit
/// knows: one of several FULLTEXT indexes, or a UNIQUE index of several
/// columns. The lab measured a table's only FULLTEXT index and a UNIQUE
/// index of one column (drop-fulltext-index, drop-unique-index).
fn check_dropped_index(table: &Table, name: &str) -> Result<(), String> {
    let Some(index) = table
        .indexes
        .iter()
        .find(|index| index.name.as_deref().is_some_and(|n| same_column(n, name)))
    else {
        return Ok(());
    };
    // Without a primary key, InnoDB keys the rows on a UNIQUE index of NOT
    // NULL columns: dropping it changes the primary key.
    if index.kind == IndexKind::Unique
        && table.primary_key.is_empty()
        && index.columns.iter().all(|entry| {
            !has_prefix(entry)
                && table
                    .column(entry)
                    .is_some_and(|column| !table.is_nullable(column))
        })
    {
        return Err(format!(
            "{} has no primary key, and its UNIQUE index {name} of NOT NULL columns may be the \
             one InnoDB keys the rows on; dropping it has not been measured yet",
            table.name
        ));
    }
    match index.kind {
        IndexKind::Fulltext
            if table
                .indexes
                .iter()
                .filter(|index| index.kind == IndexKind::Fulltext)
                .count()
                > 1 =>
        {
            Err(format!(
                "{} has several FULLTEXT indexes, and dropping one of them has not been \
                 measured yet",
                table.name
            ))
        }
        IndexKind::Unique if index.columns.len() > 1 => Err(format!(
            "dropping the UNIQUE index {name} of several columns has not been measured yet"
        )),
        _ => Ok(()),
    }
}

/// What kilit cannot see about a new foreign key unless it knows both
/// tables: the shapes [`foreign_key_seen`] leaves unknown.
const FOREIGN_KEY_UNSEEN: &str = "if a column of the key is missing, or NOT NULL with SET NULL \
     in another form than ON DELETE SET NULL alone, or the table is MyISAM: not measured, so \
     kilit cannot tell (it can when it knows both tables)";

/// The case of a new foreign key, by the session's foreign_key_checks.
fn foreign_key_case(
    key: &ForeignKey,
    session: &Session,
    table: TableState<'_>,
    schema: &Schema,
    notes: &mut Vec<String>,
) -> Result<&'static Case, String> {
    Ok(match session.foreign_key_checks {
        Setting::On => &ADD_FOREIGN_KEY,
        Setting::Off => {
            foreign_key_with_checks_off(key, table, schema)?;
            &ADD_FOREIGN_KEY_CHECKS_OFF
        }
        Setting::ServerDefault => {
            notes.push(session.assumes_foreign_key_checks().to_owned());
            &ADD_FOREIGN_KEY
        }
        Setting::Unclear => {
            return Err("foreign_key_checks is set to a value kilit does not understand".into());
        }
        Setting::Unknown => return Err(session.foreign_key_checks_unknown.clone()),
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

/// How a statement gives a column its default, for [`refused_default`].
#[derive(Clone, Copy, PartialEq, Eq)]
enum Refused {
    Add,
    Modify,
}

/// The types that take no literal default, as the lab measured them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum NoLiteralDefault {
    /// TINYTEXT to LONGTEXT and TINYBLOB to LONGBLOB: in a non-strict SQL
    /// mode MySQL takes `''`, drops it and warns.
    TextOrBlob,
    Json,
    /// GEOMETRY and its kinds, such as POINT.
    Geometry,
}

/// Whether a column of this type takes no literal default, by the name of
/// the type; an error for a name of that family the lab has not measured,
/// such as `TEXT(1000)` or `LONG`.
fn no_literal_default(data_type: &DataType) -> Option<Result<NoLiteralDefault, String>> {
    let (DataType::Text(name) | DataType::Other(name)) = data_type else {
        return None;
    };
    let written = name.trim().to_ascii_uppercase();
    let base = written
        .split(|c: char| c == '(' || c.is_whitespace())
        .next()
        .unwrap_or_default();
    let family = match base {
        "TINYTEXT" | "TEXT" | "MEDIUMTEXT" | "LONGTEXT" | "TINYBLOB" | "BLOB" | "MEDIUMBLOB"
        | "LONGBLOB" => NoLiteralDefault::TextOrBlob,
        "JSON" => NoLiteralDefault::Json,
        "GEOMETRY" | "POINT" | "LINESTRING" | "POLYGON" | "MULTIPOINT" | "MULTILINESTRING"
        | "MULTIPOLYGON" | "GEOMETRYCOLLECTION" => NoLiteralDefault::Geometry,
        "LONG" | "GEOMCOLLECTION" => {
            return Some(Err(format!(
                "a default on a {name} column has not been measured yet"
            )));
        }
        _ => return None,
    };
    if written != base {
        return Some(Err(format!(
            "a default on a {name} column has not been measured yet; the lab measured the \
             types without a length"
        )));
    }
    Some(Ok(family))
}

/// A default that is a quoted string, such as `''` or `'[]'`: the only
/// literals the lab measured on these types.
fn is_quoted(default: &str) -> bool {
    default.len() >= 2 && default.starts_with('\'') && default.ends_with('\'')
}

/// Laravel's `'strict' => false` sets the session's sql_mode without a
/// statement kilit can see.
const STRICT_FOR_AN_EMPTY_DEFAULT: &str = "assumes strict SQL mode, the MySQL default and \
     Laravel's ('strict' => true); with 'strict' => false Laravel sets sql_mode = \
     'NO_ENGINE_SUBSTITUTION' without a statement kilit sees, and then";

const PLAIN_TABLE_ONLY: &str = "measured on a plain table only: not on a table with a FULLTEXT \
     index, nor after 64 instant column changes";

/// The case of a new definition whose default MySQL refuses (error 1101),
/// or `None` when it has no such default: a TEXT, BLOB, JSON or GEOMETRY
/// column with a quoted default. The lab measured them in its strict SQL
/// mode and with `sql_mode` set to `''` and `'NO_ENGINE_SUBSTITUTION'`:
/// MySQL refuses every one in each, except `''` on TEXT and BLOB, which in
/// the non-strict modes it drops with a warning. A MODIFY or CHANGE was
/// measured on TEXT, in strict mode only.
fn refused_default(
    column: &Column,
    session: &Session,
    refused: Refused,
    notes: &mut Vec<String>,
) -> Result<Option<&'static Case>, String> {
    let Some(family) = no_literal_default(&column.data_type) else {
        return Ok(None);
    };
    let Some(default) = column.default.as_deref() else {
        return Ok(None);
    };
    if default.eq_ignore_ascii_case("NULL") || is_expression(default) {
        return Ok(None);
    }
    let family = family?;
    let type_name = type_name(&column.data_type);
    if !is_quoted(default) {
        return Err(format!(
            "the default {default} on a {type_name} column has not been measured yet; the lab \
             measured quoted defaults, which MySQL refuses (error 1101)"
        ));
    }
    let empty = family == NoLiteralDefault::TextOrBlob && default == "''";
    match (&session.sql_mode, refused) {
        (SqlMode::Unclear(reason), _) => Err(format!(
            "{reason}, and whether MySQL refuses the default {default} on a {type_name} column \
             depends on it"
        )),
        (SqlMode::NotStrict, Refused::Add) if empty => {
            Ok(Some(&ADD_COLUMN_BLOB_EMPTY_DEFAULT_NOT_STRICT))
        }
        (SqlMode::NotStrict, Refused::Modify) if empty => Err(format!(
            "the input sets a non-strict sql_mode, in which MySQL drops the default {default} \
             of a {type_name} column with a warning, and kilit has no lab scenario for a MODIFY \
             or CHANGE that does so"
        )),
        (SqlMode::NotStrict, Refused::Modify) => Err(format!(
            "the input sets a non-strict sql_mode, and kilit has no lab scenario for a MODIFY \
             or CHANGE with the default {default} on a {type_name} column in it"
        )),
        (_, Refused::Modify) if family != NoLiteralDefault::TextOrBlob => Err(format!(
            "a MODIFY or CHANGE with the default {default} on a {type_name} column has not been \
             measured yet; the lab measured it on TEXT"
        )),
        (mode, _) => {
            if empty && *mode == SqlMode::ServerDefault {
                notes.push(match refused {
                    Refused::Add => format!(
                        "{STRICT_FOR_AN_EMPTY_DEFAULT} MySQL drops the default and adds the \
                         column (add-column-text-default-literal-sql-mode-not-strict and 3 more \
                         lab scenarios)"
                    ),
                    Refused::Modify => format!(
                        "{STRICT_FOR_AN_EMPTY_DEFAULT} MySQL drops the default, which the lab \
                         has not measured for MODIFY or CHANGE"
                    ),
                });
            }
            Ok(Some(match refused {
                Refused::Add => &ADD_COLUMN_BLOB_LITERAL_DEFAULT,
                Refused::Modify => &MODIFY_COLUMN_BLOB_LITERAL_DEFAULT,
            }))
        }
    }
}

/// The expressions `('')` and `('[]')` were measured as the default of a
/// TEXT and of a JSON column, and nowhere else.
fn check_default_type(case: &Case, column: &Column, default: &str) -> Result<(), String> {
    if !std::ptr::eq(case, &ADD_COLUMN_BLOB_EXPRESSION_DEFAULT) {
        return Ok(());
    }
    let measured_on = if default == "('')" { "TEXT" } else { "JSON" };
    match &column.data_type {
        DataType::Text(name) | DataType::Other(name) if name.eq_ignore_ascii_case(measured_on) => {
            Ok(())
        }
        other => Err(format!(
            "the expression default {default} has been measured on a {measured_on} column \
             only, not on {}",
            type_name(other)
        )),
    }
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

/// kilit reads `(SYSDATE ())` as `(SYSDATE())`, but MySQL does not: with
/// a space or a comment before the parenthesis, NOW, CURDATE, SYSDATE and
/// others are not the built-in function, and MySQL refuses the default
/// (error 3770). So a measured default must be written in the statement
/// with each function name right before its parenthesis.
fn check_written_default(sql: &str, default: &str) -> Result<(), String> {
    let not_measured = || {
        format!(
            "the expression default {default} is written with a space or a comment where \
             the lab measured none, and MySQL may read it as another function"
        )
    };
    let written = written_default(sql).ok_or_else(not_measured)?;
    if written.contains("/*") || written.contains("--") || written.contains('#') {
        return Err(not_measured());
    }
    let bytes = written.as_bytes();
    for (at, byte) in bytes.iter().enumerate() {
        if *byte != b'(' {
            continue;
        }
        let before = written[..at].trim_end();
        let spaced = before.len() < at;
        let after_name = before
            .bytes()
            .next_back()
            .is_some_and(|last| last.is_ascii_alphanumeric() || last == b'_');
        if spaced && after_name {
            return Err(not_measured());
        }
    }
    Ok(())
}

/// The parenthesized default of an ADD COLUMN as written in `sql`: from
/// the parenthesis after the word DEFAULT to the one that closes it.
/// Quoted text is skipped. `None` if there is none, or something stands
/// between DEFAULT and its parenthesis.
fn written_default(sql: &str) -> Option<&str> {
    let bytes = sql.as_bytes();
    let mut at = 0;
    let mut quote = None;
    while at < bytes.len() {
        let byte = bytes[at];
        if let Some(open) = quote {
            if byte == open {
                quote = None;
            }
            at += 1;
            continue;
        }
        if matches!(byte, b'\'' | b'"' | b'`') {
            quote = Some(byte);
            at += 1;
            continue;
        }
        let word_starts = at == 0 || !is_word_byte(bytes[at - 1]);
        let end = at + "DEFAULT".len();
        if word_starts
            && sql
                .get(at..end)
                .is_some_and(|word| word.eq_ignore_ascii_case("DEFAULT"))
            && bytes.get(end).is_none_or(|next| !is_word_byte(*next))
        {
            let open = end + (sql[end..].len() - sql[end..].trim_start().len());
            return (bytes.get(open) == Some(&b'(')).then(|| closing(sql, open))?;
        }
        at += 1;
    }
    None
}

fn is_word_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'$'
}

/// `sql` from the parenthesis at `open` to the one that closes it.
fn closing(sql: &str, open: usize) -> Option<&str> {
    let mut depth = 0usize;
    let mut quote = None;
    for (at, byte) in sql.bytes().enumerate().skip(open) {
        if let Some(open_quote) = quote {
            if byte == open_quote {
                quote = None;
            }
            continue;
        }
        match byte {
            b'\'' | b'"' | b'`' => quote = Some(byte),
            b'(' => depth += 1,
            b')' => {
                depth -= 1;
                if depth == 0 {
                    return Some(&sql[open..=at]);
                }
            }
            _ => {}
        }
    }
    None
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
        TableState::Lost { reason, .. } => {
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
/// FULLTEXT indexes, or that has a UNIQUE index of its own, or that is the
/// only column of a foreign key (MySQL refuses that one). Other shapes it
/// has not measured, so the answer the lab gave for indexed columns must
/// not reach them. Without the table kilit cannot see the shape; the notes
/// say what the lab measured.
fn check_dropped_column(
    table: TableState<'_>,
    column: &str,
    schema: &Schema,
    notes: &mut Vec<String>,
) -> Result<(), String> {
    let TableState::Known(table) = table else {
        notes.push(DROPPED_COLUMN_UNSEEN.to_owned());
        return Ok(());
    };
    for key in &table.foreign_keys {
        if !key.columns.iter().any(|c| c.eq_ignore_ascii_case(column)) {
            continue;
        }
        let name = key.name.as_deref().unwrap_or_default();
        if key.columns.len() > 1 {
            return Err(format!(
                "{column} is in the foreign key {name} with other columns, and \
                 {DROPPING_NOT_MEASURED}"
            ));
        }
        // MySQL keeps an index on a foreign key's columns, and the lab
        // measured the key with its index; kilit can lose that index only
        // by following a statement MySQL refuses.
        if table.index_on(column).is_none() {
            return Err(format!(
                "{column} is in the foreign key {name} but in no index kilit knows of, and \
                 {DROPPING_NOT_MEASURED}"
            ));
        }
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
    if let Some(referenced) = indexed.then(|| referenced(schema, table, column)).flatten() {
        return Err(format!(
            "{column} {referenced}, and {DROPPING_NOT_MEASURED}"
        ));
    }
    if let Some(reason) = functional.or(unmeasured) {
        return Err(reason);
    }
    // The lab measured the column as the whole primary key, and as the
    // second of two: see the exceptions of [`DROP_COLUMN`].
    if table.in_primary_key(column) {
        match table.primary_key.as_slice() {
            [key] if same_column(key, column) => {}
            [_, second] if same_column(second, column) => {}
            [_] => {
                return Err(format!(
                    "{column} is in the primary key with a prefix length, and \
                     {DROPPING_NOT_MEASURED}"
                ));
            }
            _ => {
                return Err(format!(
                    "{column} is in the primary key with other columns, which must stay \
                     unique without it; that depends on the rows, and the lab has measured \
                     dropping the second column of a primary key of two only"
                ));
            }
        }
    }
    // An indexed column that is generated, or that a generated column uses,
    // has not been measured: it must not get the answer for indexed columns.
    // Without an index, see [`generated_column_source`].
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

/// What kilit cannot see when it does not know the table: the shapes
/// [`check_dropped_column`] leaves unknown on a table it knows.
const DROPPED_COLUMN_UNSEEN: &str = "if the column is in a foreign key or a unique index with \
     other columns, in a primary key of another shape than the lab measured, in a functional or \
     spatial index, or is referenced by another table's foreign key: not measured, so kilit \
     cannot tell (it can on a table it knows)";

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
        TableState::Lost { reason, .. } => {
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
            let one_byte = |length: u64| length.saturating_mul(charset.max_bytes) <= 255;
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
    use crate::schema::three_ways;
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
            Prediction::Unknown { reason, .. } => reason,
            other => panic!("expected unknown, got {other:?}"),
        }
    }

    /// On a table kilit does not know: the case and answer the reason gives
    /// as likely, and the notes.
    fn likely(prediction: Prediction) -> (String, String, Vec<String>) {
        match prediction {
            Prediction::Unknown { reason, notes } => {
                let Some((_, measured)) = reason.split_once("the lab measured ") else {
                    panic!("no likely answer in: {reason}");
                };
                let (answer, case) = measured.rsplit_once(" (").unwrap();
                (
                    case.trim_end_matches(')').to_owned(),
                    answer.to_owned(),
                    notes,
                )
            }
            other => panic!("expected unknown with a likely answer, got {other:?}"),
        }
    }

    /// The case, what every answer shares, the reason and the notes of an
    /// answer that depends on the table.
    fn depends(prediction: Prediction) -> (&'static str, Shared, String, Vec<String>) {
        match prediction {
            Prediction::DependsOnTable {
                case,
                shared,
                reason,
                notes,
                ..
            } => (case, shared, reason, notes),
            other => panic!("expected an answer that depends on the table, got {other:?}"),
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
    fn a_foreign_key_name_of_mysql_s_form_follows_the_table_whatever_its_suffix() {
        // By hand on 8.0.46 and 8.4.13: RENAME TABLE renames orders_ibfk_abc
        // and orders_ibfk_0 to purchases_ibfk_abc and purchases_ibfk_0; the
        // drop of the new name and then of the column are accepted.
        for suffix in ["abc", "0"] {
            let schema = format!(
                "CREATE TABLE users (id BIGINT NOT NULL, PRIMARY KEY (id));
                 CREATE TABLE orders (id BIGINT NOT NULL, user_id BIGINT DEFAULT NULL,
                     PRIMARY KEY (id), KEY orders_ibfk_{suffix} (user_id),
                     CONSTRAINT orders_ibfk_{suffix} FOREIGN KEY (user_id) REFERENCES users (id));"
            );
            for rename in [
                "RENAME TABLE orders TO purchases",
                "ALTER TABLE orders RENAME TO purchases",
            ] {
                let predictions = predictions_with(
                    &schema,
                    &format!(
                        "{rename};
                         ALTER TABLE purchases DROP FOREIGN KEY purchases_ibfk_{suffix};
                         ALTER TABLE purchases DROP COLUMN user_id;"
                    ),
                );
                let (case, _, _) = measured(predictions[1].clone());
                assert_eq!(case, "drop-foreign-key", "{suffix}, {rename}");
                assert!(fails_with(&predictions[2]).is_err(), "{suffix}, {rename}");
            }
        }
    }

    #[test]
    fn a_varchar_whose_bytes_overflow_is_unknown_and_no_panic() {
        // 4611686018427387904 utf8mb4 characters are 2^64 bytes: a MODIFY to
        // it, alone or with a second change, and an ADD COLUMN of it.
        for sql in [
            "ALTER TABLE orders MODIFY note VARCHAR(4611686018427387904) NULL",
            "ALTER TABLE orders MODIFY note VARCHAR(4611686018427387904) \
             CHARACTER SET utf8mb4 COLLATE utf8mb4_bin NULL",
            "ALTER TABLE orders ADD COLUMN c VARCHAR(4611686018427387904) NULL",
        ] {
            let prediction = predictions_with(SHOP, sql).pop().unwrap();
            assert!(
                matches!(prediction, Prediction::Unknown { .. }),
                "{sql}: {prediction:?}"
            );
            assert!(!reason(prediction).contains(" 0 bytes"), "{sql}");
        }
    }

    #[test]
    fn a_compact_record_past_what_a_page_keeps_is_unknown() {
        // By hand on both servers: ten VARCHAR(255) utf8mb4 columns fit a
        // COMPACT page, an eleventh gives error 1118.
        let columns: Vec<String> = (1..=10)
            .map(|i| format!("a{i} VARCHAR(255) NULL"))
            .collect();
        let schema = format!(
            "CREATE TABLE c10 (id BIGINT PRIMARY KEY, {}) ENGINE=InnoDB DEFAULT \
             CHARSET=utf8mb4 ROW_FORMAT=COMPACT;",
            columns.join(", ")
        );
        let prediction =
            predictions_with(&schema, "ALTER TABLE c10 ADD COLUMN c VARCHAR(255) NULL")
                .pop()
                .unwrap();
        assert!(
            reason(prediction).contains("past the 8125 its row format allows"),
            "{schema}"
        );
        let fewer = schema.replace(", a10 VARCHAR(255) NULL", "");
        let prediction = predictions_with(&fewer, "ALTER TABLE c10 ADD COLUMN c VARCHAR(255) NULL")
            .pop()
            .unwrap();
        let (case, _, _) = measured(prediction);
        assert_eq!(case, "compact-add-column");
    }

    #[test]
    fn sql_mode_is_read_only_from_a_session_list_the_lab_measured() {
        let mode = |sql: &str| {
            let statement = parse(sql).pop().unwrap();
            SqlMode::read(&statement.sql.to_ascii_lowercase(), &statement.kind, 1)
        };
        for sql in [
            "SET sql_mode = ''",
            "SET SESSION sql_mode = 'NO_ENGINE_SUBSTITUTION'",
            "SET @@sql_mode = 'no_engine_substitution'",
        ] {
            assert_eq!(mode(sql), SqlMode::NotStrict, "{sql}");
        }
        for sql in [
            "SET sql_mode = 'STRICT_TRANS_TABLES'",
            "SET sql_mode = 'ONLY_FULL_GROUP_BY,STRICT_TRANS_TABLES,NO_ZERO_IN_DATE,NO_ZERO_DATE,\
             ERROR_FOR_DIVISION_BY_ZERO,NO_ENGINE_SUBSTITUTION'",
            "SET sql_mode = 'STRICT_ALL_TABLES'",
        ] {
            assert_eq!(mode(sql), SqlMode::Strict, "{sql}");
        }
        for sql in [
            "SET GLOBAL sql_mode = ''",
            "SET PERSIST sql_mode = ''",
            "SET sql_mode = 'ANSI'",
            "SET sql_mode = 'TRADITIONAL'",
            "SET sql_mode = CONCAT(@@sql_mode, ',ANSI_QUOTES')",
            "SET @old = @@sql_mode, sql_mode = ''",
            "SET sql_mode = DEFAULT",
            "SELECT @@sql_mode",
        ] {
            assert!(matches!(mode(sql), SqlMode::Unclear(_)), "{sql}");
        }
    }

    #[test]
    fn every_situation_hidden_in_the_rows_is_a_row_situation() {
        let history = "if the table has had 64 instant column changes since it was last rebuilt";
        let cases = CASES
            .iter()
            .chain(&MYISAM_CASES)
            .chain(&ROW_FORMAT_CASES)
            .chain(&OLD_ALTER_TABLE_CASES);
        for exception in cases.flat_map(|case| case.exceptions) {
            let row = ROW_SITUATIONS.contains(&exception.when);
            if exception.conditions.contains(&Condition::Hidden) {
                assert!(row || exception.when == history, "{}", exception.when);
            } else {
                assert!(!row, "{}", exception.when);
            }
        }
    }

    #[test]
    fn a_note_depends_on_rows_when_it_starts_with_a_row_situation() {
        assert!(depends_on_rows(
            "if a row points to a parent row that does not exist: fails with error 1452 \
             (add-foreign-key-orphans)"
        ));
        assert!(depends_on_rows(
            "if the columns hold duplicate values: not measured on a COMPACT table; DYNAMIC \
             gives fails with error 1062 (add-unique-index-duplicates)"
        ));
        for note in [
            "if the table has had 64 instant column changes since it was last rebuilt: INPLACE, \
             rebuilds the table, writes continue (add-column-after-64-changes)",
            ASSUMES_FOREIGN_KEY_CHECKS,
            "if a row holds NULL",
            "user_id is in the foreign key orders_user; if a row holds NULL: x",
        ] {
            assert!(!depends_on_rows(note), "{note}");
        }
    }

    #[test]
    fn a_global_foreign_key_checks_is_told_from_a_session_one() {
        use Scope::{Global, Persist, PersistOnly, Session};
        for (sql, scopes) in [
            ("set global foreign_key_checks = 0", vec![Global]),
            ("set @@global.foreign_key_checks = 0", vec![Global]),
            ("set persist foreign_key_checks = 0", vec![Persist]),
            ("set @@persist.foreign_key_checks = 0", vec![Persist]),
            (
                "set session sql_mode = '', global foreign_key_checks := 0",
                vec![Global],
            ),
            ("set foreign_key_checks = 0", vec![Session]),
            ("set session foreign_key_checks = 0", vec![Session]),
            ("set local foreign_key_checks = 0", vec![Session]),
            ("set @@foreign_key_checks = 0", vec![Session]),
            ("set `foreign_key_checks` = 0", vec![Session]),
            // A scope keyword carries to the assignments after it that have
            // none of their own.
            (
                "set persist foreign_key_checks = 0, foreign_key_checks = 1",
                vec![Persist, Persist],
            ),
            (
                "set global sql_log_bin = 1, foreign_key_checks = 0",
                vec![Global],
            ),
            (
                "set persist_only innodb_strict_mode = on, foreign_key_checks = 0",
                vec![PersistOnly],
            ),
            (
                "set global a = 1, session b = 2, foreign_key_checks = 0",
                vec![Session],
            ),
            ("set global a = 'x,y', foreign_key_checks = 0", vec![Global]),
            // `@@global.` and `@@` belong to their own variable only.
            ("set @@global.a = 1, foreign_key_checks = 0", vec![Session]),
            ("set global a = 1, @@foreign_key_checks = 0", vec![Session]),
            (
                "set global a = 1, @@session.foreign_key_checks = 0, foreign_key_checks = 1",
                vec![Session, Global],
            ),
            ("set persist_only foreign_key_checks = 0", vec![PersistOnly]),
            (
                "set @@persist_only.foreign_key_checks = 0",
                vec![PersistOnly],
            ),
            ("set my_global foreign_key_checks = 0", vec![]),
            ("set global sql_log_bin = 1", vec![]),
            // Reading the global value assigns nothing.
            ("set @saved = @@global.foreign_key_checks", vec![]),
            ("set @a = 1, @saved = @@global.foreign_key_checks", vec![]),
            ("select foreign_key_checks = 0", vec![]),
        ] {
            assert_eq!(scopes_assigned(sql, "foreign_key_checks"), scopes, "{sql}");
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
    fn a_new_column_on_a_table_kilit_does_not_know_is_unknown_and_likely_instant() {
        let (case, answer, notes) = likely(only(
            "ALTER TABLE orders ADD coupon VARCHAR(255) NULL AFTER amount",
        ));
        assert_eq!(case, "add-column");
        assert_eq!(answer, "INSTANT, no rebuild, writes continue");
        // kilit does not know the table, so the exceptions are notes, and
        // so is the limit of a record in a page.
        assert_eq!(notes.len(), ADD_COLUMN.exceptions.len() + 1);
        assert_eq!(notes[0], PAGE_RECORD_UNSEEN);
        assert!(notes[1].starts_with("if the table has a FULLTEXT index: COPY"));
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
        // Without the tables each one is only likely.
        let cases: Vec<String> = predictions
            .iter()
            .map(|prediction| match prediction {
                Prediction::NotChecked { .. } => "not checked".to_owned(),
                other => likely(other.clone()).0,
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
            likely(prediction.clone())
                .2
                .iter()
                .any(|note| note.starts_with("assumes foreign_key_checks"))
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
        assert_eq!(
            likely(predictions[1].clone()).0,
            "add-foreign-key-checks-off"
        );
        assert!(reason(predictions.remove(3)).contains("does not understand"));
    }

    const ADD_ORDERS_USER: &str =
        "ALTER TABLE orders ADD CONSTRAINT fk FOREIGN KEY (user_id) REFERENCES users (id)";

    #[test]
    fn foreign_key_checks_set_to_default_are_unknown_until_set_again() {
        for set in [
            "SET foreign_key_checks = DEFAULT",
            "SET SESSION foreign_key_checks = DEFAULT",
            "SET LOCAL foreign_key_checks = default",
            "SET @@foreign_key_checks = DEFAULT",
            "SET @@session.foreign_key_checks=DEFAULT",
            "SET @@local.FOREIGN_KEY_CHECKS = DEFAULT",
            "SET sql_log_bin = 1, foreign_key_checks = DEFAULT",
        ] {
            let mut predictions = predictions(&format!(
                "{set};\n{ADD_ORDERS_USER};\nSET foreign_key_checks = 1;\n{ADD_ORDERS_USER};"
            ));
            assert_eq!(likely(predictions.pop().unwrap()).0, "add-foreign-key");
            assert_eq!(
                reason(predictions.remove(1)),
                "line 1 sets foreign_key_checks to DEFAULT, which the lab has not measured",
                "{set}"
            );
        }
        // A foreign key's column, and the index that serves it.
        let setup = "ALTER TABLE orders ADD CONSTRAINT orders_user FOREIGN KEY (user_id) \
                     REFERENCES users (id);\nALTER TABLE orders ADD INDEX by_user (user_id);";
        for (sql, expected) in [
            (
                "ALTER TABLE orders DROP COLUMN user_id",
                "user_id is in the foreign key orders_user, and line 1 sets foreign_key_checks \
                 to DEFAULT, which the lab has not measured",
            ),
            (
                "ALTER TABLE orders DROP INDEX by_user",
                "by_user serves the foreign key orders_user, and line 1 sets \
                 foreign_key_checks to DEFAULT, which the lab has not measured",
            ),
        ] {
            let sql = format!("SET foreign_key_checks = DEFAULT;\n{sql}");
            assert_eq!(reason(on_shop(setup, &sql)), expected, "{sql}");
        }
        // The global value only changes the global value.
        let (_, _, notes) = measured(on_shop(
            "",
            &format!("SET GLOBAL foreign_key_checks = DEFAULT;\n{ADD_ORDERS_USER}"),
        ));
        assert_eq!(notes[0], ASSUMES_FOREIGN_KEY_CHECKS_AFTER_GLOBAL);
    }

    /// The predictions for `files`, each one a session, on [`SHOP`].
    fn in_sessions(files: &[&str]) -> Vec<Prediction> {
        let parsed: Vec<Vec<Statement>> = files.iter().map(|sql| parse(sql)).collect();
        let sessions: Vec<&[Statement]> = parsed.iter().map(Vec::as_slice).collect();
        check_sessions(&sessions, &Schema::from_sql(SHOP), results())
            .into_iter()
            .map(|finding| finding.prediction)
            .collect()
    }

    #[test]
    fn each_session_starts_with_the_mysql_defaults() {
        let last = |files: &[&str]| in_sessions(files).pop().unwrap();
        // In one session the checks stay off.
        assert_eq!(
            case_id(&last(&[&format!(
                "SET foreign_key_checks = 0;\n{ADD_ORDERS_USER}"
            )])),
            "add-foreign-key-checks-off"
        );
        let (case, _, notes) = measured(last(&["SET foreign_key_checks = 0;", ADD_ORDERS_USER]));
        assert_eq!(case, "add-foreign-key");
        assert_eq!(notes[0], ASSUMES_FOREIGN_KEY_CHECKS_IN_A_NEW_SESSION);
        let (case, _, notes) = measured(last(&[
            "SET foreign_key_checks = 0;",
            &format!("SET foreign_key_checks = 1;\n{ADD_ORDERS_USER}"),
        ]));
        assert_eq!(case, "add-foreign-key");
        assert!(
            !notes.iter().any(|note| note.starts_with("assumes")),
            "{notes:?}"
        );

        let (case, _, notes) = measured(last(&["SET old_alter_table = ON;", ADD_COUPON]));
        assert_eq!(case, "add-column");
        assert!(
            notes.contains(&ASSUMES_OLD_ALTER_TABLE_IN_A_NEW_SESSION.to_owned()),
            "{notes:?}"
        );
        let (_, _, notes) = measured(last(&[
            "SET old_alter_table = ON;",
            &format!("{ADD_COUPON}, ALGORITHM=INPLACE"),
        ]));
        assert!(!notes.contains(&ASSUMES_OLD_ALTER_TABLE_IN_A_NEW_SESSION.to_owned()));

        let default = "ALTER TABLE orders ADD COLUMN due DATE NULL DEFAULT (CURDATE())";
        let (_, _, notes) = measured(last(&["SET sql_log_bin = 0;", default]));
        assert_eq!(notes, [ASSUMES_BINARY_LOG_IN_A_NEW_SESSION]);
        let (_, _, notes) = measured(last(&["SELECT 1;", default]));
        assert_eq!(notes, [ASSUMES_BINARY_LOG]);
    }

    #[test]
    fn a_global_value_set_in_an_earlier_session_is_unknown_in_the_later_ones() {
        let last = |files: &[&str]| in_sessions(files).pop().unwrap();
        for set in [
            "SET GLOBAL foreign_key_checks = 0;",
            "SET @@global.foreign_key_checks = 0;",
            "SET PERSIST foreign_key_checks = 0;",
            "SET GLOBAL innodb_lock_wait_timeout = 50, foreign_key_checks = 0;",
        ] {
            assert_eq!(
                reason(last(&[set, "SELECT 1;", ADD_ORDERS_USER])),
                "an earlier file sets foreign_key_checks globally, and each file runs in a new \
                 session, which starts with the global value",
                "{set}"
            );
            // Until the session sets it.
            assert_eq!(
                case_id(&last(&[
                    set,
                    &format!("SET foreign_key_checks = 1;\n{ADD_ORDERS_USER}")
                ])),
                "add-foreign-key"
            );
        }
        // PERSIST_ONLY changes the value the server starts with next time.
        let (_, _, notes) = measured(last(&[
            "SET PERSIST_ONLY foreign_key_checks = 0;",
            ADD_ORDERS_USER,
        ]));
        assert_eq!(notes[0], ASSUMES_FOREIGN_KEY_CHECKS);

        for set in [
            "SET GLOBAL old_alter_table = ON;",
            "SET PERSIST old_alter_table = ON;",
            "SET GLOBAL foreign_key_checks = 1, old_alter_table = ON;",
        ] {
            let reason = reason(last(&[set, ADD_COUPON]));
            assert!(
                reason.starts_with("an earlier file sets old_alter_table globally"),
                "{set}: {reason}"
            );
        }
        let default = "ALTER TABLE orders ADD COLUMN due DATE NULL DEFAULT (CURDATE())";
        let reason = reason(last(&["SET GLOBAL sql_log_bin = 1;", default]));
        assert!(
            reason.contains("sets sql_log_bin globally in an earlier file"),
            "{reason}"
        );
        // What is named anywhere earlier stays named.
        let reason = super::tests::reason(last(&["SET binlog_format = 'ROW';", default]));
        assert!(reason.contains("names binlog_format"), "{reason}");
    }

    #[test]
    fn a_set_mysql_may_refuse_leaves_what_it_sets_unknown() {
        for set in [
            "SET foreign_key_checks = 0, GLOBAL sql_log_bin = 1",
            "SET foreign_key_checks = 0, restrict_fk_on_non_standard_key = OFF",
        ] {
            assert_eq!(
                reason(on_shop("", &format!("{set};\n{ADD_ORDERS_USER}"))),
                "line 1 sets foreign_key_checks in a SET that MySQL may refuse as a whole, and \
                 the lab has not measured it",
                "{set}"
            );
        }
        let reason = reason(on_shop(
            "",
            &format!("SET old_alter_table = ON, GLOBAL sql_log_bin = 1;\n{ADD_COUPON}"),
        ));
        assert!(
            reason.starts_with("line 1 sets old_alter_table"),
            "{reason}"
        );
    }

    #[test]
    fn foreign_key_checks_assigned_in_a_form_kilit_cannot_read_are_unknown() {
        for set in [
            "SET foreign_key_checks := DEFAULT",
            "SET @@`foreign_key_checks` = DEFAULT",
            "SET NAMES utf8mb4, foreign_key_checks = 0",
            "SET PERSIST a = 1, SESSION foreign_key_checks = 0",
        ] {
            assert_eq!(
                reason(on_shop("", &format!("{set};\n{ADD_ORDERS_USER}"))),
                "line 1 sets foreign_key_checks in a form kilit cannot read",
                "{set}"
            );
        }
        // MySQL refuses DEFAULT in quotes and keeps the value.
        let (case, _, notes) = measured(on_shop(
            "",
            &format!("SET foreign_key_checks = 'DEFAULT';\n{ADD_ORDERS_USER}"),
        ));
        assert_eq!(
            (case, notes[0].as_str()),
            ("add-foreign-key", ASSUMES_FOREIGN_KEY_CHECKS)
        );
        // A comment is not a scope.
        assert_eq!(
            scopes_assigned("set/**/global foreign_key_checks = 0", "foreign_key_checks"),
            [Scope::Global]
        );
        assert_eq!(
            scopes_assigned(
                "set global a = '/* x', foreign_key_checks = 0 -- b",
                "foreign_key_checks"
            ),
            [Scope::Global]
        );
    }

    #[test]
    fn a_table_comment_longer_than_mysql_takes_is_unknown() {
        let sql = format!("ALTER TABLE orders COMMENT = '{}'", "é".repeat(2048));
        assert_eq!(measured(on_shop("", &sql)).0, "set-table-comment");
        let sql = format!("ALTER TABLE orders COMMENT = '{}'", "a".repeat(2049));
        assert_eq!(
            reason(on_shop("", &sql)),
            "a table comment longer than 2048 characters has not been measured"
        );
    }

    #[test]
    fn a_table_in_another_letter_case_is_followed_where_the_migration_goes_on() {
        let letter_case = |sql: &str| {
            let mut schema = Schema::from_sql(SHOP);
            let mut session = Session::default();
            let mut agreements = Agreements::default();
            let mut last = None;
            for statement in parse(sql) {
                let found = schema.in_another_case(&statement);
                last = check_one(
                    &statement,
                    found,
                    &mut session,
                    &mut schema,
                    results(),
                    &mut agreements,
                )
                .1;
            }
            match last {
                Some(LetterCase::SameTable { .. }) => "same table",
                Some(LetterCase::AsWritten) => "as written",
                Some(LetterCase::Unclear) => "unclear",
                None => "exact",
            }
        };
        for (sql, expected) in [
            // Refused with 0 (by hand): 1146, 1051, 1049, 1824.
            ("ALTER TABLE ORDERS ADD COLUMN c INT NULL", "same table"),
            ("CREATE INDEX i ON Orders (amount)", "same table"),
            ("DROP INDEX i ON ORDERS", "same table"),
            ("RENAME TABLE ORDERS TO o2", "same table"),
            ("DROP TABLE ORDERS", "same table"),
            ("TRUNCATE TABLE ORDERS", "same table"),
            (
                "ALTER TABLE orders ADD CONSTRAINT k FOREIGN KEY (user_id) REFERENCES USERS (id)",
                "same table",
            ),
            // Refused with 1 (1050), or the same table either way.
            ("CREATE TABLE ORDERS (id INT PRIMARY KEY)", "as written"),
            ("RENAME TABLE orders TO ORDERS", "as written"),
            (
                "SET foreign_key_checks = 0; \
                 ALTER TABLE orders ADD CONSTRAINT k FOREIGN KEY (user_id) REFERENCES USERS (id)",
                "as written",
            ),
            // Both go on with different tables, or no hand test.
            ("DROP TABLE IF EXISTS ORDERS", "unclear"),
            ("ALTER TABLE orders RENAME TO ORDERS", "unclear"),
            ("DROP TABLE ORDERS, users", "unclear"),
            ("RENAME TABLE ORDERS TO o2, users TO u2", "unclear"),
            ("RENAME TABLE users TO ORDERS", "unclear"),
            (
                "CREATE TABLE IF NOT EXISTS ORDERS (id INT PRIMARY KEY)",
                "unclear",
            ),
            (
                "CREATE TEMPORARY TABLE ORDERS (id INT PRIMARY KEY)",
                "unclear",
            ),
            (
                "ALTER TABLE orders ADD CONSTRAINT k FOREIGN KEY (user_id) REFERENCES ORDERS (id)",
                "unclear",
            ),
            ("ALTER TABLE ORDERS FROBNICATE", "unclear"),
            (
                "CREATE TABLE IF NOT EXISTS ORDERS (id INT PRIMARY KEY); \
                 ALTER TABLE ORDERS ADD COLUMN c INT NULL",
                "unclear",
            ),
            // A table kilit may not see, another database's table of the
            // written name, a temporary table, or a name that is taken.
            (
                "RENAME TABLES users TO archive; ALTER TABLE ORDERS ADD COLUMN c INT NULL",
                "unclear",
            ),
            (
                "CREATE TABLE shop.ORDERS (id INT PRIMARY KEY); \
                 ALTER TABLE ORDERS ADD COLUMN c INT NULL",
                "unclear",
            ),
            (
                "CREATE TEMPORARY TABLE t1 (id INT); CREATE TABLE T1 (id INT PRIMARY KEY)",
                "unclear",
            ),
            (
                "CREATE TEMPORARY TABLE t1 (id INT); ALTER TABLE T1 ADD COLUMN c INT NULL",
                "same table",
            ),
            ("RENAME TABLE ORDERS TO users", "unclear"),
            ("RENAME TABLE ORDERS TO orders", "unclear"),
            ("ALTER TABLE orders ADD COLUMN c INT NULL", "exact"),
        ] {
            assert_eq!(letter_case(sql), expected, "{sql}");
        }
        // Letters the hand tests changed, and others.
        assert!(tested_letters("ÇİZELGE", "çizelge"));
        assert!(tested_letters("ÇIZELGE", "çizelge"));
        assert!(tested_letters("Çizelge", "çizelge"));
        assert!(!tested_letters("çızelge", "çizelge"));
        assert!(!tested_letters("STRASSE", "straße"));
        assert!(!tested_letters("ÖDEME", "ödeme"));
    }

    #[test]
    fn a_new_engine_or_row_format_the_keys_may_not_fit_loses_the_table() {
        let table = |sql: &str| {
            let mut schema = Schema::from_sql(SHOP);
            let mut session = Session::default();
            let mut agreements = Agreements::default();
            for statement in parse(sql) {
                check_one(
                    &statement,
                    schema.in_another_case(&statement),
                    &mut session,
                    &mut schema,
                    results(),
                    &mut agreements,
                );
            }
            match schema.table("orders") {
                TableState::Known(_) => "known",
                TableState::Lost { .. } => "lost",
                TableState::Missing => "missing",
            }
        };
        for (sql, state) in [
            // Integer keys fit every limit.
            ("ALTER TABLE orders ENGINE=MyISAM", "known"),
            ("ALTER TABLE orders ROW_FORMAT=COMPACT", "known"),
            // 255 utf8mb4 characters are 1020 bytes.
            (
                "ALTER TABLE orders ADD INDEX by_email (email);\nALTER TABLE orders ENGINE=MyISAM",
                "lost",
            ),
            (
                "ALTER TABLE orders ADD INDEX by_email (email);\n\
                 ALTER TABLE orders ROW_FORMAT=REDUNDANT",
                "lost",
            ),
            (
                "ALTER TABLE orders ADD INDEX by_note (note);\nALTER TABLE orders ENGINE=MyISAM",
                "known",
            ),
            (
                "ALTER TABLE orders ADD INDEX by_email (email);\n\
                 ALTER TABLE orders ROW_FORMAT=COMPRESSED",
                "known",
            ),
            (
                "ALTER TABLE orders ADD FULLTEXT ft (note);\nALTER TABLE orders ENGINE=MyISAM",
                "lost",
            ),
            (
                "ALTER TABLE orders ADD INDEX by_prefix (email(10));\n\
                 ALTER TABLE orders ROW_FORMAT=COMPACT",
                "lost",
            ),
        ] {
            assert_eq!(table(sql), state, "{sql}");
        }
    }

    #[test]
    fn a_myisam_table_whose_auto_increment_column_leads_no_key_may_not_become_innodb() {
        let mut keys = Table {
            engine: Some(Engine::MyIsam),
            columns: parse("CREATE TABLE t (a INT, b INT AUTO_INCREMENT)")
                .into_iter()
                .find_map(|statement| match statement.kind {
                    StatementKind::CreateTable { table, .. } => Some(table.columns),
                    _ => None,
                })
                .unwrap(),
            primary_key: vec!["a".into(), "b".into()],
            ..Table::default()
        };
        let innodb = TableOptions {
            engine: Some(Engine::InnoDb),
            ..TableOptions::default()
        };
        assert!(keys_fit(&keys, &innodb).is_err());
        keys.primary_key = vec!["b".into(), "a".into()];
        assert!(keys_fit(&keys, &innodb).is_ok());
    }

    #[test]
    fn forced_variants_use_the_lab_run_for_that_variant() {
        // Forcing INPLACE on a new column: accepted, but the table is rebuilt.
        let Prediction::Measured { verdict, .. } = on_shop(
            "",
            "ALTER TABLE orders ADD coupon VARCHAR(255) NULL, ALGORITHM=INPLACE",
        ) else {
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
    fn statements_in_a_syntax_mysql_rejects_are_unknown_on_any_table() {
        // One of each kind; parse.rs tests every form. Each failed with
        // error 1064 on MySQL 8.0.46 and 8.4.13, run by hand.
        for (sql, expected) in [
            (
                r#"ALTER TABLE orders DROP COLUMN "note""#,
                "a double-quoted column, index or constraint name works only when sql_mode \
                 has ANSI_QUOTES, and kilit does not know the server's sql_mode",
            ),
            (
                "CREATE INDEX i ON orders USING BTREE (amount)",
                "MySQL 8.0.46 and 8.4.13 do not accept USING between the table name and the \
                 column list in CREATE INDEX",
            ),
            (
                "ALTER TABLE orders ADD COLUMN due_on DATE NULL DEFAULT CURRENT_DATE",
                "MySQL 8.0.46 and 8.4.13 do not accept DEFAULT CURRENT_DATE without parentheses",
            ),
            (
                "ALTER TABLE orders ALTER COLUMN amount SET DEFAULT CURRENT_TIMESTAMP",
                "MySQL 8.0.46 and 8.4.13 do not accept SET DEFAULT CURRENT_TIMESTAMP without \
                 parentheses",
            ),
            (
                "ALTER TABLE orders ADD INDEX i (lower(email))",
                "MySQL 8.0.46 and 8.4.13 do not accept the key part lower(email); an \
                 expression in an index needs its own parentheses, and a prefix length is a \
                 whole number",
            ),
        ] {
            // A table from the schema, and the same table created earlier in
            // the input.
            assert_eq!(reason(on_shop("", sql)), expected, "{sql}");
            let created = predictions(&format!("{SHOP}\n{sql}")).pop().unwrap();
            assert_eq!(reason(created), expected, "{sql}");
        }
    }

    #[test]
    fn a_statement_in_a_syntax_mysql_rejects_changes_no_table() {
        let predictions = predictions_with(
            SHOP,
            r#"ALTER TABLE orders DROP COLUMN "note";
               ALTER TABLE orders RENAME TO "orders2";
               ALTER TABLE orders ADD INDEX i (lower(email));
               ALTER TABLE orders MODIFY note VARCHAR(100) NULL;
               ALTER TABLE orders DROP COLUMN email;"#,
        );
        assert!(
            predictions[..3]
                .iter()
                .all(|prediction| matches!(prediction, Prediction::Unknown { .. })),
            "{predictions:?}"
        );
        // note is still there, the table is still orders, and email is in
        // no index.
        assert_eq!(
            measured(predictions[3].clone()).0,
            "lengthen-varchar-past-255-bytes"
        );
        assert_eq!(
            said(&predictions[4]),
            "INSTANT, no rebuild, writes continue"
        );
    }

    #[test]
    fn an_index_statement_kilit_cannot_read_loses_its_table() {
        // MySQL 8.0.46 and 8.4.13 accept each one, run by hand. kilit
        // cannot read them, so it no longer knows which indexes orders has:
        // the FULLTEXT index is a note, as on a table it does not know.
        for unreadable in [
            "CREATE UNIQUE INDEX u_invisible ON orders (email) INVISIBLE",
            "CREATE INDEX i_block ON orders (amount) KEY_BLOCK_SIZE=8",
        ] {
            let predictions = predictions_with(
                SHOP,
                &format!("{unreadable};\nALTER TABLE orders ADD COLUMN coupon INT NULL;"),
            );
            assert!(
                reason(predictions[0].clone()).starts_with("kilit could not read this statement"),
                "{unreadable}"
            );
            let (case, verdict, notes) = likely(predictions[1].clone());
            assert_eq!(
                (case.as_str(), verdict.as_str()),
                ("add-column", "INSTANT, no rebuild, writes continue"),
                "{unreadable}"
            );
            assert_eq!(
                notes,
                [
                    PAGE_RECORD_UNSEEN,
                    "if the table has a FULLTEXT index: COPY, copies every row, writes wait \
                     (add-column-with-fulltext)",
                    "if the table has had 64 instant column changes since it was last \
                     rebuilt: INPLACE, rebuilds the table, writes continue \
                     (add-column-after-64-changes)",
                    "if the row grows past the 65,535 bytes MySQL allows: fails with error 1118 \
                     (add-column-over-row-size-limit)",
                ],
                "{unreadable}"
            );
        }
        // After DROP INDEX ... ON with ALGORITHM, kilit no longer claims the
        // index is there.
        let predictions = predictions_with(
            SHOP,
            "ALTER TABLE orders ADD INDEX ix (amount);
             DROP INDEX ix ON orders ALGORITHM=INPLACE;
             ALTER TABLE orders DROP COLUMN amount;",
        );
        assert!(reason(predictions[1].clone()).starts_with("kilit could not read this statement"));
        let (case, verdict, notes) = likely(predictions[2].clone());
        assert_eq!(
            (case.as_str(), verdict.as_str()),
            ("drop-column", "INSTANT, no rebuild, writes continue")
        );
        assert!(
            notes
                .iter()
                .all(|note| note.starts_with("if ") && !note.contains("ix")),
            "{notes:?}"
        );
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

    // Step B1c: what kilit says on a table it does not know, the five
    // statements it now follows, columns longer than their type, and the
    // shapes the lab has not measured on a table it knows.

    const FULLTEXT_ON_NOTE: &str = "ALTER TABLE orders ADD FULLTEXT INDEX ft_note (note);";
    const ADD_C: &str = "ALTER TABLE orders ADD COLUMN c INT NULL";

    #[test]
    fn on_a_table_kilit_does_not_know_only_a_failure_from_the_statement_stays() {
        // 1674 and 1101 come from the statement; anything else is likely.
        for (sql, fails) in [
            (
                "ALTER TABLE orders ADD COLUMN t CHAR(36) NULL DEFAULT (UUID())",
                true,
            ),
            (
                "ALTER TABLE orders ADD COLUMN t TEXT NULL DEFAULT 'x'",
                true,
            ),
            ("ALTER TABLE orders MODIFY t TEXT NULL DEFAULT 'x'", true),
            ("ALTER TABLE orders ALTER COLUMN t SET DEFAULT 'x'", false),
            ("ALTER TABLE orders RENAME COLUMN a TO b", false),
            ("ALTER TABLE orders DROP COLUMN user_id", false),
            (
                "ALTER TABLE orders ADD INDEX i (note) , ALGORITHM=INSTANT",
                false,
            ),
        ] {
            let prediction = only(sql);
            if fails {
                assert!(said(&prediction).starts_with("fails with error"), "{sql}");
            } else {
                assert!(
                    reason(prediction.clone()).contains("on a plain InnoDB table the lab measured"),
                    "{sql}: {prediction:?}"
                );
            }
        }
    }

    #[test]
    fn a_table_lost_or_dropped_without_a_dump_says_why_kilit_does_not_know_it() {
        let lost = format!("{SHOP}\nALTER TABLE orders CONVERT TO CHARACTER SET latin1;");
        let why = reason(predictions_with(&lost, ADD_C).remove(0));
        assert!(
            why.starts_with("kilit lost track of orders (line ")
                && why.contains(": kilit could not read a statement on it); on a plain InnoDB"),
            "{why}"
        );
        let dropped = predictions(&format!(
            "CREATE TABLE orders (id INT PRIMARY KEY);\nDROP TABLE orders;\n{ADD_C};"
        ))
        .remove(2);
        assert!(
            reason(dropped.clone())
                .starts_with("orders, which the statement on line 2 drops, and no statement"),
            "{dropped:?}"
        );
    }

    #[test]
    fn an_earlier_input_file_is_named_as_given() {
        let first = parse("CREATE TABLE orders (id INT PRIMARY KEY);\nDROP TABLE orders;");
        let second = parse(ADD_C);
        let dump = Schema::from_dump("CREATE TABLE users (id INT PRIMARY KEY);");
        let findings = check_files(
            &[&first, &second],
            &["db/1_drop.sql", "db/2_add.sql"],
            &dump,
            results(),
        );
        assert!(
            reason(findings[2].prediction.clone())
                .contains("the statement on line 2 of db/1_drop.sql drops"),
            "{:?}",
            findings[2].prediction
        );
    }

    #[test]
    fn a_name_with_a_database_next_to_a_table_kilit_has_is_unknown_with_or_without_a_dump() {
        let created = "CREATE TABLE orders (id INT PRIMARY KEY);";
        let prediction =
            predictions(&format!("{created}\nALTER TABLE shop.orders ADD c INT;")).remove(1);
        assert_eq!(
            reason(prediction),
            "shop.orders names the database shop, and kilit has a table orders: kilit does not \
             know the database the statements run in, so it cannot tell whether shop.orders is \
             orders"
        );
        let dump = Schema::from_dump(created);
        let finding = check(
            &parse("ALTER TABLE shop.orders ADD c INT"),
            &dump,
            results(),
        )
        .remove(0);
        assert!(reason(finding.prediction).starts_with(
            "shop.orders names the database shop, and the schema dump has a table orders"
        ));
    }

    #[test]
    fn auto_increment_drop_default_and_engine_innodb_keep_the_table_known() {
        for (statement, why) in [
            (
                "ALTER TABLE orders AUTO_INCREMENT = 5000",
                "setting AUTO_INCREMENT has not been measured yet",
            ),
            (
                "ALTER TABLE orders ALTER COLUMN note DROP DEFAULT",
                "dropping a column's default has not been measured yet",
            ),
            (
                "ALTER TABLE orders ENGINE = 'InnoDB'",
                "changing a table's ENGINE has not been measured yet",
            ),
        ] {
            let predictions = predictions_with(
                &format!("{SHOP}\n{FULLTEXT_ON_NOTE}"),
                &format!("{statement};\n{ADD_C};"),
            );
            assert_eq!(reason(predictions[0].clone()), why, "{statement}");
            // The FULLTEXT index is still there, so the new column copies.
            assert_eq!(case_of(predictions[1].clone()), "add-column", "{statement}");
            assert_eq!(said(&predictions[1]), COPY_ANSWER, "{statement}");
        }
        assert!(
            reason(on_shop(
                "",
                "ALTER TABLE orders ALTER COLUMN nope DROP DEFAULT"
            ))
            .contains("error 1054")
        );
    }

    #[test]
    fn a_column_without_its_default_has_none_after_drop_default() {
        let mut schema = Schema::from_sql(&format!(
            "{SHOP}\nALTER TABLE orders ALTER COLUMN amount SET DEFAULT 0;"
        ));
        schema.apply(&parse("ALTER TABLE orders ALTER COLUMN amount DROP DEFAULT").remove(0));
        let TableState::Known(orders) = schema.table("orders") else {
            panic!("orders should be known");
        };
        assert_eq!(orders.column("amount").unwrap().default, None);
    }

    #[test]
    fn rename_index_renames_it_or_names_the_error_and_keeps_the_table() {
        let setup = "ALTER TABLE orders ADD INDEX i_amount (amount);\n\
                     ALTER TABLE orders ADD INDEX i_note (note);";
        for (rename, error) in [
            ("ALTER TABLE orders RENAME INDEX nope TO x", "1176"),
            ("ALTER TABLE orders RENAME INDEX i_amount TO I_NOTE", "1061"),
            (
                "ALTER TABLE orders RENAME INDEX i_amount TO `PRIMARY`",
                "1280",
            ),
        ] {
            let predictions = predictions_with(
                &format!("{SHOP}\n{setup}"),
                &format!("{rename};\nALTER TABLE orders DROP INDEX i_amount;"),
            );
            assert!(reason(predictions[0].clone()).contains(error), "{rename}");
            // The index kept its name.
            assert_eq!(case_of(predictions[1].clone()), "drop-index", "{rename}");
        }
        let predictions = predictions_with(
            &format!("{SHOP}\n{setup}"),
            "ALTER TABLE orders RENAME KEY i_amount TO by_amount;
             ALTER TABLE orders DROP INDEX by_amount;
             ALTER TABLE orders DROP INDEX i_amount;",
        );
        assert_eq!(
            reason(predictions[0].clone()),
            "renaming an index has not been measured yet"
        );
        assert_eq!(case_of(predictions[1].clone()), "drop-index");
        assert!(reason(predictions[2].clone()).contains("error 1091"));
    }

    #[test]
    fn create_fulltext_index_is_unknown_and_followed_as_the_alter_table_mysql_runs() {
        let predictions = predictions_with(
            SHOP,
            &format!("CREATE FULLTEXT INDEX ft ON orders (note);\n{ADD_C};"),
        );
        assert!(reason(predictions[0].clone()).starts_with("CREATE FULLTEXT INDEX has not been"));
        assert_eq!(said(&predictions[1]), COPY_ANSWER);
        // A taken name or a missing column: MySQL refuses it, no index.
        for create in [
            "CREATE FULLTEXT INDEX amount_ix ON orders (nope)",
            "CREATE FULLTEXT INDEX i_amount ON orders (note)",
        ] {
            let predictions = predictions_with(
                &format!("{SHOP}\nALTER TABLE orders ADD INDEX i_amount (amount);"),
                &format!("{create};\n{ADD_C};"),
            );
            let why = reason(predictions[0].clone());
            assert!(
                why.contains("1072") || why.contains("1061"),
                "{create}: {why}"
            );
            assert_eq!(said(&predictions[1]), INSTANT_ANSWER, "{create}");
        }
    }

    #[test]
    fn a_column_longer_than_its_type_leaves_the_table_or_loses_it_as_mysql_takes_it() {
        let next = "ALTER TABLE orders ADD COLUMN d INT NULL";
        for (session, column, follows) in [
            // Without a strict sql_mode set earlier in the input, the
            // server's or a framework's may not be strict.
            ("", "body VARCHAR(70000) NULL", "lost"),
            (
                "SET sql_mode = 'STRICT_ALL_TABLES';",
                "body VARCHAR(70000) NULL",
                "kept",
            ),
            (
                "SET sql_mode = 'STRICT_TRANS_TABLES';",
                "raw VARBINARY(70000) NULL",
                "kept",
            ),
            ("", "raw VARBINARY(70000) NULL", "lost"),
            ("", "code CHAR(300) NULL", "kept"),
            ("SET sql_mode = '';", "body VARCHAR(70000) NULL", "lost"),
            (
                "SET sql_mode = 'NO_ENGINE_SUBSTITUTION';",
                "raw VARBINARY(70000) NULL",
                "lost",
            ),
            ("SET sql_mode = @saved;", "body VARCHAR(70000) NULL", "lost"),
            ("SET sql_mode = '';", "code CHAR(300) NULL", "kept"),
            ("SET sql_mode = '';", "bin BINARY(300) NULL", "kept"),
        ] {
            for change in ["ADD COLUMN", "MODIFY"] {
                let sql = format!("{session}\nALTER TABLE orders {change} {column};\n{next};");
                let last = predictions_with(SHOP, &sql).pop().unwrap();
                match follows {
                    "kept" => assert_eq!(said(&last), INSTANT_ANSWER, "{sql}"),
                    _ => assert!(
                        reason(last).starts_with("kilit lost track of orders"),
                        "{sql}"
                    ),
                }
            }
        }
        // A CREATE TABLE of such a column: MySQL refuses it, or changes it.
        let last = predictions(
            "CREATE TABLE t (id INT PRIMARY KEY, body VARCHAR(70000));\n\
                                ALTER TABLE t ADD COLUMN d INT NULL;",
        )
        .pop()
        .unwrap();
        assert!(reason(last).starts_with("kilit lost track of t"));
    }

    #[test]
    fn shapes_the_lab_has_not_measured_on_a_known_table_are_unknown() {
        let setup = "ALTER TABLE orders ADD INDEX i_amount (amount);\n\
                     ALTER TABLE orders ADD COLUMN body VARCHAR(1000) NULL;\n\
                     ALTER TABLE orders ADD FULLTEXT INDEX ft_a (note);\n\
                     ALTER TABLE orders ADD FULLTEXT INDEX ft_b (email);\n\
                     ALTER TABLE orders ADD UNIQUE INDEX u_two (amount, note);";
        for (sql, why) in [
            (
                "ALTER TABLE orders ADD COLUMN d DATE NOT NULL",
                "NOT NULL DATE",
            ),
            (
                "ALTER TABLE orders ADD COLUMN d DATETIME(6) NOT NULL",
                "NOT NULL DATETIME",
            ),
            (
                "ALTER TABLE orders ADD COLUMN d TIMESTAMP NOT NULL",
                "NOT NULL TIMESTAMP",
            ),
            ("ALTER TABLE orders ADD INDEX i (nope)", "error 1072"),
            (
                "ALTER TABLE orders ADD UNIQUE INDEX I_AMOUNT (note)",
                "error 1061",
            ),
            ("ALTER TABLE orders ADD INDEX i (body)", "4000 bytes"),
            ("ALTER TABLE orders ADD INDEX i (note(51))", "error 1089"),
            (
                "ALTER TABLE orders DROP INDEX ft_a",
                "several FULLTEXT indexes",
            ),
            (
                "ALTER TABLE orders DROP INDEX u_two",
                "UNIQUE index u_two of several",
            ),
            (
                "ALTER TABLE orders MODIFY id BIGINT UNSIGNED NULL",
                "primary key column id",
            ),
            (
                "ALTER TABLE orders MODIFY note VARCHAR(60) NULL AFTER note",
                "AFTER itself",
            ),
        ] {
            let why_said = reason(on_shop(setup, sql));
            assert!(why_said.contains(why), "{sql}: {why_said}");
        }
        // The controls keep their answers.
        let body = "ALTER TABLE orders ADD COLUMN body VARCHAR(1000) NULL;";
        for sql in [
            "ALTER TABLE orders ADD COLUMN d TIME NOT NULL",
            "ALTER TABLE orders ADD COLUMN d DATE NULL",
            "ALTER TABLE orders ADD COLUMN d DATE NOT NULL DEFAULT '2020-01-01'",
            "ALTER TABLE orders ADD INDEX i (note(50))",
            "ALTER TABLE orders ADD INDEX i (body(768))",
        ] {
            assert!(
                matches!(on_shop(body, sql), Prediction::Measured { .. }),
                "{sql}: {:?}",
                on_shop(body, sql)
            );
        }
    }

    #[test]
    fn an_index_named_as_a_renamed_foreign_key_was_may_be_automatic() {
        // By hand on MySQL 8.0.46 and 8.4.13: after RENAME TABLE the index
        // keeps orders_ibfk_abc, and MySQL still drops it without a word
        // when another index can serve the key.
        let dump = "CREATE TABLE users (id BIGINT UNSIGNED NOT NULL PRIMARY KEY);
            CREATE TABLE orders (id BIGINT UNSIGNED NOT NULL PRIMARY KEY,
                user_id BIGINT UNSIGNED NOT NULL, KEY orders_ibfk_abc (user_id),
                CONSTRAINT orders_ibfk_abc FOREIGN KEY (user_id) REFERENCES users (id));";
        for rename in [
            "RENAME TABLE orders TO purchases",
            "ALTER TABLE orders RENAME TO purchases",
        ] {
            let last = predictions_with(
                dump,
                &format!(
                    "{rename};\nALTER TABLE purchases ADD INDEX ix (user_id);\n\
                     ALTER TABLE purchases DROP INDEX ix;"
                ),
            )
            .pop()
            .unwrap();
            assert!(
                reason(last.clone()).contains("the name MySQL gives the index it makes"),
                "{rename}: {last:?}"
            );
        }
        // Renamed by RENAME INDEX, it is MySQL's no longer: the measured answer.
        let last = predictions_with(
            dump,
            "ALTER TABLE orders RENAME INDEX orders_ibfk_abc TO ix_renamed;
             ALTER TABLE orders ADD INDEX ix (user_id);
             ALTER TABLE orders DROP INDEX ix;",
        )
        .pop()
        .unwrap();
        assert_eq!(case_of(last), "drop-index");
    }

    #[test]
    fn a_foreign_key_whose_bytes_overflow_is_unknown_not_a_failure() {
        let schema = "CREATE TABLE users (id INT PRIMARY KEY,
                          k VARCHAR(4611686018427387904) CHARACTER SET utf8mb4 NOT NULL, UNIQUE KEY (k));
                      CREATE TABLE orders (id INT PRIMARY KEY,
                          k VARCHAR(4611686018427387904) CHARACTER SET utf8mb4 NOT NULL);";
        let why = reason(
            predictions_with(
                schema,
                "ALTER TABLE orders ADD FOREIGN KEY (k) REFERENCES users (k)",
            )
            .remove(0),
        );
        assert!(why.contains("more bytes than kilit can count"), "{why}");
    }

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
            Prediction::Unknown { reason, .. } => format!("unknown: {reason}"),
            Prediction::NotChecked { .. } => "not checked".to_owned(),
            Prediction::DependsOnTable { shared, .. } => {
                format!("depends on the table: {shared:?}")
            }
        }
    }

    #[test]
    fn a_second_fulltext_index_does_not_rebuild_the_table() {
        // The first FULLTEXT index is in the schema dump.
        let dump = "CREATE TABLE posts (id INT PRIMARY KEY, title VARCHAR(100), body TEXT,
                    FULLTEXT KEY posts_title_fulltext (title)) DEFAULT CHARSET=utf8mb4;";
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
    fn without_the_table_a_fulltext_index_makes_writes_wait_with_each_answer_in_a_note() {
        // Every engine and row format the lab measured, and a second
        // FULLTEXT index, make writes wait; a missing column or a taken
        // name fails. So writes wait, and the algorithm depends on the table.
        let notes_for = |column: &str, name: &str| {
            vec![
                "if the table already has a FULLTEXT index: INPLACE, no rebuild, writes wait \
                 (add-second-fulltext-index)"
                    .to_owned(),
                "if the column is TEXT in the binary character set, which MySQL makes a BLOB: \
                 fails with error 1283 (add-fulltext-index-binary-text)"
                    .to_owned(),
                "if the table is MyISAM: COPY, copies every row, writes wait \
                 (myisam-add-fulltext-index and myisam-add-fulltext-index-text)"
                    .to_owned(),
                "if the table is COMPRESSED (ROW_FORMAT=COMPRESSED, or KEY_BLOCK_SIZE=8): \
                 INPLACE, rebuilds the table, writes wait (compressed-add-fulltext-index)"
                    .to_owned(),
                "if the table is COMPACT: INPLACE, rebuilds the table, writes wait \
                 (compact-add-fulltext-index)"
                    .to_owned(),
                "if the table is REDUNDANT: INPLACE, rebuilds the table, writes wait \
                 (redundant-add-fulltext-index)"
                    .to_owned(),
                format!(
                    "if the table has no column {column}: fails with error 1072 (by hand on \
                     MySQL 8.0.46 and 8.4.13)"
                ),
                format!(
                    "if the table has an index named {name} already: fails with error 1061 (by \
                     hand on MySQL 8.0.46 and 8.4.13)"
                ),
                "on another engine, row format or KEY_BLOCK_SIZE, or one of these in a shape the \
                 lab did not measure there: not measured"
                    .to_owned(),
            ]
        };
        let (case, shared, reason, notes) =
            depends(only("ALTER TABLE posts ADD FULLTEXT INDEX ft (body)"));
        assert_eq!((case, shared), ("add-fulltext-index", Shared::WritesWait));
        assert_eq!(
            reason,
            "no --schema was given, so kilit does not know posts; every answer the lab \
             measured, or a hand test showed, for the tables it may be makes writes wait or \
             fails: on a plain InnoDB table INPLACE, rebuilds the table, writes wait \
             (add-fulltext-index)"
        );
        assert_eq!(notes, notes_for("body", "ft"));
        // So on a table kilit lost track of.
        let predictions = predictions_with(
            SHOP,
            "ALTER TABLE orders CONVERT TO CHARACTER SET latin1;
             ALTER TABLE orders ADD FULLTEXT INDEX ft_note (note);",
        );
        let (_, shared, reason, notes) = depends(predictions[1].clone());
        assert_eq!(shared, Shared::WritesWait);
        assert!(reason.starts_with("kilit lost track of orders"), "{reason}");
        assert_eq!(notes, notes_for("note", "ft_note"));
    }

    #[test]
    fn a_fulltext_index_on_one_text_column_is_measured_on_every_engine_and_row_format() {
        let fulltext = "ALTER TABLE posts ADD FULLTEXT INDEX ft (body)";
        let table = |options: &str| {
            format!(
                "CREATE TABLE posts (id INT PRIMARY KEY, title VARCHAR(100), body TEXT, \
                 tags MEDIUMTEXT, raw TEXT CHARACTER SET binary) DEFAULT CHARSET=utf8mb4 \
                 {options};"
            )
        };
        for (options, case, verdict) in [
            (
                "ENGINE=MyISAM",
                "myisam-add-fulltext-index",
                "COPY, copies every row, writes wait",
            ),
            (
                "ROW_FORMAT=COMPRESSED",
                "compressed-add-fulltext-index",
                "INPLACE, rebuilds the table, writes wait",
            ),
            (
                "KEY_BLOCK_SIZE=8",
                "compressed-add-fulltext-index",
                "INPLACE, rebuilds the table, writes wait",
            ),
            (
                "ROW_FORMAT=COMPACT",
                "compact-add-fulltext-index",
                "INPLACE, rebuilds the table, writes wait",
            ),
            (
                "ROW_FORMAT=REDUNDANT",
                "redundant-add-fulltext-index",
                "INPLACE, rebuilds the table, writes wait",
            ),
        ] {
            let schema = table(options);
            let (found, said, _) = measured(predictions_with(&schema, fulltext).remove(0));
            assert_eq!((found, said.as_str()), (case, verdict), "{options}");
            // A type the lab did not measure there, two columns, or a second
            // FULLTEXT index stay unknown.
            // A TEXT column in the binary character set is a BLOB, which
            // MySQL refuses (error 1283, by hand on both servers).
            for sql in [
                "ALTER TABLE posts ADD FULLTEXT INDEX ft (tags)",
                "ALTER TABLE posts ADD FULLTEXT INDEX ft (title, body)",
                "ALTER TABLE posts ADD FULLTEXT INDEX ft (raw)",
            ] {
                let prediction = predictions_with(&schema, sql).remove(0);
                assert!(
                    matches!(prediction, Prediction::Unknown { .. }),
                    "{options}: {sql}: {prediction:?}"
                );
            }
            if case != "myisam-add-fulltext-index" {
                let second = predictions_with(
                    &schema,
                    &format!("ALTER TABLE posts ADD FULLTEXT INDEX ft_title (title);\n{fulltext};"),
                )
                .remove(1);
                assert!(
                    matches!(second, Prediction::Unknown { .. }),
                    "{options}: {second:?}"
                );
            }
        }
        // MyISAM's VARCHAR answer stays.
        let (found, _, _) = measured(
            predictions_with(
                &table("ENGINE=MyISAM"),
                "ALTER TABLE posts ADD FULLTEXT INDEX ft (title)",
            )
            .remove(0),
        );
        assert_eq!(found, "myisam-add-fulltext-index");
    }

    #[test]
    fn without_the_table_answers_that_all_fail_fail_and_one_that_continues_is_unknown() {
        // Rule 3: forced INSTANT fails on every engine and row format measured.
        let (case, shared, why, _) = depends(only(
            "ALTER TABLE posts ADD FULLTEXT INDEX ft (body), ALGORITHM=INSTANT",
        ));
        assert_eq!((case, shared), ("add-fulltext-index", Shared::Fails));
        assert!(
            why.contains("or a hand test showed, for the tables it may be fails"),
            "{why}"
        );
        // A plain index lets writes continue on a plain table: writes
        // continuing is never kept on a table kilit does not know.
        let (case, _, _) = likely(only("ALTER TABLE posts ADD INDEX i (body)"));
        assert_eq!(case, "add-index");
        // Rule 2: a foreign key has shapes kilit cannot see without both
        // tables, and is not measured on COMPACT or COMPRESSED tables.
        let (case, _, _) = likely(only(
            "ALTER TABLE orders ADD CONSTRAINT fk FOREIGN KEY (user_id) REFERENCES users (id)",
        ));
        assert_eq!(case, "add-foreign-key");
        // A CREATE FULLTEXT INDEX is not measured itself.
        let prediction = only("CREATE FULLTEXT INDEX ft ON posts (body)");
        assert!(reason(prediction).contains("CREATE FULLTEXT INDEX itself has not been measured"),);
    }

    /// The lab's tables, as scenarios/tables.sql writes them.
    const LAB_TABLES: &str = include_str!("../../../scenarios/tables.sql");

    #[test]
    fn columns_at_the_limits_of_their_type_and_of_the_row_get_what_mysql_does() {
        // By hand on MySQL 8.0.46 and 8.4.13, on the lab's orders, in strict
        // mode and with sql_mode = '' alike unless said.
        let next = "ALTER TABLE orders ADD COLUMN d INT NULL";
        let strict = "SET sql_mode = 'STRICT_TRANS_TABLES';";
        for (session, column, said_first, then) in [
            // Under the limit of the type: the row is too large (1118).
            (
                "",
                "c VARCHAR(16383) CHARACTER SET utf8mb4 NULL",
                "fails with error 1118",
                "kept",
            ),
            (
                "",
                "c VARCHAR(65535) CHARACTER SET latin1 NULL",
                "fails with error 1118",
                "kept",
            ),
            (
                "",
                "c VARBINARY(65535) NULL",
                "fails with error 1118",
                "kept",
            ),
            // Past it: refused in strict mode (1074), else another type.
            (
                strict,
                "c VARCHAR(16384) CHARACTER SET utf8mb4 NULL",
                "unknown",
                "kept",
            ),
            (
                "",
                "c VARCHAR(16384) CHARACTER SET utf8mb4 NULL",
                "unknown",
                "lost",
            ),
            (
                strict,
                "c VARCHAR(65536) CHARACTER SET latin1 NULL",
                "unknown",
                "kept",
            ),
            (
                "SET sql_mode = '';",
                "c VARCHAR(65536) CHARACTER SET latin1 NULL",
                "unknown",
                "lost",
            ),
            (strict, "c VARBINARY(65536) NULL", "unknown", "kept"),
            ("", "c VARBINARY(65536) NULL", "unknown", "lost"),
            // BINARY holds 255 in every mode.
            (
                "",
                "c BINARY(255) NULL",
                "INSTANT, no rebuild, writes continue",
                "kept",
            ),
            ("", "c BINARY(256) NULL", "unknown", "kept"),
        ] {
            let sql = format!("{session}\nALTER TABLE orders ADD COLUMN {column};\n{next};");
            let mut predictions = predictions_with(LAB_TABLES, &sql);
            let last = predictions.pop().unwrap();
            let first = predictions.pop().unwrap();
            assert!(said(&first).starts_with(said_first), "{sql}: {first:?}");
            match then {
                "kept" => assert_eq!(said(&last), INSTANT_ANSWER, "{sql}"),
                _ => assert!(
                    reason(last).starts_with("kilit lost track of orders"),
                    "{sql}"
                ),
            }
        }
        // CREATE TABLE: MySQL refuses a row past 65,535 bytes (1118) and makes
        // no table; at the limit it takes it.
        for (create, made) in [
            (
                "CREATE TABLE t (c VARCHAR(65535) CHARACTER SET latin1 NULL)",
                false,
            ),
            ("CREATE TABLE t (c VARBINARY(65535) NULL)", false),
            (
                "CREATE TABLE t (c VARCHAR(65533) CHARACTER SET latin1 NULL)",
                false,
            ),
            (
                "CREATE TABLE t (c VARCHAR(65533) CHARACTER SET latin1 NOT NULL)",
                true,
            ),
            (
                "CREATE TABLE t (c VARCHAR(65532) CHARACTER SET latin1 NULL)",
                true,
            ),
            (
                "CREATE TABLE t (c VARCHAR(16383) CHARACTER SET utf8mb4 NULL)",
                true,
            ),
            (
                "CREATE TABLE t (id BIGINT PRIMARY KEY, c VARCHAR(16383) CHARACTER SET utf8mb4)",
                false,
            ),
            ("CREATE TABLE t (c BINARY(255) NULL)", true),
        ] {
            let after = predictions(&format!(
                "{create};\nALTER TABLE t ADD COLUMN x TINYINT NULL;"
            ))
            .pop()
            .unwrap();
            if made {
                assert!(!said(&after).starts_with("unknown"), "{create}: {after:?}");
            } else {
                assert!(
                    reason(after).contains("past the 65535 it allows (error 1118)"),
                    "{create}"
                );
            }
        }
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
            // The lab measured the second column of a primary key of two.
            (
                "ALTER TABLE orders DROP PRIMARY KEY, ADD PRIMARY KEY (amount, id);",
                "ALTER TABLE orders DROP COLUMN amount",
                "amount is in the primary key with other columns, which must stay unique \
                 without it; that depends on the rows, and the lab has measured dropping the \
                 second column of a primary key of two only",
            ),
            (
                "ALTER TABLE orders DROP PRIMARY KEY, ADD PRIMARY KEY (id, user_id, amount);",
                "ALTER TABLE orders DROP COLUMN user_id",
                "user_id is in the primary key with other columns, which must stay unique \
                 without it; that depends on the rows, and the lab has measured dropping the \
                 second column of a primary key of two only",
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
        // A column in a foreign key of its own fails with error 1828: see
        // dropping_a_column_in_a_foreign_key_fails.
        let foreign_key = "ALTER TABLE orders ADD CONSTRAINT orders_user \
                           FOREIGN KEY (user_id) REFERENCES users (id);";
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
        // Not indexed: MySQL refuses to drop a column a generated column
        // uses, as the lab measured.
        assert_eq!(
            fails_with(&on_shop(generated, "ALTER TABLE orders DROP COLUMN amount")),
            Ok(3108)
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
        assert_eq!(evidence, DROP_COLUMN.scenarios);
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
    fn without_the_table_dropping_a_column_lists_what_was_likely() {
        let (case, verdict, notes) = likely(only("ALTER TABLE orders DROP COLUMN amount"));
        assert_eq!(case, "drop-column");
        assert_eq!(verdict, "INSTANT, no rebuild, writes continue");
        assert_eq!(
            notes,
            [
                DROPPED_COLUMN_UNSEEN,
                "if the table keeps a FULLTEXT index: COPY, copies every row, writes wait \
                 (drop-column-with-fulltext)",
                "if the column is in a plain or FULLTEXT index, or has a UNIQUE index of its \
                 own: INPLACE, rebuilds the table, writes continue (drop-column-indexed and 7 \
                 more lab scenarios)",
                "if the column is in an index and the table keeps a FULLTEXT index: COPY, \
                 copies every row, writes wait (drop-column-indexed-with-fulltext and 6 more \
                 lab scenarios)",
                "if the column is in a foreign key: fails with error 1828 \
                 (drop-column-in-foreign-key)",
                "if a generated column uses the column: fails with error 3108 \
                 (drop-column-generated-source and drop-column-stored-generated-source)",
                "if a CHECK constraint uses the column and another column: fails with error \
                 3959 (drop-column-in-two-column-check and \
                 drop-column-in-not-enforced-two-column-check)",
                "if the column is in an index and a CHECK constraint uses it and another \
                 column: fails with error 3959 (drop-column-indexed-in-two-column-check)",
                "if the column is the primary key: COPY, copies every row, writes wait \
                 (drop-primary-key-column)",
                "if the column is the second of a primary key of two columns: INPLACE, rebuilds \
                 the table, writes continue (drop-column-in-two-column-primary-key)",
                "if the column is the second of a primary key of two, and the first holds \
                 duplicate values: fails with error 1062 \
                 (drop-column-in-two-column-primary-key-duplicates)",
                "if the table has no such column: fails with error 1091 (drop-column-missing)",
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
                .enumerate()
                .map(|(at, exception)| (at, *exception, vec![format!("{} holds", exception.when)]))
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
            let (_, decides, why) = deciding(holding(&exceptions)).unwrap();
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
                "ref BIGINT UNSIGNED NOT NULL DEFAULT (UUID_SHORT())",
                "add-column-binlog-unsafe-default",
                "fails with error 1674",
            ),
            (
                "ref BIGINT UNSIGNED NULL DEFAULT (uuid_short( ))",
                "add-column-binlog-unsafe-default",
                "fails with error 1674",
            ),
            (
                "placed_at DATETIME NOT NULL DEFAULT (SYSDATE())",
                "add-column-binlog-unsafe-default",
                "fails with error 1674",
            ),
            (
                "placed_at DATETIME NULL DEFAULT (sysdate())",
                "add-column-binlog-unsafe-default",
                "fails with error 1674",
            ),
            (
                "created_by VARCHAR(288) NOT NULL DEFAULT (USER())",
                "add-column-binlog-unsafe-default",
                "fails with error 1674",
            ),
            (
                "created_by VARCHAR(288) NULL DEFAULT (user( ))",
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
            let (found, said, notes) = measured(on_shop("", &sql));
            assert_eq!(found, case, "{sql}");
            assert!(said.starts_with(verdict), "{sql}: {said}");
            assert_eq!(notes, [ASSUMES_BINARY_LOG], "{sql}");
            // Without the table, 1674 comes from the statement alone; the
            // copy is only likely: the row may grow too long.
            let unseen = only(&sql);
            if verdict.starts_with("fails") {
                assert_eq!(measured(unseen).0, case, "{sql}");
            } else {
                let (found, said, notes) = likely(unseen);
                assert_eq!((found.as_str(), said.as_str()), (case, verdict), "{sql}");
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
            // Next to the measured UUID_SHORT(), SYSDATE() and USER().
            "((SYSDATE()))",
            "(SYSDATE(6))",
            "(UUID_SHORT() + 1)",
            "(CURRENT_USER())",
            "(SESSION_USER())",
            "(SYSTEM_USER())",
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
    fn a_measured_default_must_be_written_without_a_space_before_a_parenthesis() {
        let spaced = "is written with a space or a comment where the lab measured none";
        for written in [
            "(SYSDATE ())",
            "( SYSDATE ( ) )",
            "(SYSDATE /*x*/ ())",
            "(NOW ())",
            "(CURDATE\t())",
            "(uuid_short ())",
            "(JSON_ARRAY ())",
        ] {
            let sql = format!("ALTER TABLE orders ADD COLUMN c DATETIME NULL DEFAULT {written}");
            assert!(reason(only(&sql)).contains(spaced), "{sql}");
        }
        for (written, case) in [
            ("(sysdate( ))", "add-column-binlog-unsafe-default"),
            ("( NOW() )", "add-column-expression-default"),
            (
                "(current_date + interval 1 year)",
                "add-column-expression-default",
            ),
        ] {
            let sql = format!("ALTER TABLE orders ADD COLUMN c DATE NULL DEFAULT {written}");
            assert_eq!(said_case(only(&sql)), case, "{sql}");
        }
        // The word DEFAULT in a comment or a name is not the default.
        let sql = "ALTER TABLE orders ADD COLUMN `default` VARCHAR(288) NOT NULL \
                   COMMENT 'default (x)' DEFAULT (USER())";
        assert_eq!(measured(only(sql)).0, "add-column-binlog-unsafe-default");
        assert_eq!(
            written_default("ADD COLUMN c INT COMMENT 'DEFAULT (1)' DEFAULT (RAND())"),
            Some("(RAND())")
        );
        assert_eq!(written_default("ADD COLUMN c INT DEFAULT 1"), None);
        assert_eq!(
            written_default("ADD COLUMN c INT DEFAULT/**/(RAND())"),
            None
        );
    }

    #[test]
    fn binlog_format_anywhere_earlier_makes_expression_defaults_unknown() {
        for set in [
            "SET SESSION binlog_format = 'STATEMENT';",
            "SET binlog_format = ROW;",
            "SET GLOBAL binlog_format = 'MIXED';",
            "SET @@SESSION.binlog_format = 'STATEMENT', sql_log_bin = 1;",
        ] {
            for column in [
                "t CHAR(36) NULL DEFAULT (UUID())",
                "d DATETIME NULL DEFAULT (SYSDATE())",
                "n DATETIME NOT NULL DEFAULT (NOW())",
            ] {
                let sql = format!("{set}\nALTER TABLE orders ADD COLUMN {column};");
                let predictions = predictions(&sql);
                let Prediction::Unknown { reason, .. } = &predictions[1] else {
                    panic!("{sql}: {:?}", predictions[1]);
                };
                assert!(reason.contains("names binlog_format"), "{sql}: {reason}");
            }
        }
        // A literal default does not depend on it.
        let sql = "SET binlog_format = 'STATEMENT';\n\
                   ALTER TABLE orders ADD COLUMN c INT NULL DEFAULT 1;";
        assert_eq!(likely(predictions(sql).remove(1)).0, "add-column");
    }

    #[test]
    fn the_binary_log_follows_the_session() {
        let uuid = "ALTER TABLE orders ADD COLUMN token CHAR(36) NOT NULL DEFAULT (UUID());";
        let now = "ALTER TABLE orders ADD COLUMN placed_at DATETIME NOT NULL DEFAULT (NOW());";
        let told: Vec<String> = predictions_with(
            SHOP,
            &format!(
                "{uuid} SET sql_log_bin = 0; {uuid} {now} ALTER TABLE orders ADD coupon INT NULL;
             SET sql_log_bin = 1; {uuid} {now}"
            ),
        )
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
            let (case, said, notes) = likely(only(&sql));
            assert_eq!(
                (case.as_str(), said.as_str()),
                ("add-column", "INSTANT, no rebuild, writes continue"),
                "{sql}"
            );
            // The exceptions kilit cannot check without the table, and the
            // limit of a record in a page.
            assert_eq!(
                notes.len(),
                ADD_COLUMN.exceptions.len() + 1,
                "{sql}: {notes:?}"
            );
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
            let (found, said, _) = measured(on_shop("", sql));
            assert_eq!((found, said.as_str()), (case, verdict), "{sql}");
            // Without the table every measured answer of a FULLTEXT index
            // makes writes wait or fails: rule 4.
            if case == "add-fulltext-index" {
                let (found, shared, reason, _) = depends(only(sql));
                assert_eq!((found, shared), (case, Shared::WritesWait), "{sql}");
                assert!(reason.ends_with(&format!("{verdict} ({case})")), "{reason}");
                continue;
            }
            let (found, said, _) = likely(only(sql));
            assert_eq!((found.as_str(), said.as_str()), (case, verdict), "{sql}");
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

    /// For each statement of `sql` on MySQL 8.4.13, with the tables of
    /// `schema`: whether its answer may depend on the server.
    fn server_dependent(schema: &str, sql: &str) -> Vec<bool> {
        check(&parse(sql), &Schema::from_sql(schema), results())
            .iter()
            .map(|finding| finding.server_dependent)
            .collect()
    }

    #[test]
    fn answers_from_results_every_server_shares_do_not_depend_on_the_server() {
        let sql = "ALTER TABLE users ADD COLUMN nick VARCHAR(50) NULL;
                   ALTER TABLE users DROP COLUMN nick;
                   ALTER TABLE users MODIFY name VARCHAR(300) NOT NULL;
                   CREATE TABLE coupons (id BIGINT UNSIGNED NOT NULL PRIMARY KEY);
                   SET foreign_key_checks = 0;
                   ALTER TABLE users ADD COLUMN code CHAR(8) NULL, ALGORITHM=INPLACE;";
        assert_eq!(server_dependent(LARAVEL_TABLES, sql), [false; 6]);
        // Forcing an algorithm under old_alter_table reads the twin runs.
        let sql = "SET old_alter_table = ON;
                   ALTER TABLE users ADD COLUMN code CHAR(8) NULL, ALGORITHM=INPLACE;";
        assert_eq!(server_dependent(LARAVEL_TABLES, sql), [false; 2]);
    }

    #[test]
    fn a_foreign_key_the_servers_judge_differently_depends_on_the_server() {
        let schema = "CREATE TABLE users (id BIGINT UNSIGNED NOT NULL, name VARCHAR(50) NOT NULL,
                          PRIMARY KEY (id), KEY users_name_index (name))
                          DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_unicode_ci;
                      CREATE TABLE orders (id BIGINT UNSIGNED NOT NULL,
                          user_id BIGINT UNSIGNED NOT NULL, user_name VARCHAR(50) NOT NULL,
                          PRIMARY KEY (id)) DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_unicode_ci;";
        // To the primary key: the case's own scenarios, the same on every
        // server. To a plain index: 8.0.46 copies the table, 8.4.13 fails
        // with error 6125, also the second time, from what is remembered.
        let sql = "ALTER TABLE orders ADD FOREIGN KEY (user_id) REFERENCES users (id);
                   ALTER TABLE orders ADD FOREIGN KEY (user_name) REFERENCES users (name);
                   ALTER TABLE orders ADD FOREIGN KEY (user_name) REFERENCES users (name);";
        assert_eq!(server_dependent(schema, sql), [false, true, true]);
        // Without the tables, those results are read for the notes.
        let sql = "ALTER TABLE orders ADD FOREIGN KEY (user_id) REFERENCES users (id);";
        assert_eq!(server_dependent("", sql), [true]);
    }

    #[test]
    fn naming_restrict_fk_on_non_standard_key_makes_every_later_answer_depend_on_the_server() {
        // 8.0.46 has no such variable and refuses the SET; 8.4.13 has it.
        let sql = "ALTER TABLE users ADD COLUMN a INT NULL;
                   SET restrict_fk_on_non_standard_key = OFF;
                   ALTER TABLE users ADD COLUMN b INT NULL;";
        assert_eq!(server_dependent(LARAVEL_TABLES, sql), [false, true, true]);
        let sql = "SELECT @@restrict_fk_on_non_standard_key;
                   ALTER TABLE users ADD COLUMN b INT NULL;";
        assert_eq!(server_dependent(LARAVEL_TABLES, sql), [true, true]);
    }

    #[test]
    fn a_prefix_index_in_a_schema_dump_counts_for_its_column() {
        // As mysqldump and SHOW CREATE TABLE write it, with backticks.
        let dump = "CREATE TABLE `orders` (
              `id` bigint unsigned NOT NULL AUTO_INCREMENT,
              `note` varchar(50) DEFAULT NULL,
              PRIMARY KEY (`id`),
              KEY `orders_note_index` (`note`(10))
            ) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_unicode_ci;";
        let on_dump = |sql| predictions_with(dump, sql).pop().unwrap();
        let (case, verdict, notes) = measured(on_dump("ALTER TABLE orders DROP COLUMN note"));
        assert_eq!(
            (case, verdict.as_str()),
            (
                "drop-column",
                "INPLACE, rebuilds the table, writes continue"
            )
        );
        assert_eq!(notes, ["note is in the index orders_note_index"]);
        let (case, verdict, notes) = measured(on_dump(
            "ALTER TABLE orders MODIFY note VARCHAR(50) COLLATE utf8mb4_bin NULL",
        ));
        assert_eq!(
            (case, verdict.as_str()),
            (
                "change-column-collation",
                "COPY, copies every row, writes wait"
            )
        );
        assert_eq!(notes[1], "note is in the index orders_note_index");
    }

    /// Hand-written tables for [`three_ways`]: a prefix index, a plain one,
    /// a functional one, a generated column, a foreign key, a FULLTEXT index
    /// and a prefix in a primary key.
    const NAMES_THREE_WAYS: &str = "
        CREATE TABLE users ({id} BIGINT UNSIGNED NOT NULL, PRIMARY KEY ({id}))
            DEFAULT CHARSET=utf8mb4;
        CREATE TABLE orders (
            {id} BIGINT UNSIGNED NOT NULL,
            {user_id} BIGINT UNSIGNED NOT NULL,
            {amount} INT NOT NULL,
            {amount2} INT AS ({amount} * 2) VIRTUAL,
            {email} VARCHAR(255) NULL,
            {note} VARCHAR(50) NULL,
            PRIMARY KEY ({id}),
            KEY {note} ({note}(10)),
            KEY {ix_amount} ({amount}),
            KEY {f_email} ((lower({email}))),
            CONSTRAINT {fk_user} FOREIGN KEY ({user_id}) REFERENCES users ({id})
        ) DEFAULT CHARSET=utf8mb4;
        CREATE TABLE posts ({id} INT NOT NULL, {title} VARCHAR(100) NULL, {body} TEXT NULL,
            PRIMARY KEY ({id}), FULLTEXT KEY {ft_body} ({body})) DEFAULT CHARSET=utf8mb4;
        CREATE TABLE codes ({code} VARCHAR(50) NOT NULL, {label} VARCHAR(20) NULL,
            PRIMARY KEY ({code}(10))) DEFAULT CHARSET=utf8mb4;";

    #[test]
    fn answers_do_not_depend_on_quotes_or_case_in_the_schema_dump() {
        // Each name lookup, with the tables written as they are, in
        // backticks and in capitals. A reason names an index or a key as
        // the dump writes it, so the comparison ignores case.
        for (sql, expected) in [
            // The index a column is in: a prefix, then the collation.
            (
                "ALTER TABLE orders DROP COLUMN note",
                "INPLACE, rebuilds the table, writes continue",
            ),
            (
                "ALTER TABLE orders MODIFY note VARCHAR(50) COLLATE utf8mb4_bin NULL",
                "COPY, copies every row, writes wait",
            ),
            // The column itself.
            (
                "ALTER TABLE orders MODIFY note VARCHAR(60) NULL",
                "INPLACE, no rebuild, writes continue",
            ),
            // An expression in an index, and a generated column.
            (
                "ALTER TABLE orders DROP COLUMN email",
                "unknown: email is used by the functional index f_email, and dropping such \
                 a column has not been measured yet",
            ),
            (
                "ALTER TABLE orders DROP COLUMN amount",
                "unknown: the generated column amount2 uses amount, and dropping such a \
                 column has not been measured yet",
            ),
            // A foreign key, from both ends.
            (
                "ALTER TABLE orders MODIFY user_id INT UNSIGNED NOT NULL",
                "fails with error 3780",
            ),
            (
                "ALTER TABLE users DROP COLUMN id",
                "unknown: id is referenced by the foreign key fk_user on orders, and \
                 dropping such a column has not been measured yet",
            ),
            // The column of a functional index.
            (
                "ALTER TABLE orders ADD INDEX f2 ((LOWER(email)))",
                "INPLACE, no rebuild, writes wait",
            ),
            // A FULLTEXT index that stays, and one that goes with its column.
            (
                "ALTER TABLE posts ADD COLUMN c INT NULL",
                "COPY, copies every row, writes wait",
            ),
            (
                "ALTER TABLE posts DROP COLUMN body",
                "INPLACE, rebuilds the table, writes continue",
            ),
            // A prefix in the primary key: the column is in it, and NOT NULL.
            (
                "ALTER TABLE codes DROP COLUMN code",
                "unknown: code is in the primary key with a prefix length, and dropping such a \
                 column has not been measured yet",
            ),
            (
                "ALTER TABLE codes MODIFY code VARCHAR(50)",
                "INSTANT, no rebuild, writes continue",
            ),
        ] {
            for dump in three_ways(NAMES_THREE_WAYS) {
                let told = said(&predictions_with(&dump, sql).pop().unwrap());
                assert!(
                    told.to_lowercase().starts_with(&expected.to_lowercase()),
                    "{sql}: {told}\n{dump}"
                );
            }
        }
    }

    /// [`SHOP`]'s orders with a column of each TEXT and BLOB type, and of
    /// the types kilit keeps next to them.
    const TEXT_COLUMNS: &str = "
        ALTER TABLE orders ADD COLUMN body TEXT NULL;
        ALTER TABLE orders ADD COLUMN tiny TINYTEXT NULL;
        ALTER TABLE orders ADD COLUMN medium MEDIUMTEXT NULL;
        ALTER TABLE orders ADD COLUMN long_text LONGTEXT NULL;
        ALTER TABLE orders ADD COLUMN data BLOB NULL;
        ALTER TABLE orders ADD COLUMN tiny_data TINYBLOB NULL;
        ALTER TABLE orders ADD COLUMN medium_data MEDIUMBLOB NULL;
        ALTER TABLE orders ADD COLUMN long_data LONGBLOB NULL;
        ALTER TABLE orders ADD COLUMN sized TEXT(1000) NULL;
        ALTER TABLE orders ADD COLUMN sized_data BLOB(100) NULL;
        ALTER TABLE orders ADD COLUMN old_text LONG NULL;
        ALTER TABLE orders ADD COLUMN code CHAR(36) NULL;
        ALTER TABLE orders ADD COLUMN state ENUM('new', 'paid') NULL;
        ALTER TABLE orders ADD COLUMN flags SET('gift', 'rush') NULL;
        ALTER TABLE orders ADD COLUMN raw VARBINARY(100) NULL;
        ALTER TABLE orders ADD COLUMN doc JSON NULL;";

    /// The error of a measured failure, or what else the prediction is.
    fn fails_with(prediction: &Prediction) -> Result<u16, String> {
        match prediction {
            Prediction::Measured {
                verdict: Verdict::Fails { code, .. },
                ..
            } => Ok(*code),
            other => Err(said(other)),
        }
    }

    #[test]
    fn an_index_that_takes_a_text_or_blob_column_whole_fails() {
        for column in ["body", "tiny", "medium", "long_text", "data"] {
            for (index, case) in [("INDEX", &ADD_INDEX), ("UNIQUE INDEX", &ADD_UNIQUE_INDEX)] {
                let sql = format!("ALTER TABLE orders ADD {index} i ({column})");
                let prediction = on_shop(TEXT_COLUMNS, &sql);
                assert_eq!(fails_with(&prediction), Ok(1170), "{sql}");
                let Prediction::Measured {
                    case: found,
                    evidence,
                    notes,
                    ..
                } = prediction
                else {
                    unreachable!();
                };
                assert_eq!(found, case.id, "{sql}");
                assert_eq!(evidence, case.exceptions.last().unwrap().scenarios, "{sql}");
                assert_eq!(notes.len(), 1, "{sql}");
                assert!(
                    notes[0].starts_with(&format!("{column} is ")),
                    "{sql}: {notes:?}"
                );
            }
        }
        // A TEXT column after another one, as the lab measured it.
        let prediction = on_shop(
            TEXT_COLUMNS,
            "ALTER TABLE orders ADD INDEX i (user_id, body)",
        );
        assert_eq!(fails_with(&prediction), Ok(1170));
        let (_, _, notes) = measured(prediction);
        assert_eq!(
            notes,
            ["body is TEXT, and the index takes it without a prefix length"]
        );
        // MySQL refuses it before it looks at the algorithm.
        assert_eq!(
            fails_with(&on_shop(
                TEXT_COLUMNS,
                "ALTER TABLE orders ADD INDEX i (body), ALGORITHM=INSTANT"
            )),
            Ok(1170)
        );
    }

    #[test]
    fn text_and_blob_index_shapes_the_lab_has_not_measured_are_unknown() {
        for (column, data_type) in [
            ("tiny_data", "TINYBLOB"),
            ("medium_data", "MEDIUMBLOB"),
            ("long_data", "LONGBLOB"),
            ("sized", "TEXT(1000)"),
            ("sized_data", "BLOB(100)"),
            ("old_text", "LONG"),
        ] {
            for index in ["INDEX", "UNIQUE INDEX"] {
                let sql = format!("ALTER TABLE orders ADD {index} i ({column})");
                assert_eq!(
                    reason(on_shop(TEXT_COLUMNS, &sql)),
                    format!(
                        "{column} is {data_type}, and an index on such a column without a \
                         prefix length has not been measured yet"
                    ),
                    "{sql}"
                );
            }
        }
        for index in [
            "UNIQUE INDEX i (user_id, body)",
            "INDEX i (body, user_id)",
            "INDEX i (amount, user_id, body)",
            "INDEX i (body, long_text)",
            "INDEX i (long_text(10), body)",
            "INDEX i (missing, body)",
        ] {
            let sql = format!("ALTER TABLE orders ADD {index}");
            assert_eq!(
                reason(on_shop(TEXT_COLUMNS, &sql)),
                "body is TEXT, and an index of several columns that takes it without a prefix \
                 length has not been measured yet; the lab measured a TEXT column after one \
                 other column, in a plain index",
                "{sql}"
            );
        }
    }

    #[test]
    fn indexes_on_other_types_and_prefixes_keep_their_answer() {
        // CHAR, ENUM and SET are kept with TEXT, and VARBINARY with BLOB,
        // but they are none of them; a prefix length is what MySQL asks for.
        for columns in [
            "code",
            "state",
            "flags",
            "raw",
            "email",
            "body(10)",
            "data(10)",
            "user_id, body(10)",
        ] {
            let index = on_shop(
                TEXT_COLUMNS,
                &format!("ALTER TABLE orders ADD INDEX i ({columns})"),
            );
            assert_eq!(
                on_shop("", "ALTER TABLE orders ADD INDEX i (amount)"),
                index
            );
            let unique = on_shop(
                TEXT_COLUMNS,
                &format!("ALTER TABLE orders ADD UNIQUE INDEX i ({columns})"),
            );
            assert_eq!(
                on_shop("", "ALTER TABLE orders ADD UNIQUE INDEX i (amount)"),
                unique,
                "{columns}"
            );
        }
        // A column the table does not have: MySQL refuses it (1072, by hand).
        assert!(
            reason(on_shop(
                TEXT_COLUMNS,
                "ALTER TABLE orders ADD INDEX i (missing)"
            ))
            .contains("error 1072")
        );
        // FULLTEXT takes TEXT whole, and LOWER() of a VARCHAR column keeps
        // the answer of step 4.
        assert_eq!(
            said(&on_shop(
                TEXT_COLUMNS,
                "ALTER TABLE orders ADD FULLTEXT INDEX ft (body)"
            )),
            "INPLACE, rebuilds the table, writes wait"
        );
        let (case, ..) = measured(on_shop(
            TEXT_COLUMNS,
            "ALTER TABLE orders ADD INDEX i ((LOWER(email)))",
        ));
        assert_eq!(case, "add-functional-index");
    }

    #[test]
    fn an_index_on_a_json_column_is_unknown() {
        // MySQL refuses it with error 3152, which the lab has not measured:
        // neither the answer of an index it accepts, nor the error 1170.
        for index in [
            "INDEX i (doc)",
            "UNIQUE INDEX i (doc)",
            "INDEX i (user_id, doc)",
        ] {
            assert_eq!(
                reason(on_shop(
                    TEXT_COLUMNS,
                    &format!("ALTER TABLE orders ADD {index}")
                )),
                "doc is a JSON column, and an index on a JSON column has not been measured yet",
                "{index}"
            );
        }
    }

    #[test]
    fn without_the_table_an_index_on_text_is_a_note() {
        let note = |case: &Case| {
            format!(
                "{BLOB_OR_TEXT_WITHOUT_PREFIX}: fails with error 1170 ({} and {} more lab \
                 scenarios)",
                case.exceptions.last().unwrap().scenarios[0],
                case.exceptions.last().unwrap().scenarios.len() - 1
            )
        };
        let (_, verdict, notes) = likely(only("ALTER TABLE posts ADD INDEX i (body)"));
        assert_eq!(verdict, "INPLACE, no rebuild, writes continue");
        assert_eq!(notes, [JSON_INDEX_UNSEEN.to_owned(), note(&ADD_INDEX)]);
        let (_, verdict, notes) = likely(only("ALTER TABLE posts ADD UNIQUE INDEX i (body)"));
        assert_eq!(verdict, "INPLACE, no rebuild, writes continue");
        assert_eq!(
            notes,
            [
                JSON_INDEX_UNSEEN.to_owned(),
                "if the columns hold duplicate values: fails with error 1062 \
                 (add-unique-index-duplicates)"
                    .to_owned(),
                note(&ADD_UNIQUE_INDEX)
            ]
        );
        // Nor on a table kilit lost track of.
        let predictions = predictions_with(
            &format!("{SHOP}\n{TEXT_COLUMNS}"),
            "ALTER TABLE orders CONVERT TO CHARACTER SET latin1;
             ALTER TABLE orders ADD INDEX i (body);",
        );
        let (_, verdict, notes) = likely(predictions[1].clone());
        assert_eq!(verdict, "INPLACE, no rebuild, writes continue");
        assert_eq!(notes, [JSON_INDEX_UNSEEN.to_owned(), note(&ADD_INDEX)]);
    }

    #[test]
    fn a_text_index_fails_on_a_table_created_in_the_same_input() {
        // Schema::create('tags', ...) with $table->text('name')->unique():
        // Laravel adds the index in its own statement.
        let statements = parse(
            "create table `tags` (`id` bigint unsigned not null auto_increment primary key, \
             `name` text not null) default character set utf8mb4 collate 'utf8mb4_unicode_ci';
             alter table `tags` add unique `tags_name_unique`(`name`);",
        );
        let findings = check(&statements, &Schema::new(), results());
        assert_eq!(fails_with(&findings[1].prediction), Ok(1170));
        assert_eq!(
            crate::impact::impacts(&statements, &findings),
            [
                crate::impact::Impact::NotChecked,
                crate::impact::Impact::Fails
            ]
        );
    }

    #[test]
    fn dropping_a_column_in_a_foreign_key_fails() {
        let foreign_key = "ALTER TABLE orders ADD CONSTRAINT orders_user \
                           FOREIGN KEY (user_id) REFERENCES users (id);";
        let prediction = on_shop(foreign_key, "ALTER TABLE orders DROP COLUMN user_id");
        assert_eq!(fails_with(&prediction), Ok(1828));
        let Prediction::Measured {
            case,
            evidence,
            notes,
            ..
        } = prediction
        else {
            unreachable!();
        };
        assert_eq!(case, "drop-column");
        assert_eq!(evidence, ["drop-column-in-foreign-key"]);
        assert_eq!(
            notes,
            [
                "user_id is in the index orders_user".to_owned(),
                format!("user_id is in the foreign key orders_user; {ASSUMES_FOREIGN_KEY_CHECKS}")
            ]
        );
        // With the checks on as the lab ran them, and in any variant.
        for (session, sql) in [
            (
                "SET foreign_key_checks = 1;",
                "ALTER TABLE orders DROP COLUMN user_id",
            ),
            (
                "SET foreign_key_checks = ON;",
                "ALTER TABLE orders DROP COLUMN user_id",
            ),
            (
                "",
                "ALTER TABLE orders DROP COLUMN user_id, ALGORITHM=INSTANT",
            ),
            ("", "ALTER TABLE orders DROP COLUMN user_id, ALGORITHM=COPY"),
        ] {
            let predictions = predictions_with(
                &format!("{SHOP}\n{foreign_key}"),
                &format!("{session}\n{sql}"),
            );
            assert_eq!(
                fails_with(predictions.last().unwrap()),
                Ok(1828),
                "{session} {sql}"
            );
        }
        // On a table kilit lost track of, the failure is a note.
        let predictions = predictions_with(
            &format!("{SHOP}\n{foreign_key}"),
            "ALTER TABLE orders CONVERT TO CHARACTER SET latin1;
             ALTER TABLE orders DROP COLUMN user_id;",
        );
        let (_, verdict, notes) = likely(predictions[1].clone());
        assert_eq!(verdict, "INSTANT, no rebuild, writes continue");
        assert!(
            notes.contains(
                &"if the column is in a foreign key: fails with error 1828 \
                  (drop-column-in-foreign-key)"
                    .to_owned()
            ),
            "{notes:?}"
        );
    }

    #[test]
    fn dropping_a_foreign_key_column_the_lab_has_not_measured_is_unknown() {
        let foreign_key = "ALTER TABLE orders ADD CONSTRAINT orders_user \
                           FOREIGN KEY (user_id) REFERENCES users (id);";
        let drop = "ALTER TABLE orders DROP COLUMN user_id";
        let checks_off = "user_id is in the foreign key orders_user, and the lab has not \
                          measured changing such a column with foreign_key_checks off";
        for session in [
            "SET foreign_key_checks = 0;",
            "SET FOREIGN_KEY_CHECKS=0;",
            "SET foreign_key_checks = OFF;",
            "SET foreign_key_checks = 'maybe';",
        ] {
            let predictions = predictions_with(
                &format!("{SHOP}\n{foreign_key}"),
                &format!("{session}\n{drop}"),
            );
            assert_eq!(
                reason(predictions.last().unwrap().clone()),
                checks_off,
                "{session}"
            );
        }
        // A foreign key of two columns, on either of them.
        let pair = "CREATE TABLE pairs (a BIGINT UNSIGNED NOT NULL, b INT NOT NULL, \
                    PRIMARY KEY (a, b));
                    ALTER TABLE orders ADD CONSTRAINT orders_pair \
                    FOREIGN KEY (user_id, amount) REFERENCES pairs (a, b);";
        for column in ["user_id", "amount"] {
            assert_eq!(
                reason(on_shop(
                    pair,
                    &format!("ALTER TABLE orders DROP COLUMN {column}")
                )),
                format!(
                    "{column} is in the foreign key orders_pair with other columns, and dropping \
                     such a column has not been measured yet"
                )
            );
        }
        // The column another table's foreign key points at.
        assert_eq!(
            reason(on_shop(foreign_key, "ALTER TABLE users DROP COLUMN id")),
            "id is referenced by the foreign key orders_user on orders, and dropping such a \
             column has not been measured yet"
        );
        // The foreign key and its column in one statement.
        assert!(
            reason(on_shop(
                foreign_key,
                "ALTER TABLE orders DROP FOREIGN KEY orders_user, DROP COLUMN user_id"
            ))
            .starts_with("2 changes in one statement.")
        );
        // A FULLTEXT index on the table too: measured apart, not together.
        assert!(
            reason(on_shop(
                &format!("{foreign_key} ALTER TABLE orders ADD FULLTEXT INDEX ft (note);"),
                drop
            ))
            .ends_with(": 4 measured situations apply, and the lab has not measured them together")
        );
        // kilit follows a DROP INDEX that MySQL refuses for the foreign
        // key's index (error 1553, not measured yet), so it no longer knows
        // the index the lab measured the key with.
        assert_eq!(
            reason(on_shop(
                &format!("{foreign_key} ALTER TABLE orders DROP INDEX orders_user;"),
                drop
            )),
            "user_id is in the foreign key orders_user but in no index kilit knows of, and \
             dropping such a column has not been measured yet"
        );
    }

    #[test]
    fn the_exception_that_includes_the_others_decides_in_any_order() {
        // Dropping a column in a foreign key: the exceptions for an indexed
        // column and for a foreign key column both hold, and the second was
        // measured with the first's condition too.
        let indexed = &DROP_COLUMN.exceptions[1];
        let foreign_key = &DROP_COLUMN.exceptions[3];
        assert_eq!(indexed.conditions, [Condition::IndexedColumn]);
        assert!(includes(foreign_key, indexed) && !includes(indexed, foreign_key));
        for holding in [
            vec![
                (1, indexed, vec!["i".to_owned()]),
                (3, foreign_key, vec!["f".to_owned()]),
            ],
            vec![
                (3, foreign_key, vec!["f".to_owned()]),
                (1, indexed, vec!["i".to_owned()]),
            ],
        ] {
            let (at, decides, why) = deciding(holding).unwrap();
            assert_eq!(at, 3);
            assert_eq!(decides.scenarios, ["drop-column-in-foreign-key"]);
            assert_eq!(why, ["f"]);
        }
        // Both ways in the tables: the foreign key column fails, and an
        // indexed column outside any foreign key gets the indexed answer.
        let foreign_key = "ALTER TABLE orders ADD CONSTRAINT orders_user \
                           FOREIGN KEY (user_id) REFERENCES users (id);
                           ALTER TABLE orders ADD INDEX idx_amount (amount);";
        assert_eq!(
            fails_with(&on_shop(
                foreign_key,
                "ALTER TABLE orders DROP COLUMN user_id"
            )),
            Ok(1828)
        );
        let Prediction::Measured {
            verdict, evidence, ..
        } = on_shop(foreign_key, "ALTER TABLE orders DROP COLUMN amount")
        else {
            panic!("expected a measured prediction");
        };
        assert_eq!(
            verdict.to_string(),
            "INPLACE, rebuilds the table, writes continue"
        );
        assert_eq!(evidence, DROP_COLUMN.exceptions[1].scenarios);
    }

    #[test]
    fn after_dropping_its_foreign_key_a_column_and_its_index_drop_as_measured() {
        let foreign_key = "ALTER TABLE orders ADD CONSTRAINT orders_user \
                           FOREIGN KEY (user_id) REFERENCES users (id);";
        // The index MySQL made for the key stays, and goes with its column.
        let predictions = predictions_with(
            &format!("{SHOP}\n{foreign_key}"),
            "ALTER TABLE orders DROP FOREIGN KEY orders_user;
             ALTER TABLE orders DROP COLUMN user_id;",
        );
        let Prediction::Measured {
            verdict, evidence, ..
        } = &predictions[1]
        else {
            panic!("expected a measured prediction: {predictions:?}");
        };
        assert_eq!(
            verdict.to_string(),
            "INPLACE, rebuilds the table, writes continue"
        );
        assert!(evidence.contains(&"drop-column-after-dropping-its-foreign-key"));
        // Or the index goes on its own.
        let predictions = predictions_with(
            &format!("{SHOP}\n{foreign_key}"),
            "ALTER TABLE orders DROP FOREIGN KEY orders_user;
             ALTER TABLE orders DROP INDEX orders_user;",
        );
        let (case, verdict, _) = measured(predictions[1].clone());
        assert_eq!(case, "drop-index");
        assert_eq!(verdict, "INPLACE, no rebuild, writes continue");
    }

    #[test]
    fn failing_index_and_column_drops_leave_the_tables_as_they_were() {
        // The TEXT index is not there afterwards: dropping its column does
        // not rebuild the table.
        let predictions = predictions_with(
            &format!("{SHOP}\n{TEXT_COLUMNS}"),
            "ALTER TABLE orders ADD INDEX i (body);
             ALTER TABLE orders DROP COLUMN body;",
        );
        assert_eq!(fails_with(&predictions[0]), Ok(1170));
        assert_eq!(
            said(&predictions[1]),
            "INSTANT, no rebuild, writes continue"
        );
        // The column and its foreign key are still there: changing the
        // column's type fails as for any foreign key column.
        let predictions = predictions_with(
            &format!(
                "{SHOP}\nALTER TABLE orders ADD CONSTRAINT orders_user \
                 FOREIGN KEY (user_id) REFERENCES users (id);"
            ),
            "ALTER TABLE orders DROP COLUMN user_id;
             ALTER TABLE orders MODIFY user_id INT UNSIGNED NOT NULL;",
        );
        assert_eq!(fails_with(&predictions[0]), Ok(1828));
        assert_eq!(fails_with(&predictions[1]), Ok(3780));
    }

    const FOREIGN_KEY: &str = "ALTER TABLE orders ADD CONSTRAINT fk_orders_user \
                               FOREIGN KEY (user_id) REFERENCES users (id);";
    const OWN_INDEX: &str = "ALTER TABLE orders ADD INDEX idx_user (user_id);";
    const SECOND_INDEX: &str = "ALTER TABLE orders ADD INDEX idx_user_amount (user_id, amount);";

    /// `users` and `orders` as the lab has them, with the column a key needs.
    const KEY_TABLES: &str = "
        CREATE TABLE users (id BIGINT UNSIGNED AUTO_INCREMENT PRIMARY KEY,
            name VARCHAR(100) NOT NULL) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4;
        CREATE TABLE orders (id BIGINT UNSIGNED AUTO_INCREMENT PRIMARY KEY,
            user_id BIGINT UNSIGNED NOT NULL, amount INT NOT NULL,
            user_name VARCHAR(100) NULL) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4;";

    const NAME_KEY: &str = "ALTER TABLE orders ADD CONSTRAINT fk_orders_user_name \
                            FOREIGN KEY (user_name) REFERENCES users (name)";

    /// What each statement gets on MySQL 8.0.46 and on 8.4.13.
    fn on_both_servers(schema: &str, sql: &str) -> [Vec<String>; 2] {
        ["mysql-8.0.46", "mysql-8.4.13"].map(|server| {
            let results = Knowledge::built_in().server(server).unwrap();
            check(&parse(sql), &Schema::from_sql(schema), results)
                .into_iter()
                .map(|finding| said(&finding.prediction))
                .collect()
        })
    }

    /// The key on `users (name)` after `index`, on each server.
    fn name_key_after(index: &str) -> [String; 2] {
        let schema = format!("{KEY_TABLES} {index}");
        on_both_servers(&schema, NAME_KEY).map(|mut said| said.remove(0))
    }

    #[test]
    fn a_unique_descending_index_serves_a_new_foreign_key_on_both_servers() {
        for index in [
            "ALTER TABLE users ADD UNIQUE INDEX uq (name DESC);",
            "ALTER TABLE users ADD UNIQUE INDEX uq (`name` DESC);",
            "ALTER TABLE users ADD UNIQUE INDEX uq (`NAME` DESC);",
            "CREATE UNIQUE INDEX uq ON users (NAME DESC);",
            "ALTER TABLE users ADD UNIQUE INDEX uq (name ASC);",
        ] {
            assert_eq!(name_key_after(index), [COPY_ANSWER, COPY_ANSWER], "{index}");
        }
    }

    #[test]
    fn a_plain_descending_index_gets_each_servers_measured_answer() {
        for index in [
            "ALTER TABLE users ADD INDEX i (name DESC);",
            "ALTER TABLE users ADD INDEX i (`name` DESC);",
            "ALTER TABLE users ADD INDEX i (`NAME` DESC);",
            "CREATE INDEX i ON users (NAME DESC);",
        ] {
            let [mysql_80, mysql_84] = name_key_after(index);
            assert_eq!(mysql_80, COPY_ANSWER, "{index}");
            assert!(mysql_84.contains("6125"), "{index}: {mysql_84}");
        }
    }

    #[test]
    fn a_shorter_part_of_the_column_serves_no_foreign_key() {
        for index in [
            "ALTER TABLE users ADD INDEX i (name(10));",
            "ALTER TABLE users ADD UNIQUE INDEX i (`NAME`(10) DESC);",
        ] {
            let [mysql_80, mysql_84] = name_key_after(index);
            assert!(mysql_80.contains("1822"), "{index}: {mysql_80}");
            assert!(mysql_84.contains("6125"), "{index}: {mysql_84}");
        }
    }

    #[test]
    fn indexes_the_lab_has_not_measured_under_a_key_get_no_answer() {
        for index in [
            // As long as the column, and longer than it.
            "ALTER TABLE users ADD UNIQUE INDEX i (name(100));",
            "ALTER TABLE users ADD INDEX i (name(200));",
            "ALTER TABLE users ADD FULLTEXT INDEX i (name);",
            "ALTER TABLE users ADD INDEX i ((lower(name)));",
            "ALTER TABLE users ADD INDEX i (name, id, id);",
            "ALTER TABLE users ADD INDEX i (name(10), id);",
            // Several indexes without a unique one.
            "ALTER TABLE users ADD INDEX i (name); ALTER TABLE users ADD INDEX j (name, id);",
            "ALTER TABLE users ADD INDEX i (name); ALTER TABLE users ADD INDEX j (name(10));",
            // Beside a unique index, only plain indexes were measured.
            "ALTER TABLE users ADD UNIQUE INDEX i (name); ALTER TABLE users ADD INDEX j (name(10));",
            "ALTER TABLE users ADD UNIQUE INDEX i (name); \
             ALTER TABLE users ADD UNIQUE INDEX j (name, id);",
        ] {
            for answer in name_key_after(index) {
                assert!(answer.starts_with("unknown"), "{index}: {answer}");
            }
        }
    }

    #[test]
    fn a_failed_foreign_key_adds_no_key_and_an_accepted_one_does() {
        let schema = format!("{KEY_TABLES} ALTER TABLE users ADD INDEX i (name);");
        let sql = format!("{NAME_KEY}; ALTER TABLE orders DROP COLUMN user_name");
        let [mysql_80, mysql_84] = on_both_servers(&schema, &sql);
        // 8.0.46 accepts the key, and then refuses to drop its column.
        assert_eq!(mysql_80[0], COPY_ANSWER);
        assert!(mysql_80[1].contains("1828"), "{}", mysql_80[1]);
        // 8.4.13 refuses the key, so nothing holds the column.
        assert!(mysql_84[0].contains("6125"), "{}", mysql_84[0]);
        assert!(!mysql_84[1].contains("1828"), "{}", mysql_84[1]);
    }

    #[test]
    fn a_new_foreign_key_of_a_known_table_to_an_unseen_one_is_unknown_with_notes_and_no_failure() {
        let predictions = predictions_with(
            "CREATE TABLE orders (id INT PRIMARY KEY, amount INT NOT NULL);",
            "ALTER TABLE orders ADD CONSTRAINT fk FOREIGN KEY (amount) REFERENCES users (id)",
        );
        assert!(
            reason(predictions[0].clone()).starts_with(
                "the foreign key points at users: no --schema was given, so kilit does not know \
                 users"
            ),
            "{:?}",
            predictions[0]
        );
        let (case, answer, notes) = likely(predictions[0].clone());
        assert_eq!(
            (case.as_str(), answer.as_str()),
            ("add-foreign-key", COPY_ANSWER)
        );
        assert_eq!(notes.len(), 8, "{notes:?}");
        assert_eq!(notes[0], FOREIGN_KEY_UNSEEN);
    }

    #[test]
    fn a_foreign_key_from_an_unknown_character_set_gets_no_answer() {
        let predictions = predictions_with(
            "CREATE TABLE users (id INT PRIMARY KEY, name VARCHAR(10), UNIQUE KEY u (name));
             CREATE TABLE orders (id INT PRIMARY KEY, name VARCHAR(10));",
            "ALTER TABLE orders ADD CONSTRAINT fk FOREIGN KEY (name) REFERENCES users (name)",
        );
        assert!(said(&predictions[0]).starts_with("unknown"));
    }

    /// The answer for the drop of an index that kilit should treat as any
    /// other index.
    const DROP_INDEX_ANSWER: &str = "INPLACE, no rebuild, writes continue";

    #[test]
    fn dropping_the_only_index_of_a_foreign_key_fails() {
        for (setup, index) in [
            (format!("{OWN_INDEX}{FOREIGN_KEY}"), "idx_user"),
            (format!("{SECOND_INDEX}{FOREIGN_KEY}"), "idx_user_amount"),
            // The index MySQL makes for the key, named after it.
            (FOREIGN_KEY.to_owned(), "fk_orders_user"),
            // A key without a name: MySQL names the index after its column.
            (
                "ALTER TABLE orders ADD FOREIGN KEY (user_id) REFERENCES users (id);".to_owned(),
                "user_id",
            ),
        ] {
            let prediction = on_shop(&setup, &format!("ALTER TABLE orders DROP INDEX {index}"));
            assert_eq!(fails_with(&prediction), Ok(1553), "{setup}");
            let (case, _, notes) = measured(prediction);
            assert_eq!(case, "drop-index");
            assert!(
                notes[0].starts_with(&format!(
                    "{index} is the only index that serves the foreign key"
                )),
                "{notes:?}"
            );
        }
    }

    #[test]
    fn dropping_an_index_when_another_serves_the_foreign_key_gets_the_drop_index_answer() {
        let setup = format!("{OWN_INDEX}{FOREIGN_KEY}{SECOND_INDEX}");
        for index in ["idx_user", "idx_user_amount"] {
            let prediction = on_shop(&setup, &format!("ALTER TABLE orders DROP INDEX {index}"));
            let (case, verdict, _) = measured(prediction);
            assert_eq!(case, "drop-index");
            assert_eq!(verdict, DROP_INDEX_ANSWER, "{index}");
        }
        // After that, the index that is left is the only one.
        let predictions = predictions_with(
            &format!("{SHOP}\n{setup}"),
            "ALTER TABLE orders DROP INDEX idx_user;
             ALTER TABLE orders DROP INDEX idx_user_amount;",
        );
        assert_eq!(said(&predictions[0]), DROP_INDEX_ANSWER);
        assert_eq!(fails_with(&predictions[1]), Ok(1553));
    }

    #[test]
    fn an_index_with_the_column_second_does_not_serve_the_foreign_key() {
        let setup = format!(
            "{OWN_INDEX}{FOREIGN_KEY}ALTER TABLE orders ADD INDEX idx_amount_user (amount, user_id);"
        );
        assert_eq!(
            fails_with(&on_shop(&setup, "ALTER TABLE orders DROP INDEX idx_user")),
            Ok(1553)
        );
        assert_eq!(
            said(&on_shop(
                &setup,
                "ALTER TABLE orders DROP INDEX idx_amount_user"
            )),
            DROP_INDEX_ANSWER
        );
    }

    #[test]
    fn a_prefix_or_a_fulltext_index_does_not_serve_a_foreign_key() {
        let schema = "CREATE TABLE codes (code VARCHAR(50) NOT NULL PRIMARY KEY);
             CREATE TABLE orders (id INT PRIMARY KEY, code VARCHAR(50) NOT NULL,
                 KEY idx_code (code), KEY idx_code_prefix (code(10)),
                 FULLTEXT KEY ft_code (code),
                 CONSTRAINT fk_code FOREIGN KEY (code) REFERENCES codes (code));";
        for (sql, expected) in [
            ("ALTER TABLE orders DROP INDEX idx_code", Ok(1553)),
            (
                "ALTER TABLE orders DROP INDEX idx_code_prefix",
                Err(DROP_INDEX_ANSWER.to_owned()),
            ),
            (
                "ALTER TABLE orders DROP INDEX ft_code",
                Err(DROP_INDEX_ANSWER.to_owned()),
            ),
        ] {
            let predictions = predictions_with(schema, sql);
            assert_eq!(fails_with(&predictions[0]), expected, "{sql}");
        }
    }

    #[test]
    fn a_primary_key_on_the_column_serves_the_foreign_key() {
        let schema = "
            CREATE TABLE users (id BIGINT UNSIGNED NOT NULL PRIMARY KEY);
            CREATE TABLE profiles (
                user_id BIGINT UNSIGNED NOT NULL,
                PRIMARY KEY (user_id),
                KEY idx_user (user_id),
                CONSTRAINT fk_profile FOREIGN KEY (user_id) REFERENCES users (id)
            );";
        let predictions = predictions_with(schema, "ALTER TABLE profiles DROP INDEX idx_user");
        assert_eq!(said(&predictions[0]), DROP_INDEX_ANSWER);
        // The first columns count, as for any index.
        let schema = "
            CREATE TABLE users (id BIGINT UNSIGNED NOT NULL PRIMARY KEY);
            CREATE TABLE lines (
                user_id BIGINT UNSIGNED NOT NULL,
                line INT NOT NULL,
                PRIMARY KEY (line, user_id),
                KEY idx_user (user_id),
                CONSTRAINT fk_line FOREIGN KEY (user_id) REFERENCES users (id)
            );";
        let predictions = predictions_with(schema, "ALTER TABLE lines DROP INDEX idx_user");
        assert_eq!(fails_with(&predictions[0]), Ok(1553));
    }

    #[test]
    fn an_index_mysql_may_have_replaced_gets_no_answer() {
        // MySQL replaced the first index with the second, without a word,
        // but kilit's table has both, so neither drop gets an answer.
        for (key, automatic) in [
            (FOREIGN_KEY, "fk_orders_user"),
            (
                "ALTER TABLE orders ADD FOREIGN KEY (user_id) REFERENCES users (id);",
                "user_id",
            ),
        ] {
            for second in [
                SECOND_INDEX,
                "ALTER TABLE orders ADD INDEX idx_user_amount (user_id);",
            ] {
                let setup = format!("{key}{second}");
                for index in [automatic, "idx_user_amount"] {
                    assert_eq!(
                        reason(on_shop(
                            &setup,
                            &format!("ALTER TABLE orders DROP INDEX {index}")
                        )),
                        format!(
                            "the index {automatic} has the name MySQL gives the index it makes \
                             for the foreign key {}, and MySQL drops that index silently when \
                             another one can serve the key; no schema shows which of the two \
                             this is",
                            if automatic == "user_id" {
                                "without a name"
                            } else {
                                automatic
                            }
                        ),
                        "{setup} {index}"
                    );
                }
            }
        }
        // A dump shows such an index as any other, whichever it is. An index
        // named after the column looks like the one of a key without a name.
        let schema = "
            CREATE TABLE users (id BIGINT UNSIGNED NOT NULL PRIMARY KEY);
            CREATE TABLE orders (
                id INT NOT NULL PRIMARY KEY,
                user_id BIGINT UNSIGNED NOT NULL,
                amount INT NOT NULL,
                KEY user_id (user_id),
                CONSTRAINT orders_ibfk_1 FOREIGN KEY (user_id) REFERENCES users (id)
            );";
        let predictions = predictions_with(
            schema,
            "ALTER TABLE orders ADD INDEX idx_user_amount (user_id, amount);
             ALTER TABLE orders DROP INDEX user_id;",
        );
        assert!(reason(predictions[1].clone()).starts_with("the index user_id has the name"));
        // Alone, it is the only index whichever it is: the answer holds.
        let predictions = predictions_with(schema, "ALTER TABLE orders DROP INDEX user_id");
        assert_eq!(fails_with(&predictions[0]), Ok(1553));
    }

    #[test]
    fn an_automatic_index_is_not_followed_after_its_foreign_key_is_gone() {
        // Pinned as it is, a known gap. MySQL 8.0.46 and 8.4.13 replace the
        // index they made for a key even after the key is gone (run by hand),
        // so this drop fails there with error 1091. kilit's table still has
        // the old index, and no key is left to tell where it came from.
        let setup = "ALTER TABLE orders ADD CONSTRAINT fk_old \
                     FOREIGN KEY (user_id) REFERENCES users (id);
             ALTER TABLE orders DROP FOREIGN KEY fk_old;
             ALTER TABLE orders ADD INDEX idx_user (user_id);";
        assert_eq!(
            said(&on_shop(setup, "ALTER TABLE orders DROP INDEX fk_old")),
            DROP_INDEX_ANSWER
        );
    }

    #[test]
    fn two_foreign_keys_on_one_index_get_no_answer() {
        let setup = format!(
            "{FOREIGN_KEY}ALTER TABLE orders ADD CONSTRAINT fk_two \
             FOREIGN KEY (user_id) REFERENCES users (id);"
        );
        assert_eq!(
            reason(on_shop(
                &setup,
                "ALTER TABLE orders DROP INDEX fk_orders_user"
            )),
            "the foreign keys fk_orders_user and fk_two both use fk_orders_user, and dropping \
             such an index has not been measured yet"
        );
    }

    #[test]
    fn dropping_the_index_of_a_foreign_key_follows_the_checks_as_measured() {
        let own = format!("{OWN_INDEX}{FOREIGN_KEY}");
        let run = |setup: &str, session: &str, index: &str| {
            on_shop(
                setup,
                &format!("{session}; ALTER TABLE orders DROP INDEX {index}"),
            )
        };
        // Measured with the checks off: an index of one column.
        assert_eq!(
            fails_with(&run(&own, "SET foreign_key_checks = 0", "idx_user")),
            Ok(1553)
        );
        assert_eq!(
            fails_with(&run(&own, "SET foreign_key_checks = 1", "idx_user")),
            Ok(1553)
        );
        // The rest with the checks off has not been measured.
        assert_eq!(
            reason(run(
                &format!("{SECOND_INDEX}{FOREIGN_KEY}"),
                "SET foreign_key_checks = 0",
                "idx_user_amount"
            )),
            "idx_user_amount is the only index that serves the foreign key fk_orders_user with \
             foreign_key_checks off, and the lab has not measured this shape of index and key"
        );
        assert_eq!(
            reason(run(
                &format!("{own}{SECOND_INDEX}"),
                "SET foreign_key_checks = 0",
                "idx_user"
            )),
            "another index also serves the foreign key fk_orders_user, and dropping idx_user \
             with that key has not been measured yet"
        );
        assert_eq!(
            reason(run(&own, "SET foreign_key_checks = @old", "idx_user")),
            "idx_user serves the foreign key fk_orders_user, and foreign_key_checks is set to \
             a value kilit does not understand"
        );
        // A global value does not change this session's.
        assert_eq!(
            fails_with(&run(&own, "SET GLOBAL foreign_key_checks = 0", "idx_user")),
            Ok(1553)
        );
    }

    /// A foreign key of two columns, with `index` on the child table.
    fn two_column_key(index: &str) -> String {
        format!(
            "CREATE TABLE parents (a INT NOT NULL, b INT NOT NULL, c INT NOT NULL,
                 PRIMARY KEY (a, b));
             CREATE TABLE children (id INT PRIMARY KEY, a INT NOT NULL, b INT NOT NULL,
                 c INT NOT NULL, {index}
                 CONSTRAINT fk_ab FOREIGN KEY (a, b) REFERENCES parents (a, b));"
        )
    }

    #[test]
    fn a_foreign_key_of_two_columns_gets_only_the_measured_answer() {
        let drop = "ALTER TABLE children DROP INDEX i";
        // The index of exactly its columns, the measured shape.
        let schema = two_column_key("KEY i (a, b),");
        assert_eq!(fails_with(&predictions_with(&schema, drop)[0]), Ok(1553));
        // The other order does not serve the key: MySQL has its own index.
        let schema = two_column_key("KEY i (b, a),");
        assert_eq!(said(&predictions_with(&schema, drop)[0]), DROP_INDEX_ANSWER);
        // With the checks off, a longer index, or a second one: not measured.
        let schema = two_column_key("KEY i (a, b),");
        let off = format!("SET foreign_key_checks = 0; {drop}");
        assert!(
            reason(predictions_with(&schema, &off).pop().unwrap())
                .contains("with foreign_key_checks off")
        );
        let schema = two_column_key("KEY i (a, b, c),");
        assert!(
            reason(predictions_with(&schema, drop).pop().unwrap())
                .contains("plain indexes of one or two columns only")
        );
        let schema = two_column_key("KEY i (a, b), KEY other_index (a, b),");
        assert!(
            reason(predictions_with(&schema, drop).pop().unwrap())
                .starts_with("another index also serves the foreign key fk_ab")
        );
    }

    #[test]
    fn index_shapes_around_a_foreign_key_the_lab_has_not_measured_are_unknown() {
        // A unique index, dropped or staying.
        let schema = format!(
            "{SHOP}{FOREIGN_KEY}ALTER TABLE orders ADD UNIQUE INDEX uq_user (user_id, id);"
        );
        assert!(
            reason(predictions_with(&schema, "ALTER TABLE orders DROP INDEX uq_user").remove(0))
                .starts_with("uq_user serves the foreign key fk_orders_user")
        );
        let schema = format!(
            "{SHOP}{OWN_INDEX}{FOREIGN_KEY}ALTER TABLE orders ADD UNIQUE INDEX uq_user (user_id, id);"
        );
        assert_eq!(
            reason(predictions_with(&schema, "ALTER TABLE orders DROP INDEX idx_user").remove(0)),
            "the unique index uq_user also serves the foreign key fk_orders_user, and dropping \
             such an index has not been measured yet"
        );
        // Three columns.
        let setup =
            format!("ALTER TABLE orders ADD INDEX i3 (user_id, amount, note);{FOREIGN_KEY}");
        assert!(
            reason(on_shop(&setup, "ALTER TABLE orders DROP INDEX i3"))
                .contains("plain indexes of one or two columns only")
        );
        // The index a foreign key of another table points at.
        let schema = "
            CREATE TABLE users (id INT PRIMARY KEY, name VARCHAR(100) NOT NULL,
                UNIQUE KEY uq_name (name));
            CREATE TABLE children (id INT PRIMARY KEY, name VARCHAR(100) NOT NULL,
                CONSTRAINT fk_name FOREIGN KEY (name) REFERENCES users (name));";
        assert_eq!(
            reason(predictions_with(schema, "ALTER TABLE users DROP INDEX uq_name").remove(0)),
            "uq_name serves the foreign key fk_name on children, which points at this table, \
             and dropping such an index has not been measured yet"
        );
    }

    #[test]
    fn an_index_a_key_added_later_points_at_gets_no_answer() {
        // The key comes after the tables.
        let schema = "
            CREATE TABLE users (id INT PRIMARY KEY, name VARCHAR(100) NOT NULL,
                UNIQUE KEY uq_name (name));
            CREATE TABLE children (id INT PRIMARY KEY, name VARCHAR(100) NOT NULL);";
        let predictions = predictions_with(
            schema,
            "ALTER TABLE users DROP INDEX uq_name;
             ALTER TABLE users ADD UNIQUE INDEX uq_name (name);
             ALTER TABLE children ADD CONSTRAINT fk_name FOREIGN KEY (name) REFERENCES users (name);
             ALTER TABLE users DROP INDEX uq_name;
             ALTER TABLE users ADD UNIQUE INDEX uq_name (name);
             ALTER TABLE children DROP FOREIGN KEY fk_name;
             ALTER TABLE users DROP INDEX uq_name;",
        );
        assert_eq!(said(&predictions[0]), DROP_INDEX_ANSWER);
        assert_eq!(
            reason(predictions[3].clone()),
            "uq_name serves the foreign key fk_name on children, which points at this table, \
             and dropping such an index has not been measured yet"
        );
        // Without the key, the table's old name only makes kilit look again.
        assert_eq!(said(&predictions[6]), DROP_INDEX_ANSWER);
    }

    #[test]
    fn the_only_index_of_a_foreign_key_is_found_in_any_quotes_or_case() {
        for dump in three_ways(
            "CREATE TABLE users ({id} INT PRIMARY KEY);
             CREATE TABLE orders ({id} INT PRIMARY KEY, {user_id} INT NOT NULL, {amount} INT,
                 KEY {idx_user} ({user_id}), KEY {idx_amount} ({amount}, {user_id}),
                 CONSTRAINT {fk_user} FOREIGN KEY ({user_id}) REFERENCES users ({id}));",
        ) {
            assert_eq!(
                fails_with(&predictions_with(&dump, "ALTER TABLE orders DROP INDEX idx_user")[0]),
                Ok(1553),
                "{dump}"
            );
            assert_eq!(
                said(&predictions_with(&dump, "ALTER TABLE orders DROP INDEX idx_amount")[0]),
                DROP_INDEX_ANSWER,
                "{dump}"
            );
        }
    }

    #[test]
    fn without_the_table_error_1553_and_3108_are_notes() {
        // kilit never saw the table, or lost track of it.
        let lost = format!(
            "{SHOP}{OWN_INDEX}{FOREIGN_KEY}ALTER TABLE orders CONVERT TO CHARACTER SET latin1;"
        );
        for schema in ["", lost.as_str()] {
            for (sql, case, verdict, note) in [
                (
                    "ALTER TABLE orders DROP INDEX idx_user",
                    "drop-index",
                    DROP_INDEX_ANSWER,
                    "if the index is the only one that serves a foreign key: fails with error \
                     1553 (drop-index-needed-by-foreign-key and 4 more lab scenarios)",
                ),
                (
                    "ALTER TABLE orders DROP COLUMN amount",
                    "drop-column",
                    "INSTANT, no rebuild, writes continue",
                    "if a generated column uses the column: fails with error 3108 \
                     (drop-column-generated-source and drop-column-stored-generated-source)",
                ),
                (
                    "ALTER TABLE orders RENAME COLUMN amount TO total",
                    "rename-column",
                    "INSTANT, no rebuild, writes continue",
                    "if a generated column uses the column: fails with error 3108 \
                     (rename-column-generated-source and rename-column-stored-generated-source)",
                ),
            ] {
                let (found, said, notes) = likely(predictions_with(schema, sql).pop().unwrap());
                assert_eq!(found, case, "{sql}");
                assert_eq!(said, verdict, "{sql}");
                assert!(notes.iter().any(|n| n == note), "{sql}: {notes:?}");
            }
        }
    }

    #[test]
    fn the_index_stays_after_a_drop_that_fails() {
        let predictions = predictions_with(
            &format!("{SHOP}{OWN_INDEX}{FOREIGN_KEY}"),
            "ALTER TABLE orders DROP INDEX idx_user;
             ALTER TABLE orders DROP INDEX idx_user;
             ALTER TABLE orders DROP COLUMN user_id;",
        );
        assert_eq!(fails_with(&predictions[0]), Ok(1553));
        assert_eq!(fails_with(&predictions[1]), Ok(1553));
        assert_eq!(fails_with(&predictions[2]), Ok(1828));
    }

    const VIRTUAL_COLUMN: &str =
        "ALTER TABLE orders ADD COLUMN amount2 INT AS (amount * 2) VIRTUAL;";
    const STORED_COLUMN: &str = "ALTER TABLE orders ADD COLUMN amount2 INT AS (amount * 2) STORED;";

    #[test]
    fn dropping_or_renaming_a_column_a_generated_column_uses_fails() {
        for setup in [VIRTUAL_COLUMN, STORED_COLUMN] {
            for (sql, case) in [
                ("ALTER TABLE orders DROP COLUMN amount", "drop-column"),
                (
                    "ALTER TABLE orders RENAME COLUMN amount TO total",
                    "rename-column",
                ),
            ] {
                let prediction = on_shop(setup, sql);
                assert_eq!(fails_with(&prediction), Ok(3108), "{setup} {sql}");
                let (found, _, notes) = measured(prediction);
                assert_eq!(found, case);
                assert_eq!(notes, ["the generated column amount2 uses amount"]);
            }
        }
    }

    #[test]
    fn a_generated_column_is_found_in_any_quotes_or_case() {
        for dump in three_ways(
            "CREATE TABLE orders ({id} INT PRIMARY KEY, {amount} INT NOT NULL,
                 {amount2} INT GENERATED ALWAYS AS (({amount} * 2)) VIRTUAL);",
        ) {
            for sql in [
                "ALTER TABLE orders DROP COLUMN amount",
                "ALTER TABLE orders DROP COLUMN AMOUNT",
                "ALTER TABLE orders RENAME COLUMN Amount TO total",
            ] {
                assert_eq!(
                    fails_with(&predictions_with(&dump, sql)[0]),
                    Ok(3108),
                    "{dump} {sql}"
                );
            }
        }
    }

    #[test]
    fn dropping_the_generated_column_itself_keeps_its_answer() {
        for setup in [VIRTUAL_COLUMN, STORED_COLUMN] {
            for sql in [
                "ALTER TABLE orders DROP COLUMN amount2",
                "ALTER TABLE orders RENAME COLUMN amount2 TO doubled",
            ] {
                assert_eq!(
                    said(&on_shop(setup, sql)),
                    "INSTANT, no rebuild, writes continue",
                    "{setup} {sql}"
                );
            }
        }
    }

    #[test]
    fn a_column_no_generated_column_uses_keeps_its_answer() {
        for sql in [
            "ALTER TABLE orders DROP COLUMN note",
            "ALTER TABLE orders RENAME COLUMN note TO memo",
        ] {
            assert_eq!(
                said(&on_shop(VIRTUAL_COLUMN, sql)),
                "INSTANT, no rebuild, writes continue",
                "{sql}"
            );
        }
        // A part of a name is not the name.
        let setup = "ALTER TABLE orders ADD COLUMN total INT NOT NULL;
                     ALTER TABLE orders ADD COLUMN g INT AS (total + 1) VIRTUAL;
                     ALTER TABLE orders ADD COLUMN total_cost INT NOT NULL;";
        assert_eq!(
            said(&on_shop(setup, "ALTER TABLE orders DROP COLUMN total_cost")),
            "INSTANT, no rebuild, writes continue"
        );
    }

    #[test]
    fn an_expression_that_is_not_plain_gets_no_3108() {
        let not_measured = |generated: &str, column: &str| {
            format!(
                "the expression of the generated column {generated} names {column}, and the lab \
                 measured expressions of column names, numbers and arithmetic operators only, \
                 not strings or function calls"
            )
        };
        // The name only in a string: MySQL accepts dropping amount (run by
        // hand). The same answer for a column the function really uses.
        let concat = "ALTER TABLE orders ADD COLUMN g VARCHAR(60) AS \
                      (concat('amount', note)) VIRTUAL;";
        for (sql, column) in [
            ("ALTER TABLE orders DROP COLUMN amount", "amount"),
            ("ALTER TABLE orders RENAME COLUMN amount TO total", "amount"),
            ("ALTER TABLE orders DROP COLUMN note", "note"),
            ("ALTER TABLE orders RENAME COLUMN note TO memo", "note"),
        ] {
            assert_eq!(
                reason(on_shop(concat, sql)),
                not_measured("g", column),
                "{sql}"
            );
        }
        // A column named like the function: MySQL accepts dropping it (run
        // by hand).
        let lower = "ALTER TABLE orders ADD COLUMN `lower` INT NULL;
                     ALTER TABLE orders ADD COLUMN g VARCHAR(255) AS (lower(email)) VIRTUAL;";
        for (sql, column) in [
            ("ALTER TABLE orders DROP COLUMN `lower`", "lower"),
            ("ALTER TABLE orders RENAME COLUMN `lower` TO low", "lower"),
            ("ALTER TABLE orders DROP COLUMN email", "email"),
        ] {
            assert_eq!(
                reason(on_shop(lower, sql)),
                not_measured("g", column),
                "{sql}"
            );
        }
        // MySQL prints a string in a dump with its character set.
        let dump = "CREATE TABLE orders (id INT PRIMARY KEY, amount INT NOT NULL,
                        note VARCHAR(50) NULL, g VARCHAR(60) GENERATED ALWAYS AS \
                        (concat(_utf8mb4'amount',`note`)) VIRTUAL);";
        assert_eq!(
            reason(predictions_with(dump, "ALTER TABLE orders DROP COLUMN amount").remove(0)),
            not_measured("g", "amount")
        );
        // An expression that does not name the column changes nothing.
        assert_eq!(
            said(&on_shop(concat, "ALTER TABLE orders DROP COLUMN email")),
            "INSTANT, no rebuild, writes continue"
        );
    }

    #[test]
    fn plain_expressions_are_column_names_numbers_and_arithmetic() {
        let schema = Schema::from_sql(
            "CREATE TABLE t (a INT, b INT, `div` INT, `lower` INT, c VARCHAR(10));",
        );
        let TableState::Known(table) = schema.table("t") else {
            panic!("expected the table");
        };
        for (expression, columns) in [
            ("a * 2", vec!["a"]),
            ("(`a` * 2)", vec!["a"]),
            ("((A + `B`) / 2.5) - 1 % a", vec!["A", "B", "a"]),
            ("-a", vec!["a"]),
            ("2", vec![]),
        ] {
            assert_eq!(
                plain_expression_columns(expression, table),
                Some(columns),
                "{expression}"
            );
        }
        for expression in [
            "concat(a, 'b')",
            "lower(c)",
            "`lower`(c)",
            "lower (c)",
            "a + 'b'",
            "a + \"b\"",
            "_utf8mb4'a'",
            "a > 1",
            "a DIV 2",
            "a `div` 2",
            "nope + 1",
            "t.a + 1",
            "0x1F + a",
            "1e3 + a",
            "(a) b",
            "`a",
        ] {
            assert_eq!(
                plain_expression_columns(expression, table),
                None,
                "{expression}"
            );
        }
    }

    #[test]
    fn generated_column_shapes_the_lab_has_not_measured_are_unknown() {
        let drop = "ALTER TABLE orders DROP COLUMN amount";
        let rename = "ALTER TABLE orders RENAME COLUMN amount TO total";
        // CHANGE that renames. Keeping the name is the change it makes.
        assert_eq!(
            reason(on_shop(
                VIRTUAL_COLUMN,
                "ALTER TABLE orders CHANGE amount total INT NOT NULL"
            )),
            "the generated column amount2 uses amount, and renaming such a column with CHANGE \
             has not been measured yet"
        );
        assert_eq!(
            said(&on_shop(
                VIRTUAL_COLUMN,
                "ALTER TABLE orders CHANGE amount amount BIGINT NOT NULL"
            )),
            said(&on_shop(
                VIRTUAL_COLUMN,
                "ALTER TABLE orders MODIFY amount BIGINT NOT NULL"
            )),
        );
        // A generated column that uses another.
        let chain = format!(
            "{VIRTUAL_COLUMN}ALTER TABLE orders ADD COLUMN amount4 INT AS (amount2 * 2) VIRTUAL;"
        );
        assert_eq!(
            reason(on_shop(&chain, drop)),
            "the generated column amount2 uses amount, and the generated column amount4 uses \
             amount2, which has not been measured together"
        );
        assert_eq!(
            reason(on_shop(&chain, "ALTER TABLE orders DROP COLUMN amount2")),
            "the generated column amount4 uses amount2, and amount2 is a generated column too, \
             which has not been measured together"
        );
        assert_eq!(
            reason(on_shop(
                &chain,
                "ALTER TABLE orders RENAME COLUMN amount2 TO doubled"
            )),
            "the generated column amount4 uses amount2, and amount2 is a generated column too, \
             which has not been measured together"
        );
        // The source in an index: renaming it. Dropping it has its own reason.
        let indexed = format!("{VIRTUAL_COLUMN}ALTER TABLE orders ADD INDEX i (amount);");
        assert_eq!(
            reason(on_shop(&indexed, rename)),
            "the generated column amount2 uses amount, and amount is in the index i, which has \
             not been measured together"
        );
        assert_eq!(
            reason(on_shop(&indexed, drop)),
            "the generated column amount2 uses amount, and dropping such a column has not been \
             measured yet"
        );
        // The generated column in an index.
        let indexed = format!("{VIRTUAL_COLUMN}ALTER TABLE orders ADD INDEX i (amount2);");
        for sql in [drop, rename] {
            assert_eq!(
                reason(on_shop(&indexed, sql)),
                "the generated column amount2 uses amount, and amount2 is in the index i, which \
                 has not been measured together",
                "{sql}"
            );
        }
        // A table with a FULLTEXT index.
        let fulltext = format!("{VIRTUAL_COLUMN}ALTER TABLE orders ADD FULLTEXT INDEX ft (note);");
        for sql in [drop, rename] {
            assert_eq!(
                reason(on_shop(&fulltext, sql)),
                "the generated column amount2 uses amount, and orders has the FULLTEXT index ft, \
                 which has not been measured together",
                "{sql}"
            );
        }
        // The generated column and its source in one ALTER.
        assert!(
            reason(on_shop(
                VIRTUAL_COLUMN,
                "ALTER TABLE orders DROP COLUMN amount2, DROP COLUMN amount"
            ))
            .starts_with("2 changes in one statement")
        );
    }

    #[test]
    fn the_column_stays_after_a_drop_or_a_rename_that_fails() {
        for first in [
            "ALTER TABLE orders DROP COLUMN amount;",
            "ALTER TABLE orders RENAME COLUMN amount TO total;",
        ] {
            // The statement after it still sees amount and what uses it.
            let predictions = predictions_with(
                &format!("{SHOP}{VIRTUAL_COLUMN}"),
                &format!(
                    "{first}
                     ALTER TABLE orders DROP COLUMN amount;
                     ALTER TABLE orders DROP COLUMN amount2;
                     ALTER TABLE orders RENAME COLUMN amount TO total;
                     ALTER TABLE orders DROP COLUMN total;"
                ),
            );
            assert_eq!(fails_with(&predictions[0]), Ok(3108), "{first}");
            assert_eq!(fails_with(&predictions[1]), Ok(3108), "{first}");
            // With the generated column gone, the source has nothing in the way.
            for prediction in &predictions[2..] {
                assert_eq!(
                    said(prediction),
                    "INSTANT, no rebuild, writes continue",
                    "{first}"
                );
            }
        }
    }

    const CHECK_ALONE: &str = "ALTER TABLE orders ADD CONSTRAINT chk_amount CHECK (amount > 0);";
    const CHECK_ALONE_OFF: &str =
        "ALTER TABLE orders ADD CONSTRAINT chk_amount CHECK (amount > 0) NOT ENFORCED;";
    const CHECK_TWO: &str =
        "ALTER TABLE orders ADD CONSTRAINT chk_amount CHECK (amount + user_id > 0);";
    const CHECK_TWO_OFF: &str =
        "ALTER TABLE orders ADD CONSTRAINT chk_amount CHECK (amount + user_id > 0) NOT ENFORCED;";
    const AMOUNT_INDEX: &str = "ALTER TABLE orders ADD INDEX idx_amount (amount);";
    const DROP_AMOUNT: &str = "ALTER TABLE orders DROP COLUMN amount";
    const RENAME_AMOUNT: &str = "ALTER TABLE orders RENAME COLUMN amount TO total";
    const INSTANT_ANSWER: &str = "INSTANT, no rebuild, writes continue";
    const COPY_ANSWER: &str = "COPY, copies every row, writes wait";

    #[test]
    fn dropping_a_column_a_check_uses_with_another_column_fails() {
        for setup in [CHECK_TWO, CHECK_TWO_OFF] {
            let prediction = on_shop(setup, DROP_AMOUNT);
            assert_eq!(fails_with(&prediction), Ok(3959), "{setup}");
            let (case, _, notes) = measured(prediction);
            assert_eq!(case, "drop-column");
            assert_eq!(
                notes,
                ["the CHECK constraint chk_amount uses amount and user_id"]
            );
            // And so does the other column.
            let prediction = on_shop(setup, "ALTER TABLE orders DROP COLUMN user_id");
            assert_eq!(fails_with(&prediction), Ok(3959), "{setup}");
            let (_, _, notes) = measured(prediction);
            assert_eq!(
                notes,
                ["the CHECK constraint chk_amount uses user_id and amount"]
            );
        }
    }

    #[test]
    fn one_check_with_another_column_is_enough_to_keep_the_column() {
        // Run by hand: the constraint on the column alone does not save it.
        let setup = "ALTER TABLE orders ADD CONSTRAINT chk_one CHECK (amount > 0);
                     ALTER TABLE orders ADD CONSTRAINT chk_two CHECK (amount + user_id > 0);";
        let prediction = on_shop(setup, DROP_AMOUNT);
        assert_eq!(fails_with(&prediction), Ok(3959));
        let (_, _, notes) = measured(prediction);
        assert_eq!(
            notes,
            ["the CHECK constraint chk_two uses amount and user_id"]
        );
    }

    #[test]
    fn renaming_a_column_a_check_uses_fails_also_when_it_uses_no_other_column() {
        for setup in [CHECK_ALONE, CHECK_ALONE_OFF, CHECK_TWO, CHECK_TWO_OFF] {
            let prediction = on_shop(setup, RENAME_AMOUNT);
            assert_eq!(fails_with(&prediction), Ok(3959), "{setup}");
            let (case, _, notes) = measured(prediction);
            assert_eq!(case, "rename-column");
            assert_eq!(notes, ["the CHECK constraint chk_amount uses amount"]);
        }
    }

    #[test]
    fn dropping_the_only_column_of_a_check_keeps_the_answer_for_dropping_a_column() {
        let twice = "ALTER TABLE orders ADD CONSTRAINT chk_amount \
                     CHECK (amount > 0 AND amount < 100000);";
        for setup in [CHECK_ALONE, CHECK_ALONE_OFF, twice] {
            let (case, verdict, notes) = measured(on_shop(setup, DROP_AMOUNT));
            assert_eq!(case, "drop-column", "{setup}");
            assert_eq!(verdict, INSTANT_ANSWER, "{setup}");
            assert!(notes.is_empty(), "{setup}: {notes:?}");
        }
    }

    #[test]
    fn a_check_goes_with_its_only_column() {
        // The new column of that name has no CHECK constraint. The name is free.
        let predictions = predictions_with(
            &format!("{SHOP}\n{CHECK_ALONE}"),
            "ALTER TABLE orders DROP COLUMN amount;
             ALTER TABLE orders ADD COLUMN amount INT NOT NULL;
             ALTER TABLE orders RENAME COLUMN amount TO total;
             ALTER TABLE orders ADD CONSTRAINT chk_amount CHECK (total > 0);",
        );
        assert_eq!(said(&predictions[0]), INSTANT_ANSWER);
        assert_eq!(said(&predictions[1]), INSTANT_ANSWER);
        assert_eq!(said(&predictions[2]), INSTANT_ANSWER);
        assert_eq!(said(&predictions[3]), COPY_ANSWER);
    }

    #[test]
    fn a_check_on_a_column_with_a_plain_index() {
        let indexed_alone = format!("{AMOUNT_INDEX}{CHECK_ALONE}");
        let (_, verdict, notes) = measured(on_shop(&indexed_alone, DROP_AMOUNT));
        assert_eq!(verdict, "INPLACE, rebuilds the table, writes continue");
        assert_eq!(notes, ["amount is in the index idx_amount"]);
        assert_eq!(
            fails_with(&on_shop(&indexed_alone, RENAME_AMOUNT)),
            Ok(3959)
        );

        let indexed_two = format!("{AMOUNT_INDEX}{CHECK_TWO}");
        let prediction = on_shop(&indexed_two, DROP_AMOUNT);
        assert_eq!(fails_with(&prediction), Ok(3959));
        // The exception measured with both conditions decides.
        let Prediction::Measured {
            evidence, notes, ..
        } = prediction
        else {
            panic!("expected a measured prediction");
        };
        assert_eq!(evidence, ["drop-column-indexed-in-two-column-check"]);
        assert_eq!(
            notes,
            [
                "amount is in the index idx_amount",
                "the CHECK constraint chk_amount uses amount and user_id"
            ]
        );
        assert_eq!(fails_with(&on_shop(&indexed_two, RENAME_AMOUNT)), Ok(3959));
    }

    #[test]
    fn a_column_no_check_uses_keeps_its_answer() {
        for setup in [CHECK_ALONE, CHECK_TWO] {
            for sql in [
                "ALTER TABLE orders DROP COLUMN note",
                "ALTER TABLE orders RENAME COLUMN note TO memo",
            ] {
                assert_eq!(said(&on_shop(setup, sql)), INSTANT_ANSWER, "{setup} {sql}");
            }
        }
        // A part of a name is not the name.
        let setup = "ALTER TABLE orders ADD COLUMN amount_due INT NOT NULL;
                     ALTER TABLE orders ADD CONSTRAINT chk CHECK (amount_due > user_id);";
        assert_eq!(said(&on_shop(setup, DROP_AMOUNT)), INSTANT_ANSWER);
    }

    #[test]
    fn a_column_named_only_in_a_string_of_a_check_is_not_used() {
        // In all these MySQL accepts the change and keeps the constraint (run
        // by hand; the lab measured the first).
        for check in [
            "email <> 'note'",
            "email <> _utf8mb4'note'",
            "email NOT LIKE 'note%'",
            "email NOT IN ('note', 'a')",
            "char_length(concat(email, 'note')) > 0",
        ] {
            let setup = format!("ALTER TABLE orders ADD CONSTRAINT chk_email CHECK ({check});");
            for sql in [
                "ALTER TABLE orders DROP COLUMN note",
                "ALTER TABLE orders RENAME COLUMN note TO memo",
            ] {
                assert_eq!(said(&on_shop(&setup, sql)), INSTANT_ANSWER, "{check} {sql}");
            }
        }
        // kilit does not read a string with a backslash, so a word in it counts.
        let setup = r"ALTER TABLE orders ADD CONSTRAINT chk_email CHECK (email <> 'note\\');";
        assert!(
            reason(on_shop(setup, "ALTER TABLE orders DROP COLUMN note"))
                .starts_with("the expression of the CHECK constraint chk_email names note"),
        );
        // With ANSI_QUOTES a double-quoted word is a column's name, and kilit
        // does not know the server's sql_mode.
        let setup = "ALTER TABLE orders ADD CONSTRAINT chk_email CHECK (email <> \"note\");";
        assert!(
            reason(on_shop(setup, "ALTER TABLE orders DROP COLUMN note"))
                .starts_with("the expression of the CHECK constraint chk_email names note"),
        );
    }

    #[test]
    fn a_check_that_is_not_plain_gets_no_answer_for_the_names_it_has() {
        for (check, names) in [
            ("note IN ('a', 'b')", &["note"][..]),
            ("char_length(note) > 0", &["note"]),
            ("note LIKE 'a%'", &["note"]),
            ("amount BETWEEN 1 AND 1000", &["amount"]),
            ("amount IN (user_id, 1)", &["amount", "user_id"]),
            ("char_length(note) < amount", &["note", "amount"]),
            ("amount <=> user_id OR amount > 0", &["amount", "user_id"]),
            ("amount DIV 2 >= user_id", &["amount", "user_id"]),
            ("orders.amount > 0", &["amount"]),
        ] {
            let setup = format!("ALTER TABLE orders ADD CONSTRAINT chk CHECK ({check});");
            for name in names {
                for sql in [
                    format!("ALTER TABLE orders DROP COLUMN {name}"),
                    format!("ALTER TABLE orders RENAME COLUMN {name} TO other_name"),
                ] {
                    assert_eq!(
                        reason(on_shop(&setup, &sql)),
                        format!(
                            "the expression of the CHECK constraint chk names {name}, and the \
                             lab measured expressions of column names, numbers and arithmetic, \
                             comparison and logical operators only, not strings, function \
                             calls, IN, BETWEEN or LIKE"
                        ),
                        "{check} {sql}"
                    );
                }
            }
            // The answer for a column it does not name does not depend on it.
            assert_eq!(
                said(&on_shop(&setup, "ALTER TABLE orders DROP COLUMN email")),
                INSTANT_ANSWER,
                "{check}"
            );
        }
        // A word that is a function's name is no column, but kilit cannot tell.
        let setup = "ALTER TABLE orders ADD COLUMN lower INT NULL;
                     ALTER TABLE orders ADD CONSTRAINT chk CHECK (lower(note) <> 'x');";
        assert!(
            reason(on_shop(setup, "ALTER TABLE orders DROP COLUMN lower"))
                .starts_with("the expression of the CHECK constraint chk names lower")
        );
    }

    #[test]
    fn a_check_is_found_in_any_quotes_or_case() {
        for dump in three_ways(
            "CREATE TABLE orders (
                 {id} BIGINT UNSIGNED NOT NULL,
                 {amount} INT NOT NULL,
                 {note} VARCHAR(50) DEFAULT NULL,
                 PRIMARY KEY ({id}),
                 CONSTRAINT {orders_chk_1} CHECK (({amount} > {id})),
                 CONSTRAINT {orders_chk_2} CHECK (({note} IS NULL))
             );",
        ) {
            for sql in [
                "ALTER TABLE orders DROP COLUMN amount",
                "ALTER TABLE orders DROP COLUMN AMOUNT",
                "ALTER TABLE orders DROP COLUMN `Amount`",
                "ALTER TABLE orders RENAME COLUMN Amount TO total",
                "ALTER TABLE orders RENAME COLUMN note TO memo",
                "ALTER TABLE orders RENAME COLUMN `NOTE` TO memo",
            ] {
                assert_eq!(
                    fails_with(&predictions_with(&dump, sql)[0]),
                    Ok(3959),
                    "{dump} {sql}"
                );
            }
            assert_eq!(
                said(&predictions_with(&dump, "ALTER TABLE orders DROP COLUMN NOTE")[0]),
                INSTANT_ANSWER,
                "{dump}"
            );
            assert_eq!(
                said(
                    &predictions_with(&dump, "ALTER TABLE orders DROP CONSTRAINT ORDERS_CHK_2")[0]
                ),
                INSTANT_ANSWER,
                "{dump}"
            );
        }
    }

    #[test]
    fn a_check_written_as_a_schema_dump_writes_it_is_followed() {
        // SHOW CREATE TABLE on MySQL 8.0.46 and 8.4.13, run by hand.
        let dump = "CREATE TABLE `orders` (
              `id` bigint unsigned NOT NULL,
              `amount` int NOT NULL,
              `note` varchar(50) DEFAULT NULL,
              `email` varchar(255) DEFAULT NULL,
              PRIMARY KEY (`id`),
              CONSTRAINT `orders_chk_1` CHECK ((`amount` > `id`)),
              CONSTRAINT `orders_chk_2` CHECK ((`note` <> _utf8mb4'x')),
              CONSTRAINT `orders_chk_3` CHECK ((`email` > 0)) /*!80016 NOT ENFORCED */
            ) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_0900_ai_ci;";
        let (_, _, notes) = measured(predictions_with(dump, DROP_AMOUNT).remove(0));
        assert_eq!(
            notes,
            ["the CHECK constraint orders_chk_1 uses amount and id"]
        );
        assert!(
            reason(predictions_with(dump, "ALTER TABLE orders DROP COLUMN note").remove(0))
                .starts_with("the expression of the CHECK constraint orders_chk_2 names note")
        );
        assert_eq!(
            fails_with(
                &predictions_with(dump, "ALTER TABLE orders RENAME COLUMN email TO mail")[0]
            ),
            Ok(3959)
        );
        assert_eq!(
            said(&predictions_with(dump, "ALTER TABLE orders DROP COLUMN email")[0]),
            INSTANT_ANSWER
        );
    }

    #[test]
    fn checks_without_a_name_and_on_a_column_count_as_well() {
        let dump = "CREATE TABLE orders (
                        id INT PRIMARY KEY,
                        user_id INT NOT NULL,
                        amount INT NOT NULL CHECK (amount > 0),
                        price INT NOT NULL CONSTRAINT price_positive CHECK (price > 0),
                        note VARCHAR(50) NULL,
                        CHECK (user_id > amount)
                    );";
        let (_, _, notes) = measured(predictions_with(dump, DROP_AMOUNT).remove(0));
        assert_eq!(
            notes,
            ["the CHECK constraint orders_chk_2 uses amount and user_id"]
        );
        let (_, _, notes) = measured(
            predictions_with(dump, "ALTER TABLE orders RENAME COLUMN price TO cost").remove(0),
        );
        assert_eq!(notes, ["the CHECK constraint price_positive uses price"]);
        assert_eq!(
            said(&predictions_with(dump, "ALTER TABLE orders DROP COLUMN price")[0]),
            INSTANT_ANSWER
        );
        // The automatic names work in DROP CONSTRAINT.
        for sql in [
            "ALTER TABLE orders DROP CONSTRAINT orders_chk_1",
            "ALTER TABLE orders DROP CONSTRAINT orders_chk_2",
            "ALTER TABLE orders DROP CONSTRAINT price_positive",
        ] {
            assert_eq!(
                said(&predictions_with(dump, sql)[0]),
                INSTANT_ANSWER,
                "{sql}"
            );
        }
    }

    #[test]
    fn without_the_table_error_3959_is_a_note() {
        // kilit never saw the table, or lost track of it.
        let lost = format!("{SHOP}{CHECK_TWO}ALTER TABLE orders CONVERT TO CHARACTER SET latin1;");
        for schema in ["", lost.as_str()] {
            for (sql, case, wanted) in [
                (
                    DROP_AMOUNT,
                    "drop-column",
                    &[
                        "if a CHECK constraint uses the column and another column: fails with \
                         error 3959 (drop-column-in-two-column-check and \
                         drop-column-in-not-enforced-two-column-check)",
                        "if the column is in an index and a CHECK constraint uses it and another \
                         column: fails with error 3959 (drop-column-indexed-in-two-column-check)",
                    ][..],
                ),
                (
                    RENAME_AMOUNT,
                    "rename-column",
                    &[
                        "if a CHECK constraint uses the column: fails with error 3959 \
                       (rename-column-in-two-column-check and 5 more lab scenarios)",
                    ],
                ),
            ] {
                let (found, verdict, notes) = likely(predictions_with(schema, sql).pop().unwrap());
                assert_eq!(found, case, "{sql}");
                assert_eq!(verdict, INSTANT_ANSWER, "{sql}");
                for note in wanted {
                    assert!(notes.iter().any(|n| n == note), "{sql}: {notes:?}");
                }
            }
        }
    }

    #[test]
    fn the_column_and_its_check_stay_after_a_statement_that_fails() {
        let predictions = predictions_with(
            &format!("{SHOP}\n{CHECK_TWO}"),
            "ALTER TABLE orders DROP COLUMN amount;
             ALTER TABLE orders DROP COLUMN amount;
             ALTER TABLE orders RENAME COLUMN amount TO total;
             ALTER TABLE orders MODIFY amount BIGINT NOT NULL;
             ALTER TABLE orders DROP CONSTRAINT chk_amount;
             ALTER TABLE orders DROP COLUMN amount;",
        );
        assert_eq!(fails_with(&predictions[0]), Ok(3959));
        assert_eq!(fails_with(&predictions[1]), Ok(3959));
        assert_eq!(fails_with(&predictions[2]), Ok(3959));
        // The column is still there, as it was.
        assert_eq!(said(&predictions[3]), COPY_ANSWER);
        assert_eq!(said(&predictions[4]), INSTANT_ANSWER);
        assert_eq!(said(&predictions[5]), INSTANT_ANSWER);
    }

    #[test]
    fn adding_a_check_copies_the_table() {
        for sql in [
            "ALTER TABLE orders ADD CONSTRAINT chk_amount CHECK (amount >= 0)",
            "ALTER TABLE orders ADD CONSTRAINT chk_amount CHECK (amount >= 0) ENFORCED",
            "ALTER TABLE orders ADD CHECK (amount >= 0)",
            "ALTER TABLE orders ADD CONSTRAINT CHECK (amount >= 0)",
            "ALTER TABLE orders ADD CONSTRAINT chk_amount CHECK (amount + user_id > 0)",
            "ALTER TABLE orders ADD CONSTRAINT chk_note CHECK (note IS NULL OR amount > 0)",
        ] {
            let (case, verdict, notes) = measured(on_shop("", sql));
            assert_eq!(case, "add-check-constraint", "{sql}");
            assert_eq!(verdict, COPY_ANSWER, "{sql}");
            // No schema shows the rows, so a failure is only ever a note.
            assert_eq!(
                notes,
                ["if a row breaks the constraint: fails with error 3819 \
                     (add-check-constraint-rows-break-it)"],
                "{sql}"
            );
        }
        // The other algorithms are refused, as the lab measured.
        let add = "ALTER TABLE orders ADD CONSTRAINT chk_amount CHECK (amount >= 0)";
        for (clause, code) in [
            ("ALGORITHM=INSTANT", 1845),
            ("ALGORITHM=INPLACE", 1845),
            ("ALGORITHM=INPLACE, LOCK=NONE", 1845),
            ("LOCK=NONE", 1846),
        ] {
            assert_eq!(
                fails_with(&on_shop("", &format!("{add}, {clause}"))),
                Ok(code),
                "{clause}"
            );
        }
        assert_eq!(
            said(&on_shop("", &format!("{add}, ALGORITHM=COPY"))),
            COPY_ANSWER
        );
        // Without the table it is only likely: the table may lack the
        // column, or have a constraint of the name, which the lab has not
        // measured.
        assert_eq!(likely(only(add)).1, COPY_ANSWER);
    }

    #[test]
    fn adding_a_check_no_longer_loses_the_table() {
        let predictions = predictions_with(
            &format!("{SHOP}\n{CHECK_TWO}"),
            "ALTER TABLE orders MODIFY note VARCHAR(60) NULL;
             ALTER TABLE orders ADD FULLTEXT INDEX ft_email (email);
             ALTER TABLE orders ADD COLUMN coupon VARCHAR(20) NULL;
             ALTER TABLE orders DROP COLUMN coupon;",
        );
        assert_eq!(
            said(&predictions[0]),
            "INPLACE, no rebuild, writes continue"
        );
        assert_eq!(
            said(&predictions[1]),
            "INPLACE, rebuilds the table, writes wait"
        );
        // kilit sees the FULLTEXT index it just followed, so both are copies.
        assert_eq!(said(&predictions[2]), COPY_ANSWER);
        assert_eq!(said(&predictions[3]), COPY_ANSWER);
    }

    #[test]
    fn a_check_that_fails_to_be_added_is_not_followed() {
        // ALGORITHM=INSTANT is refused, so the name is still free.
        let predictions = predictions_with(
            SHOP,
            "ALTER TABLE orders ADD CONSTRAINT chk_amount CHECK (amount >= 0), ALGORITHM=INSTANT;
             ALTER TABLE orders RENAME COLUMN amount TO total;
             ALTER TABLE orders ADD CONSTRAINT chk_amount CHECK (total >= 0);",
        );
        assert_eq!(fails_with(&predictions[0]), Ok(1845));
        assert_eq!(said(&predictions[1]), INSTANT_ANSWER);
        assert_eq!(said(&predictions[2]), COPY_ANSWER);
    }

    #[test]
    fn adding_a_check_the_lab_has_not_measured_is_unknown() {
        let add = |check: &str| format!("ALTER TABLE orders ADD CONSTRAINT chk CHECK ({check})");
        // Measured by hand, or in no scenario of the repository: MySQL refuses
        // some expressions. NOT ENFORCED changes none of it.
        assert_eq!(
            reason(on_shop(
                "",
                &format!("{} NOT ENFORCED", add("char_length(note) > 0"))
            )),
            "adding a CHECK constraint on (char_length(note) > 0) has not been measured yet; \
             the lab measured expressions of column names, numbers and arithmetic, comparison \
             and logical operators only"
        );
        assert_eq!(
            reason(on_shop("", &format!("{} NOT ENFORCED", add("nosuch > 0")))),
            "orders has no column nosuch, as far as kilit can tell"
        );
        for check in [
            "note IN ('a', 'b')",
            "char_length(note) > 0",
            "note LIKE 'a%'",
            "amount BETWEEN 1 AND 2",
            "amount > RAND()",
            "amount",
        ] {
            assert_eq!(
                reason(on_shop("", &add(check))),
                format!(
                    "adding a CHECK constraint on ({check}) has not been measured yet; the lab \
                     measured expressions of column names, numbers and arithmetic, comparison \
                     and logical operators only"
                ),
                "{check}"
            );
        }
        assert_eq!(
            reason(on_shop("", &add("1 > 0"))),
            "adding a CHECK constraint that uses no column has not been measured yet"
        );
        // What kilit can see in the table.
        assert_eq!(
            reason(on_shop("", &add("nosuch > 0"))),
            "orders has no column nosuch, as far as kilit can tell"
        );
        assert_eq!(
            reason(on_shop("", &add("amount > id"))),
            "id is an AUTO_INCREMENT column, and a CHECK constraint on such a column has not \
             been measured yet"
        );
        assert_eq!(
            reason(on_shop(VIRTUAL_COLUMN, &add("amount2 > 0"))),
            "amount2 is a generated column, and a CHECK constraint on such a column has not \
             been measured yet"
        );
        assert_eq!(
            reason(on_shop(FOREIGN_KEY, &add("user_id > 0"))),
            "user_id is a foreign key's column, and a CHECK constraint on such a column has not \
             been measured yet"
        );
        // A name is taken across the database, in any case.
        let taken = "CREATE TABLE others (a INT, CONSTRAINT CHK CHECK (a > 0));";
        assert_eq!(
            reason(on_shop(taken, &add("amount > 0"))),
            "others already has a CHECK constraint chk, and adding one with a name the database \
             already has has not been measured yet"
        );
    }

    #[test]
    fn a_check_with_a_name_the_table_has_loses_the_table() {
        // MySQL refuses it (error 3822, run by hand). kilit has no fact for
        // that, so it gives no answer, as for a column the table already has.
        let predictions = predictions_with(
            &format!("{SHOP}\n{CHECK_ALONE}"),
            "ALTER TABLE orders ADD CONSTRAINT CHK_AMOUNT CHECK (amount > 1);
             ALTER TABLE orders MODIFY note VARCHAR(60) NULL;",
        );
        assert!(reason(predictions[0].clone()).contains("already has a CHECK constraint"));
        assert!(reason(predictions[1].clone()).starts_with("kilit lost track of this table"));
    }

    #[test]
    fn dropping_a_check_with_drop_constraint_runs_at_once() {
        for setup in [CHECK_ALONE, CHECK_ALONE_OFF, CHECK_TWO, CHECK_TWO_OFF] {
            let (case, verdict, notes) = measured(on_shop(
                setup,
                "ALTER TABLE orders DROP CONSTRAINT chk_amount",
            ));
            assert_eq!(case, "drop-check-constraint", "{setup}");
            assert_eq!(verdict, INSTANT_ANSWER, "{setup}");
            assert!(notes.is_empty(), "{setup}: {notes:?}");
            // The column has nothing in the way now.
            assert_eq!(
                said(
                    &predictions_with(
                        &format!("{SHOP}\n{setup}"),
                        "ALTER TABLE orders DROP CONSTRAINT chk_amount;
                     ALTER TABLE orders RENAME COLUMN amount TO total;"
                    )[1]
                ),
                INSTANT_ANSWER,
                "{setup}"
            );
        }
    }

    #[test]
    fn drop_constraint_finds_the_kind_by_the_name() {
        // A foreign key: no measurement, and its index stays, as run by hand.
        let predictions = predictions_with(
            &format!("{SHOP}\n{FOREIGN_KEY}"),
            "ALTER TABLE orders DROP CONSTRAINT fk_orders_user;
             ALTER TABLE orders DROP COLUMN user_id;",
        );
        assert_eq!(
            reason(predictions[0].clone()),
            "fk_orders_user is a foreign key, and dropping one with DROP CONSTRAINT has not been \
             measured yet"
        );
        assert_eq!(
            said(&predictions[1]),
            "INPLACE, rebuilds the table, writes continue"
        );
        // A unique index, which goes.
        let predictions = predictions_with(
            &format!("{SHOP}\nALTER TABLE orders ADD UNIQUE INDEX uq_email (email);"),
            "ALTER TABLE orders DROP CONSTRAINT UQ_EMAIL;
             ALTER TABLE orders DROP COLUMN email;",
        );
        assert_eq!(
            reason(predictions[0].clone()),
            "UQ_EMAIL is a unique index, and dropping one with DROP CONSTRAINT has not been \
             measured yet"
        );
        assert_eq!(said(&predictions[1]), INSTANT_ANSWER);
        // The primary key.
        let dump =
            "CREATE TABLE orders (id INT NOT NULL, note VARCHAR(50) NULL, PRIMARY KEY (id));";
        let predictions = predictions_with(
            dump,
            "ALTER TABLE orders DROP CONSTRAINT `PRIMARY`;
             ALTER TABLE orders MODIFY id INT NULL;",
        );
        assert_eq!(
            reason(predictions[0].clone()),
            "PRIMARY is the primary key, and dropping one with DROP CONSTRAINT has not been \
             measured yet"
        );
        assert_eq!(
            said(&predictions[1]),
            "INPLACE, rebuilds the table, writes continue"
        );
    }

    #[test]
    fn a_foreign_key_and_a_plain_index_with_one_name_count_as_the_foreign_key() {
        // As Laravel's tables have them: MySQL drops the key and keeps the index.
        let predictions = predictions_with(
            LARAVEL_TABLES,
            "alter table `orders` drop constraint `orders_user_id_foreign`;
             alter table `orders` drop index `orders_user_id_foreign`;",
        );
        assert!(
            reason(predictions[0].clone()).starts_with("orders_user_id_foreign is a foreign key")
        );
        assert_eq!(
            said(&predictions[1]),
            "INPLACE, no rebuild, writes continue"
        );
    }

    #[test]
    fn drop_constraint_of_a_name_kilit_cannot_place_loses_the_table_and_of_none_keeps_it() {
        // MySQL refuses all of them (errors 3940 and 3939, run by hand).
        let next = "ALTER TABLE orders MODIFY note VARCHAR(60) NULL";
        let none = |name: &str| {
            format!(
                "orders has no constraint {name} that kilit knows of (MySQL does not count a \
                 plain index as one). If it has none, MySQL fails with error 3940 \
                 (drop-constraint-missing); kilit does not answer that from its tables alone, \
                 which may be out of date"
            )
        };
        // A name no constraint has: the table stays known (the lab: 3940).
        for (setup, name) in [
            ("", "nosuch"),
            (
                "ALTER TABLE orders ADD INDEX i_amount (amount);",
                "i_amount",
            ),
            (
                "ALTER TABLE orders ADD FULLTEXT INDEX f_note (note);",
                "f_note",
            ),
        ] {
            let predictions = predictions_with(
                &format!("{SHOP}\n{setup}"),
                &format!("ALTER TABLE orders DROP CONSTRAINT {name};\n{next};"),
            );
            assert_eq!(reason(predictions[0].clone()), none(name), "{setup}");
            assert!(
                matches!(predictions[1], Prediction::Measured { .. }),
                "{setup}: {:?}",
                predictions[1]
            );
        }
        for (setup, name, wanted) in [
            (
                "ALTER TABLE orders ADD CONSTRAINT same FOREIGN KEY (user_id) REFERENCES users (id);
                 ALTER TABLE orders ADD CONSTRAINT same CHECK (amount > 0);",
                "same",
                "orders has several constraints named same, and MySQL 8.0.46 and 8.4.13 do not \
                 accept DROP CONSTRAINT then",
            ),
            (
                "ALTER TABLE orders ADD UNIQUE INDEX same (email);
                 ALTER TABLE orders ADD CONSTRAINT same CHECK (amount > 0);",
                "same",
                "orders has several constraints named same, and MySQL 8.0.46 and 8.4.13 do not \
                 accept DROP CONSTRAINT then",
            ),
        ] {
            let predictions = predictions_with(
                &format!("{SHOP}\n{setup}"),
                &format!("ALTER TABLE orders DROP CONSTRAINT {name};\n{next};"),
            );
            assert_eq!(reason(predictions[0].clone()), wanted, "{setup}");
            assert!(
                reason(predictions[1].clone()).starts_with("kilit lost track of this table"),
                "{setup}"
            );
        }
    }

    #[test]
    fn drop_constraint_without_the_table_is_unknown() {
        assert_eq!(
            reason(only("ALTER TABLE orders DROP CONSTRAINT chk_amount")),
            "kilit does not know this table, so it cannot tell what kind of constraint \
             chk_amount is"
        );
        let lost = format!("{SHOP}ALTER TABLE orders CONVERT TO CHARACTER SET latin1;");
        assert!(
            reason(predictions_with(&lost, "ALTER TABLE orders DROP CONSTRAINT c").remove(0))
                .starts_with("kilit lost track of this table")
        );
    }

    #[test]
    fn drop_check_and_alter_check_get_the_lab_answer_and_keep_the_table() {
        for (setup, sql, case, answer) in [
            (
                CHECK_ALONE,
                "ALTER TABLE orders DROP CHECK chk_amount",
                "drop-check-constraint",
                INSTANT_ANSWER,
            ),
            (
                CHECK_TWO_OFF,
                "alter table orders drop check `CHK_AMOUNT`",
                "drop-check-constraint",
                INSTANT_ANSWER,
            ),
            (
                CHECK_ALONE,
                "ALTER TABLE orders ALTER CHECK chk_amount NOT ENFORCED",
                "alter-check-constraint",
                INSTANT_ANSWER,
            ),
            (
                CHECK_ALONE_OFF,
                "ALTER TABLE orders ALTER CONSTRAINT chk_amount NOT ENFORCED",
                "alter-check-constraint",
                INSTANT_ANSWER,
            ),
            (
                CHECK_ALONE,
                "ALTER TABLE orders ALTER CONSTRAINT chk_amount ENFORCED",
                "alter-check-constraint",
                INSTANT_ANSWER,
            ),
            (
                CHECK_ALONE_OFF,
                "ALTER TABLE orders ALTER CHECK chk_amount ENFORCED",
                "enforce-check-constraint",
                COPY_ANSWER,
            ),
        ] {
            let predictions = predictions_with(
                &format!("{SHOP}\n{setup}"),
                &format!("{sql};\nALTER TABLE orders ADD COLUMN coupon INT NULL;"),
            );
            assert_eq!(said(&predictions[0]), answer, "{sql}");
            assert_eq!(measured(predictions[0].clone()).0, case, "{sql}");
            // The table stays known: no note for a table kilit lost.
            let (_, _, notes) = measured(predictions[1].clone());
            assert!(
                notes.iter().all(|note| !note.contains("FULLTEXT")),
                "{sql}: {notes:?}"
            );
        }
    }

    #[test]
    fn enforcing_a_check_again_follows_what_the_statements_before_did() {
        let predictions = predictions_with(
            &format!("{SHOP}\n{CHECK_ALONE}"),
            "ALTER TABLE orders ALTER CHECK chk_amount NOT ENFORCED;
             ALTER TABLE orders ALTER CHECK chk_amount ENFORCED;
             ALTER TABLE orders ALTER CONSTRAINT chk_amount ENFORCED;",
        );
        assert_eq!(said(&predictions[0]), INSTANT_ANSWER);
        assert_eq!(said(&predictions[1]), COPY_ANSWER);
        let (_, _, notes) = measured(predictions[1].clone());
        assert!(
            notes.iter().any(|note| note.contains("error 3819")),
            "{notes:?}"
        );
        assert_eq!(said(&predictions[2]), INSTANT_ANSWER);
        // A table created in a dump with the constraint off.
        let dump = "CREATE TABLE `orders` (
              `id` bigint unsigned NOT NULL,
              `amount` int NOT NULL,
              PRIMARY KEY (`id`),
              CONSTRAINT `orders_chk_1` CHECK ((`amount` > 0)) /*!80016 NOT ENFORCED */
            ) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_0900_ai_ci;";
        assert_eq!(
            said(
                &predictions_with(dump, "ALTER TABLE orders ALTER CHECK orders_chk_1 ENFORCED")[0]
            ),
            COPY_ANSWER
        );
    }

    #[test]
    fn a_dropped_check_no_longer_stops_a_column_drop() {
        let predictions = predictions_with(
            &format!("{SHOP}\n{CHECK_TWO}"),
            &format!("ALTER TABLE orders DROP CHECK chk_amount;\n{DROP_AMOUNT};"),
        );
        assert_eq!(said(&predictions[0]), INSTANT_ANSWER);
        assert_eq!(said(&predictions[1]), INSTANT_ANSWER);
        // A constraint no longer enforced still stops it (run by hand).
        let predictions = predictions_with(
            &format!("{SHOP}\n{CHECK_TWO}"),
            &format!("ALTER TABLE orders ALTER CHECK chk_amount NOT ENFORCED;\n{DROP_AMOUNT};"),
        );
        assert_eq!(fails_with(&predictions[1]), Ok(3959));
    }

    #[test]
    fn a_check_clause_kilit_cannot_answer_says_why() {
        let schema = format!(
            "{SHOP}\n{CHECK_ALONE}\n\
             ALTER TABLE orders ADD CONSTRAINT uq_note UNIQUE (note);\n\
             ALTER TABLE orders ADD INDEX ix_note (note);"
        );
        for (sql, why, lost) in [
            (
                "ALTER TABLE orders DROP CHECK uq_note",
                "uq_note is a unique index of orders, not a CHECK constraint, and MySQL 8.0.46 \
                 and 8.4.13 do not accept DROP CHECK for it",
                false,
            ),
            (
                "ALTER TABLE orders ALTER CHECK `PRIMARY` NOT ENFORCED",
                "PRIMARY is the primary key of orders, not a CHECK constraint, and MySQL 8.0.46 \
                 and 8.4.13 do not accept ALTER CHECK for it",
                false,
            ),
            (
                "ALTER TABLE orders ALTER CONSTRAINT ix_note ENFORCED",
                "ix_note is an index of orders, not a CHECK constraint, and MySQL 8.0.46 and \
                 8.4.13 do not accept ALTER CONSTRAINT for it",
                false,
            ),
            // MySQL refuses it (the lab: 3821), and the table stays known.
            (
                "ALTER TABLE orders DROP CHECK nosuch",
                "orders has no CHECK constraint nosuch that kilit knows of. If it has none, \
                 MySQL fails with error 3821 (drop-check-missing); kilit does not answer that \
                 from its tables alone, which may be out of date",
                false,
            ),
        ] {
            let predictions = predictions_with(
                &schema,
                &format!("{sql};\nALTER TABLE orders ADD COLUMN coupon INT NULL;"),
            );
            assert_eq!(reason(predictions[0].clone()), why, "{sql}");
            let (_, _, notes) = measured(predictions[1].clone());
            assert_eq!(
                notes.iter().any(|note| note.contains("FULLTEXT")),
                lost,
                "{sql}: {notes:?}"
            );
        }
        // Without the table, the drop of a CHECK constraint is only likely,
        // and the name the table may not have is a note.
        let unseen = predictions_with("", "ALTER TABLE orders DROP CHECK chk_amount").remove(0);
        assert!(
            reason(unseen.clone()).starts_with("no --schema was given, so kilit does not know"),
            "{unseen:?}"
        );
        let (case, _, notes) = likely(unseen);
        assert_eq!(case, "drop-check-constraint");
        assert!(
            notes
                .iter()
                .any(|note| note.contains("fails with error 3821 (drop-check-missing)")),
            "{notes:?}"
        );
        assert!(
            !notes
                .iter()
                .any(|note| note.contains("drop-constraint-missing")),
            "{notes:?}"
        );
    }

    #[test]
    fn a_check_next_to_what_the_lab_has_not_measured_together_is_unknown() {
        // By hand MySQL refuses some of these with 3959, and others with 1828
        // or 3108: which error comes first depends on the pair. A constraint
        // that uses the column alone goes with it, unless something refuses.
        for check in ["amount > 0", "amount + user_id > 0"] {
            let check = format!("ALTER TABLE orders ADD CONSTRAINT chk CHECK ({check});");
            for (other, what) in [
                (VIRTUAL_COLUMN, "the generated column amount2 uses it"),
                (
                    "ALTER TABLE orders ADD FULLTEXT INDEX ft_note (note);",
                    "orders has the FULLTEXT index ft_note",
                ),
                (
                    "ALTER TABLE orders ADD UNIQUE INDEX uq_amount (amount);",
                    "amount is in the index uq_amount, not a plain one",
                ),
            ] {
                let setup = format!("{other}{check}");
                for sql in [DROP_AMOUNT, RENAME_AMOUNT] {
                    assert_eq!(
                        reason(on_shop(&setup, sql)),
                        format!(
                            "the CHECK constraint chk uses amount, and {what}, which has not \
                             been measured together"
                        ),
                        "{setup} {sql}"
                    );
                }
            }
        }
        // A foreign key's column.
        let setup = "ALTER TABLE orders ADD CONSTRAINT fk_orders_user FOREIGN KEY (user_id) \
                     REFERENCES users (id);
                     ALTER TABLE orders ADD CONSTRAINT chk CHECK (amount + user_id > 0);";
        for sql in [
            "ALTER TABLE orders DROP COLUMN user_id",
            "ALTER TABLE orders RENAME COLUMN user_id TO customer_id",
        ] {
            assert_eq!(
                reason(on_shop(setup, sql)),
                "the CHECK constraint chk uses user_id, and user_id is in the foreign key \
                 fk_orders_user, which has not been measured together",
                "{sql}"
            );
        }
        // A generated column itself.
        let setup =
            format!("{VIRTUAL_COLUMN}ALTER TABLE orders ADD CONSTRAINT chk CHECK (amount2 > 0);");
        assert_eq!(
            reason(on_shop(&setup, "ALTER TABLE orders DROP COLUMN amount2")),
            "the CHECK constraint chk uses amount2, and amount2 is a generated column, which has \
             not been measured together"
        );
        // A functional index, the primary key, and a column another table
        // points at.
        let setup = "ALTER TABLE orders ADD INDEX i_lower ((LOWER(note)));
                     ALTER TABLE orders ADD CONSTRAINT chk CHECK (note IS NULL);";
        assert_eq!(
            reason(on_shop(
                setup,
                "ALTER TABLE orders RENAME COLUMN note TO memo"
            )),
            "the CHECK constraint chk uses note, and note is in the index i_lower, not a plain \
             one, which has not been measured together"
        );
        let dump = "CREATE TABLE parents (id INT NOT NULL, code INT NOT NULL, PRIMARY KEY (id),
                        KEY k_code (code), CONSTRAINT chk_id CHECK (id > 0),
                        CONSTRAINT chk_code CHECK (code > 0));
                    CREATE TABLE children (code INT,
                        CONSTRAINT fk_code FOREIGN KEY (code) REFERENCES parents (code));";
        assert_eq!(
            reason(
                predictions_with(dump, "ALTER TABLE parents RENAME COLUMN id TO key_id").remove(0)
            ),
            "the CHECK constraint chk_id uses id, and id is in the primary key, which has not \
             been measured together"
        );
        assert_eq!(
            reason(predictions_with(dump, "ALTER TABLE parents RENAME COLUMN code TO c").remove(0)),
            "the CHECK constraint chk_code uses code, and code is referenced by the foreign key \
             fk_code on children, which has not been measured together"
        );
    }

    #[test]
    fn change_that_renames_a_column_a_check_names_is_unknown() {
        // MySQL refuses it with error 3959 in the lab, but no scenario of the
        // repository has it.
        for (setup, name) in [
            (CHECK_ALONE, "chk_amount"),
            (CHECK_TWO, "chk_amount"),
            (
                "ALTER TABLE orders ADD CONSTRAINT chk CHECK (amount IN (1, 2));",
                "chk",
            ),
        ] {
            assert_eq!(
                reason(on_shop(
                    setup,
                    "ALTER TABLE orders CHANGE amount total INT NOT NULL"
                )),
                format!(
                    "the CHECK constraint {name} names amount, and renaming such a column with \
                     CHANGE has not been measured yet"
                ),
                "{setup}"
            );
        }
        // Keeping the name is the change the statement makes.
        assert_eq!(
            said(&on_shop(
                CHECK_TWO,
                "ALTER TABLE orders CHANGE amount amount BIGINT NOT NULL"
            )),
            COPY_ANSWER
        );
    }

    #[test]
    fn a_statement_with_no_answer_is_followed_and_the_check_with_it() {
        // MySQL refuses the first, so `total` does not exist there, but kilit
        // follows what it gives no answer for: the constraint follows too.
        let predictions = predictions_with(
            &format!("{SHOP}\n{CHECK_TWO}"),
            "ALTER TABLE orders CHANGE amount total INT NOT NULL;
             ALTER TABLE orders DROP COLUMN total;
             ALTER TABLE orders DROP COLUMN user_id;",
        );
        assert!(reason(predictions[0].clone()).contains("with CHANGE"));
        assert_eq!(fails_with(&predictions[1]), Ok(3959));
        assert_eq!(fails_with(&predictions[2]), Ok(3959));
        // Dropping both columns in one statement has no answer. The constraint
        // stays, and names columns the table no longer has.
        let predictions = predictions_with(
            &format!("{SHOP}\n{CHECK_TWO}"),
            "ALTER TABLE orders DROP COLUMN amount, DROP COLUMN user_id;
             ALTER TABLE orders ADD COLUMN amount INT NOT NULL;
             ALTER TABLE orders DROP COLUMN amount;",
        );
        assert!(reason(predictions[0].clone()).starts_with("2 changes in one statement"));
        assert_eq!(said(&predictions[1]), INSTANT_ANSWER);
        assert_eq!(
            reason(predictions[2].clone()),
            "the CHECK constraint chk_amount uses user_id, a column orders does not have, as far \
             as kilit can tell"
        );
    }

    #[test]
    fn a_check_on_a_column_changes_no_answer_for_other_changes_to_it() {
        // The lab gave the same results with and without the constraint, on
        // both servers, for all of these.
        let check_note =
            "ALTER TABLE orders ADD CONSTRAINT chk_note CHECK (note IS NULL OR amount > 0);";
        for (sql, answer) in [
            (
                "ALTER TABLE orders MODIFY note VARCHAR(60) NULL",
                "INPLACE, no rebuild, writes continue",
            ),
            (
                "ALTER TABLE orders MODIFY note VARCHAR(40) NULL",
                COPY_ANSWER,
            ),
            (
                "ALTER TABLE orders MODIFY note VARCHAR(100) NULL",
                COPY_ANSWER,
            ),
            (
                "ALTER TABLE orders MODIFY amount BIGINT NOT NULL",
                COPY_ANSWER,
            ),
            (
                "ALTER TABLE orders MODIFY amount INT NULL",
                "INPLACE, rebuilds the table, writes continue",
            ),
            (
                "ALTER TABLE orders ALTER COLUMN amount SET DEFAULT 5",
                INSTANT_ANSWER,
            ),
            (
                "ALTER TABLE orders ADD INDEX i_amount (amount)",
                "INPLACE, no rebuild, writes continue",
            ),
        ] {
            assert_eq!(said(&on_shop(check_note, sql)), answer, "{sql}");
        }
    }

    #[test]
    fn case_ids_are_unique() {
        // A remembered agreement is found by the case id.
        let mut ids: Vec<&str> = CASES.iter().chain(&MYISAM_CASES).map(|c| c.id).collect();
        ids.sort_unstable();
        for pair in ids.windows(2) {
            assert_ne!(pair[0], pair[1]);
        }
    }

    #[test]
    fn a_remembered_agreement_is_the_one_judged_again() {
        let knowledge = Knowledge::built_in();
        let mut agreements = Agreements::default();
        // Twice over, so the second round reads what the first remembered,
        // with both servers in the same memory.
        for _ in 0..2 {
            for results in knowledge.servers() {
                for case in CASES.iter().chain(&MYISAM_CASES) {
                    let groups = std::iter::once((Group::Case, case.scenarios)).chain(
                        case.exceptions
                            .iter()
                            .enumerate()
                            .map(|(at, exception)| (Group::Exception(at), exception.scenarios)),
                    );
                    for (group, scenarios) in groups {
                        for variant in Variant::ALL {
                            assert_eq!(
                                agreements.agreed(case, group, variant, results),
                                agreed(scenarios, case.id, variant, results),
                                "{} {group:?} {variant} on {}",
                                case.id,
                                results.server.name()
                            );
                        }
                    }
                }
            }
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

    /// The lab's tables with `orders` as MyISAM, as mysqldump writes them.
    const MYISAM_ORDERS: &str = "
        CREATE TABLE users (
            id BIGINT UNSIGNED AUTO_INCREMENT PRIMARY KEY,
            name VARCHAR(100) NOT NULL
        ) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4;
        CREATE TABLE orders (
            id BIGINT UNSIGNED AUTO_INCREMENT PRIMARY KEY,
            user_id BIGINT UNSIGNED NOT NULL,
            amount INT NOT NULL,
            email VARCHAR(255) NULL,
            note VARCHAR(50) NULL
        ) ENGINE=MyISAM AUTO_INCREMENT=3 DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_0900_ai_ci;";

    /// The prediction for the last statement of `sql`, with `schema`.
    fn last_with(schema: &str, sql: &str) -> Prediction {
        predictions_with(schema, sql).pop().unwrap()
    }

    fn case_of(prediction: Prediction) -> &'static str {
        measured(prediction).0
    }

    /// The case of a measured prediction, or the likely one on a table
    /// kilit does not know: a failure from the statement alone stays
    /// measured there.
    fn said_case(prediction: Prediction) -> String {
        match prediction {
            Prediction::Measured { case, .. } => case.to_owned(),
            unknown => likely(unknown).0,
        }
    }

    #[test]
    fn tables_of_other_engines_get_no_answer() {
        for engine in ["MEMORY", "ARCHIVE", "BLACKHOLE"] {
            let schema = MYISAM_ORDERS.replace("ENGINE=MyISAM", &format!("ENGINE={engine}"));
            let reason = reason(last_with(
                &schema,
                "ALTER TABLE orders ADD COLUMN c INT NULL",
            ));
            assert!(reason.contains(engine), "{reason}");
        }
    }

    #[test]
    fn a_table_created_after_default_storage_engine_changes_gets_no_answer() {
        for set in [
            "SET default_storage_engine = MyISAM",
            "SET GLOBAL default_storage_engine = MyISAM",
            "SET @@session.default_storage_engine = DEFAULT",
        ] {
            let sql =
                format!("{set}; CREATE TABLE t (id INT); ALTER TABLE t ADD COLUMN c INT NULL;");
            let reason = reason(predictions(&sql).pop().unwrap());
            assert!(reason.contains("default_storage_engine"), "{reason}");
            let sql = format!(
                "{set}; CREATE TABLE t (id INT) ENGINE=InnoDB; ALTER TABLE t ADD COLUMN c INT NULL;"
            );
            assert_eq!(case_of(predictions(&sql).pop().unwrap()), "add-column");
        }
    }

    #[test]
    fn a_lost_table_gets_the_innodb_answer_as_its_likely_one_only_when_it_was_innodb() {
        let add = "ALTER TABLE orders ADD COLUMN c INT NULL";
        let innodb = MYISAM_ORDERS.replace("ENGINE=MyISAM", "ENGINE=InnoDB");
        assert_eq!(
            likely(last_with(
                MYISAM_ORDERS,
                &format!("ALTER TABLE orders ENGINE=InnoDB, AUTO_INCREMENT=5; {add}")
            ))
            .0,
            "add-column"
        );
        assert_eq!(
            likely(last_with(
                &innodb,
                &format!("ALTER TABLE orders ENGINE=InnoDB, AUTO_INCREMENT=5; {add}")
            ))
            .0,
            "add-column"
        );
        for (schema, before) in [
            (
                innodb.as_str(),
                "ALTER TABLE orders ENGINE=MyISAM, AUTO_INCREMENT=5",
            ),
            (
                MYISAM_ORDERS,
                "ALTER TABLE orders ADD COLUMN x INT, ALGORITHM=SOMETHING",
            ),
        ] {
            let reason = reason(last_with(schema, &format!("{before}; {add}")));
            assert!(reason.contains("MyISAM"), "{reason}");
        }
        // A table kilit never knew stays InnoDB.
        assert_eq!(likely(only(add)).0, "add-column");
    }

    #[test]
    fn a_foreign_key_from_an_innodb_table_to_a_myisam_one_gets_no_answer() {
        let schema = MYISAM_ORDERS
            .replace("ENGINE=MyISAM", "ENGINE=InnoDB")
            .replacen("ENGINE=InnoDB", "ENGINE=MyISAM", 1);
        let reason = reason(last_with(
            &schema,
            "ALTER TABLE orders ADD CONSTRAINT fk FOREIGN KEY (user_id) REFERENCES users (id)",
        ));
        assert!(reason.contains("users uses the MyISAM engine"), "{reason}");
    }

    #[test]
    fn a_myisam_table_gets_the_answers_measured_on_myisam() {
        for (sql, case) in [
            (
                "ALTER TABLE orders ADD COLUMN c INT NULL",
                "myisam-add-column",
            ),
            ("ALTER TABLE orders DROP COLUMN note", "myisam-drop-column"),
            (
                "ALTER TABLE orders RENAME COLUMN note TO memo",
                "myisam-rename-column",
            ),
            (
                "ALTER TABLE orders ALTER COLUMN note SET DEFAULT 'x'",
                "myisam-set-column-default",
            ),
            (
                "ALTER TABLE orders MODIFY amount BIGINT NOT NULL",
                "myisam-change-integer-type",
            ),
            (
                "ALTER TABLE orders ADD INDEX i (amount)",
                "myisam-add-index",
            ),
            (
                "ALTER TABLE orders ADD UNIQUE INDEX u (amount)",
                "myisam-add-unique-index",
            ),
            (
                "ALTER TABLE orders ADD FULLTEXT INDEX f (note)",
                "myisam-add-fulltext-index",
            ),
            (
                "ALTER TABLE orders ADD INDEX i (amount); ALTER TABLE orders DROP INDEX i",
                "myisam-drop-index",
            ),
            (
                "ALTER TABLE orders ADD COLUMN ref INT NULL;
                 ALTER TABLE orders ADD CONSTRAINT fk FOREIGN KEY (ref) REFERENCES users (id)",
                "myisam-add-foreign-key",
            ),
            (
                "ALTER TABLE orders ADD CONSTRAINT fk FOREIGN KEY (user_id) REFERENCES users (id);
                 ALTER TABLE orders DROP COLUMN user_id",
                "myisam-drop-column",
            ),
            (
                "ALTER TABLE orders ADD FOREIGN KEY (user_id) REFERENCES users (id);
                 ALTER TABLE orders DROP INDEX user_id",
                "myisam-drop-index",
            ),
            (
                "ALTER TABLE orders DROP FOREIGN KEY anything",
                "myisam-drop-foreign-key",
            ),
        ] {
            assert_eq!(case_of(last_with(MYISAM_ORDERS, sql)), case, "{sql}");
        }
    }

    #[test]
    fn a_myisam_key_counts_its_bytes_in_utf8mb4_only() {
        let index = |definition: &str, key: &str| {
            last_with(
                MYISAM_ORDERS,
                &format!(
                    "ALTER TABLE orders ADD COLUMN code {definition} NULL;
                     ALTER TABLE orders ADD {key} k (code)"
                ),
            )
        };
        assert_eq!(case_of(index("VARCHAR(250)", "INDEX")), "myisam-add-index");
        assert_eq!(
            case_of(index("VARCHAR(250)", "UNIQUE INDEX")),
            "myisam-add-unique-index"
        );
        for key in ["INDEX", "UNIQUE INDEX"] {
            let (case, verdict, _) = measured(index("VARCHAR(251)", key));
            assert_eq!(case, "myisam-add-key-too-long");
            assert!(verdict.contains("1071"), "{verdict}");
        }
        for definition in [
            "VARCHAR(10) CHARACTER SET latin1",
            "CHAR(10)",
            "DATE",
            "TEXT",
        ] {
            assert!(
                matches!(index(definition, "INDEX"), Prediction::Unknown { .. }),
                "{definition}"
            );
        }
        for key in ["(email(10))", "(amount, user_id)"] {
            let sql = format!("ALTER TABLE orders ADD INDEX k {key}");
            assert!(
                matches!(last_with(MYISAM_ORDERS, &sql), Prediction::Unknown { .. }),
                "{sql}"
            );
        }
    }

    #[test]
    fn changes_not_measured_on_a_myisam_table_get_no_answer() {
        for sql in [
            "ALTER TABLE orders MODIFY note VARCHAR(60) NULL",
            "ALTER TABLE orders MODIFY amount SMALLINT NOT NULL",
            "ALTER TABLE orders CHANGE note memo VARCHAR(50) NULL",
            "ALTER TABLE orders ADD COLUMN c INT NULL, ALGORITHM=INSTANT, ADD COLUMN d INT",
            "ALTER TABLE orders ADD FULLTEXT INDEX f (note); ALTER TABLE orders ADD COLUMN c INT NULL",
            "ALTER TABLE orders ADD INDEX i (amount, note); ALTER TABLE orders DROP COLUMN amount",
            "ALTER TABLE orders ADD UNIQUE INDEX u (amount); ALTER TABLE orders DROP INDEX u",
            "ALTER TABLE orders ADD CONSTRAINT fk FOREIGN KEY (user_id, amount) \
             REFERENCES users (id, name)",
            "ALTER TABLE orders ADD CONSTRAINT fk FOREIGN KEY (email) REFERENCES users (name)",
            "ALTER TABLE orders ADD COLUMN g INT AS (amount * 2)",
            "ALTER TABLE orders ADD COLUMN c INT NULL DEFAULT (RAND())",
            "SET foreign_key_checks = 0;
             ALTER TABLE orders ADD CONSTRAINT fk FOREIGN KEY (user_id) REFERENCES users (id)",
        ] {
            assert!(
                matches!(last_with(MYISAM_ORDERS, sql), Prediction::Unknown { .. }),
                "{sql}"
            );
        }
    }

    /// [`SHOP`] with `options` after the character set of `orders`.
    fn shop_with(options: &str) -> String {
        let marker = "note VARCHAR(50) NULL\n        ) DEFAULT CHARSET=utf8mb4";
        assert!(SHOP.contains(marker));
        SHOP.replace(marker, &format!("{marker}{options}"))
    }

    /// The prediction for the last statement of `sql`, on [`SHOP`] with
    /// `options` on `orders`.
    fn on_orders_with(options: &str, sql: &str) -> Prediction {
        predictions_with(&shop_with(options), sql).pop().unwrap()
    }

    fn case_id(prediction: &Prediction) -> &'static str {
        match prediction {
            Prediction::Measured { case, .. } => case,
            other => panic!("expected a measured prediction, got {other:?}"),
        }
    }

    const ADD_COUPON: &str = "ALTER TABLE orders ADD COLUMN coupon VARCHAR(255) NULL";

    #[test]
    fn old_alter_table_on_in_each_session_form_copies_the_table() {
        for set in [
            "SET SESSION old_alter_table = ON",
            "SET old_alter_table = 1",
            "SET @@old_alter_table = ON",
            "SET @@session.old_alter_table = TRUE",
            "SET old_alter_table = 'ON'",
            "SET old_alter_table = 'on'",
            "SET LOCAL old_alter_table = ON",
            "SET old_alter_table = ON, foreign_key_checks = 0",
        ] {
            for alter in [ADD_COUPON, &format!("{ADD_COUPON}, ALGORITHM=DEFAULT")] {
                let prediction = on_shop("", &format!("{set};\n{alter}"));
                assert_eq!(
                    case_id(&prediction),
                    "old-alter-table-add-column",
                    "{set}; {alter}"
                );
                assert!(
                    said(&prediction).starts_with("COPY"),
                    "{set}: {prediction:?}"
                );
            }
        }
    }

    #[test]
    fn old_alter_table_off_keeps_the_answer_without_it() {
        for set in [
            "SET old_alter_table = OFF",
            "SET old_alter_table = 0",
            "SET old_alter_table = FALSE",
            "SET old_alter_table = 'OFF'",
        ] {
            let sql = format!("SET old_alter_table = ON;\n{set};\n{ADD_COUPON}");
            assert_eq!(case_id(&on_shop("", &sql)), "add-column", "{set}");
        }
    }

    #[test]
    fn old_alter_table_set_in_a_form_kilit_cannot_follow_is_unknown_from_there_on() {
        for set in [
            "SET GLOBAL old_alter_table = ON",
            "SET @@global.old_alter_table = ON",
            "SET PERSIST old_alter_table = ON",
            "SET PERSIST_ONLY old_alter_table = ON",
            "SET old_alter_table = DEFAULT",
            "SET old_alter_table = 2",
            "SET old_alter_table = YES",
            "SET old_alter_table = ON, @@global.old_alter_table = OFF",
            "SET foreign_key_checks = 0, GLOBAL old_alter_table = ON",
        ] {
            for later in [
                ADD_COUPON.to_owned(),
                format!("{ADD_COUPON}, ALGORITHM=INSTANT"),
                format!("SET old_alter_table = OFF;\n{ADD_COUPON}"),
                "ALTER TABLE orders ADD INDEX idx_amount (amount)".to_owned(),
            ] {
                let sql = format!("{set};\n{later}");
                let reason = reason(on_shop("", &sql));
                assert!(reason.contains("sets old_alter_table"), "{sql}: {reason}");
            }
        }
    }

    #[test]
    fn a_written_algorithm_wins_over_old_alter_table_where_the_lab_saw_it() {
        let on = "SET SESSION old_alter_table = ON;\n";
        for (alter, case) in [
            (format!("{ADD_COUPON}, ALGORITHM=INSTANT"), "add-column"),
            (format!("{ADD_COUPON}, ALGORITHM=INPLACE"), "add-column"),
            (
                format!("{ADD_COUPON}, ALGORITHM=INPLACE, LOCK=NONE"),
                "add-column",
            ),
            (format!("{ADD_COUPON}, ALGORITHM=COPY"), "add-column"),
            (
                "ALTER TABLE orders ADD INDEX idx_amount (amount), ALGORITHM=INPLACE".to_owned(),
                "add-index",
            ),
            (
                format!("{ADD_COUPON}, LOCK=NONE"),
                "old-alter-table-add-column",
            ),
        ] {
            let with = on_shop("", &format!("{on}{alter}"));
            assert_eq!(case_id(&with), case, "{alter}");
            if case == "add-column" || case == "add-index" {
                assert_eq!(with, on_shop("", &alter), "{alter}");
            }
        }
        let lock_none = on_shop("", &format!("{on}{ADD_COUPON}, LOCK=NONE"));
        assert!(said(&lock_none).contains("1846"), "{lock_none:?}");
    }

    #[test]
    fn with_old_alter_table_changes_the_lab_has_not_measured_are_unknown() {
        let on = "SET SESSION old_alter_table = ON;\n";
        for alter in [
            "ALTER TABLE orders ADD FULLTEXT INDEX ft_note (note)",
            "ALTER TABLE orders MODIFY COLUMN amount SMALLINT NOT NULL",
            "ALTER TABLE orders ADD COLUMN created DATE DEFAULT (CURDATE())",
            "ALTER TABLE orders CHANGE COLUMN note memo VARCHAR(50) NULL",
            "ALTER TABLE orders ADD CONSTRAINT fk_user FOREIGN KEY (user_id) REFERENCES users (id), \
             ALGORITHM=INPLACE",
            "ALTER TABLE orders ADD INDEX idx_both (amount, note)",
            "ALTER TABLE orders DROP COLUMN email, ALGORITHM=INSTANT",
        ] {
            let prediction = on_shop(
                "ALTER TABLE orders ADD INDEX idx_email (email);",
                &format!("{on}{alter}"),
            );
            assert!(
                matches!(prediction, Prediction::Unknown { .. }),
                "{alter}: {prediction:?}"
            );
        }
        // A table kilit does not know may have a FULLTEXT index.
        assert!(matches!(
            predictions(&format!("{on}{ADD_COUPON}")).pop(),
            Some(Prediction::Unknown { .. })
        ));
        // MyISAM was not measured with it.
        let myisam = predictions_with(
            "CREATE TABLE logs (id INT PRIMARY KEY, note VARCHAR(50)) ENGINE=MyISAM;",
            &format!("{on}ALTER TABLE logs ADD COLUMN coupon INT NULL"),
        );
        assert!(matches!(myisam.last(), Some(Prediction::Unknown { .. })));
    }

    #[test]
    fn each_row_format_gets_its_own_measurements() {
        let compact_email = "ALTER TABLE orders ADD INDEX idx_email (email)";
        for (options, alter, case) in [
            (
                " ROW_FORMAT=COMPRESSED",
                ADD_COUPON,
                "compressed-add-column",
            ),
            (" KEY_BLOCK_SIZE=8", ADD_COUPON, "compressed-add-column"),
            (
                " ROW_FORMAT=DEFAULT KEY_BLOCK_SIZE=8",
                ADD_COUPON,
                "compressed-add-column",
            ),
            (
                " row_format=compressed, key_block_size 8",
                ADD_COUPON,
                "compressed-add-column",
            ),
            (" ROW_FORMAT=COMPACT", ADD_COUPON, "compact-add-column"),
            (" ROW_FORMAT=REDUNDANT", ADD_COUPON, "redundant-add-column"),
            (
                " ROW_FORMAT=COMPACT",
                compact_email,
                "compact-add-key-too-long",
            ),
            (
                " ROW_FORMAT=REDUNDANT",
                compact_email,
                "redundant-add-key-too-long",
            ),
            (
                " ROW_FORMAT=COMPRESSED",
                compact_email,
                "compressed-add-index",
            ),
            (
                " ROW_FORMAT=COMPACT",
                "ALTER TABLE orders ADD INDEX idx_amount (amount)",
                "compact-add-index",
            ),
            (
                " ROW_FORMAT=COMPRESSED",
                "ALTER TABLE orders MODIFY COLUMN amount BIGINT NOT NULL",
                "compressed-change-integer-type",
            ),
            (" ROW_FORMAT=DYNAMIC", ADD_COUPON, "add-column"),
            (" ROW_FORMAT=DEFAULT", ADD_COUPON, "add-column"),
            (" KEY_BLOCK_SIZE=0", ADD_COUPON, "add-column"),
            (
                " ROW_FORMAT=DYNAMIC KEY_BLOCK_SIZE=0",
                ADD_COUPON,
                "add-column",
            ),
        ] {
            let prediction = on_orders_with(options, alter);
            assert_eq!(case_id(&prediction), case, "{options}: {alter}");
        }
        let too_long = on_orders_with(" ROW_FORMAT=COMPACT", compact_email);
        let (_, verdict, notes) = measured(too_long);
        assert!(verdict.contains("1071"), "{verdict}");
        assert_eq!(notes, [ASSUMES_STRICT_SQL_MODE]);
    }

    #[test]
    fn a_situation_measured_only_on_dynamic_is_a_note_that_says_so() {
        let prediction = on_orders_with(
            " ROW_FORMAT=COMPRESSED",
            "ALTER TABLE orders ADD UNIQUE INDEX uq_email (email)",
        );
        let (case, _, notes) = measured(prediction);
        assert_eq!(case, "compressed-add-unique-index");
        assert_eq!(notes.len(), 1, "{notes:?}");
        assert!(
            notes[0].starts_with(
                "if the columns hold duplicate values: not measured on a COMPRESSED table; \
                 on a DYNAMIC table, fails with error 1062"
            ),
            "{notes:?}"
        );
    }

    #[test]
    fn row_formats_the_lab_has_not_measured_are_unknown() {
        for (options, says) in [
            (" KEY_BLOCK_SIZE=4", "KEY_BLOCK_SIZE=8 only"),
            (
                " ROW_FORMAT=COMPRESSED KEY_BLOCK_SIZE=16",
                "KEY_BLOCK_SIZE=8 only",
            ),
            (" ROW_FORMAT=DYNAMIC KEY_BLOCK_SIZE=8", "refuse"),
            (" ROW_FORMAT=COMPACT KEY_BLOCK_SIZE=8", "refuse"),
            (" ROW_FORMAT=COMPRESSED KEY_BLOCK_SIZE=0", "not measured"),
            (" ROW_FORMAT=FIXED", "has not measured"),
            (" ROW_FORMAT=COMPRESED", "has not measured"),
            (
                " ROW_FORMAT='COMPRESSED'",
                "do not accept a quoted ROW_FORMAT",
            ),
            (
                " KEY_BLOCK_SIZE='8'",
                "do not accept a quoted KEY_BLOCK_SIZE",
            ),
        ] {
            let reason = reason(on_orders_with(options, ADD_COUPON));
            assert!(reason.contains(says), "{options}: {reason}");
        }
        let myisam = predictions_with(
            "CREATE TABLE logs (id INT PRIMARY KEY) ENGINE=MyISAM ROW_FORMAT=FIXED;",
            "ALTER TABLE logs ADD COLUMN coupon INT NULL",
        );
        assert!(matches!(myisam.as_slice(), [Prediction::Unknown { .. }]));
    }

    #[test]
    fn changes_not_measured_on_a_row_format_are_unknown() {
        for (setup, alter) in [
            ("", "ALTER TABLE orders ADD FULLTEXT INDEX ft_note (note)"),
            ("", "ALTER TABLE orders MODIFY COLUMN note VARCHAR(60) NULL"),
            (
                "",
                "ALTER TABLE orders MODIFY COLUMN amount SMALLINT NOT NULL",
            ),
            (
                "",
                "ALTER TABLE orders CHANGE COLUMN note memo VARCHAR(50) NULL",
            ),
            ("", "ALTER TABLE orders ADD INDEX idx_both (amount, note)"),
            ("", "ALTER TABLE orders ADD INDEX idx_note (note(10))"),
            (
                "ALTER TABLE orders ADD INDEX idx_note (note);",
                "ALTER TABLE orders DROP COLUMN note",
            ),
            (
                "ALTER TABLE orders ADD FULLTEXT INDEX ft_note (note);",
                "ALTER TABLE orders ADD COLUMN coupon INT NULL",
            ),
            (
                "ALTER TABLE orders ADD COLUMN body TEXT;",
                "ALTER TABLE orders ADD INDEX idx_body (body(100))",
            ),
        ] {
            for options in [" ROW_FORMAT=COMPACT", " ROW_FORMAT=COMPRESSED"] {
                let prediction =
                    predictions_with(&format!("{}\n{setup}", shop_with(options)), alter)
                        .pop()
                        .unwrap();
                assert!(
                    matches!(prediction, Prediction::Unknown { .. }),
                    "{options}: {setup} {alter}: {prediction:?}"
                );
            }
        }
    }

    #[test]
    fn innodb_default_row_format_makes_tables_without_a_row_format_unknown() {
        for set in [
            "SET GLOBAL innodb_default_row_format = COMPACT",
            "SET PERSIST innodb_default_row_format = 'compact'",
        ] {
            // Old tables too: MySQL gives a table it rebuilds the new default.
            let sql = format!("{set};\n{ADD_COUPON}");
            assert!(
                matches!(on_shop("", &sql), Prediction::Unknown { .. }),
                "{set}"
            );
            assert!(matches!(
                predictions(&sql).pop(),
                Some(Prediction::Unknown { .. })
            ));
            // A written one stays, but not the limit of a key.
            assert_eq!(
                case_id(&on_orders_with(" ROW_FORMAT=COMPACT", &sql)),
                "compact-add-column"
            );
            assert_eq!(
                case_id(&on_orders_with(" ROW_FORMAT=DYNAMIC", &sql)),
                "add-column"
            );
            let index = format!("{set};\nALTER TABLE orders ADD INDEX idx_amount (amount)");
            assert!(matches!(
                on_orders_with(" ROW_FORMAT=COMPACT", &index),
                Prediction::Unknown { .. }
            ));
        }
    }

    #[test]
    fn innodb_strict_mode_makes_new_tables_without_a_row_format_unknown() {
        let sql = "SET SESSION innodb_strict_mode = OFF;
            CREATE TABLE coupons (id INT PRIMARY KEY, code VARCHAR(20));
            ALTER TABLE coupons ADD COLUMN note VARCHAR(50) NULL;
            ALTER TABLE orders ADD COLUMN coupon VARCHAR(255) NULL;
            ALTER TABLE orders ADD INDEX idx_amount (amount)";
        let predictions = predictions_with(&shop_with(" ROW_FORMAT=COMPACT"), sql);
        assert!(
            matches!(predictions[2], Prediction::Unknown { .. }),
            "{predictions:?}"
        );
        assert_eq!(case_id(&predictions[3]), "compact-add-column");
        assert!(
            matches!(predictions[4], Prediction::Unknown { .. }),
            "{predictions:?}"
        );
    }

    #[test]
    fn a_table_lost_with_a_written_row_format_is_unknown() {
        for (options, setup, unknown_after) in [
            (
                "",
                "ALTER TABLE orders ROW_FORMAT=COMPRESSED, AUTO_INCREMENT=5;",
                true,
            ),
            (
                "",
                "ALTER TABLE orders KEY_BLOCK_SIZE=8, AUTO_INCREMENT=5;",
                true,
            ),
            (
                "",
                "ALTER TABLE orders ENGINE=InnoDB, AUTO_INCREMENT=5;",
                false,
            ),
            (
                " ROW_FORMAT=COMPACT",
                "ALTER TABLE orders ENGINE=InnoDB, AUTO_INCREMENT=5;",
                true,
            ),
        ] {
            let prediction =
                predictions_with(&format!("{}\n{setup}", shop_with(options)), ADD_COUPON)
                    .pop()
                    .unwrap();
            if unknown_after {
                assert!(
                    reason(prediction).contains("row format other than DYNAMIC"),
                    "{options} {setup}"
                );
            } else {
                // Lost, so the InnoDB DYNAMIC answer is only the likely one.
                assert_eq!(likely(prediction).0, "add-column", "{options} {setup}");
            }
        }
    }

    #[test]
    fn a_table_keeps_the_options_an_alter_table_writes() {
        for (options, setup, case) in [
            (
                "",
                "ALTER TABLE orders ROW_FORMAT=COMPRESSED",
                "compressed-add-column",
            ),
            (
                "",
                "ALTER TABLE orders KEY_BLOCK_SIZE=8",
                "compressed-add-column",
            ),
            ("", "ALTER TABLE orders ENGINE=InnoDB", "add-column"),
            ("", "ALTER TABLE orders COMMENT = 'x'", "add-column"),
            (
                " ROW_FORMAT=COMPACT",
                "ALTER TABLE orders ENGINE=InnoDB",
                "compact-add-column",
            ),
            (
                " ROW_FORMAT=COMPACT",
                "ALTER TABLE orders ROW_FORMAT=DEFAULT",
                "add-column",
            ),
            (
                " KEY_BLOCK_SIZE=8",
                "ALTER TABLE orders ROW_FORMAT=DEFAULT",
                "compressed-add-column",
            ),
        ] {
            let mut predictions =
                predictions_with(&shop_with(options), &format!("{setup};\n{ADD_COUPON}"));
            assert_eq!(
                case_id(&predictions.pop().unwrap()),
                case,
                "{options} {setup}"
            );
        }
        // Forms the lab has not measured, or MySQL refuses.
        for (options, setup, reason_has) in [
            ("", "ALTER TABLE orders ROW_FORMAT=FIXED", "FIXED"),
            (
                "",
                "ALTER TABLE orders KEY_BLOCK_SIZE=4",
                "KEY_BLOCK_SIZE=4",
            ),
            (
                " KEY_BLOCK_SIZE=8",
                "ALTER TABLE orders ROW_FORMAT=DYNAMIC",
                "KEY_BLOCK_SIZE=8 with ROW_FORMAT=DYNAMIC",
            ),
            ("", "ALTER TABLE orders ENGINE=MEMORY", "MEMORY"),
        ] {
            let mut predictions =
                predictions_with(&shop_with(options), &format!("{setup};\n{ADD_COUPON}"));
            let reason = reason(predictions.pop().unwrap());
            assert!(reason.contains(reason_has), "{options} {setup}: {reason}");
        }
    }

    #[test]
    fn a_table_comment_is_measured_and_other_options_are_not() {
        let (case, verdict, _) = measured(on_shop("", "ALTER TABLE orders COMMENT = 'Orders'"));
        assert_eq!(case, "set-table-comment");
        assert_eq!(verdict, "INPLACE, no rebuild, writes continue");
        for (sql, written) in [
            ("ALTER TABLE orders ENGINE=InnoDB", "ENGINE"),
            ("ALTER TABLE orders ROW_FORMAT=COMPRESSED", "ROW_FORMAT"),
            (
                "ALTER TABLE orders COMMENT 'x' KEY_BLOCK_SIZE 8 ROW_FORMAT COMPRESSED",
                "ROW_FORMAT and KEY_BLOCK_SIZE",
            ),
        ] {
            assert_eq!(
                reason(on_shop("", sql)),
                format!("changing a table's {written} has not been measured yet"),
                "{sql}"
            );
        }
        for (options, sql, why) in [
            (" ROW_FORMAT=COMPRESSED", "", "COMPRESSED"),
            (" ENGINE=MyISAM", "", "MyISAM"),
            ("", "SET old_alter_table = ON;", "old_alter_table"),
        ] {
            let reason = reason(on_orders_with(
                options,
                &format!("{sql}ALTER TABLE orders COMMENT = 'x'"),
            ));
            assert!(reason.contains(why), "{options}: {reason}");
        }
    }

    #[test]
    fn a_scope_keyword_carries_to_old_alter_table_after_it() {
        // MySQL: the last GLOBAL or PERSIST keyword of a SET applies to the
        // assignments after it that have none.
        for set in [
            "SET GLOBAL foreign_key_checks = 1, old_alter_table = ON",
            "SET PERSIST foreign_key_checks = 1, old_alter_table = ON",
        ] {
            let reason = reason(on_shop("", &format!("{set};\n{ADD_COUPON}")));
            assert!(reason.contains("sets old_alter_table"), "{set}: {reason}");
        }
    }

    #[test]
    fn a_quoted_value_mysql_refuses_for_old_alter_table_is_unknown() {
        // MySQL refuses '1', 'TRUE', '0' and 'FALSE' with 1231 and keeps the
        // value it had; it takes 'ON' and 'OFF'.
        for value in ["'1'", "'TRUE'", "'0'", "'FALSE'", "'YES'"] {
            let sql =
                format!("SET old_alter_table = ON;\nSET old_alter_table = {value};\n{ADD_COUPON}");
            assert!(
                reason(on_shop("", &sql)).contains("sets old_alter_table"),
                "{value}"
            );
        }
        let sql = format!("SET old_alter_table = ON;\nSET old_alter_table = 'off';\n{ADD_COUPON}");
        assert_eq!(case_id(&on_shop("", &sql)), "add-column");
    }

    #[test]
    fn a_key_too_long_on_a_compact_table_has_no_note_about_the_rows() {
        let prediction = on_orders_with(
            " ROW_FORMAT=COMPACT",
            "ALTER TABLE orders ADD UNIQUE INDEX uq_email (email)",
        );
        let (case, verdict, notes) = measured(prediction);
        assert_eq!(case, "compact-add-key-too-long");
        assert!(verdict.contains("1071"), "{verdict}");
        assert_eq!(notes, [ASSUMES_STRICT_SQL_MODE]);
    }

    #[test]
    fn a_compact_key_over_the_limit_after_sql_mode_or_descending_is_unknown() {
        // With sql_mode '' MySQL shortens a plain key to 767 bytes, with a
        // warning, instead of refusing it.
        for sql in [
            "SET SESSION sql_mode = '';\nALTER TABLE orders ADD INDEX idx_email (email)",
            "ALTER TABLE orders ADD INDEX idx_email (email DESC)",
            "ALTER TABLE orders ADD INDEX idx_amount (amount DESC)",
        ] {
            assert!(
                matches!(
                    on_orders_with(" ROW_FORMAT=COMPACT", sql),
                    Prediction::Unknown { .. }
                ),
                "{sql}"
            );
        }
    }
}
