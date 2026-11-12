//! What each statement means for the application while the migration runs,
//! in one word: from what kilit measured, and from where the statement
//! stands in the migration.

use serde::Serialize;

use std::borrow::Cow;

use crate::check::{Finding, LetterCase, Prediction, Shared};
use crate::results::{Verdict, Writes};
use crate::schema::{folded, with_table_renamed};
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
    on_new_tables(statements, findings)
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
            Prediction::DependsOnTable {
                shared: Shared::Fails,
                ..
            } => Impact::Fails,
            Prediction::DependsOnTable { .. } if on_new_table => Impact::NewTable,
            Prediction::DependsOnTable { .. } => Impact::WritesWait,
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
/// quotes and exactly, as the schema compares them; `shop.orders` and
/// `orders` are different names, and so are `orders` and `ORDERS`. A
/// statement that names a table in another letter case (see
/// [`Finding::letter_case`]) is on no new table: with lower_case_table_names
/// = 1 or 2 its table may be one that exists. It is followed as it runs
/// under the setting the migration goes on with, if kilit can tell which.
///
/// Only `ALTER TABLE` (with `CREATE INDEX` and `DROP INDEX`), `RENAME TABLE`,
/// and a statement kilit cannot read that names a table count as changing
/// one. Any other statement, such as the `CREATE TABLE` itself, a
/// `DROP TABLE` or an `INSERT`, gives `false`.
/// `findings` are what [`check`](crate::check) gave for `statements`.
pub fn on_new_tables(statements: &[Statement], findings: &[Finding]) -> Vec<bool> {
    statements
        .iter()
        .zip(findings)
        .map(|(statement, finding)| {
            let changed = changed_tables(statement);
            finding.letter_case.is_none()
                && !changed.is_empty()
                && changed
                    .iter()
                    .all(|table| finding.created_here.iter().any(|new| new == table))
        })
        .collect()
}

/// For each statement, whether it is an ALTER TABLE of a table that the
/// input created earlier and that no statement since may have put rows in.
/// A statement may have, unless kilit follows it as a change of tables
/// only, if its text names the table, as INSERT, UPDATE and LOAD DATA do.
/// CALL and EXECUTE run statements kilit does not see, which may put rows
/// in any table; so may any such statement after one that names a trigger,
/// a function, a procedure or an event, which may have created one. A
/// statement that names a table in another letter case makes no table
/// empty, as for [`on_new_tables`]; the text of one that may put rows in
/// names a table in any letter case.
pub fn on_empty_tables(statements: &[Statement], findings: &[Finding]) -> Vec<bool> {
    let mut empty_tables: Vec<String> = Vec::new();
    let mut runs_hidden_code = false;
    statements
        .iter()
        .zip(findings)
        .map(|(statement, finding)| {
            // Only a table nothing else can be using yet is empty.
            let on_empty_table = match &statement.kind {
                StatementKind::Alter(alter) if finding.letter_case.is_none() => {
                    empty_tables.contains(&key(&alter.table))
                        && finding.created_here.contains(&key(&alter.table))
                }
                _ => false,
            };
            if may_add_rows(statement) {
                let sql = folded(&statement.sql);
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
                    !sql.contains(&folded(name))
                });
            }
            if let Some(followed) = as_followed(statement, finding) {
                follow(&followed, &mut empty_tables);
            }
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
        | StatementKind::CreateTableLike { .. }
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
pub(crate) fn changed_tables(statement: &Statement) -> Vec<&str> {
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

/// `statement` as kilit follows it for `finding`: under the setting of
/// lower_case_table_names the migration goes on with, when it names a
/// table in another letter case. `None` when kilit cannot tell which.
pub(crate) fn as_followed<'s>(
    statement: &'s Statement,
    finding: &Finding,
) -> Option<Cow<'s, Statement>> {
    match &finding.letter_case {
        None | Some(LetterCase::AsWritten) => Some(Cow::Borrowed(statement)),
        Some(LetterCase::SameTable { written, known }) => {
            Some(Cow::Owned(with_table_renamed(statement, written, known)))
        }
        Some(LetterCase::Unclear) => None,
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
        StatementKind::CreateTableLike {
            name,
            if_not_exists: false,
            ..
        } => new_tables.push(key(name)),
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

/// Table names as written, as the schema compares them.
pub(crate) fn key(name: &str) -> String {
    name.to_owned()
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
                "CREATE TABLE drafts (id INT PRIMARY KEY, note TEXT) DEFAULT CHARSET=utf8mb4;
                 RENAME TABLE drafts TO notes;
                 ALTER TABLE notes ADD FULLTEXT (note);
                 ALTER TABLE notes RENAME TO memos;
                 ALTER TABLE memos ADD FULLTEXT (note);
                 DROP TABLE memos;
                 CREATE TABLE IF NOT EXISTS memos (id INT PRIMARY KEY, note TEXT) \
                 DEFAULT CHARSET=utf8mb4;
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

    /// The findings for `statements` on a database kilit knows nothing of.
    fn findings_of(statements: &[Statement]) -> Vec<Finding> {
        let results = Knowledge::built_in().server("mysql-8.4.13").unwrap();
        check(statements, &Schema::new(), results)
    }

    fn on_new_tables_of(sql: &str) -> Vec<bool> {
        let statements = parse(sql);
        on_new_tables(&statements, &findings_of(&statements))
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
    fn table_names_compare_without_quotes_and_exactly() {
        // With lower_case_table_names = 0, the default on Linux, `events`
        // is another table than `Events`; with 1 or 2 the same one. A name
        // in another letter case is never on a new table.
        assert_eq!(
            on_new_tables_of(
                "CREATE TABLE `Events` (id INT PRIMARY KEY);
                 ALTER TABLE Events ADD COLUMN a INT;
                 ALTER TABLE events ADD COLUMN b INT;
                 ALTER TABLE `EVENTS` ADD COLUMN c INT;
                 ALTER TABLE `Events` ADD COLUMN d INT;
                 CREATE TABLE app.logs (id INT PRIMARY KEY);
                 ALTER TABLE `app`.`logs` ADD COLUMN a INT;
                 ALTER TABLE `app`.`LOGS` ADD COLUMN b INT;
                 ALTER TABLE logs ADD COLUMN c INT;"
            ),
            [false, true, false, false, true, false, true, false, false]
        );
    }

    #[test]
    fn a_table_created_or_renamed_in_another_letter_case_is_new_where_the_migration_goes_on() {
        // With 1 or 2, CREATE TABLE T1 fails on t1 (1050), and so does
        // RENAME TABLE t2 TO T2: the migration goes on only with 0, where
        // T1 is a new table of its own and t2 is renamed T2. The statement
        // itself is on no new table.
        assert_eq!(
            on_new_tables_of(
                "CREATE TABLE t1 (id INT PRIMARY KEY);
                 CREATE TABLE T1 (id INT PRIMARY KEY);
                 ALTER TABLE t1 ADD COLUMN a INT;
                 ALTER TABLE T1 ADD COLUMN a INT;
                 ALTER TABLE t1 ADD COLUMN b INT;
                 CREATE TABLE t2 (id INT PRIMARY KEY);
                 RENAME TABLE t2 TO T2;
                 ALTER TABLE T2 ADD COLUMN a INT;"
            ),
            [false, false, true, true, true, false, false, true]
        );
        assert_eq!(
            on_empty_tables_of(
                "CREATE TABLE t1 (id INT PRIMARY KEY);
                 CREATE TABLE T1 (id INT PRIMARY KEY);
                 ALTER TABLE t1 ADD COLUMN a INT;
                 ALTER TABLE T1 ADD COLUMN a INT;
                 ALTER TABLE t1 ADD COLUMN b INT;"
            ),
            [false, false, true, true, true]
        );
        // DROP TABLE IF EXISTS T3 runs with both settings and leaves t3 or
        // not: kilit cannot tell, and t3 stays new as before.
        assert_eq!(
            on_new_tables_of(
                "CREATE TABLE t3 (id INT PRIMARY KEY);
                 DROP TABLE IF EXISTS T3;
                 ALTER TABLE t3 ADD COLUMN a INT;"
            ),
            [false, false, true]
        );
    }

    #[test]
    fn rows_put_in_a_table_named_in_any_letter_case_count() {
        // With 1 or 2, INSERT INTO T1 fills t1; ÇİZELGE is çizelge.
        for insert in [
            "INSERT INTO T1 VALUES (1)",
            "INSERT INTO ÇİZELGE VALUES (1)",
        ] {
            let table = if insert.contains("T1") {
                "t1"
            } else {
                "çizelge"
            };
            assert_eq!(
                on_empty_tables_of(&format!(
                    "CREATE TABLE {table} (id INT PRIMARY KEY);
                     {insert};
                     ALTER TABLE {table} ADD COLUMN a INT;"
                )),
                [false, false, false],
                "{insert}"
            );
        }
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
        let statements = parse(sql);
        on_empty_tables(&statements, &findings_of(&statements))
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

    // Which tables are new follows the tables kilit holds.

    fn impacts_in_files(schema: &Schema, files: &[&str]) -> Vec<Vec<Impact>> {
        let results = Knowledge::built_in().server("mysql-8.4.13").unwrap();
        let parsed: Vec<Vec<Statement>> = files.iter().map(|sql| parse(sql)).collect();
        let sessions: Vec<&[Statement]> = parsed.iter().map(Vec::as_slice).collect();
        let statements: Vec<Statement> = parsed.iter().flatten().cloned().collect();
        let findings = crate::check_sessions(&sessions, schema, results);
        let mut all = impacts(&statements, &findings).into_iter();
        parsed
            .iter()
            .map(|file| all.by_ref().take(file.len()).collect())
            .collect()
    }

    fn laravel_tables() -> Schema {
        Schema::from_sql(include_str!("../examples/mysql-schema.sql"))
    }

    #[test]
    fn a_table_swapped_onto_a_name_from_before_the_input_is_not_new() {
        // The copy-and-swap pattern: after the RENAME, orders is the live
        // table.
        assert_eq!(
            impacts_of(
                "create table `orders_new` (`id` bigint unsigned not null primary key, \
                 `coupon` varchar(255) null) default charset=utf8mb4;
                 rename table `orders` to `orders_old`, `orders_new` to `orders`;
                 alter table `orders` add `extra` int null;"
            ),
            [Impact::NotChecked, Impact::Unknown, Impact::WritesContinue]
        );
        // Onto a fresh name, it stays new.
        assert_eq!(
            impacts_of(
                "create table `drafts` (`id` int primary key) default charset=utf8mb4;
                 rename table `drafts` to `notes`;
                 alter table `notes` add `extra` int null;"
            )[2],
            Impact::NewTable
        );
    }

    #[test]
    fn a_temporary_table_is_new_only_in_its_own_input_file() {
        let files = [
            "create temporary table `orders` (`id` int primary key) default charset=utf8mb4;
             alter table `orders` add `extra` int null;",
            "alter table `orders` add `coupon` varchar(255) null;",
        ];
        assert_eq!(
            impacts_in_files(&laravel_tables(), &files),
            [
                vec![Impact::NotChecked, Impact::NewTable],
                vec![Impact::WritesContinue]
            ]
        );
        // The metadata lock warning names it in the next file.
        let results = Knowledge::built_in().server("mysql-8.4.13").unwrap();
        let parsed: Vec<Vec<Statement>> = files.iter().map(|sql| parse(sql)).collect();
        let sessions: Vec<&[Statement]> = parsed.iter().map(Vec::as_slice).collect();
        let statements: Vec<Statement> = parsed.iter().flatten().cloned().collect();
        let findings = crate::check_sessions(&sessions, &laravel_tables(), results);
        let locked = crate::locked_tables(&statements, &findings);
        assert!(locked[1].is_empty());
        assert_eq!(locked[2], ["orders"]);
    }

    #[test]
    fn a_create_table_of_a_dump_table_makes_no_new_table() {
        let dump = Schema::from_dump(include_str!("../examples/mysql-schema.sql"));
        assert_eq!(
            impacts_in_files(
                &dump,
                &[
                    "create table `orders` (`id` int primary key) default charset=utf8mb4;
                   alter table `orders` add `extra` int null;"
                ]
            ),
            [vec![Impact::Unknown, Impact::WritesContinue]]
        );
    }

    #[test]
    fn a_table_created_like_another_is_new() {
        assert_eq!(
            impacts_of(
                "create table `orders_copy` like `orders`;
                 alter table `orders_copy` add `extra` int null;"
            ),
            [Impact::NotChecked, Impact::NewTable]
        );
    }
}
