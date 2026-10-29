//! Runs one scenario variant on a server and reads what the server did.

use mysql_async::Conn;
use mysql_async::prelude::Queryable;

use crate::Result;
use crate::scenario::{Scenario, Variant};

/// Rows in the `orders` table before each run.
///
/// The fixture generates them with a recursive CTE, and MySQL stops those
/// after 1000 levels by default (`cte_max_recursion_depth`), so stay at or
/// below 1000.
pub const FIXTURE_ROWS: u32 = 1000;

/// What happened when one variant ran.
pub struct Observation {
    pub variant: Variant,
    pub outcome: Outcome,
}

pub enum Outcome {
    /// The server ran the statement.
    Accepted {
        /// Rows copied into a new table, from the `Records: N` part of the
        /// server's reply. Only the COPY algorithm reports copied rows; an
        /// INPLACE rebuild rewrites the table but reports 0.
        copied_rows: Option<u64>,
        /// The table got a new internal id: InnoDB rebuilt it from scratch.
        rebuilt: bool,
        /// InnoDB's counter of instant ADD/DROP COLUMN changes, before and
        /// after. MySQL 8.0 and 8.4 allow 64, then rebuild the table on their own.
        row_versions: (u64, u64),
    },
    /// The server refused the statement, e.g. because the forced algorithm
    /// is not possible for this change. That is a result, not a lab failure.
    Rejected { code: u16, message: String },
}

/// The table as InnoDB sees it.
struct TableState {
    id: u64,
    row_versions: u64,
}

/// Recreates the fixture, runs the scenario's ALTER in the given variant and
/// records what the server did.
pub async fn run(conn: &mut Conn, scenario: &Scenario, variant: Variant) -> Result<Observation> {
    reset_fixture(conn).await?;
    let before = table_state(conn).await?;

    let statement = format!(
        "ALTER TABLE orders {}{}",
        scenario.operation,
        variant.clause()
    );
    let outcome = match conn.query_drop(statement).await {
        Ok(()) => {
            // Read the server's reply first: the next query replaces it.
            let copied_rows = copied_rows(&conn.info());
            let after = table_state(conn).await?;
            Outcome::Accepted {
                copied_rows,
                rebuilt: after.id != before.id,
                row_versions: (before.row_versions, after.row_versions),
            }
        }
        Err(mysql_async::Error::Server(err)) => Outcome::Rejected {
            code: err.code,
            message: err.message,
        },
        Err(err) => return Err(err.into()),
    };
    Ok(Observation { variant, outcome })
}

/// Removes the lab's database.
pub async fn clean_up(conn: &mut Conn) -> Result<()> {
    conn.query_drop("DROP DATABASE IF EXISTS kilit_lab").await?;
    Ok(())
}

/// Builds a fresh `kilit_lab.orders` table, so every run starts the same way.
async fn reset_fixture(conn: &mut Conn) -> Result<()> {
    conn.query_drop("DROP DATABASE IF EXISTS kilit_lab").await?;
    conn.query_drop("CREATE DATABASE kilit_lab").await?;
    conn.query_drop("USE kilit_lab").await?;
    conn.query_drop(
        "CREATE TABLE orders (
             id      BIGINT UNSIGNED AUTO_INCREMENT PRIMARY KEY,
             user_id BIGINT UNSIGNED NOT NULL,
             amount  INT NOT NULL,
             note    VARCHAR(50) NULL
         ) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4",
    )
    .await?;
    conn.query_drop(format!(
        "INSERT INTO orders (user_id, amount, note)
         WITH RECURSIVE seq (n) AS (
             SELECT 1 UNION ALL SELECT n + 1 FROM seq WHERE n < {FIXTURE_ROWS}
         )
         SELECT 1 + n % 100, n, IF(n % 10 = 0, NULL, CONCAT('note ', n)) FROM seq"
    ))
    .await?;
    Ok(())
}

async fn table_state(conn: &mut Conn) -> Result<TableState> {
    let row: Option<(u64, u64)> = conn
        .query_first(
            "SELECT TABLE_ID, TOTAL_ROW_VERSIONS
             FROM information_schema.INNODB_TABLES
             WHERE NAME = 'kilit_lab/orders'",
        )
        .await?;
    let (id, row_versions) = row.ok_or("kilit_lab/orders is missing from INNODB_TABLES")?;
    Ok(TableState { id, row_versions })
}

/// Reads N from the server's reply `Records: N  Duplicates: 0  Warnings: 0`.
fn copied_rows(info: &str) -> Option<u64> {
    info.strip_prefix("Records: ")?
        .split_whitespace()
        .next()?
        .parse()
        .ok()
}

#[cfg(test)]
mod tests {
    use super::copied_rows;

    #[test]
    fn reads_records_from_the_server_reply() {
        assert_eq!(
            copied_rows("Records: 1000  Duplicates: 0  Warnings: 0"),
            Some(1000)
        );
        assert_eq!(
            copied_rows("Records: 0  Duplicates: 0  Warnings: 0"),
            Some(0)
        );
    }

    #[test]
    fn other_replies_give_none() {
        assert_eq!(copied_rows(""), None);
        assert_eq!(
            copied_rows("Rows matched: 1  Changed: 1  Warnings: 0"),
            None
        );
        assert_eq!(copied_rows("Records: many"), None);
    }
}
