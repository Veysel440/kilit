//! What the lab runs: scenarios, and the variants each one is tried with.

use std::fmt;

/// One schema change to measure.
///
/// For now scenarios are written in code. Later they will be read from files
/// under `scenarios/`, so that adding one needs no Rust change.
pub struct Scenario {
    /// Stable name, used in reports (and later in result files).
    pub id: &'static str,
    pub title: &'static str,
    /// Everything that follows `ALTER TABLE orders`.
    pub operation: &'static str,
}

pub const SCENARIOS: &[Scenario] = &[
    Scenario {
        id: "add-nullable-column",
        title: "Add a nullable column at the end of the table",
        operation: "ADD COLUMN coupon VARCHAR(255) NULL",
    },
    Scenario {
        id: "int-to-bigint",
        title: "Change a column type from INT to BIGINT",
        operation: "MODIFY COLUMN amount BIGINT NOT NULL",
    },
    Scenario {
        id: "varchar-50-to-60",
        title: "Widen a VARCHAR(50) column to VARCHAR(60)",
        operation: "MODIFY COLUMN note VARCHAR(60) NULL",
    },
    Scenario {
        id: "varchar-50-to-100",
        title: "Widen a VARCHAR(50) column to VARCHAR(100)",
        operation: "MODIFY COLUMN note VARCHAR(100) NULL",
    },
];

/// An algorithm forced with `ALGORITHM=...`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Algorithm {
    /// Change only the table's metadata; rows are not touched.
    Instant,
    /// Work on the existing table; may still rebuild it, usually without blocking writes.
    Inplace,
    /// Create a new table and copy every row into it; writes wait meanwhile.
    Copy,
}

impl fmt::Display for Algorithm {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Algorithm::Instant => "INSTANT",
            Algorithm::Inplace => "INPLACE",
            Algorithm::Copy => "COPY",
        })
    }
}

/// One way of running a scenario's ALTER statement.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Variant {
    /// `None` lets the server choose, which is what Laravel and most tools do.
    pub algorithm: Option<Algorithm>,
    /// Append `LOCK=NONE`: the server must keep the table writable or refuse.
    pub lock_none: bool,
}

impl Variant {
    /// Every variant the lab tries, in report order.
    pub const ALL: [Variant; 6] = [
        Variant {
            algorithm: None,
            lock_none: false,
        },
        Variant {
            algorithm: Some(Algorithm::Instant),
            lock_none: false,
        },
        Variant {
            algorithm: Some(Algorithm::Inplace),
            lock_none: false,
        },
        Variant {
            algorithm: Some(Algorithm::Inplace),
            lock_none: true,
        },
        Variant {
            algorithm: Some(Algorithm::Copy),
            lock_none: false,
        },
        Variant {
            algorithm: None,
            lock_none: true,
        },
    ];

    /// The options this variant adds, e.g. `["ALGORITHM=INPLACE", "LOCK=NONE"]`.
    fn options(self) -> Vec<String> {
        let mut options = Vec::new();
        if let Some(algorithm) = self.algorithm {
            options.push(format!("ALGORITHM={algorithm}"));
        }
        if self.lock_none {
            options.push("LOCK=NONE".to_owned());
        }
        options
    }

    /// The text appended to the ALTER statement, e.g. `, ALGORITHM=INPLACE, LOCK=NONE`.
    pub fn clause(self) -> String {
        self.options()
            .iter()
            .map(|option| format!(", {option}"))
            .collect()
    }
}

impl fmt::Display for Variant {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let options = self.options();
        if options.is_empty() {
            f.write_str("(server default)")
        } else {
            f.write_str(&options.join(", "))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn server_default_adds_nothing() {
        let variant = Variant {
            algorithm: None,
            lock_none: false,
        };
        assert_eq!(variant.clause(), "");
        assert_eq!(variant.to_string(), "(server default)");
    }

    #[test]
    fn algorithm_comes_before_lock() {
        let variant = Variant {
            algorithm: Some(Algorithm::Inplace),
            lock_none: true,
        };
        assert_eq!(variant.clause(), ", ALGORITHM=INPLACE, LOCK=NONE");
        assert_eq!(variant.to_string(), "ALGORITHM=INPLACE, LOCK=NONE");
    }

    #[test]
    fn lock_can_be_asked_without_an_algorithm() {
        let variant = Variant {
            algorithm: None,
            lock_none: true,
        };
        assert_eq!(variant.clause(), ", LOCK=NONE");
    }

    #[test]
    fn every_variant_is_tried_once() {
        for (i, a) in Variant::ALL.iter().enumerate() {
            for b in &Variant::ALL[i + 1..] {
                assert_ne!(a, b);
            }
        }
    }

    #[test]
    fn scenario_ids_are_unique() {
        for (i, a) in SCENARIOS.iter().enumerate() {
            for b in &SCENARIOS[i + 1..] {
                assert_ne!(a.id, b.id, "scenario id {} is used twice", a.id);
            }
        }
    }
}
