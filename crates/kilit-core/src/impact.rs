//! What each statement means for the application while the migration runs,
//! in one word: from what kilit measured, and from where the statement
//! stands in the migration.

use serde::Serialize;

use crate::check::{Finding, Prediction};
use crate::results::{Verdict, Writes};
use crate::statement::{Operation, Statement, StatementKind};

/// What a statement means for the application while it runs.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Impact {
    /// The statement fails: the migration stops there.
    Fails,
    /// Writes to the table wait until the statement ends.
    WritesWait,
    /// Writes continue while the statement runs.
    WritesContinue,
    /// kilit measured the statement, MySQL runs it, and it changes a table
    /// that an earlier statement of the same migration created. Nothing else
    /// uses the table yet, so nothing waits for it, whatever algorithm MySQL
    /// picks.
    NewTable,
    /// kilit cannot tell. Never treat this as safe, not even on a table the
    /// migration created: MySQL may still reject the statement.
    Unknown,
    /// Not something kilit checks, such as a new table or a data change.
    NotChecked,
}

/// The impact of each of `findings`, which [`check`](crate::check) gave for
/// `statements`, in the same order.
///
/// A measured statement on a table the migration created (see
/// [`on_new_tables`]) is [`Impact::NewTable`], unless it fails. What kilit
/// cannot tell stays [`Impact::Unknown`] there too: nothing waits for a new
/// table, but the statement may still fail.
pub fn impacts(statements: &[Statement], findings: &[Finding]) -> Vec<Impact> {
    on_new_tables(statements)
        .into_iter()
        .zip(findings)
        .map(|(on_new_table, finding)| match &finding.prediction {
            Prediction::Measured {
                verdict: Verdict::Fails { .. },
                ..
            } => Impact::Fails,
            Prediction::Measured { .. } if on_new_table => Impact::NewTable,
            Prediction::Measured {
                verdict: Verdict::Runs { writes, .. },
                ..
            } => match writes {
                Writes::Allowed => Impact::WritesContinue,
                Writes::Blocked => Impact::WritesWait,
                Writes::Unknown => Impact::Unknown,
            },
            Prediction::Unknown { .. } => Impact::Unknown,
            Prediction::NotChecked { .. } => Impact::NotChecked,
        })
        .collect()
}

/// For each of `statements`, in the same order, whether it changes only
/// tables that an earlier statement of `statements` created: nothing else can
/// be using them yet, so nothing holds a lock on them.
///
/// A table counts as new from a plain `CREATE TABLE` until it is dropped, and
/// stays new through `RENAME TABLE` and `ALTER TABLE ... RENAME TO`.
/// `CREATE TABLE IF NOT EXISTS` does nothing when the table is already there,
/// so it does not count; neither does `CREATE TABLE ... LIKE` or
/// `... AS SELECT`, which kilit does not read yet. Names compare without
/// quotes and in any case, as the schema compares them; `shop.orders` and
/// `orders` are different names.
///
/// Only `ALTER TABLE` (with `CREATE INDEX` and `DROP INDEX`), `RENAME TABLE`,
/// and a statement kilit cannot read that names a table count as changing
/// one. Any other statement, such as the `CREATE TABLE` itself, a
/// `DROP TABLE` or an `INSERT`, gives `false`.
pub fn on_new_tables(statements: &[Statement]) -> Vec<bool> {
    let mut new_tables: Vec<String> = Vec::new();
    statements
        .iter()
        .map(|statement| {
            let changed = changed_tables(statement);
            let on_new_table =
                !changed.is_empty() && changed.iter().all(|table| new_tables.contains(&key(table)));
            follow(statement, &mut new_tables);
            on_new_table
        })
        .collect()
}

/// The tables a statement changes, as they are named before it runs.
fn changed_tables(statement: &Statement) -> Vec<&str> {
    match &statement.kind {
        StatementKind::Alter(alter) => vec![alter.table.as_str()],
        StatementKind::RenameTable { renames } => {
            renames.iter().map(|rename| rename.from.as_str()).collect()
        }
        StatementKind::Unparsed {
            table: Some(table), ..
        } => vec![table.as_str()],
        _ => Vec::new(),
    }
}

/// Keeps `new_tables` up to date with what `statement` creates, renames and
/// drops.
fn follow(statement: &Statement, new_tables: &mut Vec<String>) {
    let mut rename = |from: &str, to: &str| {
        if let Some(table) = new_tables.iter_mut().find(|table| **table == key(from)) {
            *table = key(to);
        }
    };
    match &statement.kind {
        StatementKind::CreateTable {
            table,
            if_not_exists: false,
        } => new_tables.push(key(&table.name)),
        StatementKind::DropTable { tables } => {
            new_tables.retain(|table| !tables.iter().any(|dropped| key(dropped) == *table));
        }
        StatementKind::RenameTable { renames } => {
            for rename_one in renames {
                rename(&rename_one.from, &rename_one.to);
            }
        }
        StatementKind::Alter(alter) => {
            for operation in &alter.operations {
                if let Operation::RenameTable { new_name } = operation {
                    rename(&alter.table, new_name);
                }
            }
        }
        _ => {}
    }
}

/// Table names in any case, as the schema compares them.
fn key(name: &str) -> String {
    name.to_lowercase()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Knowledge, Schema, check, parse};

    fn impacts_of(sql: &str) -> Vec<Impact> {
        let schema = Schema::from_sql(include_str!("../examples/mysql-schema.sql"));
        let results = Knowledge::built_in().server("mysql-8.4.13").unwrap();
        let statements = parse(sql);
        impacts(&statements, &check(&statements, &schema, results))
    }

    #[test]
    fn each_verdict_has_its_impact() {
        assert_eq!(
            impacts_of(
                "alter table `orders` add `coupon` varchar(255) null;
                 alter table `orders` modify `amount` bigint not null;
                 alter table `orders` modify `user_id` int unsigned not null;
                 alter table `orders` modify `status` varchar(30) not null;
                 update `orders` set `status` = 'old';"
            ),
            [
                Impact::WritesContinue,
                Impact::WritesWait,
                Impact::Fails,
                Impact::Unknown,
                Impact::NotChecked,
            ]
        );
    }

    #[test]
    fn a_table_created_earlier_in_the_migration_is_new() {
        // Schema::create('order_events', ...) with a constrained foreignId.
        assert_eq!(
            impacts_of(
                "create table `order_events` (`id` bigint unsigned not null auto_increment primary key, \
                 `order_id` bigint unsigned not null);
                 alter table `order_events` add constraint `order_events_order_id_foreign` \
                 foreign key (`order_id`) references `orders` (`id`) on delete cascade;
                 alter table `orders` add `shop_id` bigint unsigned null;
                 alter table `orders` add constraint `orders_shop_id_foreign` \
                 foreign key (`shop_id`) references `shops` (`id`);"
            ),
            [
                Impact::NotChecked,
                Impact::NewTable,
                Impact::WritesContinue,
                Impact::WritesWait,
            ]
        );
    }

    #[test]
    fn a_table_stays_new_through_a_rename_until_it_is_dropped() {
        assert_eq!(
            impacts_of(
                "CREATE TABLE drafts (id INT PRIMARY KEY, note TEXT);
                 RENAME TABLE drafts TO notes;
                 ALTER TABLE notes ADD FULLTEXT (note);
                 ALTER TABLE notes RENAME TO memos;
                 ALTER TABLE MEMOS ADD FULLTEXT (note);
                 DROP TABLE memos;
                 CREATE TABLE IF NOT EXISTS memos (id INT PRIMARY KEY, note TEXT);
                 ALTER TABLE memos ADD FULLTEXT (note);"
            ),
            [
                Impact::NotChecked,
                // kilit has not measured renaming a table: MySQL may reject
                // it, new table or not.
                Impact::Unknown,
                Impact::NewTable,
                Impact::Unknown,
                Impact::NewTable,
                Impact::NotChecked,
                Impact::NotChecked,
                Impact::WritesWait,
            ]
        );
    }

    #[test]
    fn what_kilit_cannot_tell_stays_unknown_on_a_new_table() {
        // Nothing waits for a table the migration created, but MySQL may
        // still reject the statement: 8.0.46 and 8.4.13 fail the CHECK with
        // error 1064, as NOT VALID is PostgreSQL's. The comment is MySQL's,
        // but kilit cannot read it.
        assert_eq!(
            impacts_of(
                "create table `order_tags` (`id` bigint unsigned not null auto_increment primary key, \
                 `kind` varchar(20) not null);
                 alter table `order_tags` add constraint `order_tags_kind_check` \
                 check (`kind` in ('gift', 'rush')) not valid;
                 alter table `order_tags` comment = 'Tags on orders';
                 alter table `order_tags` add `note` varchar(255) null;"
            ),
            [
                Impact::NotChecked,
                Impact::Unknown,
                Impact::Unknown,
                Impact::NewTable,
            ]
        );
    }

    fn on_new_tables_of(sql: &str) -> Vec<bool> {
        on_new_tables(&parse(sql))
    }

    #[test]
    fn create_table_if_not_exists_does_not_make_a_table_new() {
        // The table may be there already, with rows and with users. The
        // statements alone cannot tell, so it is not new, whether the schema
        // has it or not.
        assert_eq!(
            on_new_tables_of(
                "CREATE TABLE IF NOT EXISTS orders (id INT PRIMARY KEY);
                 ALTER TABLE orders ADD COLUMN note TEXT;
                 CREATE TABLE IF NOT EXISTS archive (id INT PRIMARY KEY);
                 ALTER TABLE archive ADD COLUMN note TEXT;"
            ),
            [false, false, false, false]
        );
    }

    #[test]
    fn a_table_dropped_and_created_again_is_new() {
        assert_eq!(
            on_new_tables_of(
                "DROP TABLE orders;
                 CREATE TABLE orders (id INT PRIMARY KEY);
                 ALTER TABLE orders ADD COLUMN note TEXT;"
            ),
            [false, false, true]
        );
    }

    #[test]
    fn a_new_table_renamed_dropped_and_created_again() {
        assert_eq!(
            on_new_tables_of(
                "CREATE TABLE drafts (id INT PRIMARY KEY);
                 RENAME TABLE drafts TO notes;
                 ALTER TABLE notes ADD COLUMN body TEXT;
                 ALTER TABLE drafts ADD COLUMN body TEXT;
                 DROP TABLE notes;
                 ALTER TABLE notes ADD COLUMN title TEXT;
                 CREATE TABLE drafts (id INT PRIMARY KEY);
                 ALTER TABLE drafts ADD COLUMN body TEXT;"
            ),
            [false, true, true, false, false, false, false, true]
        );
    }

    #[test]
    fn table_names_compare_without_quotes_and_in_any_case() {
        // On Linux, MySQL keeps `Events` and `events` apart by default
        // (lower_case_table_names = 0). kilit does not, here or in the schema.
        assert_eq!(
            on_new_tables_of(
                "CREATE TABLE `Events` (id INT PRIMARY KEY);
                 ALTER TABLE events ADD COLUMN a INT;
                 ALTER TABLE `EVENTS` ADD COLUMN b INT;
                 CREATE TABLE app.logs (id INT PRIMARY KEY);
                 ALTER TABLE `app`.`LOGS` ADD COLUMN a INT;
                 ALTER TABLE logs ADD COLUMN b INT;"
            ),
            [false, true, true, false, true, false]
        );
    }

    #[test]
    fn a_table_made_with_like_or_select_is_not_new() {
        // kilit does not read CREATE TABLE ... LIKE or ... AS SELECT yet.
        assert_eq!(
            on_new_tables_of(
                "CREATE TABLE orders_copy LIKE orders;
                 ALTER TABLE orders_copy ADD COLUMN note TEXT;
                 CREATE TABLE totals AS SELECT user_id, SUM(amount) AS total FROM orders GROUP BY user_id;
                 ALTER TABLE totals ADD COLUMN note TEXT;"
            ),
            [false, false, false, false]
        );
    }

    #[test]
    fn a_failure_on_a_new_table_still_fails() {
        assert_eq!(
            impacts_of(
                "create table `order_events` (`id` bigint unsigned not null auto_increment primary key, \
                 `order_id` bigint unsigned not null, \
                 constraint `fk` foreign key (`order_id`) references `orders` (`id`));
                 alter table `order_events` modify `order_id` int unsigned not null;"
            ),
            [Impact::NotChecked, Impact::Fails]
        );
    }
}
