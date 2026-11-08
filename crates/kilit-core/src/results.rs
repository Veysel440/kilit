//! The lab's results: what kilit-lab measured on a server, in the shape it
//! writes to `matrix/*.json` and kilit reads back.
//!
//! The writer and the reader share these types, so the file format cannot
//! drift apart between them.

use std::fmt;

use serde::{Deserialize, Serialize};

use crate::statement::Algorithm;

/// The server a result file describes.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Server {
    /// `mysql` or `mariadb`.
    pub product: String,
    /// e.g. `8.4.13`
    pub version: String,
}

impl Server {
    /// Reads what `SELECT VERSION()` returns, e.g. `8.4.13` or
    /// `11.4.13-MariaDB-ubu2404`.
    pub fn from_version(raw: &str) -> Server {
        let product = if raw.contains("MariaDB") {
            "mariadb"
        } else {
            "mysql"
        };
        let version = raw.split('-').next().unwrap_or(raw);
        Server {
            product: product.to_owned(),
            version: version.to_owned(),
        }
    }

    /// The name used for result files and on the command line, e.g. `mysql-8.4.13`.
    pub fn name(&self) -> String {
        format!("{}-{}", self.product, self.version)
    }

    /// The release series, e.g. `mysql-8.4` for `mysql-8.4.13`.
    pub fn series(&self) -> String {
        let mut numbers = self.version.splitn(3, '.');
        match (numbers.next(), numbers.next()) {
            (Some(major), Some(minor)) => format!("{}-{major}.{minor}", self.product),
            _ => self.name(),
        }
    }
}

/// One way of running an ALTER statement: with the server's own choice, or
/// with `ALGORITHM=...` and `LOCK=NONE` forced.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Variant {
    /// `None` lets the server choose, which is what Laravel and most tools do.
    pub algorithm: Option<Algorithm>,
    /// Append `LOCK=NONE`: the server must keep the table writable or refuse.
    pub lock_none: bool,
}

impl Variant {
    /// Nothing forced: what a migration normally sends.
    pub const SERVER_DEFAULT: Variant = Variant {
        algorithm: None,
        lock_none: false,
    };

    /// Only `LOCK=NONE`: can the server do this while the table stays writable?
    pub const LOCK_NONE: Variant = Variant {
        algorithm: None,
        lock_none: true,
    };

    /// Only `ALGORITHM=...`.
    pub const fn forced(algorithm: Algorithm) -> Variant {
        Variant {
            algorithm: Some(algorithm),
            lock_none: false,
        }
    }

    /// Every variant the lab tries, in report order.
    pub const ALL: [Variant; 6] = [
        Variant::SERVER_DEFAULT,
        Variant::forced(Algorithm::Instant),
        Variant::forced(Algorithm::Inplace),
        Variant {
            algorithm: Some(Algorithm::Inplace),
            lock_none: true,
        },
        Variant::forced(Algorithm::Copy),
        Variant::LOCK_NONE,
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

    /// The text appended to an ALTER statement, e.g. `, ALGORITHM=INPLACE, LOCK=NONE`.
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

/// What happened when one variant ran.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Observation {
    pub variant: Variant,
    #[serde(flatten)]
    pub outcome: Outcome,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
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

/// What the server does with a scenario's statement.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "result", rename_all = "snake_case")]
pub enum Verdict {
    /// The statement runs.
    Runs {
        /// The algorithm the server uses. `None` if the lab could not tell,
        /// which should not happen.
        algorithm: Option<Algorithm>,
        rebuilds_table: bool,
        copied_rows: Option<u64>,
        writes: Writes,
        /// The lab's checks agreed with each other. If not, look at the
        /// details: the server did something we do not expect.
        consistent: bool,
    },
    /// The statement fails: the data does not allow it, or the server
    /// refuses an algorithm or lock the statement forces.
    Fails { code: u16, message: String },
}

/// A short summary: `COPY, copies every row, writes wait`, or
/// `fails with error 1062: ...`.
impl fmt::Display for Verdict {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Verdict::Runs {
                algorithm,
                rebuilds_table,
                copied_rows,
                writes,
                ..
            } => {
                match algorithm {
                    Some(algorithm) => write!(f, "{algorithm}")?,
                    None => f.write_str("unknown algorithm")?,
                }
                let table = if copied_rows.is_some_and(|rows| rows > 0) {
                    "copies every row"
                } else if *rebuilds_table {
                    "rebuilds the table"
                } else {
                    "no rebuild"
                };
                let writes = match writes {
                    Writes::Allowed => "writes continue",
                    Writes::Blocked => "writes wait",
                    Writes::Unknown => "writes not measured",
                };
                write!(f, ", {table}, {writes}")
            }
            Verdict::Fails { code, message } => write!(f, "fails with error {code}: {message}"),
        }
    }
}

/// Whether the table stays writable while the ALTER runs.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Writes {
    /// `LOCK=NONE` was accepted.
    Allowed,
    /// `LOCK=NONE` was refused as not possible: writes wait for the ALTER.
    Blocked,
    /// The lab could not tell.
    Unknown,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_mysql_versions() {
        let server = Server::from_version("8.4.13");
        assert_eq!(server.product, "mysql");
        assert_eq!(server.name(), "mysql-8.4.13");
    }

    #[test]
    fn reads_mariadb_versions() {
        let server = Server::from_version("11.4.13-MariaDB-ubu2404");
        assert_eq!(server.product, "mariadb");
        assert_eq!(server.name(), "mariadb-11.4.13");
    }

    #[test]
    fn server_default_adds_nothing() {
        assert_eq!(Variant::SERVER_DEFAULT.clause(), "");
        assert_eq!(Variant::SERVER_DEFAULT.to_string(), "(server default)");
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
        assert_eq!(Variant::LOCK_NONE.clause(), ", LOCK=NONE");
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
    fn verdicts_serialize_to_stable_json() {
        let verdict = Verdict::Runs {
            algorithm: Some(Algorithm::Instant),
            rebuilds_table: false,
            copied_rows: Some(0),
            writes: Writes::Allowed,
            consistent: true,
        };
        assert_eq!(
            serde_json::to_string(&verdict).unwrap(),
            r#"{"result":"runs","algorithm":"INSTANT","rebuilds_table":false,"copied_rows":0,"writes":"allowed","consistent":true}"#
        );
    }

    #[test]
    fn verdicts_print_as_a_short_summary() {
        let copy = Verdict::Runs {
            algorithm: Some(Algorithm::Copy),
            rebuilds_table: true,
            copied_rows: Some(1000),
            writes: Writes::Blocked,
            consistent: true,
        };
        assert_eq!(copy.to_string(), "COPY, copies every row, writes wait");

        let rebuild = Verdict::Runs {
            algorithm: Some(Algorithm::Inplace),
            rebuilds_table: true,
            copied_rows: Some(0),
            writes: Writes::Allowed,
            consistent: true,
        };
        assert_eq!(
            rebuild.to_string(),
            "INPLACE, rebuilds the table, writes continue"
        );

        let fails = Verdict::Fails {
            code: 1138,
            message: "Invalid use of NULL value".into(),
        };
        assert_eq!(
            fails.to_string(),
            "fails with error 1138: Invalid use of NULL value"
        );
    }

    #[test]
    fn observations_read_back_what_was_written() {
        let observations = [
            Observation {
                variant: Variant::forced(Algorithm::Copy),
                outcome: Outcome::Accepted {
                    copied_rows: Some(1000),
                    rebuilt: true,
                    row_versions: (0, 0),
                },
            },
            Observation {
                variant: Variant::LOCK_NONE,
                outcome: Outcome::Rejected {
                    code: 1846,
                    message: "LOCK=NONE is not supported.".into(),
                },
            },
        ];
        for observation in observations {
            let json = serde_json::to_string(&observation).unwrap();
            let read: Observation = serde_json::from_str(&json).unwrap();
            assert_eq!(read, observation, "{json}");
        }
    }
}
