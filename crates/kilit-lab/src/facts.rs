//! Facts kilit-core relies on without a scenario of their own, checked
//! against every server the lab measures.

use kilit_core::CHARSETS;
use mysql_async::Conn;
use mysql_async::prelude::Queryable;

use crate::Result;

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
