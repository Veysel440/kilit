//! kilit-core reads MySQL schema migrations, follows the tables they
//! change, and says what MySQL will do with each statement, from what
//! kilit-lab measured on real servers.
//!
//! It does no I/O: it takes text and returns data. Reading files, talking
//! to servers and printing belong to the crates that use it.
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
mod knowledge;
mod modify;
mod parse;
mod results;
mod schema;
mod split;
mod statement;

pub use charset::{CHARSETS, Charset};
pub use check::{CASES, Case, Condition, Exception, Finding, Prediction, check};
pub use knowledge::{Knowledge, ScenarioResult, ServerResults};
pub use parse::parse;
pub use results::{Observation, Outcome, Server, Variant, Verdict, Writes};
pub use schema::{Schema, TableState};
pub use statement::{
    Algorithm, Alter, Column, DataType, ForeignKey, ForeignKeyAction, Generated, Index, IndexKind,
    IntegerSize, Lock, Operation, Position, Rename, Statement, StatementKind, Table, Variable,
};
