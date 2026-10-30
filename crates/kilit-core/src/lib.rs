//! kilit-core reads MySQL schema migrations and turns them into a list of
//! operations that kilit can reason about.
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

mod parse;
mod split;
mod statement;

pub use parse::parse;
pub use statement::{
    Algorithm, Alter, Column, ForeignKey, ForeignKeyAction, Generated, Index, IndexKind, Lock,
    Operation, Position, Rename, Statement, StatementKind, Variable,
};
