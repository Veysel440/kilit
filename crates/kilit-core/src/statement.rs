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

// A whole table is larger than the other kinds, but boxing it would change
// the public type for no measurable gain: statements are read once.
#[allow(clippy::large_enum_variant)]
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum StatementKind {
    /// `ALTER TABLE`, and also `CREATE INDEX` and `DROP INDEX`, which MySQL
    /// runs as an `ALTER TABLE`.
    Alter(Alter),
    /// `CREATE TABLE`: a new table, with no rows yet.
    CreateTable {
        table: Table,
        /// `IF NOT EXISTS`: nothing happens when the table already exists.
        if_not_exists: bool,
    },
    /// `DROP TABLE`: the tables are gone, with all their rows.
    DropTable { tables: Vec<String> },
    /// `RENAME TABLE a TO b, c TO d`
    RenameTable { renames: Vec<Rename> },
    /// `SET` of session variables, such as `SET foreign_key_checks = 0`.
    /// They change how the following statements of the same session run.
    /// A global `sql_log_bin` is kept too: see [`Variable::global`].
    Set { variables: Vec<Variable> },
    /// A statement kilit reads but does not analyze, such as `INSERT`, or a
    /// `SET` that does not touch the session (`SET GLOBAL`, `SET @x`).
    Other,
    /// A statement kilit gives no answer for: it could not read it, MySQL
    /// 8.0.46 and 8.4.13 do not accept it, or kilit has not measured it
    /// (`TRUNCATE`, `LOCK TABLES`). It must never be treated as safe.
    Unparsed {
        /// Why, as kilit says it.
        error: String,
        /// For a statement kilit could not read, the table named after the
        /// word `TABLE`, if any: the statement may have changed it, so kilit
        /// stops trusting what it knew about it. `None` for a statement that
        /// changes no table's definition on the measured servers.
        table: Option<String>,
    },
}

/// A table's definition, as a `CREATE TABLE` statement gives it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Table {
    /// Without quotes; `shop.orders` if the database is named.
    pub name: String,
    /// In the table's order.
    pub columns: Vec<Column>,
    /// The primary key's columns; empty when the table has none.
    pub primary_key: Vec<String>,
    /// Plain, unique, FULLTEXT and spatial indexes.
    pub indexes: Vec<Index>,
    pub foreign_keys: Vec<ForeignKey>,
    /// CHECK constraints, also the ones written on a column: MySQL keeps
    /// those as the table's too.
    pub checks: Vec<Check>,
    /// `DEFAULT CHARSET=...`: what a column gets when it names none.
    pub charset: Option<String>,
    /// `COLLATE=...`
    pub collation: Option<String>,
    /// `ENGINE=...`, the last one when written twice, as MySQL takes it.
    /// `None` when the statement writes none: MySQL then uses the server's
    /// `default_storage_engine`, InnoDB unless something changed it.
    pub engine: Option<Engine>,
    /// `ROW_FORMAT=...`, the last one when written twice, as MySQL takes it.
    /// `None` when the statement writes none: MySQL then uses the server's
    /// `innodb_default_row_format`, DYNAMIC unless something changed it.
    pub row_format: Option<RowFormat>,
    /// `KEY_BLOCK_SIZE=...` as written, the last one when written twice:
    /// `8`. Any value but 0 makes an InnoDB table without `ROW_FORMAT`
    /// COMPRESSED.
    pub key_block_size: Option<String>,
}

/// A table's `ROW_FORMAT`, as written. The lab measured DYNAMIC, the
/// server's default, and COMPRESSED, COMPACT and REDUNDANT for the changes
/// listed in their cases.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RowFormat {
    /// `ROW_FORMAT=DEFAULT`: the server's default, as if none were written.
    Default,
    Dynamic,
    Compressed,
    Compact,
    Redundant,
    /// Any other value, as written: `FIXED`, a misspelling, or a quoted
    /// value, which MySQL does not accept.
    Other(String),
    /// A row format kilit cannot tell: the table was created without one
    /// after the input named `innodb_default_row_format` or
    /// `innodb_strict_mode`.
    Unknown,
}

impl RowFormat {
    /// The row format a `ROW_FORMAT=...` option names, in any letter case.
    /// `written` is the value as written, quotes included.
    pub fn named(written: &str) -> RowFormat {
        match written.to_ascii_uppercase().as_str() {
            "DEFAULT" => RowFormat::Default,
            "DYNAMIC" => RowFormat::Dynamic,
            "COMPRESSED" => RowFormat::Compressed,
            "COMPACT" => RowFormat::Compact,
            "REDUNDANT" => RowFormat::Redundant,
            _ => RowFormat::Other(written.to_owned()),
        }
    }
}

impl fmt::Display for RowFormat {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            RowFormat::Default => f.write_str("DEFAULT"),
            RowFormat::Dynamic => f.write_str("DYNAMIC"),
            RowFormat::Compressed => f.write_str("COMPRESSED"),
            RowFormat::Compact => f.write_str("COMPACT"),
            RowFormat::Redundant => f.write_str("REDUNDANT"),
            RowFormat::Other(written) => f.write_str(written),
            RowFormat::Unknown => f.write_str("an unknown row format"),
        }
    }
}

/// A table's storage engine. The lab measured InnoDB tables, and MyISAM
/// tables for the changes listed in its MyISAM cases.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Engine {
    InnoDb,
    MyIsam,
    /// Any other engine, as written: `MEMORY`, `ARCHIVE`.
    Other(String),
    /// An engine kilit cannot tell: the table was created after the input
    /// changed `default_storage_engine`, or a statement kilit could not
    /// read wrote one.
    Unknown,
}

impl Engine {
    /// The engine an `ENGINE=...` option names, in any letter case.
    /// `INNOBASE` is MySQL's other name for InnoDB.
    pub fn named(name: &str) -> Engine {
        if name.eq_ignore_ascii_case("InnoDB") || name.eq_ignore_ascii_case("INNOBASE") {
            Engine::InnoDb
        } else if name.eq_ignore_ascii_case("MyISAM") {
            Engine::MyIsam
        } else {
            Engine::Other(name.to_owned())
        }
    }
}

impl fmt::Display for Engine {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Engine::InnoDb => f.write_str("InnoDB"),
            Engine::MyIsam => f.write_str("MyISAM"),
            Engine::Other(name) => f.write_str(name),
            Engine::Unknown => f.write_str("an unknown engine"),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Rename {
    pub from: String,
    pub to: String,
}

/// A session variable set by `SET`, or the global `sql_log_bin`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Variable {
    /// In lowercase, without `@@session.`: `foreign_key_checks`.
    pub name: String,
    /// As written: `0`, `OFF`.
    pub value: String,
    /// `SET GLOBAL`, kept for `sql_log_bin` only: MySQL refuses it, as
    /// `sql_log_bin` belongs to the session, so kilit cannot tell what the
    /// statements after it run with.
    pub global: bool,
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
    /// `ALTER COLUMN c SET DEFAULT value`: only the default changes.
    SetColumnDefault {
        column: String,
        default: String,
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
    /// `ADD [CONSTRAINT [name]] CHECK (expression)`
    AddCheck(Check),
    /// `DROP CONSTRAINT name`. The name says what it drops: a CHECK constraint,
    /// a foreign key, a unique index or the primary key.
    DropConstraint {
        name: String,
    },
    /// `RENAME TO new_name`
    RenameTable {
        new_name: String,
    },
    /// Table options written as the statement's only changes, such as
    /// `COMMENT = 'Orders'` or `ROW_FORMAT=COMPRESSED KEY_BLOCK_SIZE=8`.
    ChangeTableOptions(TableOptions),
    /// A change the parser read but kilit does not understand yet, as the
    /// parser prints it. Rules must report it as unknown, never as safe.
    Other {
        sql: String,
    },
}

/// The table options an `ALTER TABLE` writes; `None` for each one it does
/// not write. When one is written twice, MySQL takes the last.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TableOptions {
    /// `COMMENT = '...'`, the text without its quotes.
    pub comment: Option<String>,
    pub engine: Option<Engine>,
    pub row_format: Option<RowFormat>,
    /// As written: `8`.
    pub key_block_size: Option<String>,
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
    pub data_type: DataType,
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

/// A column's data type, with the details kilit's rules look at.
///
/// Types no rule looks into are kept as the parser prints them, so two
/// columns of the same type still compare equal.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DataType {
    /// `TINYINT` to `BIGINT`. A display width such as `INT(11)` changes
    /// nothing in MySQL 8, so it is not kept; `BOOLEAN` is `TINYINT`.
    Integer { size: IntegerSize, unsigned: bool },
    /// `VARCHAR(length)`, the length counted in characters.
    VarChar { length: u64 },
    /// Any other type that stores text in a character set: `CHAR(36)`,
    /// `TEXT`, `ENUM('a','b')`. As the parser prints it.
    Text(String),
    /// Any other type, as the parser prints it: `DECIMAL(8,2)`, `JSON`.
    Other(String),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IntegerSize {
    Tiny,
    Small,
    Medium,
    Int,
    Big,
}

impl DataType {
    /// Whether values of this type are text in a character set, so that a
    /// character set and a collation apply.
    pub fn is_text(&self) -> bool {
        matches!(self, DataType::VarChar { .. } | DataType::Text(_))
    }
}

/// An empty `Other`, so that `Column` can derive `Default`.
impl Default for DataType {
    fn default() -> Self {
        DataType::Other(String::new())
    }
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

/// A CHECK constraint.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Check {
    /// `None` when the statement lets MySQL pick the name.
    pub name: Option<String>,
    /// The expression as the parser prints it, as the statement wrote it: a
    /// later rename of a column changes [`Check::columns`] only.
    pub expression: String,
    /// `NOT ENFORCED` gives `false`.
    pub enforced: bool,
    pub columns: CheckColumns,
}

/// What kilit can tell about the columns a CHECK constraint uses.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CheckColumns {
    /// A plain boolean expression, of column names, numbers, arithmetic and
    /// comparison operators, `AND`, `OR`, `NOT`, `IS [NOT] NULL` and
    /// parentheses. It uses exactly these columns.
    Plain(Vec<String>),
    /// Any other expression, with every name it has outside a string. A
    /// column it uses is among them, but a name may be no column at all, such
    /// as a function's, so kilit cannot tell which of them it uses.
    Other(Vec<String>),
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

/// In MySQL's spelling: `INT UNSIGNED`, `VARCHAR(50)`.
impl fmt::Display for DataType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            DataType::Integer { size, unsigned } => {
                f.write_str(match size {
                    IntegerSize::Tiny => "TINYINT",
                    IntegerSize::Small => "SMALLINT",
                    IntegerSize::Medium => "MEDIUMINT",
                    IntegerSize::Int => "INT",
                    IntegerSize::Big => "BIGINT",
                })?;
                if *unsigned {
                    f.write_str(" UNSIGNED")?;
                }
                Ok(())
            }
            DataType::VarChar { length } => write!(f, "VARCHAR({length})"),
            DataType::Text(name) | DataType::Other(name) => f.write_str(name),
        }
    }
}

/// `foreign_key_checks = 0`, or `GLOBAL sql_log_bin = 0`.
impl fmt::Display for Variable {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.global {
            f.write_str("GLOBAL ")?;
        }
        write!(f, "{} = {}", self.name, self.value)
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
            Operation::SetColumnDefault { column, default } => {
                write!(f, "set the default of {column} to {default}")
            }
            Operation::AddIndex(index) => write!(f, "add {index}"),
            Operation::DropIndex { name } => write!(f, "drop index {name}"),
            Operation::AddPrimaryKey { columns } => {
                write!(f, "add primary key ({})", columns.join(", "))
            }
            Operation::DropPrimaryKey => f.write_str("drop primary key"),
            Operation::AddForeignKey(key) => write!(f, "add {key}"),
            Operation::DropForeignKey { name } => write!(f, "drop foreign key {name}"),
            Operation::AddCheck(check) => write!(f, "add {check}"),
            Operation::DropConstraint { name } => write!(f, "drop constraint {name}"),
            Operation::RenameTable { new_name } => write!(f, "rename table to {new_name}"),
            Operation::ChangeTableOptions(options) => write!(f, "change table options {options}"),
            Operation::Other { sql } => write!(f, "not understood: {sql}"),
        }
    }
}

/// The options written, in MySQL's words: `COMMENT = 'Orders', ENGINE = InnoDB`.
impl fmt::Display for TableOptions {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut written = Vec::new();
        if let Some(comment) = &self.comment {
            written.push(format!("COMMENT = '{}'", comment.replace('\'', "''")));
        }
        if let Some(engine) = &self.engine {
            written.push(format!("ENGINE = {engine}"));
        }
        if let Some(row_format) = &self.row_format {
            written.push(format!("ROW_FORMAT = {row_format}"));
        }
        if let Some(size) = &self.key_block_size {
            written.push(format!("KEY_BLOCK_SIZE = {size}"));
        }
        f.write_str(&written.join(", "))
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

/// `check constraint chk_amount (amount > 0) NOT ENFORCED`, or `check
/// constraint (amount > 0)` without a name.
impl fmt::Display for Check {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("check constraint")?;
        if let Some(name) = &self.name {
            write!(f, " {name}")?;
        }
        write!(f, " ({})", self.expression)?;
        if !self.enforced {
            f.write_str(" NOT ENFORCED")?;
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
            data_type: DataType::VarChar { length: 255 },
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
            data_type: DataType::VarChar { length: 20 },
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
    fn check_constraints_show_their_name_and_enforcement() {
        let check = Check {
            name: Some("chk_amount".into()),
            expression: "amount > 0".into(),
            enforced: true,
            columns: CheckColumns::Plain(vec!["amount".into()]),
        };
        assert_eq!(
            Operation::AddCheck(check.clone()).to_string(),
            "add check constraint chk_amount (amount > 0)"
        );
        let unnamed = Check {
            name: None,
            enforced: false,
            ..check
        };
        assert_eq!(
            Operation::AddCheck(unnamed).to_string(),
            "add check constraint (amount > 0) NOT ENFORCED"
        );
        assert_eq!(
            Operation::DropConstraint {
                name: "chk_amount".into()
            }
            .to_string(),
            "drop constraint chk_amount"
        );
    }

    #[test]
    fn algorithms_serialize_in_uppercase() {
        assert_eq!(
            serde_json::to_string(&Algorithm::Inplace).unwrap(),
            r#""INPLACE""#
        );
    }

    #[test]
    fn data_types_print_in_mysql_spelling() {
        let unsigned_int = DataType::Integer {
            size: IntegerSize::Int,
            unsigned: true,
        };
        assert_eq!(unsigned_int.to_string(), "INT UNSIGNED");
        assert_eq!(DataType::VarChar { length: 50 }.to_string(), "VARCHAR(50)");
        assert!(DataType::Text("CHAR(36)".into()).is_text());
        assert!(!unsigned_int.is_text());
    }
}
