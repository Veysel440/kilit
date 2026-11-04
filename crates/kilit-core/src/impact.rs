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
    /// The statement changes a table that an earlier statement of the same
    /// migration created. Nothing else uses the table yet, so nothing waits
    /// for it, whatever MySQL does with the statement.
    NewTable,
    /// kilit cannot tell. Never treat this as safe.
    Unknown,
    /// Not something kilit checks, such as a new table or a data change.
    NotChecked,
}

/// The impact of each of `findings`, which [`check`](crate::check) gave for
/// `statements`, in the same order.
///
/// A table counts as new from a plain `CREATE TABLE` in `statements` until it
/// is dropped. `CREATE TABLE IF NOT EXISTS` does nothing when the table is
/// already there, so it does not count.
pub fn impacts(statements: &[Statement], findings: &[Finding]) -> Vec<Impact> {
    let mut new_tables: Vec<String> = Vec::new();
    statements
        .iter()
        .zip(findings)
        .map(|(statement, finding)| {
            let changed = changed_tables(statement);
            let on_new_table =
                !changed.is_empty() && changed.iter().all(|table| new_tables.contains(&key(table)));
            let impact = match &finding.prediction {
                Prediction::Measured {
                    verdict: Verdict::Fails { .. },
                    ..
                } => Impact::Fails,
                Prediction::Measured { .. } | Prediction::Unknown { .. } if on_new_table => {
                    Impact::NewTable
                }
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
            };
            follow(statement, &mut new_tables);
            impact
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
                Impact::NewTable,
                Impact::NewTable,
                Impact::NewTable,
                Impact::NewTable,
                Impact::NotChecked,
                Impact::NotChecked,
                Impact::WritesWait,
            ]
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
