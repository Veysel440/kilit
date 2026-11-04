//! The metadata lock: what every ALTER TABLE waits for before it starts.
//!
//! An ALTER TABLE needs its table to itself for a moment, even when the
//! change is INSTANT. It waits until every open transaction that has used
//! the table ends, and while it waits, every new query on the table waits
//! behind it, reads too. kilit-lab checks all of this on every server it
//! measures, with one ALTER per algorithm (`crates/kilit-lab/src/facts.rs`).

use crate::statement::Algorithm;

/// The algorithms kilit-lab checks: an ALTER that runs with any of them
/// waits for an open transaction on its table.
pub const ALGORITHMS_THAT_WAIT: [Algorithm; 3] =
    [Algorithm::Instant, Algorithm::Inplace, Algorithm::Copy];

/// How long a statement waits for a metadata lock unless told otherwise:
/// MySQL's default `lock_wait_timeout`, in seconds. A year.
pub const DEFAULT_LOCK_WAIT_TIMEOUT: u64 = 31_536_000;

/// The error a statement gets when it has waited `lock_wait_timeout` seconds.
pub const LOCK_WAIT_TIMEOUT_ERROR: u16 = 1205;

/// What kilit says about the metadata lock, once for a migration that
/// alters tables that already exist.
pub fn metadata_lock_warning() -> String {
    format!(
        "Every ALTER TABLE first waits for the open transactions that have used \
         its table to end. While it waits, new queries on that table queue behind \
         it, reads too, even for an INSTANT change, and by default MySQL lets it \
         wait a year (lock_wait_timeout = {DEFAULT_LOCK_WAIT_TIMEOUT}). Run SET \
         SESSION lock_wait_timeout = 5 before the migration to cap the wait: the \
         ALTER then fails with error {LOCK_WAIT_TIMEOUT_ERROR} after 5 seconds, \
         the queries behind it go on, and you can run the migration again later."
    )
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
}
