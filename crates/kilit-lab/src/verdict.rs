//! Turns the six observations of a scenario into one verdict: what the
//! server does when a migration does not force anything.
//!
//! The rule comes from MySQL itself: without an `ALGORITHM` clause the
//! server uses the cheapest algorithm the change supports, INSTANT first,
//! then INPLACE, then COPY. The lab forces each one, so the first one the
//! server accepted is the default. That is checked, not assumed: the
//! default run must leave exactly the same traces as the forced run of
//! that algorithm, or the verdict is marked inconsistent.

use kilit_core::{Algorithm, Observation, Outcome, Variant, Verdict, Writes};

/// The order in which MySQL tries algorithms when none is forced.
const CHEAPEST_FIRST: [Algorithm; 3] = [Algorithm::Instant, Algorithm::Inplace, Algorithm::Copy];

/// MySQL's "this algorithm or lock is not possible for this change" errors:
/// ER_ALTER_OPERATION_NOT_SUPPORTED and ER_ALTER_OPERATION_NOT_SUPPORTED_REASON.
const NOT_SUPPORTED: [u16; 2] = [1845, 1846];

/// Decides the verdict from the observations of one scenario.
///
/// # Panics
///
/// If there is no server-default observation. The lab always runs every
/// variant in `Variant::ALL`, so that would be a bug in the lab.
pub fn decide(observations: &[Observation]) -> Verdict {
    let outcome = |variant: Variant| {
        observations
            .iter()
            .find(|observation| observation.variant == variant)
            .map(|observation| &observation.outcome)
    };
    let default =
        outcome(Variant::SERVER_DEFAULT).expect("the lab always runs the server-default variant");

    match default {
        Outcome::Rejected { code, message } => Verdict::Fails {
            code: *code,
            message: message.clone(),
        },
        Outcome::Accepted {
            copied_rows,
            rebuilt,
            ..
        } => {
            let algorithm = CHEAPEST_FIRST.into_iter().find(|&algorithm| {
                matches!(
                    outcome(Variant::forced(algorithm)),
                    Some(Outcome::Accepted { .. })
                )
            });
            let consistent = algorithm
                .is_some_and(|algorithm| outcome(Variant::forced(algorithm)) == Some(default));
            Verdict::Runs {
                algorithm,
                rebuilds_table: *rebuilt,
                copied_rows: *copied_rows,
                writes: writes(outcome(Variant::LOCK_NONE)),
                consistent,
            }
        }
    }
}

fn writes(lock_none: Option<&Outcome>) -> Writes {
    match lock_none {
        Some(Outcome::Accepted { .. }) => Writes::Allowed,
        Some(Outcome::Rejected { code, .. }) if NOT_SUPPORTED.contains(code) => Writes::Blocked,
        _ => Writes::Unknown,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn accepted(copied_rows: u64, rebuilt: bool, row_versions: (u64, u64)) -> Outcome {
        Outcome::Accepted {
            copied_rows: Some(copied_rows),
            rebuilt,
            row_versions,
        }
    }

    fn rejected(code: u16) -> Outcome {
        Outcome::Rejected {
            code,
            message: format!("error {code}"),
        }
    }

    /// Pairs outcomes with variants in `Variant::ALL` order: server default,
    /// INSTANT, INPLACE, INPLACE + LOCK=NONE, COPY, LOCK=NONE.
    fn observations(outcomes: [Outcome; 6]) -> Vec<Observation> {
        Variant::ALL
            .into_iter()
            .zip(outcomes)
            .map(|(variant, outcome)| Observation { variant, outcome })
            .collect()
    }

    #[test]
    fn instant_column_add() {
        let verdict = decide(&observations([
            accepted(0, false, (0, 1)),
            accepted(0, false, (0, 1)),
            accepted(0, true, (0, 0)),
            accepted(0, true, (0, 0)),
            accepted(1000, true, (0, 0)),
            accepted(0, false, (0, 1)),
        ]));
        assert_eq!(
            verdict,
            Verdict::Runs {
                algorithm: Some(Algorithm::Instant),
                rebuilds_table: false,
                copied_rows: Some(0),
                writes: Writes::Allowed,
                consistent: true,
            }
        );
    }

    #[test]
    fn type_change_copies_and_blocks_writes() {
        let verdict = decide(&observations([
            accepted(1000, true, (0, 0)),
            rejected(1846),
            rejected(1846),
            rejected(1846),
            accepted(1000, true, (0, 0)),
            rejected(1846),
        ]));
        assert_eq!(
            verdict,
            Verdict::Runs {
                algorithm: Some(Algorithm::Copy),
                rebuilds_table: true,
                copied_rows: Some(1000),
                writes: Writes::Blocked,
                consistent: true,
            }
        );
    }

    #[test]
    fn inplace_without_rebuild() {
        let verdict = decide(&observations([
            accepted(0, false, (0, 0)),
            rejected(1845),
            accepted(0, false, (0, 0)),
            accepted(0, false, (0, 0)),
            accepted(1000, true, (0, 0)),
            accepted(0, false, (0, 0)),
        ]));
        assert!(matches!(
            verdict,
            Verdict::Runs {
                algorithm: Some(Algorithm::Inplace),
                rebuilds_table: false,
                writes: Writes::Allowed,
                consistent: true,
                ..
            }
        ));
    }

    #[test]
    fn data_errors_fail_the_scenario() {
        let verdict = decide(&observations([
            rejected(1138),
            rejected(1138),
            rejected(1138),
            rejected(1138),
            rejected(1265),
            rejected(1138),
        ]));
        assert!(matches!(verdict, Verdict::Fails { code: 1138, .. }));
    }

    #[test]
    fn a_default_run_unlike_the_chosen_algorithm_is_inconsistent() {
        // INSTANT was accepted, yet the default run rebuilt the table.
        let verdict = decide(&observations([
            accepted(0, true, (0, 0)),
            accepted(0, false, (0, 1)),
            accepted(0, true, (0, 0)),
            accepted(0, true, (0, 0)),
            accepted(1000, true, (0, 0)),
            accepted(0, false, (0, 1)),
        ]));
        assert!(matches!(
            verdict,
            Verdict::Runs {
                consistent: false,
                ..
            }
        ));
    }

    #[test]
    fn lock_none_failing_for_another_reason_is_unknown() {
        assert_eq!(writes(Some(&rejected(1062))), Writes::Unknown);
        assert_eq!(writes(Some(&rejected(1846))), Writes::Blocked);
    }
}
