//! kilit-core reads MySQL schema migrations, as SQL or as the output of
//! `php artisan migrate --pretend`, follows the tables they change, and says
//! what MySQL will do with each statement, from what kilit-lab measured on
//! real servers.
//!
//! It does no I/O: it takes text and returns data. Reading files, talking
//! to servers and printing belong to the crates that use it.
//!
//! kilit-core is kilit's engine, not a library: its API may change in any
//! release.
//!
//! ```
//! use kilit_core::{Operation, StatementKind};
//!
//! let statements = kilit_core::parse("ALTER TABLE orders DROP COLUMN note;");
//!
//! let StatementKind::Alter(alter) = &statements[0].kind else {
//!     panic!("expected an ALTER TABLE");
//! };
//! assert_eq!(alter.table, "orders");
//! assert_eq!(
//!     alter.operations,
//!     [Operation::DropColumn { name: "note".into() }]
//! );
//! ```

mod charset;
mod check;
mod impact;
mod knowledge;
mod metadata_lock;
mod modify;
mod parse;
mod pretend;
mod results;
mod schema;
mod split;
mod statement;

pub use charset::{CHARSETS, Charset};
pub use check::{
    CASES, Case, Condition, Exception, Finding, KeyProblem, MYISAM_CASES, OLD_ALTER_TABLE_CASES,
    OLD_ALTER_TABLE_TWINS, Prediction, ROW_FORMAT_CASES, check, check_sessions, depends_on_rows,
};
pub use impact::{Impact, impacts, on_empty_tables, on_new_tables};
pub use knowledge::{DEFAULT_SERIES, Knowledge, ScenarioResult, ServerResults};
pub use metadata_lock::{
    ALGORITHMS_THAT_WAIT, DEFAULT_LOCK_WAIT_TIMEOUT, LOCK_WAIT_TIMEOUT_ERROR, STATEMENTS_THAT_WAIT,
    StatementThatWaits, locked_tables, metadata_lock_warning,
};
pub use parse::parse;
pub use pretend::{
    LaravelError, LaravelMigration, PRETEND_READ_NOTE, Pretend, UnreadableLine, only_reads,
    read_pretend,
};
pub use results::{Observation, Outcome, Server, Variant, Verdict, Writes};
pub use schema::{Schema, TableState};
pub use statement::{
    Algorithm, Alter, Check, CheckColumns, Column, DataType, Engine, ForeignKey, ForeignKeyAction,
    Generated, Index, IndexKind, IntegerSize, Lock, Operation, Position, Rename, RowFormat,
    Statement, StatementKind, Table, TableOptions, Variable,
};
