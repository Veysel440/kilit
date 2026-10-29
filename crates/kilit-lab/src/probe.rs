//! Runs a scenario's variants on a server and reads what the server did.

use mysql_async::Conn;
use mysql_async::prelude::Queryable;
use serde::Serialize;

use crate::Result;
use crate::fixture::{self, DATABASE};
use crate::scenario::{Scenario, Variant};
use crate::verdict::{self, Verdict};

/// Everything the lab learned about one scenario on one server.
pub struct Measurement<'a> {
    pub scenario: &'a Scenario,
    /// One per variant, in `Variant::ALL` order.
    pub observations: Vec<Observation>,
    pub verdict: Verdict,
}

/// What happened when one variant ran.
#[derive(Debug, Serialize)]
pub struct Observation {
    pub variant: Variant,
    #[serde(flatten)]
    pub outcome: Outcome,
}

#[derive(Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "result", rename_all = "snake_case")]
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

/// Runs every variant of a scenario and decides the verdict.
pub async fn measure<'a>(conn: &mut Conn, scenario: &'a Scenario) -> Result<Measurement<'a>> {
    let mut observations = Vec::with_capacity(Variant::ALL.len());
    for variant in Variant::ALL {
        observations.push(run(conn, scenario, variant).await?);
    }
    let verdict = verdict::decide(&observations);
    Ok(Measurement {
        scenario,
        observations,
        verdict,
    })
}

/// Runs one variant on fresh tables and records what the server did.
async fn run(conn: &mut Conn, scenario: &Scenario, variant: Variant) -> Result<Observation> {
    // Settings such as foreign_key_checks belong to the session. Start every
    // run from the server's defaults, so no scenario leaks into the next one.
    if !conn.reset().await? {
        return Err("the server cannot reset a session (COM_RESET_CONNECTION)".into());
    }
    fixture::reset(conn).await?;
    for statement in &scenario.session {
        conn.query_drop(statement.as_str()).await?;
    }

    let before = table_state(conn).await?;
    let outcome = match conn.query_drop(scenario.statement(variant)).await {
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
            message: stable_message(err.code, err.message),
        },
        Err(err) => return Err(err.into()),
    };
    Ok(Observation { variant, outcome })
}

async fn table_state(conn: &mut Conn) -> Result<TableState> {
    let row: Option<(u64, u64)> = conn
        .query_first(format!(
            "SELECT TABLE_ID, TOTAL_ROW_VERSIONS
             FROM information_schema.INNODB_TABLES
             WHERE NAME = '{DATABASE}/orders'"
        ))
        .await?;
    let (id, row_versions) = row.ok_or("the orders table is missing from INNODB_TABLES")?;
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

/// Hides the parts of an error message that change from run to run, so a
/// result file only changes when the server's behavior does.
///
/// InnoDB builds an index with several threads, so which duplicate value it
/// meets first, and quotes in a duplicate-entry error, differs between runs.
fn stable_message(code: u16, message: String) -> String {
    const DUPLICATE_ENTRY: u16 = 1062;
    if code == DUPLICATE_ENTRY
        && let Some(rest) = message.strip_prefix("Duplicate entry '")
        && let Some(end) = rest.find("' for key ")
    {
        return format!("Duplicate entry '<value>{}", &rest[end..]);
    }
    message
}

#[cfg(test)]
mod tests {
    use super::{copied_rows, stable_message};

    #[test]
    fn duplicate_values_are_hidden() {
        assert_eq!(
            stable_message(
                1062,
                "Duplicate entry '8' for key 'orders.uq_orders_user'".to_owned()
            ),
            "Duplicate entry '<value>' for key 'orders.uq_orders_user'"
        );
    }

    #[test]
    fn other_messages_stay_as_they_are() {
        let message = "Invalid use of NULL value".to_owned();
        assert_eq!(stable_message(1138, message.clone()), message);
    }

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
