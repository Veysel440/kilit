//! Facts kilit-core relies on without a scenario of their own, checked
//! against every server the lab measures.

use std::time::Duration;

use kilit_core::{
    ALGORITHMS_THAT_WAIT, Algorithm, CHARSETS, DEFAULT_LOCK_WAIT_TIMEOUT, LOCK_WAIT_TIMEOUT_ERROR,
    STATEMENTS_THAT_WAIT, StatementThatWaits,
};
use mysql_async::prelude::Queryable;
use mysql_async::{Conn, Opts};

use crate::Result;
use crate::fixture;

/// Checks kilit-core's table of character sets: how many bytes a character
/// can take, and which collation a column gets by default. kilit uses both
/// to tell whether a longer VARCHAR crosses 255 bytes.
pub async fn check_charsets(conn: &mut Conn) -> Result<()> {
    for charset in CHARSETS {
        let row: Option<(u64, String)> = conn
            .exec_first(
                "SELECT MAXLEN, DEFAULT_COLLATE_NAME
                 FROM information_schema.CHARACTER_SETS
                 WHERE CHARACTER_SET_NAME = ?",
                (charset.name,),
            )
            .await?;
        let Some((max_bytes, collation)) = row else {
            return Err(format!("the server has no character set {}", charset.name).into());
        };
        if max_bytes != charset.max_bytes || collation != charset.default_collation {
            return Err(format!(
                "kilit-core says {} takes up to {} bytes per character and defaults to {}, \
                 but the server says {max_bytes} bytes and {collation}: \
                 correct CHARSETS in crates/kilit-core/src/charset.rs",
                charset.name, charset.max_bytes, charset.default_collation
            )
            .into());
        }
    }
    Ok(())
}

/// How long the probed statement may wait in the check, in seconds. Long
/// enough for the read behind it to give up first.
const STATEMENT_WAIT: u64 = 3;
/// How long the read behind the waiting statement may wait, in seconds.
const READ_WAIT: u64 = 1;

/// Checks what kilit-core says about the metadata lock
/// (`crates/kilit-core/src/metadata_lock.rs`), with one ALTER per algorithm
/// and one statement of each kind in `STATEMENTS_THAT_WAIT`:
///
/// 1. the statement waits for a transaction that has read the table and is
///    still open, and gives up with error 1205 after `lock_wait_timeout`
///    seconds;
/// 2. while it waits, a plain read of the table waits behind it;
/// 3. once it has given up, reads go on;
///
/// and that `lock_wait_timeout` is a year by default.
pub async fn check_metadata_lock(conn: &mut Conn, opts: &Opts) -> Result<()> {
    let default: Option<u64> = conn
        .query_first("SELECT @@GLOBAL.lock_wait_timeout")
        .await?;
    if default != Some(DEFAULT_LOCK_WAIT_TIMEOUT) {
        return Err(format!(
            "kilit-core says lock_wait_timeout is {DEFAULT_LOCK_WAIT_TIMEOUT} by default, \
             but the server has {default:?}: correct crates/kilit-core/src/metadata_lock.rs"
        )
        .into());
    }
    for algorithm in ALGORITHMS_THAT_WAIT {
        fixture::reset(conn, None, "").await?;
        let alter = probe(algorithm);
        check_waits(conn, opts, alter)
            .await
            .map_err(|err| format!("`{alter}`: {err}"))?;
    }
    for kind in STATEMENTS_THAT_WAIT {
        fixture::reset(conn, None, "").await?;
        let (setup, statement) = statement_probe(kind);
        if let Some(setup) = setup {
            conn.query_drop(setup).await?;
        }
        check_waits(conn, opts, statement)
            .await
            .map_err(|err| format!("`{statement}`: {err}"))?;
    }
    Ok(())
}

/// An ALTER on the lab's `orders` table that runs with `algorithm`.
fn probe(algorithm: Algorithm) -> &'static str {
    match algorithm {
        Algorithm::Instant => "ALTER TABLE orders ADD COLUMN probe INT, ALGORITHM=INSTANT",
        Algorithm::Inplace => "ALTER TABLE orders ADD INDEX probe (amount), ALGORITHM=INPLACE",
        Algorithm::Copy => "ALTER TABLE orders ADD COLUMN probe INT, ALGORITHM=COPY",
    }
}

/// A statement of `kind` on the lab's `orders` table, after what it needs
/// first: the fixture has no index on `amount` to drop.
fn statement_probe(kind: StatementThatWaits) -> (Option<&'static str>, &'static str) {
    match kind {
        StatementThatWaits::CreateIndex => (None, "CREATE INDEX probe ON orders (amount)"),
        StatementThatWaits::DropIndex => (
            Some("ALTER TABLE orders ADD INDEX probe (amount)"),
            "DROP INDEX probe ON orders",
        ),
        StatementThatWaits::DropTable => (None, "DROP TABLE orders"),
        StatementThatWaits::RenameTable => (None, "RENAME TABLE orders TO probe"),
        StatementThatWaits::TruncateTable => (None, "TRUNCATE TABLE orders"),
    }
}

async fn check_waits(conn: &mut Conn, opts: &Opts, statement: &str) -> Result<()> {
    // A transaction that has read the table and stays open, like a request
    // that is still running or a worker that forgot to commit.
    let mut holder = session(opts).await?;
    holder.query_drop("START TRANSACTION").await?;
    holder.query_drop("SELECT id FROM orders LIMIT 1").await?;

    let mut waiting = session(opts).await?;
    waiting
        .query_drop(format!("SET SESSION lock_wait_timeout = {STATEMENT_WAIT}"))
        .await?;
    let waiting_id = waiting.id();
    let mut reader = session(opts).await?;
    reader
        .query_drop(format!("SET SESSION lock_wait_timeout = {READ_WAIT}"))
        .await?;

    let (ran, queued) = tokio::join!(waiting.query_drop(statement), async {
        wait_for_metadata_lock(conn, waiting_id).await?;
        Ok::<_, Box<dyn std::error::Error>>(
            reader.query_drop("SELECT id FROM orders LIMIT 1").await,
        )
    });
    // The statement's own result first: if it did not wait, that is the news.
    gave_up(ran, "the statement")?;
    gave_up(queued?, "a read behind the waiting statement")?;
    reader
        .query_drop("SELECT id FROM orders LIMIT 1")
        .await
        .map_err(|err| format!("a read after the statement gave up failed: {err}"))?;

    holder.query_drop("ROLLBACK").await?;
    for session in [holder, waiting, reader] {
        session.disconnect().await?;
    }
    Ok(())
}

/// A new connection to the lab's database.
async fn session(opts: &Opts) -> Result<Conn> {
    let mut conn = Conn::new(opts.clone()).await?;
    conn.query_drop(format!("USE {}", fixture::DATABASE))
        .await?;
    Ok(conn)
}

/// Returns once the connection `id` waits for a metadata lock, or fails
/// after two seconds. The probed statements give up after STATEMENT_WAIT
/// seconds.
async fn wait_for_metadata_lock(conn: &mut Conn, id: u32) -> Result<()> {
    for _ in 0..100 {
        let state: Option<Option<String>> = conn
            .exec_first(
                "SELECT PROCESSLIST_STATE FROM performance_schema.threads
                 WHERE PROCESSLIST_ID = ?",
                (id,),
            )
            .await?;
        if state.flatten().as_deref() == Some("Waiting for table metadata lock") {
            return Ok(());
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    Err("the statement was not seen waiting for the metadata lock within 2 seconds".into())
}

/// Checks that `what` waited until `lock_wait_timeout` and gave up.
fn gave_up(result: mysql_async::Result<()>, what: &str) -> Result<()> {
    match result {
        Err(mysql_async::Error::Server(err)) if err.code == LOCK_WAIT_TIMEOUT_ERROR => Ok(()),
        Err(err) => Err(format!(
            "{what} should wait, then give up with error {LOCK_WAIT_TIMEOUT_ERROR}, \
             but failed with: {err}"
        )
        .into()),
        Ok(()) => Err(format!(
            "{what} should wait, then give up with error {LOCK_WAIT_TIMEOUT_ERROR}, \
             but it ran: correct crates/kilit-core/src/metadata_lock.rs"
        )
        .into()),
    }
}
