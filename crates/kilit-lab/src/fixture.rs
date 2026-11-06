//! The tables every scenario starts from.

use kilit_core::StatementKind;
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

/// The tables' definitions. kilit-core's tests read the same file, so they
/// judge every scenario on exactly the tables the lab measured it on.
const TABLES: &str = include_str!("../../../scenarios/tables.sql");

/// The statements that create the tables, with `orders` of `engine` when
/// one is given. kilit-core's tests make the same one change to the same
/// file: `ENGINE=InnoDB` becomes `ENGINE=<engine>` in `orders`' statement.
pub fn tables(engine: Option<&str>) -> Result<Vec<String>> {
    let mut statements = Vec::new();
    for statement in kilit_core::parse(TABLES) {
        let orders = matches!(
            &statement.kind,
            StatementKind::CreateTable { table, .. } if table.name == "orders"
        );
        match engine {
            Some(engine) if orders => {
                let sql = statement
                    .sql
                    .replacen("ENGINE=InnoDB", &format!("ENGINE={engine}"), 1);
                if sql == statement.sql {
                    return Err("orders in scenarios/tables.sql writes no ENGINE=InnoDB".into());
                }
                statements.push(sql);
            }
            _ => statements.push(statement.sql),
        }
    }
    Ok(statements)
}

/// Builds fresh `users` and `orders` tables, so every run starts the same
/// way; `orders` of `engine` when one is given, else InnoDB.
///
/// What scenarios can rely on (also listed in `scenarios/README.md`):
/// - `orders.user_id` repeats (1 to 100), so a UNIQUE index on it fails;
/// - `orders.email` is nullable, but filled in every row and unique;
/// - `orders.note` is NULL in every 10th row.
pub async fn reset(conn: &mut Conn, engine: Option<&str>) -> Result<()> {
    let tables = tables(engine)?;
    conn.query_drop(format!("DROP DATABASE IF EXISTS {DATABASE}"))
        .await?;
    conn.query_drop(format!("CREATE DATABASE {DATABASE}"))
        .await?;
    conn.query_drop(format!("USE {DATABASE}")).await?;
    for statement in tables {
        conn.query_drop(statement).await?;
    }
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

#[cfg(test)]
mod tests {
    use kilit_core::{Schema, TableState};

    use super::*;

    #[test]
    fn the_tables_file_creates_users_and_orders() {
        let schema = Schema::from_sql(TABLES);
        let names: Vec<&str> = schema.tables().map(|table| table.name.as_str()).collect();
        assert_eq!(names, ["orders", "users"]);
        let TableState::Known(orders) = schema.table("orders") else {
            panic!("orders is not known");
        };
        assert_eq!(orders.primary_key, ["id"]);
        assert_eq!(orders.charset.as_deref(), Some("utf8mb4"));
    }

    #[test]
    fn a_myisam_scenario_changes_only_the_engine_of_orders() {
        use kilit_core::Engine;

        let innodb = Schema::from_sql(&tables(None).unwrap().join(";\n"));
        let myisam = Schema::from_sql(&tables(Some("MyISAM")).unwrap().join(";\n"));
        let (TableState::Known(before), TableState::Known(after)) =
            (innodb.table("orders"), myisam.table("orders"))
        else {
            panic!("orders is not known");
        };
        assert_eq!(before.engine, Some(Engine::InnoDb));
        assert_eq!(after.engine, Some(Engine::MyIsam));
        assert_eq!(
            kilit_core::Table {
                engine: None,
                ..before.clone()
            },
            kilit_core::Table {
                engine: None,
                ..after.clone()
            }
        );
        let TableState::Known(users) = myisam.table("users") else {
            panic!("users is not known");
        };
        assert_eq!(users.engine, Some(Engine::InnoDb));
    }
}
