//! The tables kilit knows, and how each statement changes them.
//!
//! kilit learns tables from `CREATE TABLE` statements, such as the ones in a
//! schema dump, then follows every statement of the migration, so that each
//! statement is judged against the tables as they are at that point. When a
//! statement changes a table in a way kilit cannot follow, kilit stops
//! trusting what it knew about that table instead of guessing.

use std::borrow::Cow;
use std::collections::{BTreeMap, BTreeSet, HashMap};

use crate::charset::{Charset, canonical};
use crate::parse::{parse, truncated_table};
use crate::statement::{
    Alter, Check, CheckColumns, Column, DataType, Engine, ForeignKey, ForeignKeyAction, Index,
    IndexKind, IntegerSize, Operation, Position, RowFormat, Statement, StatementKind, Table,
};

/// The tables kilit knows at one point of the input.
#[derive(Clone, Debug, Default)]
pub struct Schema {
    /// By name, as written.
    tables: BTreeMap<String, Entry>,
    /// The names of `tables`, by [`folded`] name: the ones that may be the
    /// same table in another letter case.
    by_fold: HashMap<String, Vec<String>>,
    /// The name of every table a foreign key has pointed at, in
    /// [`folded`] form. It may hold more than the keys of today, never
    /// less: it only saves looking through every table for a key that
    /// points at one.
    pointed_at: BTreeSet<String>,
    /// A statement before this point names `default_storage_engine`, so a
    /// table created without `ENGINE` may have any engine. Only
    /// [`check`](crate::check) sets it: a schema dump writes `ENGINE` on
    /// every table.
    default_engine_changed: bool,
    /// A statement before this point names `innodb_default_row_format` or
    /// `innodb_strict_mode`, so a table created without `ROW_FORMAT` may
    /// have any row format. Only [`check`](crate::check) sets it.
    default_row_format_changed: bool,
    /// A statement kilit could not read, and that names no table it could
    /// tell, may have created tables kilit does not see.
    may_hold_unseen_tables: bool,
    /// By name, what a `CREATE TEMPORARY TABLE` of this session
    /// hides: the table of that name as it was, if any. A new session sees
    /// it again.
    hidden_by_temporary: BTreeMap<String, Option<Entry>>,
    /// The tables come from a dump of the database the migration runs on,
    /// so a table that is not in it, and that no statement creates, is not
    /// there: see [`Schema::from_dump`].
    from_dump: bool,
    /// By name, the statement after the dump that took the table away,
    /// until a statement creates it again.
    gone: BTreeMap<String, Gone>,
    /// The names of the tables the dump has: see [`Schema::from_dump`].
    dump_tables: BTreeSet<String>,
    /// How many sessions (input files) have ended: the one being checked
    /// is this one, counting from 0.
    sessions_ended: usize,
    /// Tables a foreign key of a table kilit does not know may point at:
    /// the keys a table had when kilit lost track of it, and the keys added
    /// to a table it does not know.
    unseen_keys_point_at: BTreeSet<String>,
    /// A statement kilit could not read writes REFERENCES: the keys it adds
    /// may point anywhere.
    unseen_keys_anywhere: bool,
    /// By name, the tables a `CREATE TABLE` of the input made, which no
    /// statement since dropped, and which no RENAME moved onto a name that
    /// held a table from before the input. A temporary one ends with its
    /// session. Nothing else can be using them yet.
    created: BTreeMap<String, Created>,
    /// By name, what a temporary table of this session hides of `created`.
    created_hidden: BTreeMap<String, Created>,
    /// The names the input used as tables it did not create first: it
    /// altered, renamed, dropped or read them.
    used: BTreeSet<String>,
}

/// A table the input created: see [`Schema::created`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Created {
    /// The session (input file) of the `CREATE TABLE`, counting from 0.
    pub session: usize,
    pub temporary: bool,
}

/// A statement that took a table away from kilit's schema.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Gone {
    pub line: usize,
    pub how: GoneHow,
    /// The statement is in an earlier input file than the one being
    /// checked, so its line is a line of that file.
    pub earlier_file: bool,
    /// The session (input file) of the statement, counting from 0.
    pub session: usize,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum GoneHow {
    Dropped,
    /// A DROP of a name a temporary table had, or `DROP TEMPORARY TABLE`:
    /// MySQL may have dropped only a temporary table.
    MaybeDropped,
    Renamed {
        to: String,
    },
}

// Most tables are known; a lost one is rare, so its smaller size saves little.
#[allow(clippy::large_enum_variant)]
#[derive(Clone, Debug)]
enum Entry {
    Known(Table),
    /// The table exists, but kilit could not follow a change to it.
    Lost {
        reason: String,
        /// The engine kilit last knew it to have; `None` when no statement
        /// wrote one.
        engine: Option<Engine>,
        /// See [`TableState::Lost`].
        row_format_unclear: bool,
        /// A statement that names a table in another letter case lost it:
        /// another table kilit knows in another letter case may be this one.
        in_another_case: bool,
    },
}

/// What kilit knows about one table.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TableState<'a> {
    Known(&'a Table),
    /// kilit could not follow a change to the table; the reason says which.
    Lost {
        reason: &'a str,
        /// The engine kilit last knew it to have: the one its `CREATE TABLE`
        /// wrote, or the last one a statement kilit could not read wrote.
        /// `None` when no statement wrote one.
        engine: Option<&'a Engine>,
        /// The table may have a row format other than the server's default:
        /// kilit knew it to have another one, or a statement kilit could not
        /// read writes `ROW_FORMAT` or `KEY_BLOCK_SIZE`.
        row_format_unclear: bool,
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
        // They are there before the migration.
        schema.created.clear();
        schema.used.clear();
        schema
    }

    /// The tables of a dump of the database the migration runs on, as
    /// [`Schema::from_sql`] reads them. A table missing from it is one the
    /// database does not have, or the dump is out of date.
    pub fn from_dump(sql: &str) -> Schema {
        let mut schema = Schema::from_sql(sql);
        schema.created.clear();
        schema.used.clear();
        schema.from_dump = true;
        // A table the dump itself drops is simply not in it.
        schema.gone.clear();
        schema.dump_tables = schema.tables.keys().cloned().collect();
        schema
    }

    /// Whether the tables come from a dump: see [`Schema::from_dump`].
    pub(crate) fn is_from_dump(&self) -> bool {
        self.from_dump
    }

    /// The statement that took the table `name`, written exactly so, away,
    /// when no statement created it again since.
    pub(crate) fn gone(&self, name: &str) -> Option<&Gone> {
        self.gone.get(name)
    }

    /// Whether the dump has the table `name`, written exactly so.
    pub(crate) fn dump_has(&self, name: &str) -> bool {
        self.dump_tables.contains(name)
    }

    /// What kilit knows about the table `name`, written exactly so: with
    /// lower_case_table_names = 0, the default on Linux, MySQL keeps
    /// `orders` and `ORDERS` apart, and with 1 or 2 it takes them as one
    /// table. See `Schema::in_another_case`.
    pub fn table(&self, name: &str) -> TableState<'_> {
        match self.tables.get(name) {
            Some(Entry::Known(table)) => TableState::Known(table),
            Some(Entry::Lost {
                reason,
                engine,
                row_format_unclear,
                ..
            }) => TableState::Lost {
                reason,
                engine: engine.as_ref(),
                row_format_unclear: *row_format_unclear,
            },
            None => TableState::Missing,
        }
    }

    /// From here on, a table created without `ENGINE` has an unknown engine.
    pub(crate) fn default_engine_changes(&mut self) {
        self.default_engine_changed = true;
    }

    /// From here on, a table created without `ROW_FORMAT` has an unknown
    /// row format.
    pub(crate) fn default_row_format_changes(&mut self) {
        self.default_row_format_changed = true;
    }

    /// The session ends: its temporary tables are gone, and the tables they
    /// hid are seen again as they were.
    pub(crate) fn session_ends(&mut self) {
        self.sessions_ended += 1;
        // A temporary table ends with its session, and what it hid is back.
        self.created.retain(|_, created| !created.temporary);
        self.created.append(&mut self.created_hidden);
        for gone in self.gone.values_mut() {
            gone.earlier_file = true;
        }
        for (name, hidden) in std::mem::take(&mut self.hidden_by_temporary) {
            match hidden {
                Some(entry) => self.put(name, entry),
                None => {
                    self.take(&name);
                }
            }
        }
    }

    /// kilit stops trusting what it knew about the table `name`.
    pub(crate) fn lose_table(
        &mut self,
        name: &str,
        reason: String,
        engine: Option<Engine>,
        row_format_unclear: bool,
    ) {
        self.lose(name, reason, engine, row_format_unclear);
    }

    /// Whether no table kilit knows, or lost, has the name `name` in any
    /// letter case.
    pub(crate) fn no_table_in_any_case(&self, name: &str) -> bool {
        !self.tables.contains_key(name) && !self.by_fold.contains_key(&folded(name))
    }

    /// Whether a foreign key may point at the table `name`. If not, none does.
    /// A key that points at the table in another letter case counts.
    pub(crate) fn may_be_pointed_at(&self, name: &str) -> bool {
        self.pointed_at.contains(&folded(name))
    }

    /// Whether a foreign key of a table kilit does not know (one it lost
    /// track of, or one no schema shows) may point at the table `name`:
    /// such a key is in no table kilit holds.
    pub(crate) fn unseen_key_may_point_at(&self, name: &str) -> bool {
        self.unseen_keys_anywhere || self.unseen_keys_point_at.contains(&folded(name))
    }

    /// The first table name of `statement` that matches a table kilit knows
    /// (or knew, and lost) only in another letter case, or another name the
    /// statement wrote before it. Whether they are one table depends on the
    /// server's lower_case_table_names, which kilit does not know. A name
    /// kilit knows written exactly so is never one, and neither is a name
    /// that matches nothing.
    pub(crate) fn in_another_case(&self, statement: &Statement) -> Option<InAnotherCase> {
        let names = named_tables(statement);
        let mut found: Option<InAnotherCase> = None;
        for (at, (written, role)) in names.iter().enumerate() {
            // A name kilit lost to a statement in another letter case may
            // still be the table of another case.
            let lost_in_another_case = match self.tables.get(written.as_ref()) {
                None => false,
                Some(Entry::Lost {
                    in_another_case: true,
                    ..
                }) => true,
                Some(_) => continue,
            };
            let known = self
                .by_fold
                .get(&folded(written))
                .into_iter()
                .flatten()
                .map(String::as_str)
                .find(|known| known != written)
                .or_else(|| {
                    names[..at]
                        .iter()
                        .map(|(name, _)| name.as_ref())
                        .find(|known| known != written && same_in_any_case(known, written))
                });
            if let Some(known) = known {
                match &mut found {
                    Some(first) => {
                        first.alone = false;
                        break;
                    }
                    None => {
                        // A table kilit lost to such a statement may be gone.
                        let known_unclear = matches!(
                            self.tables.get(known),
                            Some(Entry::Lost {
                                in_another_case: true,
                                ..
                            })
                        );
                        // `shop.ORDERS` is ORDERS when the session uses shop.
                        let last = |name: &str| name.rsplit('.').next().unwrap_or(name).to_owned();
                        let elsewhere = self
                            .tables
                            .keys()
                            .any(|name| name != written.as_ref() && last(name) == last(written));
                        found = Some(InAnotherCase {
                            written: written.clone().into_owned(),
                            known: known.to_owned(),
                            role: *role,
                            alone: !lost_in_another_case
                                && !known_unclear
                                && !self.may_hold_unseen_tables
                                && !elsewhere,
                            known_temporary: self.hidden_by_temporary.contains_key(known),
                        });
                    }
                }
            }
        }
        found
    }

    /// Follows a statement that names a table in another letter case (see
    /// [`Schema::in_another_case`]): kilit stops trusting every table the
    /// statement may change on one setting of lower_case_table_names or the
    /// other, and every name it may create. A table it may only point a
    /// foreign key at, or empty, stays as it is.
    pub(crate) fn follow_in_another_case(&mut self, statement: &Statement) {
        let reason = format!(
            "line {}: names a table in another letter case, so whether it changed this one \
             depends on lower_case_table_names",
            statement.line
        );
        let names = named_tables(statement);
        let temporary = creates_temporary(&statement.sql);
        // The engine and the row format the statement may give a table.
        let (engine, writes_format) = match &statement.kind {
            StatementKind::Alter(alter) => {
                let mut engine = None;
                let mut writes_format = false;
                for operation in &alter.operations {
                    if let Operation::ChangeTableOptions(options) = operation {
                        if options.engine.is_some() {
                            engine = Some(Engine::Unknown);
                        }
                        writes_format |=
                            options.row_format.is_some() || options.key_block_size.is_some();
                    }
                }
                (engine, writes_format)
            }
            StatementKind::CreateTable { table, .. } => {
                let engine = match &table.engine {
                    None if self.default_engine_changed => Some(Engine::Unknown),
                    None => Some(Engine::InnoDb),
                    written => written.clone(),
                };
                let format = row_format_unclear(table)
                    || (table.row_format.is_none() && self.default_row_format_changed);
                (engine, format)
            }
            StatementKind::Unparsed { .. } => (
                written_engine(&statement.sql),
                writes_row_format(&statement.sql),
            ),
            _ => (None, false),
        };
        // A statement kilit could not read loses the table of its name,
        // when it may create one; one it names in another letter case is
        // lost below.
        if matches!(statement.kind, StatementKind::Unparsed { .. }) && creates(&statement.sql) {
            self.apply(statement);
            if let StatementKind::Unparsed {
                table: Some(name), ..
            } = &statement.kind
                && let Some(Entry::Lost {
                    in_another_case, ..
                }) = self.tables.get_mut(name.as_str())
            {
                *in_another_case = true;
            }
        }
        for (written, role) in &names {
            let (written, role) = (written.as_ref(), *role);
            let matching: Vec<String> = self
                .by_fold
                .get(&folded(written))
                .cloned()
                .unwrap_or_default();
            // A CREATE kilit could not read gives its name to a table, as
            // CREATE TABLE does; it lost that name above.
            let names_one =
                role == Role::Names || (role == Role::Unread && creates(&statement.sql));
            let changes = match role {
                Role::Changes | Role::DropsIfExists => true,
                // A temporary table hides the table of its name.
                Role::Names => temporary,
                Role::Unread => !names_one || temporary,
                Role::PointsAt => {
                    self.pointed_at.insert(folded(written));
                    false
                }
                Role::Empties => false,
            };
            if changes {
                for name in matching
                    .iter()
                    .filter(|name| !(names_one && *name == written))
                {
                    // Until the session ends: a temporary table only hides
                    // the table of its name.
                    if names_one && !self.hidden_by_temporary.contains_key(name) {
                        let hidden = self.tables.get(name).cloned();
                        self.hidden_by_temporary.insert(name.clone(), hidden);
                    }
                    let lost_engine = merged(self.engine_of(name).cloned(), engine.clone());
                    let unclear = writes_format || self.row_format_unclear_of(name);
                    self.lose_in_another_case(name, reason.clone(), lost_engine, unclear);
                }
            }
            if role == Role::Names {
                // The table of the new name: the one the statement creates
                // or renames, or one kilit knows in another letter case.
                let mut lost_engine = match &statement.kind {
                    StatementKind::RenameTable { renames } => renames
                        .iter()
                        .find(|rename| rename.to == written)
                        .and_then(|rename| self.engine_of(&rename.from).cloned()),
                    StatementKind::Alter(alter) => {
                        merged(self.engine_of(&alter.table).cloned(), engine.clone())
                    }
                    _ => engine.clone(),
                };
                let mut unclear = writes_format;
                for name in matching.iter().filter(|name| *name != written) {
                    lost_engine = merged(lost_engine, self.engine_of(name).cloned());
                    unclear |= self.row_format_unclear_of(name);
                }
                if temporary && !self.hidden_by_temporary.contains_key(written) {
                    self.hide_for_temporary(written);
                }
                self.lose_in_another_case(written, reason.clone(), lost_engine, unclear);
            }
        }
    }

    /// The tables kilit knows, by name.
    pub fn tables(&self) -> impl Iterator<Item = &Table> {
        self.tables.values().filter_map(|entry| match entry {
            Entry::Known(table) => Some(table),
            Entry::Lost { .. } => None,
        })
    }

    /// The tables kilit lost track of, by name, with the reason.
    pub fn lost(&self) -> impl Iterator<Item = (&str, &str)> {
        self.tables.iter().filter_map(|(name, entry)| match entry {
            Entry::Lost { reason, .. } => Some((name.as_str(), reason.as_str())),
            Entry::Known(_) => None,
        })
    }

    /// Whether this session has a temporary table `name`, written exactly
    /// so.
    pub(crate) fn has_temporary(&self, name: &str) -> bool {
        self.hidden_by_temporary.contains_key(name)
    }

    /// Whether the input created a table that is still new: see
    /// [`Schema::created`].
    pub(crate) fn has_created(&self) -> bool {
        !self.created.is_empty()
    }

    /// The table the input created under the name `name`, written exactly
    /// so, if it still has the name and nothing else can be using it: see
    /// [`Schema::created`].
    pub fn created(&self, name: &str) -> Option<Created> {
        self.created.get(name).copied()
    }

    /// Whether `CREATE TABLE name`, without IF NOT EXISTS and not
    /// TEMPORARY, names a table that is there, with a `--schema` dump: one
    /// of the dump, or one a RENAME put there, but not one the input
    /// created. MySQL refuses it (error 1050, by hand on MySQL 8.0.46 and
    /// 8.4.13), and the table stays.
    pub fn creates_a_dump_table(&self, statement: &Statement) -> bool {
        match &statement.kind {
            StatementKind::CreateTable {
                table,
                if_not_exists: false,
            } => {
                self.from_dump
                    && self.base_there(&table.name)
                    && !self.hidden_by_temporary.contains_key(&table.name)
                    && !self.created.contains_key(&table.name)
                    && !creates_temporary(&statement.sql)
            }
            _ => false,
        }
    }

    /// Whether a base table, not a temporary one, has the name `name`,
    /// written exactly so, as far as kilit can tell: also one hidden by a
    /// temporary table of the name, and one kilit lost.
    pub(crate) fn base_there(&self, name: &str) -> bool {
        match self.hidden_by_temporary.get(name) {
            Some(hidden) => hidden.is_some(),
            None => self.tables.contains_key(name),
        }
    }

    /// What `CREATE [TEMPORARY] TABLE [IF NOT EXISTS] name LIKE source`
    /// does, as the hand tests on MySQL 8.0.46 and 8.4.13 show: the new
    /// table has the columns, indexes (a foreign key's index as a plain
    /// one), engine, row format, key block size, character set and
    /// collation of `source`, no foreign key, and its CHECK constraints
    /// named `name_chk_1`, `name_chk_2`, ... in the order of their names in
    /// `source`.
    pub(crate) fn like(&self, statement: &Statement) -> Like {
        let StatementKind::CreateTableLike {
            name,
            source,
            if_not_exists,
        } = &statement.kind
        else {
            return Like::Nothing;
        };
        let temporary = creates_temporary(&statement.sql);
        if temporary && self.hidden_by_temporary.contains_key(name) {
            return Like::Lost(format!(
                "a temporary table {name} is there already, which kilit does not follow"
            ));
        }
        if !temporary && self.base_there(name) {
            return if *if_not_exists {
                Like::Nothing
            } else {
                Like::Refused(refusal(&format!("a table {name} is there already"), 1050))
            };
        }
        if *if_not_exists && !temporary && !self.from_dump && self.gone(name).is_none() {
            return Like::Lost(format!(
                "CREATE TABLE IF NOT EXISTS keeps a table {name} that no --schema shows, if \
                 there is one"
            ));
        }
        let source_table = match self.tables.get(source) {
            Some(Entry::Known(table)) => table,
            _ => {
                return Like::Lost(format!(
                    "{name} takes its columns from {source}, which kilit does not know"
                ));
            }
        };
        if temporary {
            if source_table.row_format == Some(RowFormat::Compressed)
                || source_table.key_block_size.is_some()
            {
                return Like::Refused(refusal(
                    &format!("{source} is COMPRESSED, and the new table TEMPORARY"),
                    3500,
                ));
            }
            let fulltext = source_table
                .indexes
                .iter()
                .any(|index| index.kind == IndexKind::Fulltext);
            match source_table.engine {
                Some(Engine::MyIsam) => {}
                None | Some(Engine::InnoDb) if fulltext => {
                    return Like::Refused(refusal(
                        &format!("{source} has a FULLTEXT index, and the new table is TEMPORARY"),
                        1796,
                    ));
                }
                None | Some(Engine::InnoDb) => {}
                _ => {
                    return Like::Lost(format!(
                        "{source} has an engine kilit does not know, and the new table is \
                         TEMPORARY"
                    ));
                }
            }
            if matches!(source_table.row_format, Some(RowFormat::Unknown)) {
                return Like::Lost(format!(
                    "kilit cannot tell the row format of {source}, and the new table is TEMPORARY"
                ));
            }
        }
        let mut table = source_table.clone();
        name.clone_into(&mut table.name);
        table.foreign_keys.clear();
        let mut checks = std::mem::take(&mut table.checks);
        if checks
            .iter()
            .any(|check| check.name.as_deref().is_none_or(|name| !name.is_ascii()))
        {
            return Like::Lost(format!(
                "{source} has a CHECK constraint whose name kilit cannot order as MySQL does"
            ));
        }
        checks.sort_by_key(|check| {
            check
                .name
                .as_deref()
                .unwrap_or_default()
                .to_ascii_lowercase()
        });
        let prefix = generated_check_prefix(name);
        for (number, check) in (1..).zip(&mut checks) {
            check.name = Some(format!("{prefix}{number}"));
        }
        // CHECK names are the database's: one another table has may be
        // refused, which was not hand-tested.
        let taken = checks.iter().any(|check| {
            self.tables.values().any(|entry| match entry {
                Entry::Known(other) => has_name(
                    other.checks.iter().map(|check| &check.name),
                    check.name.as_deref().unwrap_or_default(),
                ),
                Entry::Lost { .. } => false,
            })
        });
        if taken {
            return Like::Lost(format!(
                "{name} gets CHECK constraint names another table has, which kilit does not follow"
            ));
        }
        table.checks = checks;
        Like::Creates(Box::new(table))
    }

    /// Keeps [`Schema::created`] and [`Schema::used`] up to date with
    /// `statement`, before the tables follow it.
    fn track_created(&mut self, statement: &Statement) {
        let session = self.sessions_ended;
        match &statement.kind {
            StatementKind::CreateTable {
                table,
                if_not_exists,
            } => {
                // IF NOT EXISTS does nothing when the table is there, and
                // kilit may not know whether it is.
                if *if_not_exists {
                    return;
                }
                let name = key(&table.name);
                let temporary = creates_temporary(&statement.sql);
                if temporary
                    && !self.hidden_by_temporary.contains_key(&name)
                    && let Some(base) = self.created.remove(&name)
                {
                    self.created_hidden.insert(name.clone(), base);
                }
                self.created.insert(name, Created { session, temporary });
            }
            StatementKind::CreateTableLike { name, source, .. } => {
                if !self.created.contains_key(source) {
                    self.used.insert(key(source));
                }
                // IF NOT EXISTS does nothing when the table is there, and
                // kilit may not know whether it is.
                if let StatementKind::CreateTableLike {
                    if_not_exists: false,
                    ..
                } = &statement.kind
                    && let Like::Creates(_) = self.like(statement)
                {
                    let name = key(name);
                    let temporary = creates_temporary(&statement.sql);
                    let created = Created { session, temporary };
                    if !temporary && self.hidden_by_temporary.contains_key(&name) {
                        // A base table under the temporary one.
                        self.created_hidden.insert(name, created);
                    } else {
                        if temporary && let Some(base) = self.created.remove(&name) {
                            self.created_hidden.insert(name.clone(), base);
                        }
                        self.created.insert(name, created);
                    }
                }
            }
            StatementKind::DropTable { tables } => {
                let temporary = crate::parse::drops_temporary_table(&statement.sql);
                for name in tables {
                    if self.hidden_by_temporary.contains_key(name) {
                        // The temporary table goes, and what it hid is back.
                        self.created.remove(name);
                        if let Some(base) = self.created_hidden.remove(name) {
                            self.created.insert(key(name), base);
                        }
                    } else if !temporary && self.created.remove(name).is_none() {
                        self.used.insert(key(name));
                    }
                }
            }
            StatementKind::RenameTable { renames } => {
                for rename in renames {
                    self.move_created(&rename.from, &rename.to);
                }
            }
            StatementKind::Alter(alter) => {
                if !self.created.contains_key(&alter.table) {
                    self.used.insert(key(&alter.table));
                }
                for operation in &alter.operations {
                    if let Operation::RenameTable { new_name } = operation {
                        self.move_created(&alter.table, new_name);
                    }
                }
            }
            StatementKind::Unparsed {
                table: Some(name), ..
            } => {
                if !self.created.contains_key(name) {
                    self.used.insert(key(name));
                }
            }
            StatementKind::Other => {
                for name in read_tables(&statement.sql) {
                    if !self.created.contains_key(&name) {
                        self.used.insert(name);
                    }
                }
            }
            StatementKind::Unparsed { table: None, .. } | StatementKind::Set { .. } => {}
        }
    }

    /// A RENAME moves the table `from` to `to`: a table the input created
    /// stays one, unless `to` held a table from before the input (the dump
    /// has it, or the input used it without creating it first).
    fn move_created(&mut self, from: &str, to: &str) {
        match self.created.remove(from) {
            Some(created) => {
                if !self.dump_tables.contains(to) && !self.used.contains(to) {
                    self.created.insert(key(to), created);
                }
            }
            None => {
                self.used.insert(key(from));
            }
        }
    }

    /// Follows one statement: tables are created, changed, renamed and
    /// dropped as the statement says.
    pub fn apply(&mut self, statement: &Statement) {
        // MySQL refuses it, and the dump's table stays.
        if self.creates_a_dump_table(statement) {
            return;
        }
        // Without a temporary table of the name, DROP TEMPORARY TABLE drops
        // nothing (by hand on MySQL 8.0.46 and 8.4.13: error 1051, or a note
        // with IF EXISTS), and the table of the name stays.
        if let StatementKind::DropTable { tables } = &statement.kind
            && crate::parse::drops_temporary_table(&statement.sql)
            && tables
                .iter()
                .any(|name| !self.hidden_by_temporary.contains_key(name))
        {
            if drops_if_exists(&statement.sql) {
                // The temporary ones go.
                let temporary: Vec<String> = tables
                    .iter()
                    .filter(|name| self.hidden_by_temporary.contains_key(*name))
                    .cloned()
                    .collect();
                if !temporary.is_empty() {
                    let mut only = statement.clone();
                    only.kind = StatementKind::DropTable { tables: temporary };
                    self.apply(&only);
                }
            }
            return;
        }
        self.track_created(statement);
        if creates_temporary(&statement.sql) {
            let name = match &statement.kind {
                StatementKind::CreateTable { table, .. } => Some(&table.name),
                StatementKind::Unparsed {
                    table: Some(name), ..
                } => Some(name),
                _ => None,
            };
            if let Some(name) = name
                && !self.hidden_by_temporary.contains_key(name)
            {
                self.hide_for_temporary(name);
            }
        }
        match &statement.kind {
            StatementKind::CreateTable {
                table,
                if_not_exists,
            } => {
                if *if_not_exists && self.tables.contains_key(&table.name) {
                    return;
                }
                let mut table = table.clone();
                if table.engine.is_none() && self.default_engine_changed {
                    table.engine = Some(Engine::Unknown);
                }
                if table.row_format.is_none() && self.default_row_format_changed {
                    table.row_format = Some(RowFormat::Unknown);
                }
                if table.engine == Some(Engine::MyIsam) {
                    // MySQL keeps no foreign key on a MyISAM table, only its index.
                    for foreign_key in std::mem::take(&mut table.foreign_keys) {
                        let indexed = if names_foreign_key_index(&statement.sql) {
                            Err(MYISAM_INDEX_NAME.to_owned())
                        } else {
                            index_ignored_foreign_key(&mut table, &foreign_key)
                        };
                        if let Err(reason) = indexed {
                            let line = statement.line;
                            let unclear = row_format_unclear(&table);
                            self.lose(
                                &table.name,
                                format!("line {line}: {reason}"),
                                table.engine,
                                unclear,
                            );
                            return;
                        }
                    }
                }
                for foreign_key in table.foreign_keys.clone() {
                    index_foreign_key(&mut table, &foreign_key);
                    self.pointed_at
                        .insert(folded(&foreign_key.referenced_table));
                }
                name_new_checks(&mut table);
                self.put(key(&table.name), Entry::Known(table));
            }
            StatementKind::CreateTableLike { name, .. } => match self.like(statement) {
                Like::Creates(table) => {
                    let temporary = creates_temporary(&statement.sql);
                    if !temporary && let Some(hidden) = self.hidden_by_temporary.get_mut(name) {
                        // The new base table is under the temporary one.
                        *hidden = Some(Entry::Known(*table));
                        return;
                    }
                    if temporary && !self.hidden_by_temporary.contains_key(name) {
                        self.hide_for_temporary(name);
                    }
                    self.put(key(name), Entry::Known(*table));
                }
                Like::Nothing | Like::Refused(_) => {}
                // `CREATE TABLE new LIKE old` copies old's engine and row
                // format.
                Like::Lost(reason) => {
                    let StatementKind::CreateTableLike { source, .. } = &statement.kind else {
                        return;
                    };
                    // A temporary table it may create ends with the session.
                    if creates_temporary(&statement.sql)
                        && !self.hidden_by_temporary.contains_key(name)
                    {
                        self.hide_for_temporary(name);
                    }
                    let engine = self.engine_of(source).cloned();
                    let unclear = self.row_format_unclear_of(source);
                    self.lose(
                        name,
                        format!("line {}: {reason}", statement.line),
                        engine,
                        unclear,
                    );
                }
            },
            StatementKind::DropTable { tables } => {
                // Comments between the words count for nothing.
                let temporary = crate::parse::drops_temporary_table(&statement.sql);
                for name in tables {
                    // With a temporary table of the name, DROP TABLE and DROP
                    // TEMPORARY TABLE drop that one only, and the table it
                    // hid is back (by hand on MySQL 8.0.46 and 8.4.13).
                    if let Some(hidden) = self.hidden_by_temporary.remove(name) {
                        match hidden {
                            Some(entry) => self.put(key(name), entry),
                            None => {
                                self.take(&key(name));
                            }
                        }
                        continue;
                    }
                    // Without a dump, a table kilit never saw is gone too:
                    // after the statement there is none of the name, with
                    // IF EXISTS or not, unless only a temporary one went.
                    if self.take(&key(name)).is_some() || !self.from_dump {
                        let how = if temporary {
                            GoneHow::MaybeDropped
                        } else {
                            GoneHow::Dropped
                        };
                        self.gone.insert(
                            key(name),
                            Gone {
                                line: statement.line,
                                how,
                                earlier_file: false,
                                session: self.sessions_ended,
                            },
                        );
                    }
                }
            }
            StatementKind::RenameTable { renames } => {
                if self.rename_refused(renames).is_some() {
                    return;
                }
                // By hand on MySQL 8.0.46 and 8.4.13, RENAME TABLE does not
                // see temporary tables: it renames the table one hides, or
                // fails. kilit does not follow that.
                if renames.iter().any(|rename| {
                    self.hidden_by_temporary.contains_key(&rename.from)
                        || self.hidden_by_temporary.contains_key(&rename.to)
                }) {
                    let reason = format!(
                        "line {}: RENAME TABLE names a temporary table, which it does not see, \
                         and kilit does not follow that",
                        statement.line
                    );
                    for rename in renames {
                        for name in [&rename.from, &rename.to] {
                            self.lose_under_temporary(name, &reason);
                        }
                    }
                    return;
                }
                for rename in renames {
                    self.rename(&rename.from, &rename.to, statement.line);
                }
            }
            StatementKind::Alter(alter) => self.alter(statement, alter),
            StatementKind::Unparsed {
                table: Some(name), ..
            } => {
                let exists = self.tables.contains_key(name);
                let engine = match written_engine(&statement.sql) {
                    // A CREATE of a table that exists changes nothing.
                    _ if exists && creates(&statement.sql) => self.engine_of(name).cloned(),
                    Some(engine) => Some(engine),
                    // `CREATE TABLE new LIKE old` copies old's engine.
                    None if creates(&statement.sql) => match like_source(&statement.sql) {
                        Some(source) => self.engine_of(source).cloned(),
                        None if self.default_engine_changed => Some(Engine::Unknown),
                        None => None,
                    },
                    None => self.engine_of(name).cloned(),
                };
                let unclear = if exists && creates(&statement.sql) {
                    self.row_format_unclear_of(name)
                } else if writes_row_format(&statement.sql) {
                    true
                } else if creates(&statement.sql) {
                    match like_source(&statement.sql) {
                        // `CREATE TABLE new LIKE old` copies old's row format.
                        Some(source) => self.row_format_unclear_of(source),
                        None => self.default_row_format_changed,
                    }
                } else {
                    self.row_format_unclear_of(name)
                };
                self.lose(
                    name,
                    format!(
                        "line {}: kilit could not read a statement on it",
                        statement.line
                    ),
                    engine,
                    unclear,
                );
                if writes_references(&statement.sql) {
                    self.unseen_keys_anywhere = true;
                }
            }
            StatementKind::Unparsed { table: None, .. } => {
                if writes_references(&statement.sql) {
                    self.unseen_keys_anywhere = true;
                }
                let first = statement.sql.split_whitespace().next().unwrap_or_default();
                if !["TRUNCATE", "LOCK", "UNLOCK"]
                    .iter()
                    .any(|word| first.eq_ignore_ascii_case(word))
                {
                    self.may_hold_unseen_tables = true;
                }
            }
            StatementKind::Set { .. } | StatementKind::Other => {}
        }
    }

    fn rename(&mut self, from: &str, to: &str, line: usize) {
        if let Some(mut entry) = self.take(&key(from)) {
            if let Entry::Known(table) = &mut entry {
                rename_generated_checks(table, to);
                to.clone_into(&mut table.name);
            }
            self.put(key(to), entry);
            self.keys_follow_their_parent(from, to);
            self.gone.insert(
                key(from),
                Gone {
                    line,
                    how: GoneHow::Renamed { to: to.to_owned() },
                    earlier_file: false,
                    session: self.sessions_ended,
                },
            );
        } else if !self.from_dump && !self.gone.contains_key(from) {
            // Without a dump, a table kilit never saw is gone after the
            // statement as after a DROP: if it was there it is `to` now,
            // and if not MySQL refused the rename (1146 either way on a
            // later statement on it, by hand on MySQL 8.0.46 and 8.4.13).
            // `to` may be a table now, whatever kilit knew of it.
            self.gone.remove(to);
            self.gone.insert(
                key(from),
                Gone {
                    line,
                    how: GoneHow::Renamed { to: to.to_owned() },
                    earlier_file: false,
                    session: self.sessions_ended,
                },
            );
        }
    }

    /// Why MySQL refuses a RENAME TABLE for a name kilit's tables show: a
    /// new name that a table has already, after the renames before it in
    /// the statement (error 1050, by hand on MySQL 8.0.46 and 8.4.13). The
    /// statement then renames nothing.
    pub(crate) fn rename_refused(&self, renames: &[crate::statement::Rename]) -> Option<String> {
        let mut moved: Vec<(&str, &str)> = Vec::new();
        for rename in renames {
            // RENAME TABLE does not see temporary tables.
            let there = |name: &str| {
                let mut there = self.base_there(name);
                for (from, to) in &moved {
                    if *from == name {
                        there = false;
                    }
                    if *to == name {
                        there = true;
                    }
                }
                there
            };
            if there(&rename.to) {
                return Some(refusal(
                    &format!("a table {} is there already", rename.to),
                    1050,
                ));
            }
            moved.push((&rename.from, &rename.to));
        }
        None
    }

    /// The foreign keys that point at the table `from` point at `to` after
    /// it is renamed: MySQL keeps them on the table, whatever its name (by
    /// hand on MySQL 8.0.46 and 8.4.13, with RENAME TABLE and ALTER TABLE
    /// ... RENAME TO alike). The keys of tables kilit does not know follow
    /// too.
    fn keys_follow_their_parent(&mut self, from: &str, to: &str) {
        for entry in self.tables.values_mut() {
            if let Entry::Known(table) = entry {
                for key in &mut table.foreign_keys {
                    if key.referenced_table == from {
                        to.clone_into(&mut key.referenced_table);
                    }
                }
            }
        }
        if self.pointed_at.contains(&folded(from)) {
            self.pointed_at.insert(folded(to));
        }
        if self.unseen_keys_point_at.contains(&folded(from)) {
            self.unseen_keys_point_at.insert(folded(to));
        }
    }

    fn lose(
        &mut self,
        name: &str,
        reason: String,
        engine: Option<Engine>,
        row_format_unclear: bool,
    ) {
        self.keys_go_unseen(name);
        self.put(
            key(name),
            Entry::Lost {
                reason,
                engine,
                row_format_unclear,
                in_another_case: false,
            },
        );
    }

    fn lose_in_another_case(
        &mut self,
        name: &str,
        reason: String,
        engine: Option<Engine>,
        row_format_unclear: bool,
    ) {
        self.keys_go_unseen(name);
        self.put(
            key(name),
            Entry::Lost {
                reason,
                engine,
                row_format_unclear,
                in_another_case: true,
            },
        );
    }

    /// A temporary table of the name `name` hides the table of that name
    /// until the session ends. The keys of the hidden table stay on the
    /// server, in no table kilit holds now.
    fn hide_for_temporary(&mut self, name: &str) {
        self.keys_go_unseen(name);
        let hidden = self.take(&key(name));
        self.hidden_by_temporary.insert(key(name), hidden);
    }

    /// kilit is about to lose track of the table `name`: the foreign keys it
    /// has, or may have, are no longer in a table kilit holds.
    fn keys_go_unseen(&mut self, name: &str) {
        match self.tables.get(name) {
            Some(Entry::Known(table)) => {
                for foreign_key in &table.foreign_keys {
                    self.unseen_keys_point_at
                        .insert(folded(&foreign_key.referenced_table));
                }
            }
            Some(Entry::Lost { .. }) | None => {}
        }
    }

    /// Sets the entry of the table `name`, keeping [`Schema::by_fold`].
    fn put(&mut self, name: String, entry: Entry) {
        self.gone.remove(&name);
        match self.tables.entry(name) {
            std::collections::btree_map::Entry::Occupied(mut known) => {
                known.insert(entry);
            }
            std::collections::btree_map::Entry::Vacant(new) => {
                self.by_fold
                    .entry(folded(new.key()))
                    .or_default()
                    .push(new.key().clone());
                new.insert(entry);
            }
        }
    }

    /// Removes the entry of the table `name`, keeping [`Schema::by_fold`].
    fn take(&mut self, name: &str) -> Option<Entry> {
        let entry = self.tables.remove(name)?;
        let fold = folded(name);
        if let Some(names) = self.by_fold.get_mut(&fold) {
            names.retain(|known| known != name);
            if names.is_empty() {
                self.by_fold.remove(&fold);
            }
        }
        Some(entry)
    }

    /// Whether the table `name` may have a row format other than the
    /// server's default, as far as kilit knows it. `false` for a table kilit
    /// does not know.
    fn row_format_unclear_of(&self, name: &str) -> bool {
        match self.tables.get(name) {
            Some(Entry::Known(table)) => row_format_unclear(table),
            Some(Entry::Lost {
                row_format_unclear, ..
            }) => *row_format_unclear,
            None => false,
        }
    }

    /// The engine kilit last knew the table `name` to have.
    fn engine_of(&self, name: &str) -> Option<&Engine> {
        match self.tables.get(name)? {
            Entry::Known(table) => table.engine.as_ref(),
            Entry::Lost { engine, .. } => engine.as_ref(),
        }
    }

    /// Follows an `ALTER TABLE` that writes ENGINE, ROW_FORMAT or
    /// KEY_BLOCK_SIZE on a table kilit does not know: the table is lost,
    /// with the engine written, and a row format that may not be the
    /// server's default.
    fn options_of_unknown_table(&mut self, line: usize, alter: &Alter) {
        // A key added to a table kilit does not know is in no table it holds.
        for operation in &alter.operations {
            if let Operation::AddForeignKey(foreign_key) = operation {
                self.unseen_keys_point_at
                    .insert(folded(&foreign_key.referenced_table));
            }
        }
        // With a dump, a table it lacks that no statement created is not
        // there: it stays missing, and every statement on it unknown.
        if self.from_dump && !self.tables.contains_key(&alter.table) {
            return;
        }
        for operation in &alter.operations {
            let Operation::ChangeTableOptions(options) = operation else {
                continue;
            };
            let writes_row_format =
                options.row_format.is_some() || options.key_block_size.is_some();
            if options.engine.is_none() && !writes_row_format {
                continue;
            }
            let engine = match &options.engine {
                Some(engine) => Some(engine.clone()),
                None => self.engine_of(&alter.table).cloned(),
            };
            let unclear = writes_row_format || self.row_format_unclear_of(&alter.table);
            let reason = match self.tables.get(&alter.table) {
                Some(Entry::Lost { reason, .. }) => reason.clone(),
                _ => format!("line {line}: changes the options of a table kilit does not know"),
            };
            self.lose(&alter.table, reason, engine, unclear);
        }
    }

    fn alter(&mut self, statement: &Statement, alter: &Alter) {
        let line = statement.line;
        // A missing table stays missing, and a lost one stays lost, with
        // what kilit can tell of a new engine or row format.
        let resolution = match self.tables.get(&key(&alter.table)) {
            Some(Entry::Known(table)) => self.resolve(table, alter),
            _ => {
                self.options_of_unknown_table(line, alter);
                return;
            }
        };
        let operations = match resolution {
            // MySQL refuses the statement: the table stays as it was.
            Resolution::Refused { .. } => return,
            Resolution::Lost(why) => {
                let (engine, unclear) = match self.tables.get(&key(&alter.table)) {
                    Some(Entry::Known(table)) => (table.engine.clone(), row_format_unclear(table)),
                    _ => (None, false),
                };
                self.lose(&alter.table, format!("line {line}: {why}"), engine, unclear);
                return;
            }
            Resolution::Runs(None) => Cow::Borrowed(alter.operations.as_slice()),
            Resolution::Runs(Some(ordered)) => Cow::Owned(ordered),
        };
        let Some(Entry::Known(table)) = self.tables.get_mut(&key(&alter.table)) else {
            return;
        };
        // The table changes in place: an operation kilit cannot follow loses
        // it, so what the operations before that one did is never seen.
        let mut new_name = None;
        for operation in operations.iter() {
            let changed = match operation {
                Operation::RenameTable { new_name: name } => {
                    new_name = Some(name);
                    Ok(())
                }
                // MySQL keeps no foreign key on a table of another engine,
                // and refuses to change it in some cases; kilit has measured
                // none of them.
                Operation::ChangeTableOptions(options)
                    if options
                        .engine
                        .as_ref()
                        .is_some_and(|engine| *engine != Engine::InnoDb)
                        && (!table.foreign_keys.is_empty()
                            || self.pointed_at.contains(&folded(&alter.table))) =>
                {
                    Err(
                        "changes the engine of a table with foreign keys, which kilit does not \
                         follow"
                            .to_owned(),
                    )
                }
                // MySQL names a MyISAM key's index after `FOREIGN KEY name`
                // too, which kilit does not keep.
                Operation::AddForeignKey(_)
                    if table.engine == Some(Engine::MyIsam)
                        && names_foreign_key_index(&statement.sql) =>
                {
                    Err(MYISAM_INDEX_NAME.to_owned())
                }
                // MySQL takes every RENAME INDEX of a statement against the
                // names the table had before it, and the other clauses too:
                // kilit follows one alone only.
                Operation::RenameIndex { .. } if alter.operations.len() > 1 => Err(
                    "renames an index beside other changes, which kilit does not follow".to_owned(),
                ),
                // MySQL refuses a CHECK constraint clause on a name the table
                // does not have, and with it the whole statement: alone, the
                // table stays as it was; beside other changes, kilit cannot
                // tell what an earlier one of them did to the name.
                Operation::DropCheck { name }
                | Operation::AlterCheck { name, .. }
                | Operation::DropConstraint { name }
                    if alter.operations.len() > 1 && missing_constraint(table, operation, name) =>
                {
                    Err(format!(
                        "names the constraint {name} beside other changes, and the table has \
                         no constraint of that name"
                    ))
                }
                other => change(table, other),
            };
            if let Err(reason) = changed {
                // The engine may have changed, or not.
                let engine = match operation {
                    Operation::ChangeTableOptions(options) if options.engine.is_some() => {
                        Some(Engine::Unknown)
                    }
                    _ => table.engine.clone(),
                };
                let unclear = row_format_unclear(table);
                self.lose(
                    &alter.table,
                    format!("line {line}: {reason}"),
                    engine,
                    unclear,
                );
                return;
            }
            if let Operation::AddForeignKey(foreign_key) = operation
                && table.engine != Some(Engine::MyIsam)
            {
                self.pointed_at
                    .insert(folded(&foreign_key.referenced_table));
            }
        }
        if let Some(name) = new_name {
            // ALTER TABLE ... RENAME TO moves a temporary table (by hand on
            // MySQL 8.0.46 and 8.4.13): the table it hid is back, and kilit
            // does not follow the temporary table under its new name.
            if let Some(hidden) = self.hidden_by_temporary.remove(&alter.table) {
                match hidden {
                    Some(entry) => self.put(key(&alter.table), entry),
                    None => {
                        self.take(&key(&alter.table));
                    }
                }
                // The table of the new name, if any, is back when the
                // session ends.
                if !self.hidden_by_temporary.contains_key(name.as_str()) {
                    self.hide_for_temporary(name);
                }
                let engine = self.engine_of(name).cloned();
                let unclear = self.row_format_unclear_of(name);
                self.lose(
                    name,
                    format!(
                        "line {line}: a temporary table takes the name, which kilit does not \
                         follow"
                    ),
                    engine,
                    unclear,
                );
            } else if self.hidden_by_temporary.contains_key(name.as_str()) {
                // A base table renamed to the name of a temporary table goes
                // under it (by hand on MySQL 8.0.46 and 8.4.13): kilit does
                // not follow that.
                let reason = format!(
                    "line {line}: a table takes the name of a temporary table, which kilit does \
                     not follow"
                );
                self.lose_under_temporary(&alter.table.clone(), &reason);
                self.lose_under_temporary(name, &reason);
            } else {
                self.rename(&alter.table, name, statement.line);
            }
        }
    }

    /// kilit stops trusting the table `name`, and what a temporary table of
    /// the name hides, until the session ends.
    fn lose_under_temporary(&mut self, name: &str, reason: &str) {
        if let Some(hidden) = self.hidden_by_temporary.get_mut(name) {
            *hidden = Some(Entry::Lost {
                reason: reason.to_owned(),
                engine: None,
                row_format_unclear: false,
                in_another_case: false,
            });
        }
        let engine = self.engine_of(name).cloned();
        let unclear = self.row_format_unclear_of(name);
        self.lose(name, reason.to_owned(), engine, unclear);
    }
}

/// Table names are kept as written: MySQL compares them so with
/// lower_case_table_names = 0, the default on Linux. A name that differs
/// only in letter case is the same table with 1 or 2, the defaults on
/// Windows and macOS; [`Schema::in_another_case`] finds those.
fn key(name: &str) -> String {
    name.to_owned()
}

/// What a statement does to a table it names.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Role {
    /// It changes, drops or renames the table: an ALTER TABLE (also CREATE
    /// INDEX and DROP INDEX), a DROP TABLE, the old name of a RENAME.
    Changes,
    /// `DROP TABLE IF EXISTS`.
    DropsIfExists,
    /// It gives the name to a table: CREATE TABLE, the new name of a RENAME.
    Names,
    /// A foreign key points at it.
    PointsAt,
    /// TRUNCATE TABLE: the rows go, the definition stays.
    Empties,
    /// A statement kilit could not read names it.
    Unread,
}

/// A table name that a statement writes in another letter case than a
/// table kilit knows, or than another name of the same statement.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct InAnotherCase {
    pub written: String,
    pub known: String,
    pub role: Role,
    /// No other name of the statement matches in another letter case,
    /// neither the written name nor the one it matches was lost to such a
    /// statement, no statement kilit could not read may have created a
    /// table kilit does not see, and no table of another database has the
    /// written name.
    pub alone: bool,
    /// The table it matches was created TEMPORARY in this session.
    pub known_temporary: bool,
}

/// `statement` with the table `written` named `known`, wherever it names a
/// table: what it does when the two are one table.
pub(crate) fn with_table_renamed(statement: &Statement, written: &str, known: &str) -> Statement {
    let mut renamed = statement.clone();
    let rename = |name: &mut String| {
        if name == written {
            known.clone_into(name);
        }
    };
    match &mut renamed.kind {
        StatementKind::Alter(alter) => {
            rename(&mut alter.table);
            for operation in &mut alter.operations {
                match operation {
                    Operation::RenameTable { new_name } => rename(new_name),
                    Operation::AddForeignKey(key) => rename(&mut key.referenced_table),
                    _ => {}
                }
            }
        }
        StatementKind::CreateTable { table, .. } => {
            rename(&mut table.name);
            for key in &mut table.foreign_keys {
                rename(&mut key.referenced_table);
            }
        }
        StatementKind::CreateTableLike { name, source, .. } => {
            rename(name);
            rename(source);
        }
        StatementKind::DropTable { tables } => tables.iter_mut().for_each(rename),
        StatementKind::RenameTable { renames } => {
            for one in renames {
                rename(&mut one.from);
                rename(&mut one.to);
            }
        }
        StatementKind::Unparsed {
            table: Some(name), ..
        } => rename(name),
        StatementKind::Unparsed { table: None, .. }
        | StatementKind::Set { .. }
        | StatementKind::Other => {}
    }
    renamed
}

/// The tables `statement` names, in order, with what it does to each.
fn named_tables(statement: &Statement) -> Vec<(Cow<'_, str>, Role)> {
    fn named(name: &str, role: Role) -> (Cow<'_, str>, Role) {
        (Cow::Borrowed(name), role)
    }
    match &statement.kind {
        StatementKind::Alter(alter) => {
            let mut names = vec![named(&alter.table, Role::Changes)];
            for operation in &alter.operations {
                match operation {
                    Operation::RenameTable { new_name } => {
                        names.push(named(new_name, Role::Names));
                    }
                    Operation::AddForeignKey(key) => {
                        names.push(named(&key.referenced_table, Role::PointsAt));
                    }
                    _ => {}
                }
            }
            names
        }
        StatementKind::CreateTable { table, .. } => {
            std::iter::once(named(&table.name, Role::Names))
                .chain(
                    table
                        .foreign_keys
                        .iter()
                        .map(|key| named(&key.referenced_table, Role::PointsAt)),
                )
                .collect()
        }
        StatementKind::DropTable { tables } => {
            let role = if drops_if_exists(&statement.sql) {
                Role::DropsIfExists
            } else {
                Role::Changes
            };
            tables.iter().map(|name| named(name, role)).collect()
        }
        StatementKind::RenameTable { renames } => renames
            .iter()
            .flat_map(|rename| {
                [
                    named(&rename.from, Role::Changes),
                    named(&rename.to, Role::Names),
                ]
            })
            .collect(),
        StatementKind::Unparsed {
            table: Some(name), ..
        } => vec![named(name, Role::Unread)],
        StatementKind::CreateTableLike { name, .. } => vec![named(name, Role::Names)],
        StatementKind::Unparsed { table: None, .. } => truncated_table(&statement.sql)
            .map(|name| (Cow::Owned(name), Role::Empties))
            .into_iter()
            .collect(),
        StatementKind::Set { .. } | StatementKind::Other => Vec::new(),
    }
}

/// Whether a DROP TABLE has `IF EXISTS`, outside quotes.
pub(crate) fn drops_if_exists(sql: &str) -> bool {
    let masked = outside_quotes(sql);
    word_starts(sql, "IF").any(|start| masked[start + 2..].trim_start().starts_with("EXISTS"))
}

/// Whether two table names are the same in any letter case, as MySQL
/// compares them with lower_case_table_names = 1 or 2, or may: by hand
/// with 1, `ÇİZELGE` and `ÇIZELGE` both reach `çizelge`. kilit errs on the
/// side of a match: `ı`, `ß` and other letters MySQL may fold otherwise
/// match too.
pub(crate) fn same_in_any_case(a: &str, b: &str) -> bool {
    if a.is_ascii() && b.is_ascii() {
        return a.eq_ignore_ascii_case(b);
    }
    folding(a).eq(folding(b))
}

fn folding(name: &str) -> impl Iterator<Item = char> + '_ {
    name.chars().flat_map(|c| {
        let c = if matches!(c, 'İ' | 'ı') { 'i' } else { c };
        c.to_uppercase().flat_map(char::to_lowercase)
    })
}

/// `name` as [`same_in_any_case`] compares it.
pub(crate) fn folded(name: &str) -> String {
    if name.is_ascii() {
        return name.to_ascii_lowercase();
    }
    folding(name).collect()
}

/// The engine of a table that may be either of two, as far as kilit knows:
/// `None` (no engine written) for InnoDB.
fn merged(a: Option<Engine>, b: Option<Engine>) -> Option<Engine> {
    let innodb = |engine: &Option<Engine>| engine.clone().unwrap_or(Engine::InnoDb);
    match (a, b) {
        (a, None) => a,
        (None, b) => {
            if innodb(&b) == Engine::InnoDb {
                None
            } else {
                Some(Engine::Unknown)
            }
        }
        (a, b) if innodb(&a) == innodb(&b) => a,
        _ => Some(Engine::Unknown),
    }
}

/// What a `CREATE TABLE ... LIKE` does: see [`Schema::like`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Like {
    /// It creates this table.
    Creates(Box<Table>),
    /// It does nothing: IF NOT EXISTS, and the table is there.
    Nothing,
    /// MySQL refuses it, and the tables stay as they were: why, naming the
    /// error.
    Refused(String),
    /// kilit cannot tell what it creates, if anything: why.
    Lost(String),
}

/// How MySQL takes an `ALTER TABLE` on a table kilit knows, as far as the
/// names it writes tell: every clause is taken against the table as it was
/// before the statement (by hand on MySQL 8.0.46 and 8.4.13).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Resolution {
    /// MySQL runs it: the changes to follow, in this order; `None` when the
    /// order written is the one to follow.
    Runs(Option<Vec<Operation>>),
    /// MySQL refuses it for a name kilit's tables show, and the table stays
    /// as it was: the error, and why.
    Refused { code: u16, why: String },
    /// The statement names a column kilit does not know, or names one
    /// column in a way kilit does not follow: kilit stops trusting the
    /// table, and says why.
    Lost(String),
}

const BY_HAND: &str = "by hand on MySQL 8.0.46 and 8.4.13";

/// A refusal for a name: what the statement asks, the error, and that
/// kilit's tables may be out of date.
fn refusal(what: &str, code: u16) -> String {
    format!(
        "{what}, and MySQL refuses the statement (error {code}, {BY_HAND}); kilit does not \
         answer that from its tables alone, which may be out of date"
    )
}

/// The columns a clause writes or names as columns of the table: a new
/// name, a dropped, changed or renamed one. Index and key clauses name
/// columns too, but only read them.
fn named_columns(operation: &Operation) -> Vec<&str> {
    match operation {
        Operation::AddColumn { column, .. } | Operation::ModifyColumn { column, .. } => {
            vec![column.name.as_str()]
        }
        Operation::ChangeColumn {
            old_name, column, ..
        } if same(old_name, &column.name) => vec![old_name.as_str()],
        Operation::ChangeColumn {
            old_name, column, ..
        } => vec![old_name.as_str(), column.name.as_str()],
        Operation::DropColumn { name } => vec![name.as_str()],
        Operation::RenameColumn { from, to } => vec![from.as_str(), to.as_str()],
        Operation::SetColumnDefault { column, .. } | Operation::DropColumnDefault { column } => {
            vec![column.as_str()]
        }
        _ => Vec::new(),
    }
}

/// A rename of a column: its old name, its new name.
fn renamed(operation: &Operation) -> Option<(&str, &str)> {
    match operation {
        Operation::ChangeColumn {
            old_name, column, ..
        } if !same(old_name, &column.name) => Some((old_name, &column.name)),
        Operation::RenameColumn { from, to } if !same(from, to) => Some((from, to)),
        _ => None,
    }
}

fn is_serial(column: &Column) -> bool {
    column.data_type.to_string().eq_ignore_ascii_case("SERIAL")
}

/// A SERIAL column with nothing else written.
fn bare_serial(column: &Column) -> bool {
    column.nullable.is_none()
        && column.default.is_none()
        && !column.auto_increment
        && column.charset.is_none()
        && column.collation.is_none()
        && column.comment.is_none()
        && column.on_update.is_none()
        && column.generated.is_none()
        && column.other_options.is_empty()
}

/// What MySQL makes of a SERIAL column (by hand on MySQL 8.0.46 and
/// 8.4.13): BIGINT UNSIGNED NOT NULL AUTO_INCREMENT, with a UNIQUE index of
/// its name.
fn as_serial(column: &Column) -> Column {
    Column {
        data_type: DataType::Integer {
            size: IntegerSize::Big,
            unsigned: true,
        },
        nullable: Some(false),
        auto_increment: true,
        ..column.clone()
    }
}

fn serial_index(name: &str) -> Operation {
    Operation::AddIndex(Index {
        name: Some(name.to_owned()),
        kind: IndexKind::Unique,
        columns: vec![name.to_owned()],
    })
}

/// Whether the column definition writes PRIMARY KEY.
pub(crate) fn inline_primary_key(column: &Column) -> bool {
    column
        .other_options
        .iter()
        .any(|option| option.eq_ignore_ascii_case("PRIMARY KEY"))
}

impl Schema {
    /// How MySQL takes `alter` on `table`, which kilit knows: see
    /// [`Resolution`].
    pub(crate) fn resolve(&self, table: &Table, alter: &Alter) -> Resolution {
        let name = &table.name;
        let operations = &alter.operations;
        let mut refused: Option<(u16, String)> = None;
        let mut refuse = |code: u16, what: String| {
            refused.get_or_insert_with(|| (code, refusal(&what, code)));
        };
        // Columns two clauses write (MySQL takes both against the table
        // before the statement).
        let mut seen: Vec<&str> = Vec::new();
        let mut overlapping: Vec<&str> = Vec::new();
        for operation in operations.iter().filter(|_| operations.len() > 1) {
            let mut own: Vec<&str> = Vec::new();
            for column in named_columns(operation) {
                if seen.iter().any(|other| same(other, column)) {
                    overlapping.push(column);
                }
                own.push(column);
            }
            seen.extend(own);
        }
        let overlaps = |column: &str| overlapping.iter().any(|other| same(other, column));
        let has = |column: &str| table.position_of(column).is_some();
        // A primary key the statement does not drop first.
        let keeps_primary_key = !table.primary_key.is_empty()
            && !operations
                .iter()
                .any(|operation| matches!(operation, Operation::DropPrimaryKey));

        // A primary key written twice, or written inline beside a DROP
        // PRIMARY KEY: no hand test.
        let inline_keys = operations
            .iter()
            .filter(|operation| match operation {
                Operation::ModifyColumn { column, .. } | Operation::ChangeColumn { column, .. } => {
                    inline_primary_key(column)
                }
                _ => false,
            })
            .count();
        let added_keys = operations
            .iter()
            .filter(|operation| matches!(operation, Operation::AddPrimaryKey { .. }))
            .count();
        let drops_key = operations
            .iter()
            .any(|operation| matches!(operation, Operation::DropPrimaryKey));
        if inline_keys + added_keys > 1 || (inline_keys > 0 && drops_key) {
            return Resolution::Lost(
                "writes PRIMARY KEY in a column beside another primary key clause, which kilit \
                 does not follow"
                    .to_owned(),
            );
        }
        let mut serial = false;
        // Clause by clause, against the table before the statement.
        for operation in operations {
            match operation {
                Operation::AddColumn { column, .. } if !overlaps(&column.name) => {
                    if has(&column.name) {
                        refuse(
                            1060,
                            format!(
                                "{name} has a column {} already; under --pretend, a migration \
                                 that checks Schema::hasColumn() prints such an ADD where the \
                                 real run skips it, and the table is the same either way",
                                column.name
                            ),
                        );
                    } else if is_serial(column) {
                        return Resolution::Lost(format!(
                            "adds {} as SERIAL, which kilit does not follow",
                            column.name
                        ));
                    }
                }
                Operation::ModifyColumn { column, .. } if !overlaps(&column.name) => {
                    if !has(&column.name) {
                        return Resolution::Lost(refusal(
                            &format!("{name} has no column {} that kilit knows", column.name),
                            1054,
                        ));
                    }
                }
                Operation::ChangeColumn {
                    old_name, column, ..
                } if !overlaps(old_name) && !overlaps(&column.name) => {
                    if !has(old_name) {
                        return Resolution::Lost(refusal(
                            &format!("{name} has no column {old_name} that kilit knows"),
                            1054,
                        ));
                    }
                    if !same(old_name, &column.name) && has(&column.name) {
                        return Resolution::Lost(refusal(
                            &format!(
                                "{name} has a column {} already, and the statement renames \
                                 {old_name} to it",
                                column.name
                            ),
                            1060,
                        ));
                    }
                }
                Operation::RenameColumn { from, to } if !overlaps(from) && !overlaps(to) => {
                    if !has(from) {
                        return Resolution::Lost(refusal(
                            &format!("{name} has no column {from} that kilit knows"),
                            1054,
                        ));
                    }
                    if !same(from, to) && has(to) {
                        return Resolution::Lost(refusal(
                            &format!(
                                "{name} has a column {to} already, and the statement renames \
                                 {from} to it"
                            ),
                            1060,
                        ));
                    }
                }
                Operation::SetColumnDefault { column, .. } if !overlaps(column) && !has(column) => {
                    return Resolution::Lost(refusal(
                        &format!("{name} has no column {column} that kilit knows"),
                        1054,
                    ));
                }
                // A MyISAM table keeps no foreign key.
                Operation::AddForeignKey(key) if table.engine != Some(Engine::MyIsam) => {
                    if let Some(key_name) = &key.name {
                        let taken = self.tables.values().any(|entry| match entry {
                            Entry::Known(other) => {
                                has_name(other.foreign_keys.iter().map(|key| &key.name), key_name)
                            }
                            Entry::Lost { .. } => false,
                        });
                        let dropped = operations.iter().any(|operation| {
                            matches!(operation, Operation::DropForeignKey { name }
                                if same(name, key_name))
                        });
                        if taken || dropped {
                            refuse(
                                1826,
                                format!("a foreign key named {key_name} is there already"),
                            );
                        } else if table.indexes.iter().any(|index| {
                            has_name([&index.name], key_name) && !serves(index, &key.columns)
                        }) {
                            // By hand only with an index that does not
                            // serve the key.
                            refuse(
                                1061,
                                format!("{name} has an index named {key_name} already"),
                            );
                        }
                    }
                }
                // By hand: an index named PRIMARY is refused (1280).
                Operation::AddIndex(index)
                    if index
                        .name
                        .as_deref()
                        .is_some_and(|index| index.eq_ignore_ascii_case("PRIMARY")) =>
                {
                    refuse(1280, "the new index is named PRIMARY".to_owned());
                }
                // By hand: 1091 on a table with no primary key and no
                // UNIQUE index MySQL could take as one.
                Operation::DropPrimaryKey
                    if table.primary_key.is_empty()
                        && !table
                            .indexes
                            .iter()
                            .any(|index| index.kind == IndexKind::Unique) =>
                {
                    refuse(1091, format!("{name} has no primary key"));
                }
                Operation::AddPrimaryKey { .. } if keeps_primary_key => {
                    refuse(1068, format!("{name} has a primary key already"));
                }
                Operation::AddCheck(check) => {
                    if let Some(check_name) = &check.name {
                        let taken = self.tables.values().any(|entry| match entry {
                            Entry::Known(other) => {
                                has_name(other.checks.iter().map(|check| &check.name), check_name)
                            }
                            Entry::Lost { .. } => false,
                        });
                        if taken {
                            refuse(
                                3822,
                                format!("a CHECK constraint named {check_name} is there already"),
                            );
                        }
                    }
                }
                Operation::DropConstraint { name: dropped }
                    if table.constraints_named(dropped)
                        == [ConstraintKind::Check, ConstraintKind::UniqueIndex] =>
                {
                    refuse(
                        3939,
                        format!("{dropped} names a CHECK constraint and a UNIQUE index of {name}"),
                    );
                }
                // A temporary table takes the name of another temporary
                // one only; a base table that of another base table only (by
                // hand on MySQL 8.0.46 and 8.4.13).
                Operation::RenameTable { new_name }
                    if !same(new_name, name)
                        && if self.hidden_by_temporary.contains_key(name.as_str()) {
                            self.hidden_by_temporary.contains_key(new_name.as_str())
                        } else {
                            self.base_there(new_name)
                        } =>
                {
                    refuse(1050, format!("a table {new_name} is there already"));
                }
                Operation::DropColumn { name: column } if operations.len() > 1 => {
                    let drops_a_key = |key: &ForeignKey| {
                        operations.iter().any(|operation| match operation {
                            Operation::DropForeignKey { name }
                            | Operation::DropConstraint { name } => {
                                key.name.as_deref().is_some_and(|key| same(key, name))
                            }
                            _ => false,
                        })
                    };
                    let drops_some_key = operations.iter().any(|operation| {
                        matches!(
                            operation,
                            Operation::DropForeignKey { .. } | Operation::DropConstraint { .. }
                        )
                    });
                    for key in &table.foreign_keys {
                        if !key.columns.iter().any(|c| same(c, column)) || drops_a_key(key) {
                            continue;
                        }
                        // kilit does not know the name MySQL gave a key
                        // written without one.
                        if key.name.is_none() && drops_some_key {
                            return Resolution::Lost(format!(
                                "drops {column}, a column of a foreign key without a name kilit \
                                 knows, beside a DROP of a foreign key or constraint"
                            ));
                        }
                        refuse(1828, format!("{column} is in a foreign key of {name}"));
                    }
                }
                _ => {}
            }
            // An inline PRIMARY KEY, and SERIAL, in MODIFY and CHANGE.
            if let Operation::ModifyColumn { column, .. } | Operation::ChangeColumn { column, .. } =
                operation
            {
                let old = match operation {
                    Operation::ChangeColumn { old_name, .. } => old_name.as_str(),
                    _ => column.name.as_str(),
                };
                if inline_primary_key(column) {
                    if keeps_primary_key {
                        refuse(1068, format!("{name} has a primary key already"));
                    } else if column.nullable == Some(true) {
                        refuse(
                            1171,
                            format!("{} is written NULL and PRIMARY KEY", column.name),
                        );
                    }
                }
                if is_serial(column) {
                    let keys: Vec<&ForeignKey> = table
                        .foreign_keys
                        .iter()
                        .filter(|key| key.columns.iter().any(|c| same(c, old)))
                        .collect();
                    let sets_null = keys.iter().any(|key| {
                        key.on_delete == Some(ForeignKeyAction::SetNull)
                            || key.on_update == Some(ForeignKeyAction::SetNull)
                    });
                    let cascades = keys.iter().any(|key| {
                        key.on_delete == Some(ForeignKeyAction::Cascade)
                            || key.on_update == Some(ForeignKeyAction::Cascade)
                    });
                    let other_auto = table
                        .columns
                        .iter()
                        .any(|other| other.auto_increment && !same(&other.name, old));
                    if sets_null {
                        refuse(
                            1830,
                            format!("{old} is in a foreign key that sets it to NULL"),
                        );
                    } else if cascades {
                        refuse(1832, format!("{old} is in a foreign key"));
                    } else if other_auto {
                        refuse(1075, format!("{name} has an AUTO_INCREMENT column already"));
                    } else if !bare_serial(column)
                        || has_name(table.indexes.iter().map(|index| &index.name), &column.name)
                    {
                        return Resolution::Lost(format!(
                            "gives {old} the type SERIAL with other options, or beside an index \
                             named {}, which kilit does not follow",
                            column.name
                        ));
                    } else {
                        serial = true;
                    }
                }
            }
        }

        // Beside other clauses, a column a CHECK constraint or a generated
        // column uses: by hand on MySQL 8.0.46 and 8.4.13, a rename of it is
        // refused (3959 or 3108), and so is a drop when the CHECK uses
        // another column too; the table stays as it was. Clauses on one
        // column twice are below.
        if operations.len() > 1 {
            let dropped: Vec<&str> = operations
                .iter()
                .filter_map(|operation| match operation {
                    Operation::DropColumn { name } => Some(name.as_str()),
                    _ => None,
                })
                .collect();
            let dropped_checks = dropped_checks(operations);
            for operation in operations {
                let (column, renames) = match operation {
                    Operation::DropColumn { name } => (name.as_str(), false),
                    _ => match renamed(operation) {
                        Some((from, _)) => (from, true),
                        None => continue,
                    },
                };
                if overlaps(column) {
                    continue;
                }
                // By hand: a CHECK constraint the statement drops counts for
                // nothing, and MySQL drops one that uses no other column
                // with the column; a CHECK of several columns refuses the
                // drop of any of them, also when the statement drops them
                // all (3959).
                let checks = Uses {
                    dropped_columns: dropped.clone(),
                    dropped_checks: dropped_checks.clone(),
                    drops: !renames,
                };
                match used_elsewhere(table, column, &checks) {
                    Used::No => {}
                    Used::ByPlainCheck if renames => refuse(
                        3959,
                        format!("a CHECK constraint uses {column}, which the statement renames"),
                    ),
                    Used::ByPlainCheck => refuse(
                        3959,
                        format!(
                            "a CHECK constraint uses {column} and another column, and the \
                             statement drops {column}"
                        ),
                    ),
                    Used::Maybe => {
                        return Resolution::Lost(format!(
                            "drops or renames {column}, which a CHECK constraint or a generated \
                             column uses, beside other clauses, which kilit does not follow"
                        ));
                    }
                }
            }
        }

        // Clauses that write one column twice: MySQL takes each against the
        // names before the statement.
        let two_clauses = |column: &str, code: Option<u16>| {
            let mut why = format!(
                "names the column {column} in two of its clauses, which MySQL takes against \
                 the names before the statement"
            );
            if let Some(code) = code {
                why.push_str(&format!(
                    "; MySQL refuses this one (error {code}, {BY_HAND})"
                ));
            }
            Resolution::Lost(why)
        };
        let all_renames: Vec<(&str, &str)> = if operations.len() > 1 {
            operations.iter().filter_map(renamed).collect()
        } else {
            Vec::new()
        };
        let mut drop_then_add = false;
        let mut cycle = false;
        for &column in &overlapping {
            let writing: Vec<&Operation> = operations
                .iter()
                .filter(|operation| named_columns(operation).iter().any(|c| same(c, column)))
                .collect();
            let renames = writing.iter().filter(|o| renamed(o).is_some()).count();
            let others: Vec<&&Operation> =
                writing.iter().filter(|o| renamed(o).is_none()).collect();
            if renames > 0 && !others.is_empty() {
                // By hand: a MODIFY of the old name or of the new one, beside
                // the rename, is refused with 1054.
                if renames == 1
                    && others
                        .iter()
                        .all(|o| matches!(o, Operation::ModifyColumn { .. }))
                {
                    refuse(
                        1054,
                        format!("the statement renames {column} and changes it in a MODIFY clause"),
                    );
                    continue;
                }
                return two_clauses(column, None);
            }
            if renames == 0 {
                // By hand: DROP x and ADD x, in either order, drop the old x
                // and add the new one; with no x to drop, 1091.
                let drops = others
                    .iter()
                    .filter(|o| matches!(o, Operation::DropColumn { .. }))
                    .count();
                let adds = others
                    .iter()
                    .filter(|o| matches!(o, Operation::AddColumn { .. }))
                    .count();
                if others.len() == 2 && drops == 1 && adds == 1 {
                    if !has(column) {
                        return two_clauses(column, Some(1091));
                    }
                    if used_elsewhere(table, column, &Uses::renaming(operations)) != Used::No {
                        return two_clauses(column, None);
                    }
                    drop_then_add = true;
                    continue;
                }
                return two_clauses(column, None);
            }
            // Renames only. By hand: two names swapped, or each moved to
            // the name another gives up, are taken at once; a rename of a
            // name the table does not have yet is refused with 1054.
            let known = all_renames.iter().all(|(from, _)| has(from));
            let free = all_renames
                .iter()
                .all(|(_, to)| !has(to) || all_renames.iter().any(|(from, _)| same(from, to)));
            let distinct = all_renames.iter().enumerate().all(|(at, (from, to))| {
                all_renames[at + 1..]
                    .iter()
                    .all(|(other_from, other_to)| !same(from, other_from) && !same(to, other_to))
            });
            if !(known && free && distinct) {
                return two_clauses(column, (!known).then_some(1054));
            }
            for (from, _) in &all_renames {
                match used_elsewhere(table, from, &Uses::renaming(operations)) {
                    Used::ByPlainCheck => {
                        refuse(
                            3959,
                            format!("a CHECK constraint uses {from}, which the statement renames"),
                        );
                    }
                    Used::Maybe => {
                        return Resolution::Lost(format!(
                            "renames {from}, which a CHECK constraint or a generated column may \
                             use, beside other renames"
                        ));
                    }
                    Used::No => {}
                }
            }
            cycle = true;
        }
        // Keys, CHECK constraints and AFTER name the columns as the statement
        // leaves them (by hand on MySQL 8.0.46 and 8.4.13): a column it drops
        // or renames away is gone (1072 for a key, 1054 for the others), a
        // column it adds or renames to is there.
        if operations.len() > 1 {
            let added = |column: &str| {
                operations.iter().any(|operation| match operation {
                    Operation::AddColumn { column: added, .. } => same(&added.name, column),
                    _ => false,
                }) || all_renames.iter().any(|(_, to)| same(to, column))
            };
            let gone = |column: &str| {
                !added(column)
                    && (all_renames.iter().any(|(from, _)| same(from, column))
                        || operations.iter().any(|operation| {
                            matches!(operation, Operation::DropColumn { name } if same(name, column))
                        }))
            };
            let after_the_statement =
                |column: &str| (has(column) && !gone(column)) || added(column);
            for operation in operations {
                let (columns, code): (Vec<&str>, u16) = match operation {
                    Operation::AddIndex(index) => (
                        index
                            .columns
                            .iter()
                            .map(|entry| indexed_column(entry))
                            .collect(),
                        1072,
                    ),
                    Operation::AddForeignKey(key) => {
                        (key.columns.iter().map(String::as_str).collect(), 1072)
                    }
                    Operation::AddPrimaryKey { columns } => (
                        columns.iter().map(|entry| indexed_column(entry)).collect(),
                        1072,
                    ),
                    Operation::AddCheck(check) => match &check.columns {
                        CheckColumns::Plain(columns) | CheckColumns::Other(columns) => {
                            (columns.iter().map(String::as_str).collect(), 1054)
                        }
                    },
                    _ => (Vec::new(), 0),
                };
                if let Some(column) = columns.into_iter().find(|column| gone(column)) {
                    return Resolution::Lost(format!(
                        "a clause names {column}, which another clause drops or renames; MySQL \
                         refuses that (error {code}, {BY_HAND})"
                    ));
                }
                let position = match operation {
                    Operation::AddColumn { position, .. }
                    | Operation::ModifyColumn { position, .. }
                    | Operation::ChangeColumn { position, .. } => position.as_ref(),
                    _ => None,
                };
                if let Some(Position::After(after)) = position
                    && !after_the_statement(after)
                {
                    return Resolution::Lost(refusal(
                        &format!(
                            "a clause puts a column after {after}, which {name} does not have \
                             after the statement, as far as kilit can tell"
                        ),
                        1054,
                    ));
                }
            }
        } else if let [
            Operation::AddColumn {
                position: Some(Position::After(after)),
                ..
            },
        ] = operations.as_slice()
            && !has(after)
        {
            return Resolution::Lost(refusal(
                &format!("{name} has no column {after} that kilit knows to put the new one after"),
                1054,
            ));
        }
        if let Some((code, why)) = refused {
            return Resolution::Refused { code, why };
        }

        // The order to follow.
        let inline = operations.iter().any(|operation| match operation {
            Operation::ModifyColumn { column, .. } | Operation::ChangeColumn { column, .. } => {
                inline_primary_key(column)
            }
            _ => false,
        });
        let adds_columns = operations.iter().any(|operation| {
            matches!(
                operation,
                Operation::AddColumn { .. }
                    | Operation::ChangeColumn { .. }
                    | Operation::RenameColumn { .. }
            )
        });
        let indexes = operations.iter().any(|operation| {
            matches!(
                operation,
                Operation::AddIndex(_) | Operation::AddForeignKey(_)
            )
        });
        if !drop_then_add && !cycle && !inline && !serial && !(adds_columns && indexes) {
            return Resolution::Runs(None);
        }
        let mut ordered: Vec<Operation> = Vec::new();
        // Drops first, renames at once through names no table has, then
        // the other column clauses, then indexes and keys.
        for operation in operations {
            if matches!(operation, Operation::DropColumn { .. }) {
                ordered.push(operation.clone());
            }
        }
        let mut finish: Vec<Operation> = Vec::new();
        for (at, operation) in operations.iter().enumerate() {
            if cycle && let Some((from, _)) = renamed(operation) {
                let through = format!("\0renaming {at}");
                ordered.push(Operation::RenameColumn {
                    from: from.to_owned(),
                    to: through.clone(),
                });
                finish.push(match operation {
                    Operation::ChangeColumn {
                        column, position, ..
                    } => Operation::ChangeColumn {
                        old_name: through,
                        column: column.clone(),
                        position: position.clone(),
                    },
                    Operation::RenameColumn { to, .. } => Operation::RenameColumn {
                        from: through,
                        to: to.clone(),
                    },
                    _ => unreachable!("renamed() reads only these"),
                });
            }
        }
        ordered.extend(finish);
        for operation in operations {
            let moved = matches!(operation, Operation::DropColumn { .. })
                || (cycle && renamed(operation).is_some())
                || matches!(
                    operation,
                    Operation::AddIndex(_) | Operation::AddForeignKey(_)
                );
            if moved {
                continue;
            }
            match operation {
                Operation::ModifyColumn { column, position } if inline_primary_key(column) => {
                    let mut column = column.clone();
                    column
                        .other_options
                        .retain(|option| !option.eq_ignore_ascii_case("PRIMARY KEY"));
                    column.nullable = Some(false);
                    let name = column.name.clone();
                    ordered.push(Operation::ModifyColumn {
                        column,
                        position: position.clone(),
                    });
                    ordered.push(Operation::AddPrimaryKey {
                        columns: vec![name],
                    });
                }
                Operation::ChangeColumn {
                    old_name,
                    column,
                    position,
                } if inline_primary_key(column) => {
                    let mut column = column.clone();
                    column
                        .other_options
                        .retain(|option| !option.eq_ignore_ascii_case("PRIMARY KEY"));
                    column.nullable = Some(false);
                    let name = column.name.clone();
                    ordered.push(Operation::ChangeColumn {
                        old_name: old_name.clone(),
                        column,
                        position: position.clone(),
                    });
                    ordered.push(Operation::AddPrimaryKey {
                        columns: vec![name],
                    });
                }
                Operation::ModifyColumn { column, position } if is_serial(column) => {
                    ordered.push(Operation::ModifyColumn {
                        column: as_serial(column),
                        position: position.clone(),
                    });
                    ordered.push(serial_index(&column.name));
                }
                Operation::ChangeColumn {
                    old_name,
                    column,
                    position,
                } if is_serial(column) => {
                    ordered.push(Operation::ChangeColumn {
                        old_name: old_name.clone(),
                        column: as_serial(column),
                        position: position.clone(),
                    });
                    ordered.push(serial_index(&column.name));
                }
                other => ordered.push(other.clone()),
            }
        }
        for operation in operations {
            if matches!(
                operation,
                Operation::AddIndex(_) | Operation::AddForeignKey(_)
            ) {
                ordered.push(operation.clone());
            }
        }
        Resolution::Runs(Some(ordered))
    }
}

/// Whether `index` starts with `columns`, so that a foreign key on them can
/// use it.
fn serves(index: &Index, columns: &[String]) -> bool {
    index.columns.len() >= columns.len()
        && index
            .columns
            .iter()
            .zip(columns)
            .all(|(entry, column)| same(indexed_column(entry), column))
}

/// The tables a statement kilit does not model reads, as far as its text
/// shows: each name after FROM or JOIN, without backticks. A name of
/// another database counts as written.
fn read_tables(sql: &str) -> Vec<String> {
    let mut names = Vec::new();
    let mut words = sql
        .split(|c: char| c.is_whitespace() || c == ',' || c == '(' || c == ')' || c == ';')
        .filter(|word| !word.is_empty());
    while let Some(word) = words.next() {
        if (word.eq_ignore_ascii_case("from") || word.eq_ignore_ascii_case("join"))
            && let Some(name) = words.next()
        {
            let name = name.replace('`', "");
            if !name.is_empty() {
                names.push(name);
            }
        }
    }
    names
}

/// Whether something else in the table uses a column, so that MySQL may
/// refuse to rename or drop it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Used {
    No,
    /// A CHECK constraint of plain columns names it: 3959 on a rename.
    ByPlainCheck,
    /// A CHECK constraint or a generated column may name it.
    Maybe,
}

/// What of the table the statement takes away with a column, when kilit
/// asks whether something else uses it.
struct Uses<'a> {
    /// The columns the statement drops: their own uses count for nothing.
    dropped_columns: Vec<&'a str>,
    /// The CHECK constraints the statement drops (DROP CHECK or DROP
    /// CONSTRAINT): they count for nothing.
    dropped_checks: Vec<&'a str>,
    /// The column is dropped: a CHECK constraint that uses it alone goes
    /// with it.
    drops: bool,
}

impl Uses<'_> {
    /// For a rename, or a DROP and ADD of one name.
    fn renaming(operations: &[Operation]) -> Uses<'_> {
        Uses {
            dropped_columns: Vec::new(),
            dropped_checks: dropped_checks(operations),
            drops: false,
        }
    }
}

/// The names of the CHECK constraints, or constraints of any kind, that
/// the statement drops.
fn dropped_checks(operations: &[Operation]) -> Vec<&str> {
    operations
        .iter()
        .filter_map(|operation| match operation {
            Operation::DropCheck { name } | Operation::DropConstraint { name } => {
                Some(name.as_str())
            }
            _ => None,
        })
        .collect()
}

fn used_elsewhere(table: &Table, column: &str, uses: &Uses<'_>) -> Used {
    let dropped = &uses.dropped_columns;
    let mut used = Used::No;
    for check in &table.checks {
        let name = check.name.as_deref().unwrap_or_default();
        if uses
            .dropped_checks
            .iter()
            .any(|dropped| same(dropped, name))
            || (uses.drops && uses_only(check, column))
        {
            continue;
        }
        match &check.columns {
            CheckColumns::Plain(columns) if columns.iter().any(|c| same(c, column)) => {
                used = Used::ByPlainCheck;
            }
            CheckColumns::Other(names) if names.iter().any(|c| same(c, column)) => {
                return Used::Maybe;
            }
            _ => {}
        }
    }
    let lower = column.to_ascii_lowercase();
    if table.columns.iter().any(|other| {
        !dropped.iter().any(|name| same(name, &other.name))
            && other
                .generated
                .as_ref()
                .is_some_and(|generated| generated.expression.to_ascii_lowercase().contains(&lower))
    }) {
        return Used::Maybe;
    }
    used
}

/// `table` as `alter` leaves it, when kilit can follow every one of its
/// changes; else why it cannot tell. A new table name changes nothing
/// here; MySQL takes a RENAME INDEX beside other changes against the names
/// before the statement, which kilit does not follow.
pub(crate) fn table_after(schema: &Schema, table: &Table, alter: &Alter) -> Result<Table, String> {
    let operations = match schema.resolve(table, alter) {
        // MySQL refuses it for a name: the table stays as it was.
        Resolution::Refused { .. } => return Ok(table.clone()),
        Resolution::Lost(why) => return Err(why),
        Resolution::Runs(None) => Cow::Borrowed(alter.operations.as_slice()),
        Resolution::Runs(Some(ordered)) => Cow::Owned(ordered),
    };
    let mut after = table.clone();
    for operation in operations.iter() {
        match operation {
            Operation::RenameTable { .. } => {}
            Operation::RenameIndex { .. } if alter.operations.len() > 1 => {
                return Err("renames an index beside other changes".to_owned());
            }
            other => change(&mut after, other)?,
        }
    }
    Ok(after)
}

/// Changes `table` as one `ALTER TABLE` operation does, or says why kilit
/// cannot follow it.
fn change(table: &mut Table, operation: &Operation) -> Result<(), String> {
    match operation {
        // Each option written replaces the table's; the others stay, as
        // MySQL keeps them. kilit does not keep the comment.
        Operation::ChangeTableOptions(options) => {
            if let Some(engine) = &options.engine {
                table.engine = Some(engine.clone());
            }
            if let Some(row_format) = &options.row_format {
                table.row_format = Some(row_format.clone());
            }
            if let Some(size) = &options.key_block_size {
                table.key_block_size = Some(size.clone());
            }
        }
        Operation::AddColumn { column, position } => {
            if table.position_of(&column.name).is_some() {
                return Err(format!("adds {}, which the table already has", column.name));
            }
            unfollowed_options(column)?;
            let at = insert_at(table, position.as_ref(), table.columns.len())?;
            table.columns.insert(at, column.clone());
        }
        // A drop of what the table does not have changes nothing: MySQL
        // refuses it (error 1091), and if kilit's tables are out of date
        // and the table had it, what kilit knows of the table stays right.
        Operation::DropColumn { name } => {
            let Some(at) = table.position_of(name) else {
                return Ok(());
            };
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
            // And a CHECK constraint that uses no other column. One that does
            // makes MySQL refuse the statement, which kilit may follow anyway
            // when it gives no answer: such a constraint stays.
            table.checks.retain(|check| !uses_only(check, name));
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
        // By hand on MySQL 8.0.46 and 8.4.13: the column has no default
        // after it, whatever it had; of a column the table does not have,
        // MySQL refuses it (error 1054), and the table stays as it was.
        Operation::DropColumnDefault { column } => {
            if let Some(at) = table.position_of(column) {
                table.columns[at].default = None;
            }
        }
        // By hand on MySQL 8.0.46 and 8.4.13: only the name changes. MySQL
        // refuses an index that is not there (1176), a name another index
        // has in any letter case (1061) and PRIMARY (1280); the table stays
        // as it was then.
        Operation::RenameIndex { from, to } => {
            if same(from, "PRIMARY") || same(to, "PRIMARY") {
                return Ok(());
            }
            let named = |name: &str| {
                table
                    .indexes
                    .iter()
                    .position(|index| index.name.as_deref().is_some_and(|n| same(n, name)))
            };
            match named(from) {
                Some(at) if named(to).is_none_or(|other| other == at) => {
                    table.indexes[at].name = Some(to.clone());
                }
                Some(_) => {}
                // MySQL named the index kilit holds without a name, and
                // kilit cannot tell whether it is this one.
                None if table.indexes.iter().any(|index| index.name.is_none()) => {
                    return Err(format!(
                        "renames the index {from}, which may be one kilit holds without its name"
                    ));
                }
                None => {}
            }
        }
        // MySQL keeps the counter apart from the table's definition, and so
        // does kilit: by hand on MySQL 8.0.46 and 8.4.13, SHOW CREATE TABLE
        // changes only in AUTO_INCREMENT=.
        Operation::SetAutoIncrement { .. } => {}
        // MySQL refuses an index of a name the table has in any letter case
        // (1061) and one on a column the table does not have (1072), by hand
        // on MySQL 8.0.46 and 8.4.13; the table stays as it was then.
        Operation::AddIndex(index) if refused_index(table, index) => {}
        Operation::AddIndex(index) => {
            let mut index = index.clone();
            if index.name.is_none() {
                index.name = Some(generated_index_name(table, &index));
            }
            table.indexes.push(index);
        }
        Operation::DropIndex { name } => {
            // `PRIMARY` is the primary key, which kilit keeps apart.
            if same(name, "PRIMARY") {
                return Err(format!("drops the index {name}"));
            }
            match table
                .indexes
                .iter()
                .position(|index| index.name.as_deref().is_some_and(|n| same(n, name)))
            {
                Some(at) => {
                    table.indexes.remove(at);
                }
                // MySQL named the index kilit holds without a name, and
                // kilit cannot tell whether it is this one.
                None if table.indexes.iter().any(|index| index.name.is_none()) => {
                    return Err(format!(
                        "drops the index {name}, which may be one kilit holds without its name"
                    ));
                }
                None => {}
            }
        }
        Operation::AddPrimaryKey { columns } => columns.clone_into(&mut table.primary_key),
        Operation::DropPrimaryKey => table.primary_key.clear(),
        // MySQL keeps no foreign key on a MyISAM table, only its index, and
        // drops one of any name without complaint.
        Operation::AddForeignKey(foreign_key) if table.engine == Some(Engine::MyIsam) => {
            index_ignored_foreign_key(table, foreign_key)?;
        }
        Operation::DropForeignKey { .. } if table.engine == Some(Engine::MyIsam) => {}
        Operation::AddForeignKey(foreign_key) => {
            table.foreign_keys.push(foreign_key.clone());
            index_foreign_key(table, foreign_key);
        }
        Operation::DropForeignKey { name } => {
            // The index MySQL made for the key stays.
            match table
                .foreign_keys
                .iter()
                .position(|key| key.name.as_deref().is_some_and(|n| same(n, name)))
            {
                Some(at) => {
                    table.foreign_keys.remove(at);
                }
                None if table.foreign_keys.iter().any(|key| key.name.is_none()) => {
                    return Err(format!(
                        "drops the foreign key {name}, which may be one kilit holds without \
                         its name"
                    ));
                }
                None => {}
            }
        }
        Operation::AddCheck(check) => {
            let mut check = check.clone();
            match &check.name {
                Some(name) if has_name(table.checks.iter().map(|c| &c.name), name) => {
                    return Err(format!(
                        "adds the CHECK constraint {name}, which the table already has"
                    ));
                }
                Some(_) => {}
                None => check.name = Some(generated_check_name(table)),
            }
            table.checks.push(check);
        }
        Operation::DropConstraint { name } => match table.constraints_named(name).as_slice() {
            [ConstraintKind::Check] => table.checks.retain(|check| !has_name([&check.name], name)),
            // The index MySQL made for the key stays.
            [ConstraintKind::ForeignKey] => table
                .foreign_keys
                .retain(|key| !has_name([&key.name], name)),
            [ConstraintKind::UniqueIndex] => table
                .indexes
                .retain(|index| index.kind != IndexKind::Unique || !has_name([&index.name], name)),
            [ConstraintKind::PrimaryKey] => table.primary_key.clear(),
            // MySQL named a foreign key kilit holds without a name, and
            // kilit cannot tell whether it is this one.
            [] if table.foreign_keys.iter().any(|key| key.name.is_none()) => {
                return Err(format!(
                    "drops the constraint {name}, which may be a foreign key kilit holds \
                     without its name"
                ));
            }
            // MySQL refuses a name the table does not have (error 3940, by
            // the lab), and if kilit's tables are out of date and the table
            // had it, what kilit knows of the table stays right but for it.
            [] => {}
            _ => {
                return Err(format!(
                    "drops the constraint {name}, a name that several of its constraints have"
                ));
            }
        },
        Operation::DropCheck { name } => match table.check_target(name) {
            CheckTarget::Check(at) => {
                table.checks.remove(at);
            }
            // MySQL 8.0.46 and 8.4.13 refuse the statement: the table stays.
            CheckTarget::NotACheck(_) => {}
            // MySQL refuses it (error 3821, by the lab): see DropConstraint.
            CheckTarget::Nothing => {}
            CheckTarget::Several => {
                return Err(format!(
                    "drops the CHECK constraint {name}, a name that several of its constraints \
                     have"
                ));
            }
        },
        Operation::AlterCheck { name, enforced, .. } => match table.check_target(name) {
            CheckTarget::Check(at) => table.checks[at].enforced = *enforced,
            // MySQL 8.0.46 and 8.4.13 refuse the statement: the table stays.
            CheckTarget::NotACheck(_) => {}
            // MySQL refuses it (error 3821 for ALTER CHECK, 3940 for ALTER
            // CONSTRAINT, by the lab): see DropConstraint.
            CheckTarget::Nothing => {}
            CheckTarget::Several => {
                return Err(format!(
                    "changes the CHECK constraint {name}, a name that several of its \
                     constraints have"
                ));
            }
        },
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
    // MySQL refuses to rename a column a CHECK constraint uses, but kilit may
    // follow the statement anyway when it gives no answer.
    for check in &mut table.checks {
        let (CheckColumns::Plain(names) | CheckColumns::Other(names)) = &mut check.columns;
        for name in names {
            if same(name, from) {
                to.clone_into(name);
            }
        }
    }
}

/// Whether a plain CHECK constraint uses `column` and no other column. Of any
/// other expression kilit cannot tell.
fn uses_only(check: &Check, column: &str) -> bool {
    match &check.columns {
        CheckColumns::Plain(columns) => {
            !columns.is_empty() && columns.iter().all(|c| same(c, column))
        }
        CheckColumns::Other(_) => false,
    }
}

/// Whether one of `names` is `name`, in any case.
fn has_name<'a>(names: impl IntoIterator<Item = &'a Option<String>>, name: &str) -> bool {
    names
        .into_iter()
        .any(|found| found.as_deref().is_some_and(|found| same(found, name)))
}

/// The start of the name MySQL gives a CHECK constraint of `table`, without
/// its database: `orders_chk_` for `shop.orders`.
fn generated_check_prefix(table: &str) -> String {
    let table = table.rsplit('.').next().unwrap_or(table);
    format!("{table}_chk_")
}

/// The number in a name that `prefix` starts, such as `2` in `orders_chk_2`.
/// MySQL 8.0.46 and 8.4.13 compare the letters of the prefix exactly: they do
/// not take `ORDERS_CHK_9` for the automatic name of a CHECK of `orders`.
fn generated_check_number<'a>(prefix: &str, name: &'a str) -> Option<&'a str> {
    name.strip_prefix(prefix)
        .filter(|number| !number.is_empty() && number.bytes().all(|b| b.is_ascii_digit()))
}

/// In a new table, MySQL names the CHECK constraints left without a name
/// `<table>_chk_1`, `<table>_chk_2` and so on, in the order written, whatever
/// names the others have.
fn name_new_checks(table: &mut Table) {
    let prefix = generated_check_prefix(&table.name);
    let unnamed = table.checks.iter_mut().filter(|check| check.name.is_none());
    for (number, check) in (1..).zip(unnamed) {
        check.name = Some(format!("{prefix}{number}"));
    }
}

/// The name MySQL gives a CHECK constraint added to a table without one: the
/// number after the largest automatic one the table has.
fn generated_check_name(table: &Table) -> String {
    let prefix = generated_check_prefix(&table.name);
    let largest = table
        .checks
        .iter()
        .filter_map(|check| generated_check_number(&prefix, check.name.as_deref()?))
        .filter_map(|number| number.parse::<u64>().ok())
        .max()
        .unwrap_or(0);
    format!("{prefix}{}", largest + 1)
}

/// MySQL renames a table's CHECK names of the form `<table>_chk_<suffix>`
/// along with the table, and its foreign key names `<table>_ibfk_<suffix>`
/// too, whatever the suffix, written or generated.
fn rename_generated_checks(table: &mut Table, to: &str) {
    // By hand on 8.0.46 and 8.4.13, for both: every name that starts with
    // the old table's prefix, exactly as written, and goes on after it.
    let from = generated_check_prefix(&table.name);
    let to_prefix = generated_check_prefix(to);
    for check in &mut table.checks {
        let Some(name) = &mut check.name else {
            continue;
        };
        if let Some(suffix) = name.strip_prefix(&from).filter(|suffix| !suffix.is_empty()) {
            *name = format!("{to_prefix}{suffix}");
        }
    }
    let from = generated_foreign_key_prefix(&table.name);
    let to_prefix = generated_foreign_key_prefix(to);
    for key in &mut table.foreign_keys {
        let Some(name) = &mut key.name else {
            continue;
        };
        if let Some(suffix) = name.strip_prefix(&from).filter(|suffix| !suffix.is_empty()) {
            let renamed = format!("{to_prefix}{suffix}");
            key.former_names.push(std::mem::replace(name, renamed));
        }
    }
}

/// The start of the name MySQL gives a foreign key of `table` written
/// without one: `orders_ibfk_` for `shop.orders`.
fn generated_foreign_key_prefix(table: &str) -> String {
    let table = table.rsplit('.').next().unwrap_or(table);
    format!("{table}_ibfk_")
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

/// The index MySQL adds for a foreign key on a MyISAM table, which keeps
/// the key itself nowhere: named after the constraint, or after the column
/// when the key has no name, unless a plain index of exactly that column
/// is there already. The lab saw these shapes only; any other loses the
/// table.
fn index_ignored_foreign_key(table: &mut Table, key: &ForeignKey) -> Result<(), String> {
    let [column] = key.columns.as_slice() else {
        return Err(
            "adds a foreign key of several columns to a MyISAM table, and kilit has not \
             seen which index MySQL adds for it"
                .into(),
        );
    };
    let exactly = |columns: &[String]| matches!(columns, [only] if same(only, column));
    if table
        .indexes
        .iter()
        .any(|index| index.kind == IndexKind::Plain && exactly(&index.columns))
    {
        return Ok(());
    }
    let uses = |columns: &[String]| {
        columns
            .iter()
            .any(|entry| same(indexed_column(entry), column))
    };
    if uses(&table.primary_key) || table.indexes.iter().any(|index| uses(&index.columns)) {
        return Err(format!(
            "adds a foreign key on {column}, which an index of the MyISAM table has in \
             another shape, and kilit has not seen which index MySQL adds then"
        ));
    }
    let name = key.name.clone().unwrap_or_else(|| column.clone());
    if table
        .indexes
        .iter()
        .any(|index| index.name.as_deref().is_some_and(|n| same(n, &name)))
    {
        return Err(format!(
            "adds a foreign key whose index would be named {name}, a name the MyISAM table \
             already has"
        ));
    }
    table.indexes.push(Index {
        name: Some(name),
        kind: IndexKind::Plain,
        columns: key.columns.clone(),
    });
    Ok(())
}

/// The engine a statement kilit could not read writes: the last `ENGINE`
/// in its text, with or without `=` and quotes, as MySQL takes it. A word
/// `ENGINE` outside quotes counts, even a column's name, which can only
/// make a table's engine unknown. `None` when the text has no such word.
fn written_engine(sql: &str) -> Option<Engine> {
    let is_word = |c: char| c.is_ascii_alphanumeric() || c == '_' || c == '$';
    let mut engine = None;
    for start in word_starts(sql, "ENGINE") {
        let end = start + "ENGINE".len();
        let rest = sql[end..].trim_start();
        let rest = rest.strip_prefix('=').unwrap_or(rest).trim_start();
        let (quote, rest) = match rest.chars().next() {
            Some(quote @ ('\'' | '"' | '`')) => (Some(quote), &rest[1..]),
            _ => (None, rest),
        };
        let length = rest.find(|c: char| !is_word(c)).unwrap_or(rest.len());
        let name = &rest[..length];
        let closed = quote.is_none_or(|quote| rest[length..].starts_with(quote));
        engine = Some(if name.is_empty() || !closed {
            Engine::Unknown
        } else {
            Engine::named(name)
        });
    }
    engine
}

/// Whether a table may have a row format other than the server's default:
/// its `ROW_FORMAT` names another one or none kilit knows, or it has a
/// `KEY_BLOCK_SIZE` other than 0, which compresses it.
fn row_format_unclear(table: &Table) -> bool {
    let written = table
        .row_format
        .as_ref()
        .is_some_and(|format| !matches!(format, RowFormat::Default | RowFormat::Dynamic));
    written
        || table
            .key_block_size
            .as_deref()
            .is_some_and(|size| size != "0")
}

/// Whether a statement kilit could not read writes `ROW_FORMAT` or
/// `KEY_BLOCK_SIZE`, outside quotes: the table it names may have any row
/// format after it.
fn writes_row_format(sql: &str) -> bool {
    word_starts(sql, "ROW_FORMAT").next().is_some()
        || word_starts(sql, "KEY_BLOCK_SIZE").next().is_some()
}

/// Where the word `word` (in uppercase) starts in `sql`, in any letter
/// case, outside quotes, strings and backticks.
fn word_starts<'a>(sql: &'a str, word: &'a str) -> impl Iterator<Item = usize> + 'a {
    let is_word = |c: char| c.is_ascii_alphanumeric() || c == '_' || c == '$';
    let masked = outside_quotes(sql);
    let mut from = 0;
    std::iter::from_fn(move || {
        while let Some(found) = masked[from..].find(word) {
            let start = from + found;
            let end = start + word.len();
            from = end;
            let before = masked[..start].chars().next_back();
            let after = masked[end..].chars().next();
            if !(before.is_some_and(is_word) || after.is_some_and(is_word)) {
                return Some(start);
            }
        }
        None
    })
}

/// `sql` in uppercase, with what is inside quotes, strings and backticks
/// blanked out, byte for byte, so that positions stay the same.
fn outside_quotes(sql: &str) -> String {
    let mut masked = sql.to_ascii_uppercase().into_bytes();
    let mut quote = None;
    let mut escaped = false;
    for byte in &mut masked {
        match quote {
            None => {
                if matches!(*byte, b'\'' | b'"' | b'`') {
                    quote = Some(*byte);
                }
            }
            Some(open) => {
                if escaped {
                    escaped = false;
                    *byte = b' ';
                } else if *byte == b'\\' && open != b'`' {
                    escaped = true;
                    *byte = b' ';
                } else if *byte == open {
                    quote = None;
                } else {
                    *byte = b' ';
                }
            }
        }
    }
    // Only whole characters inside quotes were blanked.
    String::from_utf8(masked).unwrap_or_default()
}

/// Whether a `FOREIGN KEY` in `sql` names its index: `FOREIGN KEY ix (c)`.
pub(crate) fn names_foreign_key_index(sql: &str) -> bool {
    let masked = outside_quotes(sql);
    word_starts(sql, "FOREIGN").any(|start| {
        let rest = masked[start + "FOREIGN".len()..].trim_start();
        rest.strip_prefix("KEY")
            .is_some_and(|rest| !rest.trim_start().starts_with('('))
    })
}

/// Whether `sql` orders a column `DESC`, outside quotes.
pub(crate) fn has_descending(sql: &str) -> bool {
    word_starts(sql, "DESC").next().is_some()
}

const MYISAM_INDEX_NAME: &str = "adds a foreign key that names its index to a MyISAM table, and \
     kilit does not follow which name MySQL gives the index then";

/// The table after the word `LIKE`, without quotes: `old` in `CREATE TABLE
/// new LIKE old`.
fn like_source(sql: &str) -> Option<&str> {
    let upper = sql.to_ascii_uppercase();
    let is_word = |c: char| c.is_ascii_alphanumeric() || c == '_' || c == '$';
    let mut from = 0;
    while let Some(found) = upper[from..].find("LIKE") {
        let start = from + found;
        let end = start + "LIKE".len();
        from = end;
        let before = sql[..start].chars().next_back();
        let after = sql[end..].chars().next();
        if before.is_some_and(is_word) || after.is_some_and(is_word) {
            continue;
        }
        let rest = sql[end..].trim_start().trim_start_matches('`');
        let length = rest
            .find(|c: char| !(is_word(c) || c == '.' || c == '`'))
            .unwrap_or(rest.len());
        let name = rest[..length].trim_end_matches('`');
        return (!name.is_empty()).then_some(name);
    }
    None
}

/// Whether the statement starts with `CREATE TEMPORARY`.
pub(crate) fn creates_temporary(sql: &str) -> bool {
    let mut words = sql.split_whitespace();
    creates(sql)
        && words
            .next()
            .is_some_and(|word| word.eq_ignore_ascii_case("CREATE"))
        && words
            .next()
            .is_some_and(|word| word.eq_ignore_ascii_case("TEMPORARY"))
}

fn creates(sql: &str) -> bool {
    sql.trim_start()
        .get(..6)
        .is_some_and(|word| word.eq_ignore_ascii_case("CREATE"))
}

/// The name MySQL gives an index added without one: its first column's,
/// with `_2`, `_3` and so on when that is taken.
/// Whether `sql` writes the word REFERENCES, as a foreign key does.
fn writes_references(sql: &str) -> bool {
    sql.split(|c: char| !c.is_ascii_alphanumeric() && c != '_')
        .any(|word| word.eq_ignore_ascii_case("references"))
}

/// Whether `operation`, a CHECK constraint clause or `DROP CONSTRAINT`,
/// names what `table` does not have.
fn missing_constraint(table: &Table, operation: &Operation, name: &str) -> bool {
    match operation {
        Operation::DropConstraint { .. } => table.constraints_named(name).is_empty(),
        _ => table.check_target(name) == CheckTarget::Nothing,
    }
}

/// Whether MySQL refuses to add `index` to `table`: its name is one the
/// table's indexes have in any letter case, or a column of it is one the
/// table does not have. An expression part names no column.
pub(crate) fn refused_index(table: &Table, index: &Index) -> bool {
    let taken = index.name.as_deref().is_some_and(|name| {
        table
            .indexes
            .iter()
            .any(|other| other.name.as_deref().is_some_and(|n| same(n, name)))
    });
    // The parser cuts a name that starts with a digit, such as `2e`, at the
    // digits: such a key part says nothing kilit can check.
    let missing = index.columns.iter().any(|entry| {
        let column = indexed_column(entry.trim());
        !column.starts_with(|c: char| c == '(' || c.is_ascii_digit())
            && table.position_of(column.trim_matches('`')).is_none()
    });
    taken || missing
}

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
pub(crate) fn indexed_column(entry: &str) -> &str {
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

/// What `DROP CONSTRAINT` can drop. A plain index is no constraint for MySQL.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ConstraintKind {
    Check,
    ForeignKey,
    UniqueIndex,
    PrimaryKey,
}

/// What `DROP CHECK name`, `ALTER CHECK name` and `ALTER CONSTRAINT name ...
/// ENFORCED` find in a table. By hand, MySQL 8.0.46 and 8.4.13 refuse each
/// of them for a name that is not a CHECK constraint.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum CheckTarget {
    /// The table's one constraint with the name is a CHECK constraint, at
    /// this place in [`Table::checks`], and no index has the name.
    Check(usize),
    /// No CHECK constraint has the name, but these have: "a foreign key", "a
    /// unique index", "the primary key", or else "an index".
    NotACheck(Vec<&'static str>),
    /// Nothing in the table has the name, as far as kilit can tell.
    Nothing,
    /// A CHECK constraint has the name, and so does another constraint or an
    /// index.
    Several,
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

    /// Every constraint of the table with this name, in any case. MySQL 8.0.46
    /// and 8.4.13 refuse `DROP CONSTRAINT` when there are several: a foreign
    /// key and a plain index count as one, as Laravel's tables have them.
    pub(crate) fn constraints_named(&self, name: &str) -> Vec<ConstraintKind> {
        let mut kinds = Vec::new();
        if has_name(self.checks.iter().map(|check| &check.name), name) {
            kinds.push(ConstraintKind::Check);
        }
        if has_name(self.foreign_keys.iter().map(|key| &key.name), name) {
            kinds.push(ConstraintKind::ForeignKey);
        }
        let unique = self
            .indexes
            .iter()
            .filter(|index| index.kind == IndexKind::Unique);
        if has_name(unique.map(|index| &index.name), name) {
            kinds.push(ConstraintKind::UniqueIndex);
        }
        if same(name, "PRIMARY") && !self.primary_key.is_empty() {
            kinds.push(ConstraintKind::PrimaryKey);
        }
        kinds
    }

    /// What a CHECK constraint clause finds under `name`, in any case.
    pub(crate) fn check_target(&self, name: &str) -> CheckTarget {
        let kinds = self.constraints_named(name);
        let index = self
            .indexes
            .iter()
            .any(|index| index.kind != IndexKind::Unique && has_name([&index.name], name));
        match (kinds.as_slice(), index) {
            ([ConstraintKind::Check], false) => self
                .checks
                .iter()
                .position(|check| has_name([&check.name], name))
                .map_or(CheckTarget::Nothing, CheckTarget::Check),
            ([], false) => CheckTarget::Nothing,
            ([], true) => CheckTarget::NotACheck(vec!["an index"]),
            (kinds, _) if kinds.contains(&ConstraintKind::Check) => CheckTarget::Several,
            (kinds, _) => CheckTarget::NotACheck(
                kinds
                    .iter()
                    .map(|kind| match kind {
                        ConstraintKind::Check => "a CHECK constraint",
                        ConstraintKind::ForeignKey => "a foreign key",
                        ConstraintKind::UniqueIndex => "a unique index",
                        ConstraintKind::PrimaryKey => "the primary key",
                    })
                    .collect(),
            ),
        }
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

        let orders = known(&schema, "orders");
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
    fn check_and_foreign_key_names_of_mysql_s_form_follow_the_table_whatever_their_suffix() {
        // By hand on 8.0.46 and 8.4.13: the prefix must match exactly and be
        // followed by something; the rest is kept.
        for rename in [
            "rename table `orders` to `purchases`",
            "alter table `orders` rename to `purchases`",
        ] {
            let schema = after(&format!(
                "create table `users` (`id` int primary key);
                 create table `orders` (`id` int primary key, `user_id` int, `amount` int,
                     constraint `orders_chk_7` check (`amount` > 0),
                     constraint `orders_chk_abc` check (`amount` > 1),
                     constraint `orders_chk_0` check (`amount` > 2),
                     constraint `orders_chk_` check (`amount` > 3),
                     constraint `ORDERS_CHK_5` check (`amount` > 4),
                     constraint `orders_ibfk_abc` foreign key (`user_id`) references `users` (`id`),
                     constraint `orders_ibfk_` foreign key (`user_id`) references `users` (`id`));
                 {rename}"
            ));
            let table = known(&schema, "purchases");
            let checks: Vec<&str> = table
                .checks
                .iter()
                .filter_map(|check| check.name.as_deref())
                .collect();
            assert_eq!(
                checks,
                [
                    "purchases_chk_7",
                    "purchases_chk_abc",
                    "purchases_chk_0",
                    "orders_chk_",
                    "ORDERS_CHK_5"
                ],
                "{rename}"
            );
            let keys: Vec<&str> = table
                .foreign_keys
                .iter()
                .filter_map(|key| key.name.as_deref())
                .collect();
            assert_eq!(keys, ["purchases_ibfk_abc", "orders_ibfk_"], "{rename}");
        }
    }

    #[test]
    fn dropping_what_a_table_does_not_have_keeps_it_known_and_unchanged() {
        let sql = "create table `shops` (`id` int primary key, `name` varchar(50), \
                   index `i_name` (`name`))";
        let before = after(sql);
        let schema = after(&format!(
            "{sql}; alter table `shops` drop `nope`; alter table `shops` drop index `nope`; \
             alter table `shops` drop foreign key `nope`"
        ));
        assert_eq!(schema.lost().count(), 0);
        assert_eq!(known(&schema, "shops"), known(&before, "shops"));
    }

    #[test]
    fn a_change_kilit_cannot_follow_loses_the_table() {
        let schema = after(
            "alter table `orders` alter column `note` set not null;
             ALTER TABLE users CONVERT TO CHARACTER SET latin1;
             alter table `shops` rename column `nope` to `yes`",
        );
        let lost: Vec<(&str, &str)> = schema.lost().collect();
        assert_eq!(
            lost,
            [
                (
                    "orders",
                    "line 1: kilit does not follow `ALTER COLUMN `note` SET NOT NULL`"
                ),
                (
                    "shops",
                    "line 3: shops has no column nope that kilit knows, and MySQL refuses the \
                     statement (error 1054, by hand on MySQL 8.0.46 and 8.4.13); kilit does not \
                     answer that from its tables alone, which may be out of date"
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

    /// The table's CHECK constraints as `name: expression`, in its order.
    fn checks(table: &Table) -> Vec<String> {
        table
            .checks
            .iter()
            .map(|check| {
                format!(
                    "{}: {}",
                    check.name.as_deref().unwrap_or("?"),
                    check.expression
                )
            })
            .collect()
    }

    /// The tables after `sql`, from nothing.
    fn from_sql(sql: &str) -> Schema {
        Schema::from_sql(sql)
    }

    #[test]
    fn a_new_table_names_its_checks_in_the_order_written() {
        // MySQL 8.0.46 and 8.4.13 do the same (run by hand).
        let schema = from_sql(
            "CREATE TABLE t (
                 a INT CHECK (a > 0),
                 b INT CONSTRAINT b_pos CHECK (b > 0) NOT ENFORCED,
                 c INT, CHECK (c > 0),
                 CONSTRAINT named CHECK (a > b),
                 d INT CHECK (d > 0),
                 CONSTRAINT CHECK (b < 100),
                 CONSTRAINT t_chk_9 CHECK (a < 9)
             );",
        );
        assert_eq!(
            checks(&known(&schema, "t")),
            [
                "t_chk_1: a > 0",
                "b_pos: b > 0",
                "t_chk_2: c > 0",
                "named: a > b",
                "t_chk_3: d > 0",
                "t_chk_4: b < 100",
                "t_chk_9: a < 9",
            ]
        );
    }

    #[test]
    fn an_added_check_takes_the_number_after_the_largest() {
        // MySQL 8.0.46 and 8.4.13 do the same (run by hand). They compare the
        // letters of an automatic name exactly, so capitals do not count.
        let schema = from_sql(
            "CREATE TABLE orders (amount INT);
             ALTER TABLE orders ADD CHECK (amount > 0);
             ALTER TABLE orders ADD CONSTRAINT CHECK (amount > 1);
             ALTER TABLE orders ADD CONSTRAINT orders_chk_7 CHECK (amount > 2);
             ALTER TABLE orders ADD CHECK (amount > 3);
             ALTER TABLE orders DROP CONSTRAINT orders_chk_8;
             ALTER TABLE orders DROP CONSTRAINT orders_chk_7;
             ALTER TABLE orders ADD CHECK (amount > 4);
             ALTER TABLE orders ADD CONSTRAINT ORDERS_CHK_9 CHECK (amount > 5);
             ALTER TABLE orders ADD CONSTRAINT orders_chk_x CHECK (amount > 6);
             ALTER TABLE orders ADD CHECK (amount > 7);",
        );
        assert_eq!(schema.lost().count(), 0);
        assert_eq!(
            checks(&known(&schema, "orders")),
            [
                "orders_chk_1: amount > 0",
                "orders_chk_2: amount > 1",
                "orders_chk_3: amount > 4",
                "ORDERS_CHK_9: amount > 5",
                "orders_chk_x: amount > 6",
                "orders_chk_4: amount > 7",
            ]
        );
    }

    #[test]
    fn automatic_check_names_leave_out_the_database() {
        let schema = from_sql(
            "CREATE TABLE shop.orders (amount INT, CHECK (amount > 0));
             ALTER TABLE shop.orders ADD CHECK (amount > 1);",
        );
        assert_eq!(
            checks(&known(&schema, "shop.orders")),
            ["orders_chk_1: amount > 0", "orders_chk_2: amount > 1"]
        );
    }

    #[test]
    fn renaming_a_table_renames_its_automatic_check_names() {
        // By hand on MySQL 8.0.46 and 8.4.13, in both spellings, both do the
        // same: orders_chk_x becomes sales_chk_x.
        let schema = from_sql(
            "CREATE TABLE orders (amount INT, CHECK (amount > 0),
                 CONSTRAINT c_one CHECK (amount > 1), CONSTRAINT orders_chk_x CHECK (amount > 2));
             RENAME TABLE orders TO sales;",
        );
        let renamed = [
            "sales_chk_1: amount > 0",
            "c_one: amount > 1",
            "sales_chk_x: amount > 2",
        ];
        assert_eq!(checks(&known(&schema, "sales")), renamed);
        let schema = from_sql(
            "CREATE TABLE orders (amount INT, CHECK (amount > 0),
                 CONSTRAINT c_one CHECK (amount > 1), CONSTRAINT orders_chk_x CHECK (amount > 2));
             ALTER TABLE orders RENAME TO sales;",
        );
        assert_eq!(checks(&known(&schema, "sales")), renamed);
    }

    #[test]
    fn dropping_a_column_drops_a_check_that_uses_no_other_column() {
        let schema = from_sql(
            "CREATE TABLE t (a INT, b INT, c VARCHAR(9),
                 CONSTRAINT alone CHECK (a > 0),
                 CONSTRAINT twice CHECK (`A` > 0 AND a < 9),
                 CONSTRAINT two CHECK (a > b),
                 CONSTRAINT other_column CHECK (b > 0),
                 CONSTRAINT not_plain CHECK (char_length(c) > a),
                 CONSTRAINT no_column CHECK (1 > 0));
             ALTER TABLE t DROP COLUMN a;",
        );
        // kilit follows a statement it gives no answer for: `two` makes MySQL
        // refuse this one, and `not_plain` keeps a name it may not use.
        assert_eq!(
            checks(&known(&schema, "t")),
            [
                "two: a > b",
                "other_column: b > 0",
                "not_plain: char_length(c) > a",
                "no_column: 1 > 0",
            ]
        );
    }

    #[test]
    fn a_column_of_a_new_name_is_the_one_a_check_uses() {
        let schema = from_sql(
            "CREATE TABLE t (a INT, b INT, c VARCHAR(9),
                 CONSTRAINT plain CHECK (`a` > b),
                 CONSTRAINT other CHECK (char_length(A) > 0));
             ALTER TABLE t RENAME COLUMN a TO x;
             ALTER TABLE t CHANGE b y INT;",
        );
        let table = known(&schema, "t");
        assert_eq!(
            table.checks[0].columns,
            CheckColumns::Plain(vec!["x".into(), "y".into()])
        );
        assert_eq!(
            table.checks[1].columns,
            CheckColumns::Other(vec!["char_length".into(), "x".into()])
        );
        // The expression stays as written.
        assert_eq!(table.checks[0].expression, "`a` > b");
    }

    #[test]
    fn drop_constraint_finds_the_right_kind_of_constraint() {
        let dump = "CREATE TABLE users (id INT PRIMARY KEY);
            CREATE TABLE orders (
                id INT NOT NULL, user_id INT NOT NULL, email VARCHAR(50), note VARCHAR(50),
                PRIMARY KEY (id),
                UNIQUE KEY uq_email (email),
                KEY fk_user (user_id),
                KEY i_note (note),
                CONSTRAINT fk_user FOREIGN KEY (user_id) REFERENCES users (id),
                CONSTRAINT chk_note CHECK (note IS NULL)
            );";
        let after = |change: &str| {
            let mut schema = Schema::from_sql(dump);
            for statement in parse(change) {
                schema.apply(&statement);
            }
            schema
        };
        // A CHECK constraint, in any case.
        let orders = known(
            &after("ALTER TABLE orders DROP CONSTRAINT CHK_NOTE"),
            "orders",
        );
        assert!(orders.checks.is_empty());
        assert_eq!(orders.foreign_keys.len(), 1);
        // A foreign key goes, and its plain index of the same name stays.
        let orders = known(
            &after("ALTER TABLE orders DROP CONSTRAINT fk_user"),
            "orders",
        );
        assert!(orders.foreign_keys.is_empty());
        assert_eq!(orders.index_on("user_id").as_deref(), Some("fk_user"));
        assert_eq!(orders.checks.len(), 1);
        // A unique index.
        let orders = known(
            &after("ALTER TABLE orders DROP CONSTRAINT uq_email"),
            "orders",
        );
        assert!(orders.index_on("email").is_none());
        // The primary key.
        let orders = known(
            &after("ALTER TABLE orders DROP CONSTRAINT `PRIMARY`"),
            "orders",
        );
        assert!(orders.primary_key.is_empty());
        // No constraint of that name: a plain index is none. MySQL refuses
        // the statement (the lab: 3940), and the table stays as it was.
        let untouched = after("SELECT 1");
        for name in ["nosuch", "i_note"] {
            let schema = after(&format!("ALTER TABLE orders DROP CONSTRAINT {name}"));
            assert_eq!(schema.lost().count(), 0, "{name}");
            assert_eq!(
                known(&schema, "orders"),
                known(&untouched, "orders"),
                "{name}"
            );
        }
        // A name two kinds share: MySQL refuses it.
        let schema = after(
            "ALTER TABLE orders ADD CONSTRAINT fk_user CHECK (note IS NULL);
ALTER TABLE orders DROP CONSTRAINT fk_user",
        );
        assert_eq!(
            schema.lost().collect::<Vec<_>>(),
            [(
                "orders",
                "line 2: drops the constraint fk_user, a name that several of its constraints have"
            )]
        );
    }

    #[test]
    fn adding_a_check_the_table_already_has_is_refused_and_keeps_the_table() {
        // By hand on MySQL 8.0.46 and 8.4.13: error 3822, the table stays.
        let created = "CREATE TABLE t (a INT, CONSTRAINT c CHECK (a > 0));";
        let before = from_sql(created);
        let schema = from_sql(&format!(
            "{created}\nALTER TABLE t ADD CONSTRAINT C CHECK (a > 1);"
        ));
        assert_eq!(schema.lost().count(), 0);
        assert_eq!(known(&schema, "t"), known(&before, "t"));
    }

    #[test]
    fn a_change_kilit_cannot_read_on_a_check_loses_the_table() {
        let schema = from_sql(
            "CREATE TABLE t (a INT, CONSTRAINT c CHECK (a > 0));\n\
             ALTER TABLE t ADD b INT CHECK (b > 0);",
        );
        assert_eq!(schema.lost().count(), 1);
    }

    const WITH_CHECKS: &str = "CREATE TABLE t (
           id INT PRIMARY KEY, a INT, b INT, u INT, e VARCHAR(10),
           CONSTRAINT c CHECK (a > 0), CHECK (b > 0),
           CONSTRAINT off CHECK (a < 9) NOT ENFORCED,
           UNIQUE KEY uq (e), KEY ix (b),
           CONSTRAINT fk FOREIGN KEY (u) REFERENCES p (id)
         );";

    fn checks_after(sql: &str) -> Vec<(String, bool)> {
        let schema = from_sql(&format!(
            "CREATE TABLE p (id INT PRIMARY KEY);\n{WITH_CHECKS}\n{sql};"
        ));
        known(&schema, "t")
            .checks
            .iter()
            .map(|check| (check.name.clone().unwrap_or_default(), check.enforced))
            .collect()
    }

    #[test]
    fn drop_check_and_alter_check_change_the_check_they_name() {
        let all = |changed: &[(&str, bool)]| -> Vec<(String, bool)> {
            changed
                .iter()
                .map(|(name, enforced)| ((*name).to_owned(), *enforced))
                .collect()
        };
        assert_eq!(
            checks_after("ALTER TABLE t DROP CHECK C"),
            all(&[("t_chk_1", true), ("off", false)])
        );
        assert_eq!(
            checks_after("ALTER TABLE t DROP CHECK t_chk_1"),
            all(&[("c", true), ("off", false)])
        );
        assert_eq!(
            checks_after("ALTER TABLE t ALTER CHECK off ENFORCED"),
            all(&[("c", true), ("t_chk_1", true), ("off", true)])
        );
        assert_eq!(
            checks_after(
                "ALTER TABLE t ALTER CONSTRAINT c NOT ENFORCED;\n\
                 ALTER TABLE t ALTER CHECK off ENFORCED, ALTER CHECK off NOT ENFORCED"
            ),
            all(&[("c", false), ("t_chk_1", true), ("off", false)])
        );
    }

    #[test]
    fn a_check_clause_on_another_kind_of_name_leaves_the_table_as_it_was() {
        // MySQL 8.0.46 and 8.4.13 refuse each (run by hand).
        let table_after = |sql: &str| {
            let schema = from_sql(&format!(
                "CREATE TABLE p (id INT PRIMARY KEY);\n{WITH_CHECKS}\n{sql};"
            ));
            known(&schema, "t")
        };
        let untouched = table_after("SELECT 1");
        for sql in [
            "ALTER TABLE t DROP CHECK fk",
            "ALTER TABLE t DROP CHECK uq",
            "ALTER TABLE t DROP CHECK ix",
            "ALTER TABLE t DROP CHECK `PRIMARY`",
            "ALTER TABLE t ALTER CHECK fk NOT ENFORCED",
            "ALTER TABLE t ALTER CONSTRAINT uq NOT ENFORCED",
        ] {
            assert_eq!(table_after(sql), untouched, "{sql}");
        }
    }

    #[test]
    fn a_check_clause_on_a_name_kilit_does_not_know_keeps_the_table_alone_and_loses_it_beside_others()
     {
        // MySQL refuses it (the lab: 3821, 3940): the table stays as it was.
        let setup = format!("CREATE TABLE p (id INT PRIMARY KEY);\n{WITH_CHECKS}");
        let before = from_sql(&setup);
        for sql in [
            "ALTER TABLE t DROP CHECK nothing",
            "ALTER TABLE t ALTER CHECK nothing ENFORCED",
            "ALTER TABLE t ALTER CONSTRAINT nothing NOT ENFORCED",
        ] {
            let schema = from_sql(&format!("{setup}\n{sql};"));
            assert_eq!(schema.lost().count(), 0, "{sql}");
            assert_eq!(known(&schema, "t"), known(&before, "t"), "{sql}");
        }
        for sql in [
            "ALTER TABLE t DROP CHECK c, DROP CHECK c",
            "ALTER TABLE t DROP CHECK c, ALTER CHECK c ENFORCED",
        ] {
            let schema = from_sql(&format!(
                "CREATE TABLE p (id INT PRIMARY KEY);\n{WITH_CHECKS}\n{sql};"
            ));
            assert_eq!(schema.lost().count(), 1, "{sql}");
        }
        // A CHECK constraint and a foreign key of the same name.
        let schema = from_sql(
            "CREATE TABLE p (id INT PRIMARY KEY);\n\
             CREATE TABLE t (a INT, CONSTRAINT same CHECK (a > 0), \
               CONSTRAINT same FOREIGN KEY (a) REFERENCES p (id));\n\
             ALTER TABLE t DROP CHECK same;",
        );
        assert_eq!(schema.lost().count(), 1);
    }

    #[test]
    fn a_table_is_found_only_by_its_exact_name() {
        // With lower_case_table_names = 0, the default on Linux, MySQL
        // 8.0.46 and 8.4.13 refuse ALTER TABLE ORDERS while only orders
        // exists (1146, by hand); with 1 it is orders. kilit does not know
        // the setting, so another letter case is never the table.
        let schema = Schema::from_sql(DUMP);
        assert!(matches!(schema.table("orders"), TableState::Known(_)));
        assert_eq!(schema.table("ORDERS"), TableState::Missing);
        assert_eq!(schema.table("Orders"), TableState::Missing);
        // A dump of a server with 0 may hold both.
        let schema = Schema::from_sql(
            "CREATE TABLE orders (id INT PRIMARY KEY);
             CREATE TABLE ORDERS (id INT PRIMARY KEY, note TEXT);",
        );
        assert!(known(&schema, "orders").column("note").is_none());
        assert!(known(&schema, "ORDERS").column("note").is_some());
    }

    fn found_in_another_case(sql: &str) -> Option<(String, String, Role)> {
        let schema = Schema::from_sql(DUMP);
        let statements = parse(sql);
        let [statement] = statements.as_slice() else {
            panic!("{sql}");
        };
        schema
            .in_another_case(statement)
            .map(|found| (found.written, found.known, found.role))
    }

    #[test]
    fn a_name_that_differs_only_in_letter_case_is_found_wherever_a_statement_names_a_table() {
        let found =
            |written: &str, known: &str, role| Some((written.to_owned(), known.to_owned(), role));
        for (sql, expected) in [
            (
                "ALTER TABLE ORDERS ADD COLUMN c INT NULL",
                found("ORDERS", "orders", Role::Changes),
            ),
            (
                "ALTER TABLE `Orders` ADD COLUMN c INT NULL",
                found("Orders", "orders", Role::Changes),
            ),
            (
                "CREATE INDEX i ON ORDERS (amount)",
                found("ORDERS", "orders", Role::Changes),
            ),
            (
                "DROP INDEX i ON ORDERS",
                found("ORDERS", "orders", Role::Changes),
            ),
            (
                "RENAME TABLE ORDERS TO o2",
                found("ORDERS", "orders", Role::Changes),
            ),
            (
                "RENAME TABLE orders TO ORDERS",
                found("ORDERS", "orders", Role::Names),
            ),
            (
                "ALTER TABLE orders RENAME TO ORDERS",
                found("ORDERS", "orders", Role::Names),
            ),
            (
                "RENAME TABLE a TO b, B TO c",
                found("B", "b", Role::Changes),
            ),
            (
                "DROP TABLE ORDERS",
                found("ORDERS", "orders", Role::Changes),
            ),
            (
                "DROP TABLE IF EXISTS ORDERS",
                found("ORDERS", "orders", Role::DropsIfExists),
            ),
            (
                "DROP TABLE users, ORDERS",
                found("ORDERS", "orders", Role::Changes),
            ),
            (
                "TRUNCATE TABLE ORDERS",
                found("ORDERS", "orders", Role::Empties),
            ),
            (
                "CREATE TABLE ORDERS (id INT PRIMARY KEY)",
                found("ORDERS", "orders", Role::Names),
            ),
            (
                "ALTER TABLE orders ADD CONSTRAINT k FOREIGN KEY (user_id) REFERENCES USERS (id)",
                found("USERS", "users", Role::PointsAt),
            ),
            (
                "CREATE TABLE t (id INT PRIMARY KEY, p INT, FOREIGN KEY (p) REFERENCES T (id))",
                found("T", "t", Role::PointsAt),
            ),
            (
                "ALTER TABLE ORDERS FROBNICATE",
                found("ORDERS", "orders", Role::Unread),
            ),
            // Exact names, and names kilit does not know in any case.
            ("ALTER TABLE orders ADD COLUMN c INT NULL", None),
            ("ALTER TABLE orders ADD INDEX i (AMOUNT)", None),
            (
                "ALTER TABLE orders DROP FOREIGN KEY ORDERS_USER_ID_FOREIGN",
                None,
            ),
            ("ALTER TABLE invoices ADD COLUMN c INT NULL", None),
            ("ALTER TABLE shop.ORDERS ADD COLUMN c INT NULL", None),
            ("DROP TABLE IF EXISTS invoices", None),
            ("CREATE TABLE invoices (id INT PRIMARY KEY)", None),
            ("TRUNCATE TABLE orders", None),
        ] {
            assert_eq!(found_in_another_case(sql), expected, "{sql}");
        }
    }

    #[test]
    fn letter_case_is_compared_as_mysql_may_compare_it() {
        // By hand with lower_case_table_names = 1: ÇİZELGE, ÇIZELGE and
        // Çizelge all reach çizelge.
        for name in ["ÇİZELGE", "ÇIZELGE", "Çizelge", "çızelge"] {
            assert!(same_in_any_case(name, "çizelge"), "{name}");
        }
        assert!(same_in_any_case("Shop.Orders", "shop.orders"));
        assert!(!same_in_any_case("shop.orders", "orders"));
        assert!(!same_in_any_case("orders", "order"));
    }

    #[test]
    fn a_statement_in_another_letter_case_loses_what_it_may_change() {
        let follow = |sql: &str| {
            let mut schema = Schema::from_sql(DUMP);
            for statement in parse(sql) {
                if schema.in_another_case(&statement).is_some() {
                    schema.follow_in_another_case(&statement);
                } else {
                    schema.apply(&statement);
                }
            }
            schema
        };
        let state = |schema: &Schema, name: &str| match schema.table(name) {
            TableState::Known(_) => "known",
            TableState::Lost { .. } => "lost",
            TableState::Missing => "missing",
        };
        for (sql, expected) in [
            (
                "ALTER TABLE ORDERS ADD COLUMN c INT NULL",
                [
                    ("orders", "lost"),
                    ("ORDERS", "missing"),
                    ("users", "known"),
                ],
            ),
            (
                "DROP TABLE ORDERS, users",
                [("orders", "lost"), ("ORDERS", "missing"), ("users", "lost")],
            ),
            (
                "RENAME TABLE orders TO ORDERS",
                [("orders", "lost"), ("ORDERS", "lost"), ("users", "known")],
            ),
            (
                "CREATE TABLE ORDERS (id INT PRIMARY KEY)",
                [("orders", "known"), ("ORDERS", "lost"), ("users", "known")],
            ),
            (
                "TRUNCATE TABLE ORDERS",
                [
                    ("orders", "known"),
                    ("ORDERS", "missing"),
                    ("users", "known"),
                ],
            ),
            (
                "ALTER TABLE orders ADD CONSTRAINT k FOREIGN KEY (user_id) REFERENCES USERS (id)",
                [("orders", "lost"), ("USERS", "missing"), ("users", "known")],
            ),
            (
                "ALTER TABLE ORDERS FROBNICATE",
                [
                    ("orders", "lost"),
                    ("ORDERS", "missing"),
                    ("users", "known"),
                ],
            ),
            (
                "CREATE TABLE ORDERS LIKE users",
                [("orders", "known"), ("ORDERS", "lost"), ("users", "known")],
            ),
        ] {
            let schema = follow(sql);
            for (name, expected) in expected {
                assert_eq!(state(&schema, name), expected, "{sql}: {name}");
            }
        }
        // A foreign key in another case may point at users.
        assert!(
            follow(
                "ALTER TABLE orders ADD CONSTRAINT k FOREIGN KEY (user_id) REFERENCES USERS (id)"
            )
            .may_be_pointed_at("users")
        );
        // A temporary table hides the table it may be until the session
        // ends, and changes no other table: the table is seen again.
        let mut schema = follow(
            "CREATE TEMPORARY TABLE ORDERS (id INT);
             ALTER TABLE orders ADD COLUMN c INT NULL;",
        );
        assert_eq!(state(&schema, "orders"), "lost");
        assert_eq!(state(&schema, "ORDERS"), "lost");
        schema.session_ends();
        assert_eq!(state(&schema, "ORDERS"), "missing");
        assert!(known(&schema, "orders").column("c").is_none());
    }

    #[test]
    fn a_name_lost_in_another_letter_case_stays_unclear_while_its_other_case_is_known() {
        // CREATE TABLE IF NOT EXISTS ORDERS with orders there: with 0 a new
        // table, with 1 or 2 nothing happens and ORDERS is orders.
        let mut schema = Schema::from_sql(DUMP);
        let statements = parse(
            "CREATE TABLE IF NOT EXISTS ORDERS (id INT PRIMARY KEY);
             ALTER TABLE ORDERS ADD COLUMN c INT NULL;
             ALTER TABLE orders ADD COLUMN c INT NULL;
             ALTER TABLE ORDERS ADD COLUMN d INT NULL;",
        );
        let mut found = Vec::new();
        for statement in &statements {
            let named = schema.in_another_case(statement);
            found.push(named.as_ref().map(|named| named.known.clone()));
            if named.is_some() {
                schema.follow_in_another_case(statement);
            } else {
                schema.apply(statement);
            }
        }
        // orders is known exactly until a statement in another letter case
        // may change it; from then on both names are unclear.
        assert_eq!(
            found,
            [
                Some("orders".to_owned()),
                Some("orders".to_owned()),
                Some("ORDERS".to_owned()),
                Some("orders".to_owned()),
            ]
        );
        // A table lost alone keeps the lost answer for its exact name.
        let mut schema = Schema::from_sql(DUMP);
        let statements = parse(
            "ALTER TABLE ORDERS ADD COLUMN c INT NULL;
             ALTER TABLE orders ADD COLUMN d INT NULL;",
        );
        assert!(schema.in_another_case(&statements[0]).is_some());
        schema.follow_in_another_case(&statements[0]);
        assert!(schema.in_another_case(&statements[1]).is_none());
    }

    fn engine_of_lost(schema: &Schema, name: &str) -> Option<Engine> {
        match schema.table(name) {
            TableState::Lost { engine, .. } => engine.cloned(),
            other => panic!("{name}: expected lost, got {other:?}"),
        }
    }

    #[test]
    fn a_myisam_table_keeps_the_index_of_a_foreign_key_but_not_the_key() {
        let schema = from_sql(
            "CREATE TABLE users (id BIGINT UNSIGNED PRIMARY KEY);
             CREATE TABLE a (id INT, user_id BIGINT UNSIGNED,
                 CONSTRAINT fk_a FOREIGN KEY (user_id) REFERENCES users (id)) ENGINE=MyISAM;
             CREATE TABLE b (id INT, user_id BIGINT UNSIGNED,
                 FOREIGN KEY (user_id) REFERENCES users (id)) ENGINE=MyISAM;
             CREATE TABLE c (id INT, user_id BIGINT UNSIGNED REFERENCES users (id)) ENGINE=MyISAM;
             CREATE TABLE d (id INT, user_id BIGINT UNSIGNED) ENGINE=MyISAM;
             ALTER TABLE d ADD CONSTRAINT fk_d FOREIGN KEY (user_id) REFERENCES users (id);
             ALTER TABLE d ADD CONSTRAINT fk_d FOREIGN KEY (user_id) REFERENCES users (id);
             ALTER TABLE d DROP FOREIGN KEY never_added;",
        );
        for (name, index) in [
            ("a", Some("fk_a")),
            ("b", Some("user_id")),
            ("c", None),
            ("d", Some("fk_d")),
        ] {
            let table = known(&schema, name);
            assert!(table.foreign_keys.is_empty(), "{name}");
            assert_eq!(
                index_names(&table),
                index.into_iter().map(str::to_owned).collect::<Vec<_>>(),
                "{name}"
            );
        }
        assert!(!schema.may_be_pointed_at("users"));
    }

    #[test]
    fn a_foreign_key_kilit_has_not_seen_on_myisam_loses_the_table() {
        for change in [
            "ADD CONSTRAINT fk FOREIGN KEY (user_id, amount) REFERENCES users (id, n)",
            "ADD INDEX i (amount, user_id), ADD CONSTRAINT fk FOREIGN KEY (user_id) REFERENCES users (id)",
            "ADD INDEX fk (amount), ADD CONSTRAINT fk FOREIGN KEY (user_id) REFERENCES users (id)",
        ] {
            let schema = from_sql(&format!(
                "CREATE TABLE t (user_id BIGINT UNSIGNED, amount INT) ENGINE=MyISAM;
                 ALTER TABLE t {change};"
            ));
            assert_eq!(
                engine_of_lost(&schema, "t"),
                Some(Engine::MyIsam),
                "{change}"
            );
        }
    }

    #[test]
    fn a_lost_table_keeps_the_engine_kilit_last_knew() {
        let schema = from_sql(
            "CREATE TABLE m (id INT) ENGINE=MyISAM;
             ALTER TABLE m ENGINE=InnoDB, AUTO_INCREMENT=5;
             CREATE TABLE i (id INT) ENGINE=InnoDB;
             ALTER TABLE i ENGINE = 'MyISAM';
             CREATE TABLE q (id INT) ENGINE='MyISAM';
             CREATE TABLE k (id INT) ENGINE=MyISAM;
             ALTER TABLE k ADD COLUMN engine INT, ALGORITHM=SOMETHING;
             CREATE TABLE x (id INT) ENGINE=MyISAM;
             ALTER TABLE x COMMENT = 'nothing', ALGORITHM=SOMETHING;
             ALTER TABLE missing ENGINE=MyISAM, AUTO_INCREMENT=5;",
        );
        assert_eq!(engine_of_lost(&schema, "m"), Some(Engine::InnoDb));
        assert_eq!(engine_of_lost(&schema, "i"), Some(Engine::MyIsam));
        assert_eq!(engine_of_lost(&schema, "q"), Some(Engine::MyIsam));
        // A column named engine counts too: the safe side.
        assert_eq!(
            engine_of_lost(&schema, "k"),
            Some(Engine::Other("INT".into()))
        );
        assert_eq!(engine_of_lost(&schema, "x"), Some(Engine::MyIsam));
        assert_eq!(engine_of_lost(&schema, "missing"), Some(Engine::MyIsam));
    }

    #[test]
    fn table_options_change_a_known_table_and_keep_the_others() {
        let schema = from_sql(
            "CREATE TABLE t (id INT) ROW_FORMAT=COMPRESSED KEY_BLOCK_SIZE=8;
             ALTER TABLE t ROW_FORMAT=DEFAULT;
             CREATE TABLE m (id INT);
             ALTER TABLE m COMMENT = 'ENGINE=MEMORY', ENGINE=MyISAM;
             CREATE TABLE c (id INT);
             ALTER TABLE c COMMENT = 'x';",
        );
        let t = known(&schema, "t");
        assert_eq!(t.row_format, Some(RowFormat::Default));
        assert_eq!(t.key_block_size.as_deref(), Some("8"));
        assert_eq!(known(&schema, "m").engine, Some(Engine::MyIsam));
        assert_eq!(
            known(&schema, "c"),
            known(&from_sql("CREATE TABLE c (id INT);"), "c")
        );
    }

    #[test]
    fn another_engine_on_a_table_with_foreign_keys_loses_it() {
        let schema = from_sql(
            "CREATE TABLE users (id INT PRIMARY KEY);
             CREATE TABLE orders (id INT, user_id INT, FOREIGN KEY (user_id) REFERENCES users (id));
             CREATE TABLE kept (id INT, user_id INT, FOREIGN KEY (user_id) REFERENCES users (id));
             ALTER TABLE orders ENGINE=MyISAM;
             ALTER TABLE users ENGINE=MEMORY;
             ALTER TABLE kept ENGINE=InnoDB;",
        );
        assert_eq!(engine_of_lost(&schema, "orders"), Some(Engine::Unknown));
        assert_eq!(engine_of_lost(&schema, "users"), Some(Engine::Unknown));
        assert_eq!(known(&schema, "kept").engine, Some(Engine::InnoDb));
    }

    #[test]
    fn table_options_on_a_table_kilit_does_not_know_lose_it_with_what_they_write() {
        let schema = from_sql(
            "ALTER TABLE e ENGINE=MyISAM;
             ALTER TABLE r ROW_FORMAT=COMPRESSED;
             ALTER TABLE c COMMENT = 'x';",
        );
        assert_eq!(engine_of_lost(&schema, "e"), Some(Engine::MyIsam));
        assert!(row_format_unclear_of_lost(&schema, "r"));
        assert_eq!(schema.table("c"), TableState::Missing);
    }

    #[test]
    fn a_temporary_table_hides_its_name_until_the_session_ends() {
        let mut schema = from_sql(
            "CREATE TABLE orders (id INT, note TEXT);
             CREATE TEMPORARY TABLE orders (id INT);
             CREATE TEMPORARY TABLE scratch (id INT);
             CREATE TEMPORARY TABLE copy LIKE orders;",
        );
        assert_eq!(known(&schema, "orders").columns.len(), 1);
        schema.session_ends();
        assert_eq!(known(&schema, "orders").columns.len(), 2);
        assert_eq!(schema.table("scratch"), TableState::Missing);
        assert_eq!(schema.table("copy"), TableState::Missing);
        // A temporary table dropped in its session hides nothing after it.
        let mut schema = from_sql(
            "CREATE TABLE orders (id INT, note TEXT);
             CREATE TEMPORARY TABLE orders (id INT);
             DROP TEMPORARY TABLE orders;",
        );
        schema.session_ends();
        assert_eq!(known(&schema, "orders").columns.len(), 2);
    }

    #[test]
    fn a_table_created_like_another_takes_its_engine() {
        let schema = from_sql(
            "CREATE TABLE m (id INT) ENGINE=MyISAM;
             CREATE TABLE i (id INT);
             CREATE TABLE a LIKE m;
             CREATE TABLE b LIKE `m`;
             CREATE TABLE c LIKE i;",
        );
        assert_eq!(known(&schema, "a").engine, Some(Engine::MyIsam));
        assert_eq!(known(&schema, "b").engine, Some(Engine::MyIsam));
        assert_eq!(known(&schema, "c").engine, None);
        // Of a table kilit does not know, the copy is lost.
        let schema = from_sql("CREATE TABLE d LIKE nosuch;");
        assert!(schema.lost().any(|(name, _)| name == "d"));
    }

    #[test]
    fn a_table_created_like_another_has_what_mysql_copies() {
        // By hand on MySQL 8.0.46 and 8.4.13: the columns, keys (a foreign
        // key's index as a plain one), engine, row format and character set
        // are copied; the foreign keys are not; CHECK constraints are named
        // b_chk_1, b_chk_2, ... in the order of their names in the source.
        let schema = from_sql(
            "CREATE TABLE users (id BIGINT UNSIGNED PRIMARY KEY);
             CREATE TABLE a (id BIGINT UNSIGNED AUTO_INCREMENT PRIMARY KEY,
               uid BIGINT UNSIGNED NOT NULL, amount INT NOT NULL,
               KEY ix_amount (amount),
               CONSTRAINT a_uid_foreign FOREIGN KEY (uid) REFERENCES users (id),
               CHECK (amount < 1000000), CONSTRAINT a_pos CHECK (amount >= 0))
               ROW_FORMAT=COMPRESSED KEY_BLOCK_SIZE=8 DEFAULT CHARSET=latin1;
             CREATE TABLE b LIKE a;",
        );
        let a = known(&schema, "a");
        let b = known(&schema, "b");
        assert_eq!(b.columns, a.columns);
        assert_eq!(b.primary_key, a.primary_key);
        assert_eq!(b.indexes, a.indexes);
        assert!(
            b.indexes
                .iter()
                .any(|index| index.name.as_deref() == Some("a_uid_foreign"))
        );
        assert!(b.foreign_keys.is_empty());
        assert_eq!(
            (b.row_format.clone(), b.key_block_size.clone()),
            (a.row_format.clone(), a.key_block_size.clone())
        );
        assert_eq!(b.charset, a.charset);
        let names: Vec<_> = b
            .checks
            .iter()
            .map(|check| check.name.clone().unwrap())
            .collect();
        assert_eq!(names, ["b_chk_1", "b_chk_2"]);
        assert_eq!(b.checks[0].expression, a.checks[0].expression);
    }

    #[test]
    fn a_temporary_copy_mysql_refuses_creates_nothing() {
        // By hand on MySQL 8.0.46 and 8.4.13: 3500 for a COMPRESSED source,
        // 1796 for an InnoDB source with a FULLTEXT index.
        let schema = from_sql(
            "CREATE TABLE c (id INT PRIMARY KEY) ROW_FORMAT=COMPRESSED;
             CREATE TABLE f (id INT PRIMARY KEY, body TEXT, FULLTEXT KEY ft (body));
             CREATE TEMPORARY TABLE tc LIKE c;
             CREATE TEMPORARY TABLE tf LIKE f;",
        );
        assert_eq!(schema.table("tc"), TableState::Missing);
        assert_eq!(schema.table("tf"), TableState::Missing);
    }

    #[test]
    fn written_engines_are_read_like_mysql_reads_them() {
        for (sql, engine) in [
            ("ALTER TABLE t ENGINE=MyISAM", Some(Engine::MyIsam)),
            (
                "ALTER TABLE t ENGINE = \"MEMORY\"",
                Some(Engine::Other("MEMORY".into())),
            ),
            ("ALTER TABLE t engine `innodb`", Some(Engine::InnoDb)),
            (
                "ALTER TABLE t ENGINE=MyISAM, ENGINE=InnoDB",
                Some(Engine::InnoDb),
            ),
            ("ALTER TABLE t ENGINE='MyISAM", Some(Engine::Unknown)),
            ("ALTER TABLE t ENGINE=", Some(Engine::Unknown)),
            ("SET default_storage_engine = MyISAM", None),
            ("ALTER TABLE engines ADD COLUMN my_engine INT", None),
        ] {
            assert_eq!(written_engine(sql), engine, "{sql}");
        }
    }

    #[test]
    fn after_default_storage_engine_changes_a_new_table_without_engine_has_an_unknown_one() {
        let mut schema = Schema::new();
        schema.default_engine_changes();
        for statement in parse(
            "CREATE TABLE a (id INT);
             CREATE TABLE b (id INT) ENGINE=InnoDB;
             CREATE TABLE c (id INT) COMMENT 'x', PARTITION;",
        ) {
            schema.apply(&statement);
        }
        assert_eq!(known(&schema, "a").engine, Some(Engine::Unknown));
        assert_eq!(known(&schema, "b").engine, Some(Engine::InnoDb));
        assert_eq!(engine_of_lost(&schema, "c"), Some(Engine::Unknown));
        // A schema dump is read without it.
        let dump = Schema::from_sql("CREATE TABLE a (id INT);");
        assert_eq!(known(&dump, "a").engine, None);
    }

    fn row_format_unclear_of_lost(schema: &Schema, name: &str) -> bool {
        match schema.table(name) {
            TableState::Lost {
                row_format_unclear, ..
            } => row_format_unclear,
            other => panic!("{name}: expected lost, got {other:?}"),
        }
    }

    #[test]
    fn written_row_formats_are_seen_outside_quotes() {
        for (sql, written) in [
            ("ALTER TABLE t ROW_FORMAT=COMPRESSED", true),
            ("ALTER TABLE t row_format = dynamic", true),
            ("ALTER TABLE t KEY_BLOCK_SIZE 8", true),
            ("ALTER TABLE t ADD COLUMN c INT, ROW_FORMAT=COMPACT", true),
            ("ALTER TABLE t COMMENT 'ROW_FORMAT=COMPACT'", false),
            ("ALTER TABLE t ADD COLUMN row_format_x INT", false),
            ("ALTER TABLE t ENGINE=InnoDB", false),
        ] {
            assert_eq!(writes_row_format(sql), written, "{sql}");
        }
    }

    #[test]
    fn a_lost_table_remembers_whether_it_may_have_another_row_format() {
        let schema = from_sql(
            "CREATE TABLE plain (id INT);
             CREATE TABLE dynamic (id INT) ROW_FORMAT=DYNAMIC;
             CREATE TABLE compact (id INT) ROW_FORMAT=COMPACT;
             CREATE TABLE sized (id INT) KEY_BLOCK_SIZE=8;
             CREATE TABLE unsized (id INT) KEY_BLOCK_SIZE=0;
             CREATE TABLE rewritten (id INT);
             ALTER TABLE plain ENGINE=InnoDB, AUTO_INCREMENT=5;
             ALTER TABLE dynamic ENGINE=InnoDB, AUTO_INCREMENT=5;
             ALTER TABLE compact ENGINE=InnoDB, AUTO_INCREMENT=5;
             ALTER TABLE sized ENGINE=InnoDB, AUTO_INCREMENT=5;
             ALTER TABLE unsized ENGINE=InnoDB, AUTO_INCREMENT=5;
             ALTER TABLE rewritten ROW_FORMAT=DYNAMIC, AUTO_INCREMENT=5;
             CREATE TABLE copy LIKE compact;
             CREATE TABLE other_copy LIKE plain;",
        );
        for (name, unclear) in [
            ("plain", false),
            ("dynamic", false),
            ("compact", true),
            ("sized", true),
            ("unsized", false),
            // Any value written by a statement kilit could not read.
            ("rewritten", true),
            ("copy", true),
            ("other_copy", false),
        ] {
            assert_eq!(row_format_unclear_of_lost(&schema, name), unclear, "{name}");
        }
    }

    #[test]
    fn after_the_default_row_format_changes_a_new_table_without_one_has_an_unknown_one() {
        let mut schema = Schema::new();
        schema.default_row_format_changes();
        for statement in parse(
            "CREATE TABLE a (id INT);
             CREATE TABLE b (id INT) ROW_FORMAT=COMPACT;
             CREATE TABLE c (id INT) KEY_BLOCK_SIZE=8;
             CREATE TABLE d (id INT) COMMENT 'x', PARTITION;",
        ) {
            schema.apply(&statement);
        }
        assert_eq!(known(&schema, "a").row_format, Some(RowFormat::Unknown));
        assert_eq!(known(&schema, "b").row_format, Some(RowFormat::Compact));
        assert_eq!(known(&schema, "c").row_format, Some(RowFormat::Unknown));
        assert!(row_format_unclear_of_lost(&schema, "d"));
        // A schema dump is read without it.
        let dump = Schema::from_sql("CREATE TABLE a (id INT);");
        assert_eq!(known(&dump, "a").row_format, None);
    }

    // kilit's picture of the tables follows what MySQL does with
    // valid input (by hand on MySQL 8.0.46 and 8.4.13, the same on both).

    fn dump_after(sql: &str) -> Schema {
        let mut schema = Schema::from_dump(DUMP);
        for statement in parse(sql) {
            schema.apply(&statement);
        }
        schema
    }

    #[test]
    fn a_rename_of_a_parent_renames_the_parent_of_its_keys() {
        let schema = after("RENAME TABLE users TO people");
        let orders = known(&schema, "orders");
        assert_eq!(orders.foreign_keys[0].referenced_table, "people");
        let schema = after("ALTER TABLE users RENAME TO people");
        assert_eq!(
            known(&schema, "orders").foreign_keys[0].referenced_table,
            "people"
        );
        // So do the keys of a table kilit lost.
        let schema = after(
            "ALTER TABLE orders CONVERT TO CHARACTER SET latin1;
             RENAME TABLE users TO people",
        );
        assert!(schema.unseen_keys_point_at.contains("people"));
    }

    #[test]
    fn a_create_table_of_a_dump_table_is_not_followed() {
        let before = Schema::from_dump(DUMP);
        let schema = dump_after("CREATE TABLE orders (id INT PRIMARY KEY)");
        assert_eq!(known(&schema, "orders"), known(&before, "orders"));
        assert!(schema.created("orders").is_none());
        // After a DROP, it is a new table.
        let schema = dump_after("DROP TABLE orders; CREATE TABLE orders (id INT PRIMARY KEY)");
        assert_eq!(column_names(&known(&schema, "orders")), ["id"]);
        assert!(schema.created("orders").is_some());
        // IF NOT EXISTS keeps the dump's table, as before.
        let schema = dump_after("CREATE TABLE IF NOT EXISTS orders (id INT PRIMARY KEY)");
        assert_eq!(known(&schema, "orders"), known(&before, "orders"));
    }

    #[test]
    fn a_table_moved_onto_a_name_from_before_the_input_is_not_new() {
        // The dump has orders.
        let schema = dump_after(
            "CREATE TABLE orders_new (id INT PRIMARY KEY);
             RENAME TABLE orders TO orders_old, orders_new TO orders",
        );
        assert!(schema.created("orders").is_none());
        assert_eq!(column_names(&known(&schema, "orders")), ["id"]);
        // Without a dump: the input used the name first.
        let schema = from_sql_input(
            "ALTER TABLE t ADD COLUMN c INT;
             CREATE TABLE t_new (id INT PRIMARY KEY);
             RENAME TABLE t TO t_old, t_new TO t",
        );
        assert!(schema.created("t").is_none());
        // Read it.
        let schema = from_sql_input(
            "CREATE TABLE t_new (id INT PRIMARY KEY);
             INSERT INTO t_new SELECT * FROM `t`;
             RENAME TABLE t_new TO t",
        );
        assert!(schema.created("t").is_none());
        // A fresh name keeps it new.
        let schema = from_sql_input(
            "CREATE TABLE t_new (id INT PRIMARY KEY);
             RENAME TABLE t_new TO t;
             ALTER TABLE t RENAME TO u",
        );
        assert!(schema.created("u").is_some());
    }

    /// The schema after `sql`, with no tables before it.
    fn from_sql_input(sql: &str) -> Schema {
        let mut schema = Schema::new();
        for statement in parse(sql) {
            schema.apply(&statement);
        }
        schema
    }

    #[test]
    fn drop_temporary_table_leaves_the_base_table() {
        let before = Schema::from_dump(DUMP);
        for sql in [
            "DROP TEMPORARY TABLE IF EXISTS orders",
            "DROP TEMPORARY TABLE orders",
            "DROP /* x */ TEMPORARY TABLE orders",
            "DROP TEMPORARY TABLE orders, users",
        ] {
            let schema = dump_after(sql);
            assert_eq!(known(&schema, "orders"), known(&before, "orders"), "{sql}");
            assert!(schema.gone("orders").is_none(), "{sql}");
        }
    }

    #[test]
    fn dropping_a_temporary_table_shows_the_base_table_again() {
        let before = Schema::from_dump(DUMP);
        for drop in ["DROP TEMPORARY TABLE orders", "DROP TABLE orders"] {
            let schema = dump_after(&format!(
                "CREATE TEMPORARY TABLE orders (id INT PRIMARY KEY); {drop}"
            ));
            assert_eq!(known(&schema, "orders"), known(&before, "orders"), "{drop}");
        }
        // IF EXISTS drops the temporary ones it names.
        let schema = dump_after(
            "CREATE TEMPORARY TABLE orders (id INT PRIMARY KEY);
             DROP TEMPORARY TABLE IF EXISTS orders, nosuch",
        );
        assert_eq!(known(&schema, "orders"), known(&before, "orders"));
    }

    #[test]
    fn a_temporary_table_ends_with_its_session() {
        let mut schema = Schema::from_dump(DUMP);
        for statement in parse(
            "CREATE TEMPORARY TABLE orders (id INT PRIMARY KEY);
             CREATE TEMPORARY TABLE scratch (id INT PRIMARY KEY);",
        ) {
            schema.apply(&statement);
        }
        assert!(
            schema
                .created("orders")
                .is_some_and(|created| created.temporary)
        );
        schema.session_ends();
        assert!(schema.created("orders").is_none());
        assert!(schema.created("scratch").is_none());
        assert_eq!(
            known(&schema, "orders"),
            known(&Schema::from_dump(DUMP), "orders")
        );
    }

    #[test]
    fn rename_table_does_not_see_a_temporary_table_and_kilit_loses_the_names() {
        let schema = dump_after(
            "CREATE TEMPORARY TABLE orders (id INT PRIMARY KEY);
             RENAME TABLE orders TO orders_old",
        );
        assert!(matches!(schema.table("orders"), TableState::Lost { .. }));
        assert!(matches!(
            schema.table("orders_old"),
            TableState::Lost { .. }
        ));
        // ALTER TABLE ... RENAME TO moves the temporary table: the base table
        // is back.
        let schema = dump_after(
            "CREATE TEMPORARY TABLE orders (id INT PRIMARY KEY);
             ALTER TABLE orders RENAME TO orders_tmp",
        );
        assert_eq!(
            known(&schema, "orders"),
            known(&Schema::from_dump(DUMP), "orders")
        );
        assert!(matches!(
            schema.table("orders_tmp"),
            TableState::Lost { .. }
        ));
    }

    #[test]
    fn a_rename_table_onto_a_taken_name_changes_nothing() {
        let before = Schema::from_dump(DUMP);
        let schema = dump_after("RENAME TABLE orders TO users");
        assert_eq!(known(&schema, "orders"), known(&before, "orders"));
        assert_eq!(known(&schema, "users"), known(&before, "users"));
        // The names before it in the statement count.
        let schema = dump_after("RENAME TABLE users TO people, orders TO users");
        assert_eq!(known(&schema, "users"), {
            let mut orders = known(&before, "orders");
            orders.name = "users".to_owned();
            orders.foreign_keys[0].referenced_table = "people".to_owned();
            orders
        });
    }

    fn resolved(table_sql: &str, sql: &str) -> Resolution {
        let schema = Schema::from_sql(table_sql);
        let statement = parse(sql).remove(0);
        let StatementKind::Alter(alter) = &statement.kind else {
            panic!("{sql}");
        };
        let TableState::Known(table) = schema.table(&alter.table) else {
            panic!("{sql}");
        };
        schema.resolve(table, alter)
    }

    const ORDERS: &str = "CREATE TABLE users (id BIGINT UNSIGNED PRIMARY KEY);
        CREATE TABLE orders (id BIGINT UNSIGNED PRIMARY KEY, user_id BIGINT UNSIGNED NOT NULL,
        amount INT NOT NULL, note VARCHAR(50) NULL, email VARCHAR(255) NULL,
        UNIQUE KEY orders_email_unique (email), KEY ix_amount (amount),
        CONSTRAINT orders_user_id_foreign FOREIGN KEY (user_id) REFERENCES users (id),
        CONSTRAINT orders_amount_check CHECK (amount > 0));";

    fn refused_with(resolution: Resolution) -> Option<u16> {
        match resolution {
            Resolution::Refused { code, .. } => Some(code),
            _ => None,
        }
    }

    #[test]
    fn names_the_table_already_has_or_lacks_are_refused_or_lose_the_table() {
        for (alter, code) in [
            ("ALTER TABLE orders ADD COLUMN note INT NULL", 1060),
            (
                "ALTER TABLE orders ADD CONSTRAINT orders_user_id_foreign FOREIGN KEY (user_id) \
                 REFERENCES users (id)",
                1826,
            ),
            (
                "ALTER TABLE orders ADD CONSTRAINT ix_amount FOREIGN KEY (user_id) REFERENCES \
                 users (id)",
                1061,
            ),
            ("ALTER TABLE orders ADD PRIMARY KEY (amount)", 1068),
            (
                "ALTER TABLE orders MODIFY amount INT NOT NULL PRIMARY KEY",
                1068,
            ),
            (
                "ALTER TABLE orders ADD CONSTRAINT orders_amount_check CHECK (amount > 1)",
                3822,
            ),
            ("ALTER TABLE orders RENAME TO users", 1050),
            (
                "ALTER TABLE orders DROP FOREIGN KEY orders_user_id_foreign, ADD CONSTRAINT \
                 orders_user_id_foreign FOREIGN KEY (user_id) REFERENCES users (id)",
                1826,
            ),
            (
                "ALTER TABLE orders DROP COLUMN user_id, ADD COLUMN c INT",
                1828,
            ),
            (
                "ALTER TABLE orders MODIFY note VARCHAR(100) NULL, RENAME COLUMN note TO memo",
                1054,
            ),
            (
                "ALTER TABLE orders RENAME COLUMN note TO memo, MODIFY memo VARCHAR(100) NULL",
                1054,
            ),
        ] {
            assert_eq!(refused_with(resolved(ORDERS, alter)), Some(code), "{alter}");
        }
        for alter in [
            "ALTER TABLE orders MODIFY zzz INT",
            "ALTER TABLE orders CHANGE zzz yyy INT",
            "ALTER TABLE orders RENAME COLUMN zzz TO yyy",
            "ALTER TABLE orders ALTER COLUMN zzz SET DEFAULT 1",
            "ALTER TABLE orders CHANGE note email VARCHAR(50) NULL",
            "ALTER TABLE orders RENAME COLUMN note TO email",
            "ALTER TABLE orders ADD COLUMN c INT AFTER zzz",
            // A chain into a name the table does not have yet: 1054 by hand.
            "ALTER TABLE orders RENAME COLUMN amount TO total, RENAME COLUMN total TO sum",
            // An old name in a new index: 1072 by hand.
            "ALTER TABLE orders CHANGE amount c INT NOT NULL, ADD INDEX (amount)",
            // A column dropped and added that the table does not have: 1091.
            "ALTER TABLE orders ADD COLUMN extra INT, DROP COLUMN extra",
            // A shape no hand test covers.
            "ALTER TABLE orders DROP COLUMN note, RENAME COLUMN email TO note",
        ] {
            assert!(
                matches!(resolved(ORDERS, alter), Resolution::Lost(_)),
                "{alter}"
            );
        }
        // A foreign key dropped with its column runs.
        assert!(matches!(
            resolved(
                ORDERS,
                "ALTER TABLE orders DROP FOREIGN KEY orders_user_id_foreign, DROP COLUMN user_id"
            ),
            Resolution::Runs(_)
        ));
    }

    #[test]
    fn two_clauses_of_one_alter_are_taken_against_the_names_before_it() {
        // A swap.
        let schema = Schema::from_sql(&format!(
            "{ORDERS}
             ALTER TABLE orders CHANGE note email VARCHAR(50) NULL,
               CHANGE email note VARCHAR(255) NULL;"
        ));
        let orders = known(&schema, "orders");
        let note = orders.column("note").unwrap();
        assert_eq!(note.data_type.to_string(), "VARCHAR(255)");
        assert_eq!(
            orders.index_on("note").as_deref(),
            Some("orders_email_unique")
        );
        // A swap of a column a CHECK constraint uses: 3959 by hand.
        assert_eq!(
            refused_with(resolved(
                ORDERS,
                "ALTER TABLE orders CHANGE amount note INT NOT NULL, CHANGE note amount \
                 VARCHAR(50) NULL"
            )),
            Some(3959)
        );
        // Each name moved to the one another gives up.
        let schema = Schema::from_sql(&format!(
            "{ORDERS}
             ALTER TABLE orders RENAME COLUMN note TO memo, RENAME COLUMN email TO note;"
        ));
        let orders = known(&schema, "orders");
        assert_eq!(
            orders.index_on("note").as_deref(),
            Some("orders_email_unique")
        );
        assert!(orders.column("memo").is_some());
        // DROP and ADD of one name, in either order.
        for alter in [
            "ALTER TABLE orders DROP COLUMN note, ADD COLUMN note INT NULL",
            "ALTER TABLE orders ADD COLUMN note INT NULL, DROP COLUMN note",
        ] {
            let schema = Schema::from_sql(&format!("{ORDERS}\n{alter};"));
            let note = known(&schema, "orders").column("note").unwrap().clone();
            assert_eq!(note.data_type.to_string(), "INT", "{alter}");
        }
        // A new column in a new index.
        let schema = Schema::from_sql(&format!(
            "{ORDERS}
             ALTER TABLE orders ADD INDEX ix_c (c), ADD COLUMN c INT NULL;"
        ));
        assert_eq!(
            known(&schema, "orders").index_on("c").as_deref(),
            Some("ix_c")
        );
    }

    #[test]
    fn an_inline_primary_key_makes_the_column_the_primary_key() {
        let schema = Schema::from_sql(
            "CREATE TABLE np (c INT NULL, d INT NULL);
             ALTER TABLE np MODIFY c INT PRIMARY KEY;",
        );
        let np = known(&schema, "np");
        assert_eq!(np.primary_key, ["c"]);
        assert_eq!(np.column("c").unwrap().nullable, Some(false));
        assert!(np.column("c").unwrap().other_options.is_empty());
        // Written NULL: 1171 by hand.
        assert_eq!(
            refused_with(resolved(
                "CREATE TABLE np (c INT NULL);",
                "ALTER TABLE np MODIFY c INT NULL PRIMARY KEY"
            )),
            Some(1171)
        );
    }

    #[test]
    fn serial_is_followed_as_mysql_makes_it() {
        let schema = Schema::from_sql(
            "CREATE TABLE pl (id BIGINT UNSIGNED PRIMARY KEY, uid BIGINT UNSIGNED NULL);
             ALTER TABLE pl MODIFY uid SERIAL;",
        );
        let pl = known(&schema, "pl");
        let uid = pl.column("uid").unwrap();
        assert_eq!(uid.data_type.to_string(), "BIGINT UNSIGNED");
        assert_eq!(uid.nullable, Some(false));
        assert!(uid.auto_increment);
        assert_eq!(pl.index_on("uid").as_deref(), Some("uid"));
        for (table, code) in [
            (
                "CREATE TABLE users (id BIGINT UNSIGNED PRIMARY KEY);
                 CREATE TABLE t (id BIGINT UNSIGNED PRIMARY KEY, uid BIGINT UNSIGNED NULL,
                 CONSTRAINT f FOREIGN KEY (uid) REFERENCES users (id) ON DELETE SET NULL);",
                1830,
            ),
            (
                "CREATE TABLE users (id BIGINT UNSIGNED PRIMARY KEY);
                 CREATE TABLE t (id BIGINT UNSIGNED PRIMARY KEY, uid BIGINT UNSIGNED NULL,
                 CONSTRAINT f FOREIGN KEY (uid) REFERENCES users (id) ON DELETE CASCADE);",
                1832,
            ),
            (
                "CREATE TABLE t (id BIGINT UNSIGNED AUTO_INCREMENT PRIMARY KEY,
                 uid BIGINT UNSIGNED NULL);",
                1075,
            ),
        ] {
            assert_eq!(
                refused_with(resolved(table, "ALTER TABLE t MODIFY uid SERIAL")),
                Some(code),
                "{table}"
            );
        }
    }

    #[test]
    fn a_column_a_check_or_a_generated_column_uses_beside_other_clauses_is_not_followed() {
        // By hand on MySQL 8.0.46 and 8.4.13: 3959 for a rename of a column a
        // CHECK constraint uses, and for a drop of one it uses with another
        // column; 3108 for a generated column's. The table stays as it was.
        let table = "CREATE TABLE orders (id INT PRIMARY KEY, amount INT NOT NULL,
            note VARCHAR(10) NULL, extra INT NULL, g INT AS (extra + 1),
            CONSTRAINT ck2 CHECK (amount > 0 OR note IS NULL));";
        for (alter, code) in [
            (
                "ALTER TABLE orders DROP COLUMN note, ADD COLUMN y INT NULL",
                3959,
            ),
            (
                "ALTER TABLE orders RENAME COLUMN amount TO total, ADD COLUMN y INT NULL",
                3959,
            ),
            (
                "ALTER TABLE orders CHANGE amount total INT NOT NULL, ADD COLUMN y INT NULL",
                3959,
            ),
        ] {
            assert_eq!(refused_with(resolved(table, alter)), Some(code), "{alter}");
        }
        for alter in [
            "ALTER TABLE orders DROP COLUMN extra, ADD COLUMN y INT NULL",
            "ALTER TABLE orders RENAME COLUMN extra TO e2, ADD COLUMN y INT NULL",
            // DROP x and ADD x of a column a CHECK constraint uses: 3959 by hand.
            "ALTER TABLE orders DROP COLUMN amount, ADD COLUMN amount INT NULL",
        ] {
            assert!(
                matches!(resolved(table, alter), Resolution::Lost(_)),
                "{alter}"
            );
        }
    }

    #[test]
    fn a_check_the_statement_drops_or_one_on_the_dropped_column_alone_does_not_refuse_it() {
        // By hand on MySQL 8.0.46 and 8.4.13: accepted, and the CHECK goes.
        let table = "CREATE TABLE orders (id INT PRIMARY KEY, amount INT NOT NULL,
            note VARCHAR(10) NULL, seedbonus INT NULL, info INT NULL,
            CONSTRAINT ck2 CHECK (amount > 0 OR note IS NULL),
            CONSTRAINT ck_bonus CHECK (seedbonus >= 0));";
        for alter in [
            "ALTER TABLE orders DROP CONSTRAINT ck2, DROP COLUMN note",
            "ALTER TABLE orders DROP CONSTRAINT ck2, RENAME COLUMN amount TO total",
            "ALTER TABLE orders DROP COLUMN info, DROP COLUMN seedbonus",
        ] {
            assert!(
                matches!(resolved(table, alter), Resolution::Runs(_)),
                "{alter}"
            );
        }
        let schema = Schema::from_sql(&format!(
            "{table}\nALTER TABLE orders DROP COLUMN info, DROP COLUMN seedbonus;"
        ));
        let orders = known(&schema, "orders");
        assert!(
            orders
                .checks
                .iter()
                .all(|check| check.name.as_deref() != Some("ck_bonus"))
        );
        // Every column of a CHECK of several columns, in one statement: 3959.
        assert_eq!(
            refused_with(resolved(
                table,
                "ALTER TABLE orders DROP COLUMN amount, DROP COLUMN note"
            )),
            Some(3959)
        );
    }
}
