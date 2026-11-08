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

/// For each statement, whether it is an ALTER TABLE of a table that the
/// input created earlier and that no statement since may have put rows in.
/// A statement may have, unless kilit follows it as a change of tables
/// only, if its text names the table, as INSERT, UPDATE and LOAD DATA do.
/// CALL and EXECUTE run statements kilit does not see, which may put rows
/// in any table; so may any such statement after one that names a trigger,
/// a function, a procedure or an event, which may have created one.
pub fn on_empty_tables(statements: &[Statement]) -> Vec<bool> {
    let mut empty_tables: Vec<String> = Vec::new();
    let mut runs_hidden_code = false;
    statements
        .iter()
        .map(|statement| {
            let on_empty_table = match &statement.kind {
                StatementKind::Alter(alter) => empty_tables.contains(&key(&alter.table)),
                _ => false,
            };
            if may_add_rows(statement) {
                let sql = statement.sql.to_lowercase();
                if runs_hidden_code || runs_unseen_statements(&sql) {
                    empty_tables.clear();
                }
                // A trigger or a function created before a table may put
                // rows in it later, without naming it again.
                runs_hidden_code |= words(&sql)
                    .any(|word| matches!(word, "trigger" | "function" | "procedure" | "event"));
                // `shop.orders` and `orders` both name orders.
                empty_tables.retain(|table| {
                    let name = table.rsplit('.').next().unwrap_or(table);
                    !sql.contains(name)
                });
            }
            follow(statement, &mut empty_tables);
            on_empty_table
        })
        .collect()
}

/// Whether kilit cannot rule out that `statement` puts rows in a table:
/// every statement but CREATE TABLE, DROP TABLE, RENAME TABLE and an ALTER
/// TABLE that kilit reads in full.
fn may_add_rows(statement: &Statement) -> bool {
    match &statement.kind {
        StatementKind::CreateTable { .. }
        | StatementKind::DropTable { .. }
        | StatementKind::RenameTable { .. } => false,
        StatementKind::Alter(alter) => alter
            .operations
            .iter()
            .any(|operation| matches!(operation, Operation::Other { .. })),
        _ => true,
    }
}

/// `CALL`, `EXECUTE` or `DO` anywhere in `sql`, in lowercase, as a word.
fn runs_unseen_statements(sql: &str) -> bool {
    words(sql).any(|word| matches!(word, "call" | "execute" | "do"))
}

/// The words of `sql`, split at every character that cannot be in a name.
fn words(sql: &str) -> impl Iterator<Item = &str> {
    sql.split(|c: char| !(c.is_alphanumeric() || c == '_' || c == '$'))
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
pub(crate) fn follow(statement: &Statement, new_tables: &mut Vec<String>) {
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
pub(crate) fn key(name: &str) -> String {
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
        // error 1064, as NOT VALID is PostgreSQL's. The comment is measured.
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
                Impact::NewTable,
                Impact::NewTable,
            ]
        );
    }

    #[test]
    fn what_mysql_does_not_accept_is_unknown_on_old_and_new_tables() {
        // Each fails with error 1064 on MySQL 8.0.46 and 8.4.13.
        for statement in [
            "ALTER TABLE {} DROP COLUMN IF EXISTS note",
            "ALTER TABLE IF EXISTS {} ADD COLUMN coupon INT",
            "ALTER TABLE ONLY {} ADD COLUMN coupon INT NULL",
            "CREATE INDEX IF NOT EXISTS i ON {} (amount)",
            "CREATE INDEX CONCURRENTLY IF NOT EXISTS i ON {} (amount)",
            "CREATE INDEX i ON {} (amount) WHERE amount > 0",
            "DROP INDEX IF EXISTS i ON {}",
            "ALTER TABLE {} ADD CONSTRAINT fk FOREIGN KEY (user_id) REFERENCES users (id) NOT VALID",
            r#"ALTER TABLE "{}" ADD COLUMN coupon INT"#,
        ] {
            let on_orders = statement.replace("{}", "orders");
            assert_eq!(impacts_of(&on_orders), [Impact::Unknown], "{on_orders}");
            let on_new_table = format!(
                "CREATE TABLE drafts (id INT PRIMARY KEY, user_id BIGINT UNSIGNED NOT NULL, \
                 amount INT NOT NULL, note VARCHAR(50) NULL, INDEX i (amount));
                 {}",
                statement.replace("{}", "drafts")
            );
            assert_eq!(
                impacts_of(&on_new_table),
                [Impact::NotChecked, Impact::Unknown],
                "{on_new_table}"
            );
        }
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

    fn on_empty_tables_of(sql: &str) -> Vec<bool> {
        on_empty_tables(&parse(sql))
    }

    #[test]
    fn a_table_created_earlier_is_empty_until_a_statement_may_fill_it() {
        let create = "CREATE TABLE events (id INT PRIMARY KEY, n INT);";
        let alter = "ALTER TABLE events ADD UNIQUE INDEX events_n (n);";
        assert_eq!(
            on_empty_tables_of(&format!("{create} {alter} {alter}")),
            [false, true, true]
        );
        for filler in [
            "INSERT INTO events VALUES (1, 1);",
            "INSERT INTO `shop`.`Events` VALUES (1, 1);",
            "REPLACE INTO events VALUES (1, 1);",
            "UPDATE events SET n = 1;",
            "LOAD DATA INFILE '/tmp/e.csv' INTO TABLE events;",
            "ALTER TABLE orders EXCHANGE PARTITION p0 WITH TABLE events;",
            "SELECT * FROM events;",
            "CALL fill_tables();",
            "EXECUTE fill;",
        ] {
            assert_eq!(
                on_empty_tables_of(&format!("{create} {filler} {alter}")),
                [false, false, false],
                "{filler}"
            );
        }
        // A trigger or a function made before the table may fill it later.
        for routine in [
            "CREATE TRIGGER orders_audit AFTER UPDATE ON orders FOR EACH ROW \
             INSERT INTO events VALUES (OLD.id, 1);",
            "CREATE FUNCTION fill() RETURNS INT DETERMINISTIC RETURN 1;",
        ] {
            assert_eq!(
                on_empty_tables_of(&format!(
                    "{routine} {create} UPDATE orders SET note = 'x'; {alter}"
                )),
                [false, false, false, false],
                "{routine}"
            );
        }
        // A statement that names another table leaves it empty.
        assert_eq!(
            on_empty_tables_of(&format!("{create} INSERT INTO orders VALUES (1); {alter}")),
            [false, false, true]
        );
    }

    #[test]
    fn a_table_that_was_not_created_by_the_input_may_have_rows() {
        assert_eq!(
            on_empty_tables_of(
                "ALTER TABLE orders ADD UNIQUE INDEX orders_note (note);
                 CREATE TABLE IF NOT EXISTS events (id INT PRIMARY KEY, n INT);
                 ALTER TABLE events ADD UNIQUE INDEX events_n (n);
                 CREATE TABLE fresh (id INT PRIMARY KEY);
                 DROP TABLE fresh;
                 RENAME TABLE orders TO fresh;
                 ALTER TABLE fresh ADD UNIQUE INDEX fresh_note (note);"
            ),
            [false, false, false, false, false, false, false]
        );
    }

    #[test]
    fn an_empty_table_stays_empty_under_its_new_name() {
        assert_eq!(
            on_empty_tables_of(
                "CREATE TABLE events_new (id INT PRIMARY KEY, n INT);
                 RENAME TABLE events_new TO events;
                 ALTER TABLE events ADD UNIQUE INDEX events_n (n);
                 ALTER TABLE events RENAME TO events_old;
                 ALTER TABLE events_old ADD INDEX events_old_n (n);"
            ),
            [false, false, true, true, true]
        );
    }
}
