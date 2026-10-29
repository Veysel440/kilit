//! The tables every scenario starts from.

use mysql_async::Conn;
use mysql_async::prelude::Queryable;

use crate::Result;

/// The lab's own database. It is dropped and recreated before every run.
pub const DATABASE: &str = "kilit_lab";

/// Rows in `orders`, which is also what a full table copy reports.
///
/// The rows are generated with recursive CTEs, and MySQL stops those after
/// 1000 levels by default (`cte_max_recursion_depth`), so stay at or below 1000.
pub const ORDERS_ROWS: u32 = 1000;

/// Rows in `users`; every `orders.user_id` points at one of them.
const USERS_ROWS: u32 = 100;

/// Builds fresh `users` and `orders` tables, so every run starts the same way.
///
/// What scenarios can rely on (also listed in `scenarios/README.md`):
/// - `orders.user_id` repeats (1 to 100), so a UNIQUE index on it fails;
/// - `orders.email` is nullable, but filled in every row and unique;
/// - `orders.note` is NULL in every 10th row.
pub async fn reset(conn: &mut Conn) -> Result<()> {
    conn.query_drop(format!("DROP DATABASE IF EXISTS {DATABASE}"))
        .await?;
    conn.query_drop(format!("CREATE DATABASE {DATABASE}"))
        .await?;
    conn.query_drop(format!("USE {DATABASE}")).await?;
    conn.query_drop(
        "CREATE TABLE users (
             id   BIGINT UNSIGNED AUTO_INCREMENT PRIMARY KEY,
             name VARCHAR(100) NOT NULL
         ) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4",
    )
    .await?;
    conn.query_drop(
        "CREATE TABLE orders (
             id      BIGINT UNSIGNED AUTO_INCREMENT PRIMARY KEY,
             user_id BIGINT UNSIGNED NOT NULL,
             amount  INT NOT NULL,
             email   VARCHAR(255) NULL,
             note    VARCHAR(50) NULL
         ) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4",
    )
    .await?;
    conn.query_drop(format!(
        "INSERT INTO users (name)
         WITH RECURSIVE seq (n) AS (
             SELECT 1 UNION ALL SELECT n + 1 FROM seq WHERE n < {USERS_ROWS}
         )
         SELECT CONCAT('user ', n) FROM seq"
    ))
    .await?;
    conn.query_drop(format!(
        "INSERT INTO orders (user_id, amount, email, note)
         WITH RECURSIVE seq (n) AS (
             SELECT 1 UNION ALL SELECT n + 1 FROM seq WHERE n < {ORDERS_ROWS}
         )
         SELECT 1 + n % {USERS_ROWS}, n, CONCAT('customer', n, '@example.test'),
                IF(n % 10 = 0, NULL, CONCAT('note ', n))
         FROM seq"
    ))
    .await?;
    Ok(())
}

/// Removes the lab's database.
pub async fn drop(conn: &mut Conn) -> Result<()> {
    conn.query_drop(format!("DROP DATABASE IF EXISTS {DATABASE}"))
        .await?;
    Ok(())
}
