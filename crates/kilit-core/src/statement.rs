//! The statements and operations kilit understands.
//!
//! These are kilit's own types. The SQL parser's types change from one
//! version to the next, so they never leave `parse.rs`: a parser upgrade
//! can only break that one file, not the rules built on these types.

use std::fmt;

use serde::{Deserialize, Serialize};

/// One statement of the input.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Statement {
    /// The line where the statement starts, counting from 1.
    pub line: usize,
    /// The statement as written, without the final semicolon.
    pub sql: String,
    pub kind: StatementKind,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum StatementKind {
    /// `ALTER TABLE`, and also `CREATE INDEX` and `DROP INDEX`, which MySQL
    /// runs as an `ALTER TABLE`.
    Alter(Alter),
    /// `CREATE TABLE`: a new table, with no rows yet.
    CreateTable { table: String },
    /// `DROP TABLE`: the tables are gone, with all their rows.
    DropTable { tables: Vec<String> },
    /// `RENAME TABLE a TO b, c TO d`
    RenameTable { renames: Vec<Rename> },
    /// `SET` of session variables, such as `SET foreign_key_checks = 0`.
    /// They change how the following statements of the same session run.
    Set { variables: Vec<Variable> },
    /// A statement kilit reads but does not analyze, such as `INSERT`, or a
    /// `SET` that does not touch the session (`SET GLOBAL`, `SET @x`).
    Other,
    /// A statement kilit could not read. It must never be treated as safe.
    Unparsed { error: String },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Rename {
    pub from: String,
    pub to: String,
}

/// A session variable set by `SET`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Variable {
    /// In lowercase, without `@@session.`: `foreign_key_checks`.
    pub name: String,
    /// As written: `0`, `OFF`.
    pub value: String,
}

/// An `ALTER TABLE` statement.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Alter {
    /// The table's name without quotes; `shop.orders` if the database is named.
    pub table: String,
    /// The changes, in the order they are written.
    pub operations: Vec<Operation>,
    /// The algorithm forced with `ALGORITHM=...`. `ALGORITHM=DEFAULT`
    /// forces nothing, so it is `None` too.
    pub algorithm: Option<Algorithm>,
    /// The lock level asked for with `LOCK=...`. `LOCK=DEFAULT` asks for
    /// nothing, so it is `None` too.
    pub lock: Option<Lock>,
}

/// One change inside an `ALTER TABLE`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Operation {
    AddColumn {
        column: Column,
        position: Option<Position>,
    },
    DropColumn {
        name: String,
    },
    /// `MODIFY`: the column keeps its name and gets a new definition.
    ModifyColumn {
        column: Column,
        position: Option<Position>,
    },
    /// `CHANGE`: like `MODIFY`, but the column may get a new name too.
    ChangeColumn {
        old_name: String,
        column: Column,
        position: Option<Position>,
    },
    /// `RENAME COLUMN a TO b`: only the name changes.
    RenameColumn {
        from: String,
        to: String,
    },
    AddIndex(Index),
    DropIndex {
        name: String,
    },
    AddPrimaryKey {
        columns: Vec<String>,
    },
    DropPrimaryKey,
    AddForeignKey(ForeignKey),
    DropForeignKey {
        name: String,
    },
    /// `RENAME TO new_name`
    RenameTable {
        new_name: String,
    },
    /// A change the parser read but kilit does not understand yet, as the
    /// parser prints it. Rules must report it as unknown, never as safe.
    Other {
        sql: String,
    },
}

/// Where `FIRST` or `AFTER` puts a column.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Position {
    First,
    After(String),
}

/// A column definition, as written in `ADD COLUMN`, `MODIFY` or `CHANGE`.
///
/// It holds only what the statement says. Anything left out, such as the
/// character set, comes from the table and is not known here.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Column {
    pub name: String,
    /// The data type as the parser prints it: `VARCHAR(255)`, `BIGINT UNSIGNED`.
    pub data_type: String,
    /// `NULL` gives `Some(true)`, `NOT NULL` gives `Some(false)`. With
    /// neither, MySQL makes the column nullable, unless it is in a primary key.
    pub nullable: Option<bool>,
    /// The default value as written: `0`, `'pending'`, `CURRENT_TIMESTAMP`.
    pub default: Option<String>,
    pub auto_increment: bool,
    /// `CHARACTER SET` written on the column itself.
    pub charset: Option<String>,
    /// `COLLATE` written on the column itself.
    pub collation: Option<String>,
    pub comment: Option<String>,
    /// `ON UPDATE CURRENT_TIMESTAMP`
    pub on_update: Option<String>,
    /// `AS (expression)` for a generated column.
    pub generated: Option<Generated>,
    /// Column options kilit does not understand yet, as the parser prints
    /// them, e.g. an inline `UNIQUE`. Rules must treat a column with any of
    /// them as unknown, never as safe.
    pub other_options: Vec<String>,
}

/// A generated column's expression.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Generated {
    pub expression: String,
    /// `STORED` columns are written to every row; `VIRTUAL` ones (MySQL's
    /// default) are computed when read.
    pub stored: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Index {
    /// `None` when the statement lets MySQL pick the name.
    pub name: Option<String>,
    pub kind: IndexKind,
    /// The indexed columns, or expressions such as `email(10)`.
    pub columns: Vec<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IndexKind {
    Plain,
    Unique,
    Fulltext,
    Spatial,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ForeignKey {
    /// The constraint's name, used by `DROP FOREIGN KEY`. `None` when the
    /// statement lets MySQL pick it.
    pub name: Option<String>,
    pub columns: Vec<String>,
    pub referenced_table: String,
    pub referenced_columns: Vec<String>,
    pub on_delete: Option<ForeignKeyAction>,
    pub on_update: Option<ForeignKeyAction>,
}

/// What happens to referencing rows when the referenced row is deleted or updated.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ForeignKeyAction {
    Restrict,
    Cascade,
    SetNull,
    NoAction,
    SetDefault,
}

/// An algorithm, as forced with `ALGORITHM=...` or as picked by the server.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "UPPERCASE")]
pub enum Algorithm {
    /// Change only the table's metadata; rows are not touched.
    Instant,
    /// Work on the existing table; may still rebuild it, usually without blocking writes.
    Inplace,
    /// Create a new table and copy every row into it; writes wait meanwhile.
    Copy,
}

/// A lock level asked for with `LOCK=...`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Lock {
    /// `LOCK=NONE`: reads and writes continue, or the server refuses.
    None,
    /// `LOCK=SHARED`: reads continue, writes wait.
    Shared,
    /// `LOCK=EXCLUSIVE`: reads and writes wait.
    Exclusive,
}

impl fmt::Display for Algorithm {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Algorithm::Instant => "INSTANT",
            Algorithm::Inplace => "INPLACE",
            Algorithm::Copy => "COPY",
        })
    }
}

impl fmt::Display for Lock {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Lock::None => "NONE",
            Lock::Shared => "SHARED",
            Lock::Exclusive => "EXCLUSIVE",
        })
    }
}

/// A one-line summary, e.g. `add column coupon VARCHAR(255) NULL (after amount)`.
impl fmt::Display for Operation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Operation::AddColumn { column, position } => {
                write!(f, "add column {column}")?;
                write_position(f, position.as_ref())
            }
            Operation::DropColumn { name } => write!(f, "drop column {name}"),
            Operation::ModifyColumn { column, position } => {
                write!(f, "modify column {column}")?;
                write_position(f, position.as_ref())
            }
            Operation::ChangeColumn {
                old_name,
                column,
                position,
            } => {
                write!(f, "change column {old_name} to {column}")?;
                write_position(f, position.as_ref())
            }
            Operation::RenameColumn { from, to } => write!(f, "rename column {from} to {to}"),
            Operation::AddIndex(index) => write!(f, "add {index}"),
            Operation::DropIndex { name } => write!(f, "drop index {name}"),
            Operation::AddPrimaryKey { columns } => {
                write!(f, "add primary key ({})", columns.join(", "))
            }
            Operation::DropPrimaryKey => f.write_str("drop primary key"),
            Operation::AddForeignKey(key) => write!(f, "add {key}"),
            Operation::DropForeignKey { name } => write!(f, "drop foreign key {name}"),
            Operation::RenameTable { new_name } => write!(f, "rename table to {new_name}"),
            Operation::Other { sql } => write!(f, "not understood: {sql}"),
        }
    }
}

fn write_position(f: &mut fmt::Formatter<'_>, position: Option<&Position>) -> fmt::Result {
    match position {
        None => Ok(()),
        Some(Position::First) => f.write_str(" (first)"),
        Some(Position::After(column)) => write!(f, " (after {column})"),
    }
}

/// The definition in MySQL's order: `note VARCHAR(50) NULL DEFAULT 'x'`.
impl fmt::Display for Column {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} {}", self.name, self.data_type)?;
        if let Some(charset) = &self.charset {
            write!(f, " CHARACTER SET {charset}")?;
        }
        if let Some(collation) = &self.collation {
            write!(f, " COLLATE {collation}")?;
        }
        if let Some(generated) = &self.generated {
            let kind = if generated.stored {
                "STORED"
            } else {
                "VIRTUAL"
            };
            write!(f, " AS ({}) {kind}", generated.expression)?;
        }
        match self.nullable {
            Some(true) => f.write_str(" NULL")?,
            Some(false) => f.write_str(" NOT NULL")?,
            None => {}
        }
        if let Some(default) = &self.default {
            write!(f, " DEFAULT {default}")?;
        }
        if let Some(on_update) = &self.on_update {
            write!(f, " ON UPDATE {on_update}")?;
        }
        if self.auto_increment {
            f.write_str(" AUTO_INCREMENT")?;
        }
        if let Some(comment) = &self.comment {
            write!(f, " COMMENT '{}'", comment.replace('\'', "''"))?;
        }
        for option in &self.other_options {
            write!(f, " {option}")?;
        }
        Ok(())
    }
}

/// `unique index uq_orders_email (email)`, or `index (amount)` without a name.
impl fmt::Display for Index {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let kind = match self.kind {
            IndexKind::Plain => "index",
            IndexKind::Unique => "unique index",
            IndexKind::Fulltext => "fulltext index",
            IndexKind::Spatial => "spatial index",
        };
        f.write_str(kind)?;
        if let Some(name) = &self.name {
            write!(f, " {name}")?;
        }
        write!(f, " ({})", self.columns.join(", "))
    }
}

/// `foreign key fk_orders_user (user_id) references users (id) ON DELETE CASCADE`
impl fmt::Display for ForeignKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("foreign key")?;
        if let Some(name) = &self.name {
            write!(f, " {name}")?;
        }
        write!(
            f,
            " ({}) references {} ({})",
            self.columns.join(", "),
            self.referenced_table,
            self.referenced_columns.join(", ")
        )?;
        if let Some(action) = self.on_delete {
            write!(f, " ON DELETE {action}")?;
        }
        if let Some(action) = self.on_update {
            write!(f, " ON UPDATE {action}")?;
        }
        Ok(())
    }
}

impl fmt::Display for ForeignKeyAction {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            ForeignKeyAction::Restrict => "RESTRICT",
            ForeignKeyAction::Cascade => "CASCADE",
            ForeignKeyAction::SetNull => "SET NULL",
            ForeignKeyAction::NoAction => "NO ACTION",
            ForeignKeyAction::SetDefault => "SET DEFAULT",
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn coupon() -> Column {
        Column {
            name: "coupon".into(),
            data_type: "VARCHAR(255)".into(),
            nullable: Some(true),
            ..Column::default()
        }
    }

    #[test]
    fn operations_print_as_one_line() {
        let add = Operation::AddColumn {
            column: coupon(),
            position: Some(Position::After("amount".into())),
        };
        assert_eq!(
            add.to_string(),
            "add column coupon VARCHAR(255) NULL (after amount)"
        );

        let index = Operation::AddIndex(Index {
            name: None,
            kind: IndexKind::Unique,
            columns: vec!["user_id".into(), "email".into()],
        });
        assert_eq!(index.to_string(), "add unique index (user_id, email)");
    }

    #[test]
    fn columns_print_in_mysql_order() {
        let column = Column {
            name: "status".into(),
            data_type: "VARCHAR(20)".into(),
            charset: Some("utf8mb4".into()),
            nullable: Some(false),
            default: Some("'pending'".into()),
            comment: Some("the order's state".into()),
            ..Column::default()
        };
        assert_eq!(
            column.to_string(),
            "status VARCHAR(20) CHARACTER SET utf8mb4 NOT NULL DEFAULT 'pending' COMMENT 'the order''s state'"
        );
    }

    #[test]
    fn foreign_keys_show_their_actions() {
        let key = ForeignKey {
            name: Some("fk_orders_user".into()),
            columns: vec!["user_id".into()],
            referenced_table: "users".into(),
            referenced_columns: vec!["id".into()],
            on_delete: Some(ForeignKeyAction::Cascade),
            on_update: None,
        };
        assert_eq!(
            Operation::AddForeignKey(key).to_string(),
            "add foreign key fk_orders_user (user_id) references users (id) ON DELETE CASCADE"
        );
    }

    #[test]
    fn algorithms_serialize_in_uppercase() {
        assert_eq!(
            serde_json::to_string(&Algorithm::Inplace).unwrap(),
            r#""INPLACE""#
        );
    }
}
