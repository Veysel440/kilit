//! The metadata lock: what every ALTER TABLE, DROP TABLE, RENAME TABLE and
//! TRUNCATE TABLE waits for before it starts.
//!
//! These statements need their tables to themselves for a moment, even for
//! an INSTANT change. Each waits until every open transaction that has used
//! one of its tables ends, and while it waits, every new query on those
//! tables waits behind it, reads too. kilit-lab checks all of this on every
//! server it measures, with one ALTER per algorithm and one statement of
//! each kind in [`STATEMENTS_THAT_WAIT`] (`crates/kilit-lab/src/facts.rs`).

use crate::check::Finding;
use crate::impact::{as_followed, follow, key};
use crate::parse::{
    alters_named_table, drops_temporary_table, parse, truncated_table, with_table_for_tables,
};
use crate::statement::{Algorithm, Statement, StatementKind};

/// The algorithms kilit-lab checks: an ALTER that runs with any of them
/// waits for an open transaction on its table.
pub const ALGORITHMS_THAT_WAIT: [Algorithm; 3] =
    [Algorithm::Instant, Algorithm::Inplace, Algorithm::Copy];

/// A statement other than `ALTER TABLE` that kilit-lab checks the way it
/// checks an ALTER.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StatementThatWaits {
    /// `CREATE INDEX ... ON`, which kilit reads as an ALTER.
    CreateIndex,
    /// `DROP INDEX ... ON`, which kilit reads as an ALTER.
    DropIndex,
    /// `DROP TABLE`, also of several tables.
    DropTable,
    /// `RENAME TABLE a TO b`
    RenameTable,
    /// `TRUNCATE [TABLE] a`
    TruncateTable,
}

impl StatementThatWaits {
    /// The statement's first words, as the warning names it.
    pub fn name(self) -> &'static str {
        match self {
            StatementThatWaits::CreateIndex => "CREATE INDEX",
            StatementThatWaits::DropIndex => "DROP INDEX",
            StatementThatWaits::DropTable => "DROP TABLE",
            StatementThatWaits::RenameTable => "RENAME TABLE",
            StatementThatWaits::TruncateTable => "TRUNCATE TABLE",
        }
    }
}

/// The statements besides ALTER TABLE that kilit-lab checks: each waits for
/// an open transaction that has read its table. `LOCK TABLES ... READ` is
/// not one of them: it does not wait for a transaction that only read the
/// table, and reads do not queue behind it.
pub const STATEMENTS_THAT_WAIT: [StatementThatWaits; 5] = [
    StatementThatWaits::CreateIndex,
    StatementThatWaits::DropIndex,
    StatementThatWaits::DropTable,
    StatementThatWaits::RenameTable,
    StatementThatWaits::TruncateTable,
];

/// How long a statement waits for a metadata lock unless told otherwise:
/// MySQL's default `lock_wait_timeout`, in seconds. A year.
pub const DEFAULT_LOCK_WAIT_TIMEOUT: u64 = 31_536_000;

/// The error a statement gets when it has waited `lock_wait_timeout` seconds.
pub const LOCK_WAIT_TIMEOUT_ERROR: u16 = 1205;

/// What kilit says about the metadata lock, once for a migration that
/// alters, drops, renames or truncates tables that already exist.
pub fn metadata_lock_warning() -> String {
    format!(
        "Every ALTER TABLE, DROP TABLE, RENAME TABLE and TRUNCATE TABLE first \
         waits for the open transactions that have used any of its tables to end. \
         While it waits, new queries on those tables queue behind it, reads too, \
         even for an INSTANT change, and by default MySQL lets it wait a year \
         (lock_wait_timeout = {DEFAULT_LOCK_WAIT_TIMEOUT}). Run SET SESSION \
         lock_wait_timeout = 5 before the migration to cap the wait: the statement \
         then fails with error {LOCK_WAIT_TIMEOUT_ERROR} after 5 seconds, the \
         queries behind it go on, and you can run the migration again later."
    )
}

/// For each of `statements`, in the same order, the tables whose metadata
/// lock it waits for and that may exist before the input: every table it
/// names that no earlier statement of `statements` created, as
/// [`crate::on_new_tables`] follows them from `findings`, what
/// [`check`](crate::check) gave for `statements`. An ALTER TABLE (also `CREATE
/// INDEX` and `DROP INDEX`) names its table, also when kilit could not read
/// the rest; a DROP TABLE (also `DROP TABLES`) each table, unless it is
/// `DROP TEMPORARY`; a RENAME TABLE (also `RENAME TABLES`) each old name; a
/// TRUNCATE TABLE its table. Other statements name none.
pub fn locked_tables(statements: &[Statement], findings: &[Finding]) -> Vec<Vec<String>> {
    let mut new_tables: Vec<String> = Vec::new();
    statements
        .iter()
        .zip(findings)
        .map(|(statement, finding)| {
            let tables = named_tables(&statement.kind, &statement.sql)
                .into_iter()
                .filter(|table| !new_tables.contains(&key(table)))
                .collect();
            if let Some(followed) = as_followed(statement, finding) {
                follow(&followed, &mut new_tables);
            }
            tables
        })
        .collect()
}

/// The tables whose metadata lock the statement `sql`, read as `kind`,
/// waits for, as named before it runs.
fn named_tables(kind: &StatementKind, sql: &str) -> Vec<String> {
    match kind {
        StatementKind::Alter(alter) => vec![alter.table.clone()],
        StatementKind::Unparsed {
            table: Some(table), ..
        } if alters_named_table(sql) => vec![table.clone()],
        // A temporary table belongs to its session: no other transaction
        // can have used it.
        StatementKind::DropTable { .. } if drops_temporary_table(sql) => Vec::new(),
        StatementKind::DropTable { tables } => tables.clone(),
        // `RENAME TABLE a TO tmp, b TO a, tmp TO b`: `tmp` is a name the
        // statement itself gave, which no transaction can have used.
        StatementKind::RenameTable { renames } => renames
            .iter()
            .enumerate()
            .filter(|(at, rename)| {
                !renames[..*at]
                    .iter()
                    .any(|earlier| key(&earlier.to) == key(&rename.from))
            })
            .map(|(_, rename)| rename.from.clone())
            .collect(),
        StatementKind::Unparsed { table: None, .. } => {
            if let Some(singular) = with_table_for_tables(sql) {
                let read = parse(&singular);
                match read.as_slice() {
                    [
                        statement @ Statement {
                            kind:
                                StatementKind::DropTable { .. } | StatementKind::RenameTable { .. },
                            ..
                        },
                    ] => named_tables(&statement.kind, &statement.sql),
                    _ => Vec::new(),
                }
            } else {
                truncated_table(sql).into_iter().collect()
            }
        }
        _ => Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_warning_states_the_measured_numbers() {
        let warning = metadata_lock_warning();
        assert!(warning.contains("lock_wait_timeout = 31536000"));
        assert!(warning.contains("error 1205"));
    }

    #[test]
    fn the_warning_names_every_measured_kind_mysql_does_not_run_as_an_alter() {
        let warning = metadata_lock_warning();
        assert!(warning.starts_with("Every ALTER TABLE, "));
        for kind in STATEMENTS_THAT_WAIT {
            match kind {
                // MySQL runs them as ALTER TABLE.
                StatementThatWaits::CreateIndex | StatementThatWaits::DropIndex => {}
                StatementThatWaits::DropTable
                | StatementThatWaits::RenameTable
                | StatementThatWaits::TruncateTable => {
                    assert!(warning.contains(kind.name()), "{}", kind.name());
                }
            }
        }
    }

    fn locked(sql: &str) -> Vec<Vec<String>> {
        let statements = parse(sql);
        let results = crate::Knowledge::built_in().server("mysql-8.4.13").unwrap();
        let findings = crate::check(&statements, &crate::Schema::new(), results);
        locked_tables(&statements, &findings)
    }

    #[test]
    fn a_table_named_in_another_letter_case_than_a_new_one_may_exist() {
        // ALTER TABLE T1 fails with 0, so the migration goes on with 1 or 2,
        // where T1 is the new t1; kilit still names T1. RENAME TABLE t1 TO
        // T1 fails with 1 (1050), so it goes on with 0: T1 is the new table,
        // and t1 is a name no new table has.
        assert_eq!(
            locked(
                "CREATE TABLE t1 (id INT);
                 ALTER TABLE T1 ADD COLUMN a INT;
                 ALTER TABLE t1 ADD COLUMN b INT;
                 RENAME TABLE t1 TO T1;
                 ALTER TABLE T1 ADD COLUMN c INT;
                 ALTER TABLE t1 ADD COLUMN d INT;"
            ),
            [
                vec![],
                vec!["T1".to_owned()],
                vec![],
                vec![],
                vec![],
                vec!["t1".to_owned()]
            ]
        );
    }

    #[test]
    fn drop_rename_and_truncate_name_the_tables_they_wait_for() {
        assert_eq!(
            locked(
                "DROP TABLE orders, `users`; RENAME TABLE a TO b, c TO d; \
                 TRUNCATE TABLE e; TRUNCATE f; truncate table `shop`.`g`"
            ),
            [
                vec!["orders", "users"],
                vec!["a", "c"],
                vec!["e"],
                vec!["f"],
                vec!["shop.g"]
            ]
        );
    }

    #[test]
    fn alters_and_index_statements_name_their_table() {
        assert_eq!(
            locked(
                "ALTER TABLE a ADD COLUMN x INT; CREATE INDEX i ON b (x); \
                 DROP INDEX i ON c; ALTER TABLE d RENAME TO e"
            ),
            [vec!["a"], vec!["b"], vec!["c"], vec!["d"]]
        );
    }

    #[test]
    fn tables_created_in_the_input_are_never_named() {
        assert_eq!(
            locked(
                "CREATE TABLE notes (id INT); RENAME TABLE notes TO notes_old; \
                 TRUNCATE TABLE notes_old; DROP TABLE notes_old, orders"
            ),
            [vec![], vec![], vec![], vec!["orders"]]
        );
    }

    #[test]
    fn a_rename_names_its_old_names_but_not_one_it_gave_itself() {
        assert_eq!(
            locked("RENAME TABLE a TO tmp, b TO a, tmp TO b"),
            [vec!["a", "b"]]
        );
        assert_eq!(
            locked("CREATE TABLE n (id INT); RENAME TABLE n TO m, orders TO n"),
            [vec![], vec!["orders"]]
        );
    }

    #[test]
    fn the_tables_forms_name_their_tables_as_the_table_forms_do() {
        assert_eq!(
            locked(
                "DROP TABLES orders, users; drop tables if exists `a`; \
                 RENAME TABLES b TO c, d TO e"
            ),
            [vec!["orders", "users"], vec!["a"], vec!["b", "d"]]
        );
    }

    #[test]
    fn index_statements_kilit_cannot_read_name_their_table() {
        assert_eq!(
            locked(
                "CREATE FULLTEXT INDEX ft ON orders (note); \
                 /* first */ ALTER TABLE users CONVERT TO CHARACTER SET utf8mb4"
            ),
            [vec!["orders"], vec!["users"]]
        );
    }

    #[test]
    fn a_temporary_drop_names_nothing() {
        assert_eq!(
            locked("DROP TEMPORARY TABLE orders; DROP TEMPORARY TABLES IF EXISTS users"),
            [vec![], vec![]] as [Vec<String>; 2]
        );
    }

    #[test]
    fn lock_tables_and_truncate_forms_mysql_rejects_name_nothing() {
        assert_eq!(
            locked(
                "LOCK TABLES orders WRITE; LOCK TABLES orders READ; \
                 TRUNCATE TABLE a, b; TRUNCATE TABLE \"orders\"; SELECT * FROM orders; \
                 INSERT INTO orders VALUES (1)"
            ),
            [vec![], vec![], vec![], vec![], vec![], vec![]] as [Vec<String>; 6]
        );
    }
}
