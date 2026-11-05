//! The tables kilit knows, and how each statement changes them.
//!
//! kilit learns tables from `CREATE TABLE` statements, such as the ones in a
//! schema dump, then follows every statement of the migration, so that each
//! statement is judged against the tables as they are at that point. When a
//! statement changes a table in a way kilit cannot follow, kilit stops
//! trusting what it knew about that table instead of guessing.

use std::collections::BTreeMap;

use crate::charset::{Charset, canonical};
use crate::parse::parse;
use crate::statement::{
    Alter, Column, ForeignKey, Index, IndexKind, Operation, Position, Statement, StatementKind,
    Table,
};

/// The tables kilit knows at one point of the input.
#[derive(Clone, Debug, Default)]
pub struct Schema {
    /// By lowercase name.
    tables: BTreeMap<String, Entry>,
}

#[derive(Clone, Debug)]
enum Entry {
    Known(Table),
    /// The table exists, but kilit could not follow a change to it.
    Lost {
        reason: String,
    },
}

/// What kilit knows about one table.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TableState<'a> {
    Known(&'a Table),
    /// kilit could not follow a change to the table; the reason says which.
    Lost {
        reason: &'a str,
    },
    /// Nothing kilit has read defines the table.
    Missing,
}

impl Schema {
    /// A schema without tables: kilit knows nothing about the database.
    pub fn new() -> Schema {
        Schema::default()
    }

    /// The tables that `sql` defines: a schema dump, such as the file that
    /// `php artisan schema:dump` or `mysqldump --no-data` writes, or any list
    /// of `CREATE TABLE` statements. Other statements in it are followed as
    /// they would be in a migration.
    pub fn from_sql(sql: &str) -> Schema {
        let mut schema = Schema::new();
        for statement in parse(sql) {
            schema.apply(&statement);
        }
        schema
    }

    /// What kilit knows about the table `name`, in any case.
    pub fn table(&self, name: &str) -> TableState<'_> {
        match self.tables.get(&key(name)) {
            Some(Entry::Known(table)) => TableState::Known(table),
            Some(Entry::Lost { reason }) => TableState::Lost { reason },
            None => TableState::Missing,
        }
    }

    /// The tables kilit knows, by name.
    pub fn tables(&self) -> impl Iterator<Item = &Table> {
        self.tables.values().filter_map(|entry| match entry {
            Entry::Known(table) => Some(table),
            Entry::Lost { .. } => None,
        })
    }

    /// The tables kilit lost track of, by lowercase name, with the reason.
    pub fn lost(&self) -> impl Iterator<Item = (&str, &str)> {
        self.tables.iter().filter_map(|(name, entry)| match entry {
            Entry::Lost { reason } => Some((name.as_str(), reason.as_str())),
            Entry::Known(_) => None,
        })
    }

    /// Follows one statement: tables are created, changed, renamed and
    /// dropped as the statement says.
    pub fn apply(&mut self, statement: &Statement) {
        match &statement.kind {
            StatementKind::CreateTable {
                table,
                if_not_exists,
            } => {
                if *if_not_exists && self.tables.contains_key(&key(&table.name)) {
                    return;
                }
                let mut table = table.clone();
                for foreign_key in table.foreign_keys.clone() {
                    index_foreign_key(&mut table, &foreign_key);
                }
                self.tables.insert(key(&table.name), Entry::Known(table));
            }
            StatementKind::DropTable { tables } => {
                for name in tables {
                    self.tables.remove(&key(name));
                }
            }
            StatementKind::RenameTable { renames } => {
                for rename in renames {
                    self.rename(&rename.from, &rename.to);
                }
            }
            StatementKind::Alter(alter) => self.alter(statement.line, alter),
            StatementKind::Unparsed {
                table: Some(name), ..
            } => self.lose(
                name,
                format!(
                    "line {}: kilit could not read a statement on it",
                    statement.line
                ),
            ),
            StatementKind::Unparsed { table: None, .. }
            | StatementKind::Set { .. }
            | StatementKind::Other => {}
        }
    }

    fn rename(&mut self, from: &str, to: &str) {
        if let Some(mut entry) = self.tables.remove(&key(from)) {
            if let Entry::Known(table) = &mut entry {
                to.clone_into(&mut table.name);
            }
            self.tables.insert(key(to), entry);
        }
    }

    fn lose(&mut self, name: &str, reason: String) {
        self.tables.insert(key(name), Entry::Lost { reason });
    }

    fn alter(&mut self, line: usize, alter: &Alter) {
        // A missing table stays missing, and a lost one stays lost.
        let Some(Entry::Known(table)) = self.tables.get(&key(&alter.table)) else {
            return;
        };
        let mut table = table.clone();
        let mut new_name = None;
        for operation in &alter.operations {
            let changed = match operation {
                Operation::RenameTable { new_name: name } => {
                    new_name = Some(name);
                    Ok(())
                }
                other => change(&mut table, other),
            };
            if let Err(reason) = changed {
                self.lose(&alter.table, format!("line {line}: {reason}"));
                return;
            }
        }
        self.tables.insert(key(&alter.table), Entry::Known(table));
        if let Some(name) = new_name {
            self.rename(&alter.table, name);
        }
    }
}

/// Table names are compared in lowercase, which is how MySQL stores them
/// on Windows and macOS and how Laravel writes them everywhere.
fn key(name: &str) -> String {
    name.to_lowercase()
}

/// Changes `table` as one `ALTER TABLE` operation does, or says why kilit
/// cannot follow it.
fn change(table: &mut Table, operation: &Operation) -> Result<(), String> {
    match operation {
        Operation::AddColumn { column, position } => {
            if table.position_of(&column.name).is_some() {
                return Err(format!("adds {}, which the table already has", column.name));
            }
            unfollowed_options(column)?;
            let at = insert_at(table, position.as_ref(), table.columns.len())?;
            table.columns.insert(at, column.clone());
        }
        Operation::DropColumn { name } => {
            let at = table.position_of(name).ok_or_else(|| no_column(name))?;
            table.columns.remove(at);
            // MySQL takes the column out of every index, and drops an index
            // left without columns.
            for index in &mut table.indexes {
                index
                    .columns
                    .retain(|column| !same(indexed_column(column), name));
            }
            table.indexes.retain(|index| !index.columns.is_empty());
            table
                .primary_key
                .retain(|column| !same(indexed_column(column), name));
        }
        Operation::ModifyColumn { column, position } => {
            replace(table, &column.name, column, position.as_ref())?;
        }
        Operation::ChangeColumn {
            old_name,
            column,
            position,
        } => replace(table, old_name, column, position.as_ref())?,
        Operation::RenameColumn { from, to } => {
            let at = table.position_of(from).ok_or_else(|| no_column(from))?;
            to.clone_into(&mut table.columns[at].name);
            rename_references(table, from, to);
        }
        Operation::SetColumnDefault { column, default } => {
            let at = table.position_of(column).ok_or_else(|| no_column(column))?;
            table.columns[at].default = Some(default.clone());
        }
        Operation::AddIndex(index) => {
            let mut index = index.clone();
            if index.name.is_none() {
                index.name = Some(generated_index_name(table, &index));
            }
            table.indexes.push(index);
        }
        Operation::DropIndex { name } => {
            let at = table
                .indexes
                .iter()
                .position(|index| index.name.as_deref().is_some_and(|n| same(n, name)))
                .ok_or_else(|| format!("drops the index {name}, which kilit does not know"))?;
            table.indexes.remove(at);
        }
        Operation::AddPrimaryKey { columns } => columns.clone_into(&mut table.primary_key),
        Operation::DropPrimaryKey => table.primary_key.clear(),
        Operation::AddForeignKey(foreign_key) => {
            table.foreign_keys.push(foreign_key.clone());
            index_foreign_key(table, foreign_key);
        }
        Operation::DropForeignKey { name } => {
            let at = table
                .foreign_keys
                .iter()
                .position(|key| key.name.as_deref().is_some_and(|n| same(n, name)))
                .ok_or_else(|| {
                    format!("drops the foreign key {name}, which kilit does not know")
                })?;
            // The index MySQL made for the key stays.
            table.foreign_keys.remove(at);
        }
        // The caller moves the table to its new name.
        Operation::RenameTable { .. } => {}
        Operation::Other { sql } => return Err(format!("kilit does not follow `{sql}`")),
    }
    Ok(())
}

/// MODIFY and CHANGE: the column gets its new definition, and its new name
/// everywhere the table refers to it.
fn replace(
    table: &mut Table,
    old_name: &str,
    column: &Column,
    position: Option<&Position>,
) -> Result<(), String> {
    let at = table
        .position_of(old_name)
        .ok_or_else(|| no_column(old_name))?;
    unfollowed_options(column)?;
    table.columns.remove(at);
    let to = insert_at(table, position, at)?;
    table.columns.insert(to, column.clone());
    if !same(old_name, &column.name) {
        rename_references(table, old_name, &column.name);
    }
    Ok(())
}

/// Options such as an inline `UNIQUE` add keys that kilit does not track
/// on a column definition.
fn unfollowed_options(column: &Column) -> Result<(), String> {
    if column.other_options.is_empty() {
        Ok(())
    } else {
        Err(format!(
            "gives {} options kilit does not follow: {}",
            column.name,
            column.other_options.join(" ")
        ))
    }
}

/// Where a column goes: `FIRST`, `AFTER` another, or `default`.
fn insert_at(table: &Table, position: Option<&Position>, default: usize) -> Result<usize, String> {
    match position {
        None => Ok(default),
        Some(Position::First) => Ok(0),
        Some(Position::After(name)) => table
            .position_of(name)
            .map(|at| at + 1)
            .ok_or_else(|| no_column(name)),
    }
}

fn rename_references(table: &mut Table, from: &str, to: &str) {
    let entries = table
        .indexes
        .iter_mut()
        .flat_map(|index| &mut index.columns)
        .chain(&mut table.primary_key);
    for entry in entries {
        if same(indexed_column(entry), from) {
            // Keep a prefix length such as `(10)`.
            *entry = format!("{to}{}", &entry[indexed_column(entry).len()..]);
        }
    }
    for key in &mut table.foreign_keys {
        for column in &mut key.columns {
            if same(column, from) {
                to.clone_into(column);
            }
        }
    }
}

/// MySQL indexes a foreign key's columns when no index starts with them,
/// and names the index after the constraint. A prefix such as `note(10)`
/// does not count: MySQL adds its own index then.
fn index_foreign_key(table: &mut Table, key: &ForeignKey) {
    let starts_with_key = |columns: &[String]| {
        columns.len() >= key.columns.len()
            && key.columns.iter().zip(columns).all(|(a, b)| same(a, b))
    };
    let indexed = starts_with_key(&table.primary_key)
        || table.indexes.iter().any(|index| {
            matches!(index.kind, IndexKind::Plain | IndexKind::Unique)
                && starts_with_key(&index.columns)
        });
    if !indexed {
        let name = key
            .name
            .clone()
            .unwrap_or_else(|| key.columns.first().cloned().unwrap_or_default());
        table.indexes.push(Index {
            name: Some(name),
            kind: IndexKind::Plain,
            columns: key.columns.clone(),
        });
    }
}

/// The name MySQL gives an index added without one: its first column's,
/// with `_2`, `_3` and so on when that is taken.
fn generated_index_name(table: &Table, index: &Index) -> String {
    let base = index
        .columns
        .first()
        .map(|column| indexed_column(column).to_owned())
        .unwrap_or_default();
    let taken = |name: &str| {
        table
            .indexes
            .iter()
            .any(|index| index.name.as_deref().is_some_and(|n| same(n, name)))
    };
    let mut name = base.clone();
    let mut number = 2;
    while taken(&name) {
        name = format!("{base}_{number}");
        number += 1;
    }
    name
}

/// The column of an index entry: `email` in `email(10)`. An expression
/// such as `(lower(email))` is returned whole and matches no column.
fn indexed_column(entry: &str) -> &str {
    if entry.starts_with('(') {
        return entry;
    }
    entry.split('(').next().unwrap_or(entry)
}

/// MySQL compares column and index names without regard to case.
fn same(a: &str, b: &str) -> bool {
    a.eq_ignore_ascii_case(b)
}

fn no_column(name: &str) -> String {
    format!("names the column {name}, which kilit does not know")
}

/// A text column's character set and collation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct TextSettings {
    pub charset: &'static Charset,
    pub collation: String,
    /// The column names neither, so both come from the table.
    pub from_table: bool,
}

impl Table {
    /// The column named `name`, in any case.
    pub fn column(&self, name: &str) -> Option<&Column> {
        self.columns.iter().find(|column| same(&column.name, name))
    }

    pub(crate) fn position_of(&self, name: &str) -> Option<usize> {
        self.columns
            .iter()
            .position(|column| same(&column.name, name))
    }

    /// Whether `column` can hold NULL: as written, and otherwise yes,
    /// unless the column is in the primary key.
    pub fn is_nullable(&self, column: &Column) -> bool {
        column
            .nullable
            .unwrap_or_else(|| !self.in_primary_key(&column.name))
    }

    /// Whether `column` is in the primary key, also with a prefix length.
    pub(crate) fn in_primary_key(&self, column: &str) -> bool {
        self.primary_key
            .iter()
            .any(|entry| same(indexed_column(entry), column))
    }

    /// The table's FULLTEXT index, if it has one.
    pub fn fulltext_index(&self) -> Option<&Index> {
        self.indexes
            .iter()
            .find(|index| index.kind == IndexKind::Fulltext)
    }

    /// The name of an index that `column` is in, the primary key included.
    pub fn index_on(&self, column: &str) -> Option<String> {
        if self.in_primary_key(column) {
            return Some("PRIMARY".to_owned());
        }
        self.indexes
            .iter()
            .find(|index| {
                index
                    .columns
                    .iter()
                    .any(|entry| same(indexed_column(entry), column))
            })
            .map(|index| index.name.clone().unwrap_or_default())
    }

    /// The foreign key that `column` is in, if any.
    pub fn foreign_key_on(&self, column: &str) -> Option<&ForeignKey> {
        self.foreign_keys
            .iter()
            .find(|key| key.columns.iter().any(|c| same(c, column)))
    }

    /// The character set and collation of a text column: the column's own,
    /// or else the table's. A collation alone names its character set, and
    /// a character set alone comes with its default collation, as in MySQL.
    pub(crate) fn text_settings(&self, column: &Column) -> Result<TextSettings, String> {
        let from_collation = |collation: &str, from_table| {
            let charset = Charset::of_collation(collation)
                .ok_or_else(|| format!("kilit does not know the collation {collation}"))?;
            Ok(TextSettings {
                charset,
                collation: canonical(collation),
                from_table,
            })
        };
        let from_charset = |name: &str, from_table| {
            let charset = Charset::named(name)
                .ok_or_else(|| format!("kilit does not know the character set {name}"))?;
            Ok(TextSettings {
                charset,
                collation: charset.default_collation.to_owned(),
                from_table,
            })
        };
        match (
            &column.collation,
            &column.charset,
            &self.collation,
            &self.charset,
        ) {
            (Some(collation), ..) => from_collation(collation, false),
            (None, Some(charset), ..) => from_charset(charset, false),
            (None, None, Some(collation), _) => from_collation(collation, true),
            (None, None, None, Some(charset)) => from_charset(charset, true),
            (None, None, None, None) => Err(format!(
                "{} names no character set, so kilit cannot tell which one {} uses",
                self.name, column.name
            )),
        }
    }
}

/// Whether the column definition names a character set or a collation.
pub(crate) fn names_text_settings(column: &Column) -> bool {
    column.charset.is_some() || column.collation.is_some()
}

/// `template` written three ways, with each `{name}` in it as it is, in
/// backticks as mysqldump writes it, and in capitals: the same tables for
/// MySQL, which compares column and index names without regard to case.
#[cfg(test)]
pub(crate) fn three_ways(template: &str) -> [String; 3] {
    let write = |name: fn(&str) -> String| {
        let mut written = String::new();
        let mut rest = template;
        while let Some((before, after)) = rest.split_once('{') {
            let (inside, after) = after.split_once('}').expect("a closing brace");
            written.push_str(before);
            written.push_str(&name(inside));
            rest = after;
        }
        written.push_str(rest);
        written
    };
    [
        write(str::to_owned),
        write(|name| format!("`{name}`")),
        write(str::to_uppercase),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::statement::DataType;

    /// Laravel's tables, as `php artisan schema:dump` writes them.
    const DUMP: &str = include_str!("../examples/mysql-schema.sql");

    fn known(schema: &Schema, name: &str) -> Table {
        match schema.table(name) {
            TableState::Known(table) => table.clone(),
            other => panic!("{name}: {other:?}"),
        }
    }

    /// The schema after `sql` runs on Laravel's tables.
    fn after(sql: &str) -> Schema {
        let mut schema = Schema::from_sql(DUMP);
        for statement in parse(sql) {
            schema.apply(&statement);
        }
        schema
    }

    fn column_names(table: &Table) -> Vec<&str> {
        table.columns.iter().map(|c| c.name.as_str()).collect()
    }

    #[test]
    fn reads_every_table_of_a_schema_dump() {
        let schema = Schema::from_sql(DUMP);
        let names: Vec<&str> = schema.tables().map(|t| t.name.as_str()).collect();
        assert_eq!(names, ["migrations", "orders", "shops", "users"]);
        assert_eq!(schema.lost().count(), 0);

        let orders = known(&schema, "ORDERS");
        assert_eq!(
            orders.column("note").unwrap().data_type,
            DataType::VarChar { length: 50 }
        );
        assert_eq!(
            orders.index_on("user_id").as_deref(),
            Some("orders_user_id_foreign")
        );
        assert_eq!(
            orders.foreign_key_on("user_id").unwrap().referenced_table,
            "users"
        );
        assert!(orders.fulltext_index().is_none());
    }

    #[test]
    fn follows_column_changes() {
        let schema = after(
            "alter table `orders` add `coupon` varchar(255) null after `amount`;
             alter table `orders` modify `amount` bigint not null first;
             alter table `orders` drop `legacy_code`;
             alter table `orders` rename column `note` to `memo`;
             alter table `orders` alter column `status` set default 'paid'",
        );
        let orders = known(&schema, "orders");
        assert_eq!(
            column_names(&orders),
            [
                "amount",
                "id",
                "user_id",
                "coupon",
                "status",
                "memo",
                "legacy_flag",
                "created_at",
                "updated_at"
            ]
        );
        assert_eq!(
            orders.column("amount").unwrap().data_type.to_string(),
            "BIGINT"
        );
        assert_eq!(
            orders.column("status").unwrap().default.as_deref(),
            Some("'paid'")
        );
    }

    #[test]
    fn follows_index_and_key_changes() {
        let schema = after(
            "alter table `orders` add fulltext `orders_note_fulltext`(`note`);
             alter table `orders` change `note` `memo` varchar(50) null;
             alter table `orders` add index (`amount`);
             alter table `orders` add index (`amount`, `created_at`);
             alter table `orders` drop index `orders_status_index`;
             alter table `orders` add `shop_id` bigint unsigned null;
             alter table `orders` add constraint `orders_shop_id_foreign` \
             foreign key (`shop_id`) references `shops` (`id`)",
        );
        let orders = known(&schema, "orders");
        assert_eq!(orders.fulltext_index().unwrap().columns, ["memo"]);
        // An index added without a name is named after its first column.
        // MySQL made the last one for the new foreign key.
        let names: Vec<&str> = orders
            .indexes
            .iter()
            .filter_map(|index| index.name.as_deref())
            .collect();
        assert_eq!(
            names,
            [
                "orders_user_id_foreign",
                "orders_note_fulltext",
                "amount",
                "amount_2",
                "orders_shop_id_foreign"
            ]
        );
        assert!(orders.index_on("status").is_none());
        assert_eq!(
            orders.index_on("shop_id").as_deref(),
            Some("orders_shop_id_foreign")
        );
    }

    #[test]
    fn dropping_a_column_takes_it_out_of_indexes() {
        let schema = after("alter table `orders` drop `status`");
        assert!(known(&schema, "orders").index_on("status").is_none());
        assert!(
            !known(&schema, "orders")
                .indexes
                .iter()
                .any(|i| i.name.as_deref() == Some("orders_status_index"))
        );
    }

    #[test]
    fn follows_tables_being_created_renamed_and_dropped() {
        let schema = after(
            "create table `coupons` (`id` bigint unsigned not null auto_increment primary key) \
             default character set utf8mb4 collate 'utf8mb4_unicode_ci';
             rename table `shops` to `stores`;
             alter table `users` rename to `customers`;
             drop table if exists `migrations`;
             create table if not exists `orders` (`id` int)",
        );
        let names: Vec<&str> = schema.tables().map(|t| t.name.as_str()).collect();
        assert_eq!(names, ["coupons", "customers", "orders", "stores"]);
        // IF NOT EXISTS left the existing table alone.
        assert!(known(&schema, "orders").column("status").is_some());
    }

    #[test]
    fn a_change_kilit_cannot_follow_loses_the_table() {
        let schema = after(
            "alter table `orders` auto_increment = 1000;
             ALTER TABLE users CONVERT TO CHARACTER SET latin1;
             alter table `shops` drop `nope`",
        );
        let lost: Vec<(&str, &str)> = schema.lost().collect();
        assert_eq!(
            lost,
            [
                (
                    "orders",
                    "line 1: kilit does not follow `AUTO_INCREMENT = 1000`"
                ),
                (
                    "shops",
                    "line 3: names the column nope, which kilit does not know"
                ),
                ("users", "line 2: kilit could not read a statement on it"),
            ]
        );
        // A new definition makes the table known again.
        let mut schema = schema;
        for statement in parse("create table users (id int)") {
            schema.apply(&statement);
        }
        assert!(matches!(schema.table("users"), TableState::Known(_)));
    }

    #[test]
    fn text_settings_come_from_the_column_or_the_table() {
        let table = Table {
            name: "t".into(),
            charset: Some("utf8mb4".into()),
            collation: Some("utf8mb4_unicode_ci".into()),
            ..Table::default()
        };
        let column = |charset: Option<&str>, collation: Option<&str>| Column {
            name: "c".into(),
            data_type: DataType::VarChar { length: 10 },
            charset: charset.map(Into::into),
            collation: collation.map(Into::into),
            ..Column::default()
        };
        let settings = |column: &Column| {
            let settings = table.text_settings(column).unwrap();
            (
                settings.charset.name,
                settings.collation,
                settings.from_table,
            )
        };
        assert_eq!(
            settings(&column(None, None)),
            ("utf8mb4", "utf8mb4_unicode_ci".into(), true)
        );
        // A character set alone comes with its own default collation.
        assert_eq!(
            settings(&column(Some("utf8mb4"), None)),
            ("utf8mb4", "utf8mb4_0900_ai_ci".into(), false)
        );
        assert_eq!(
            settings(&column(None, Some("latin1_bin"))),
            ("latin1", "latin1_bin".into(), false)
        );
        assert!(table.text_settings(&column(Some("gbk"), None)).is_err());
        let bare = Table {
            name: "t".into(),
            ..Table::default()
        };
        assert!(bare.text_settings(&column(None, None)).is_err());
    }

    #[test]
    fn primary_key_columns_are_not_nullable() {
        let schema = Schema::from_sql("CREATE TABLE t (id INT PRIMARY KEY, a INT, b INT NOT NULL)");
        let table = known(&schema, "t");
        let nullable: Vec<bool> = table.columns.iter().map(|c| table.is_nullable(c)).collect();
        assert_eq!(nullable, [false, true, false]);
    }

    /// Hand-written tables with a prefix index, a prefix primary key and a
    /// foreign key, for [`three_ways`].
    const PREFIXES: &str = "
        CREATE TABLE parents ({code} VARCHAR(50) NOT NULL, PRIMARY KEY ({code}));
        CREATE TABLE orders (
            {id} BIGINT UNSIGNED NOT NULL,
            {user_id} BIGINT UNSIGNED NOT NULL,
            {amount} INT NOT NULL,
            {note} VARCHAR(50) NULL,
            PRIMARY KEY ({id}),
            KEY {note} ({note}(10)),
            KEY {ix_amount} ({amount}),
            CONSTRAINT {fk_user} FOREIGN KEY ({user_id}) REFERENCES parents ({code})
        );
        CREATE TABLE codes (
            {code} VARCHAR(50) NOT NULL,
            {label} VARCHAR(20) NULL,
            PRIMARY KEY ({code}(10))
        );";

    /// Index names in lowercase, to compare the three ways.
    fn index_names(table: &Table) -> Vec<String> {
        table
            .indexes
            .iter()
            .map(|index| index.name.clone().unwrap_or_default().to_lowercase())
            .collect()
    }

    #[test]
    fn a_schema_dump_gives_the_same_tables_in_any_quotes_or_case() {
        for dump in three_ways(PREFIXES) {
            let schema = Schema::from_sql(&dump);
            let orders = known(&schema, "orders");
            let codes = known(&schema, "codes");
            // A prefix names its column without quotes.
            let entries: Vec<String> = orders.indexes[0]
                .columns
                .iter()
                .map(|entry| entry.to_lowercase())
                .collect();
            assert_eq!(entries, ["note(10)"], "{dump}");
            assert!(orders.column("note").is_some(), "{dump}");
            assert_eq!(
                orders.index_on("note").map(|name| name.to_lowercase()),
                Some("note".to_owned()),
                "{dump}"
            );
            assert_eq!(
                orders.index_on("amount").map(|name| name.to_lowercase()),
                Some("ix_amount".to_owned()),
                "{dump}"
            );
            assert_eq!(orders.index_on("id").as_deref(), Some("PRIMARY"), "{dump}");
            assert!(orders.foreign_key_on("user_id").is_some(), "{dump}");
            // MySQL made an index for the foreign key.
            assert_eq!(
                index_names(&orders),
                ["note", "ix_amount", "fk_user"],
                "{dump}"
            );
            // A prefix of the primary key: the column is in it, and NOT NULL.
            assert!(codes.in_primary_key("code"), "{dump}");
            assert_eq!(codes.index_on("code").as_deref(), Some("PRIMARY"), "{dump}");
            assert!(!codes.is_nullable(codes.column("code").unwrap()), "{dump}");
            assert!(codes.is_nullable(codes.column("label").unwrap()), "{dump}");
        }
    }

    #[test]
    fn changes_find_the_names_of_a_schema_dump_in_any_quotes_or_case() {
        // Each change names the columns and indexes in lowercase, without
        // quotes. MySQL 8.0.46 and 8.4.13 name an index added without a name
        // after its first column, prefix or not, and add their own index
        // for a foreign key that only a prefix covers (run by hand).
        let changes = "
            ALTER TABLE orders ADD INDEX (note(5));
            ALTER TABLE orders DROP INDEX note;
            ALTER TABLE orders RENAME COLUMN note TO memo;
            ALTER TABLE orders DROP FOREIGN KEY fk_user;
            ALTER TABLE orders ADD CONSTRAINT fk_amount FOREIGN KEY (amount) REFERENCES parents (code);
            ALTER TABLE orders ADD CONSTRAINT fk_memo FOREIGN KEY (memo) REFERENCES parents (code);
            ALTER TABLE codes RENAME COLUMN code TO sku;";
        for dump in three_ways(PREFIXES) {
            let mut schema = Schema::from_sql(&dump);
            for statement in parse(changes) {
                schema.apply(&statement);
            }
            assert_eq!(schema.lost().count(), 0, "{dump}");
            let orders = known(&schema, "orders");
            assert_eq!(
                index_names(&orders),
                ["ix_amount", "fk_user", "note_2", "fk_memo"],
                "{dump}"
            );
            assert_eq!(orders.indexes[2].columns, ["memo(5)"], "{dump}");
            let keys: Vec<String> = orders
                .foreign_keys
                .iter()
                .map(|key| key.name.clone().unwrap_or_default())
                .collect();
            assert_eq!(keys, ["fk_amount", "fk_memo"], "{dump}");
            let codes = known(&schema, "codes");
            assert_eq!(codes.primary_key, ["sku(10)"], "{dump}");
            assert!(codes.in_primary_key("sku"), "{dump}");

            // Dropping the columns takes them out of the indexes and keys.
            for statement in
                parse("ALTER TABLE orders DROP COLUMN memo; ALTER TABLE codes DROP COLUMN sku;")
            {
                schema.apply(&statement);
            }
            assert_eq!(
                index_names(&known(&schema, "orders")),
                ["ix_amount", "fk_user"],
                "{dump}"
            );
            assert!(known(&schema, "codes").primary_key.is_empty(), "{dump}");
        }
    }

    #[test]
    fn a_table_name_in_another_case_is_the_same_table() {
        // Pinned as it is: kilit compares table names in lowercase, as MySQL
        // does with lower_case_table_names = 1 or 2. With 0, the default on
        // Linux, MySQL 8.0.46 and 8.4.13 refuse ALTER TABLE ORDERS on orders
        // with error 1146 (run by hand). Table name case is a later step.
        let schema = after("ALTER TABLE ORDERS ADD COLUMN coupon INT NULL");
        assert!(known(&schema, "orders").column("coupon").is_some());
        assert!(matches!(schema.table("Orders"), TableState::Known(_)));
    }
}
