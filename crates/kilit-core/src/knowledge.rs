//! What the lab measured, built into kilit when it is compiled.
//!
//! The result files in `matrix/` are the evidence behind every answer kilit
//! gives. They are compiled into the program, so kilit needs no files and
//! no database at run time.

use std::sync::OnceLock;

use serde::Deserialize;

use crate::results::{Observation, Outcome, Server, Variant, Verdict, Writes};
use crate::statement::Algorithm;

/// The result files, one per server version. Add a line when the lab
/// measures a new version; a test checks that none is missing.
const BUILT_IN: [&str; 2] = [
    include_str!("../../../matrix/mysql-8.0.46.json"),
    include_str!("../../../matrix/mysql-8.4.13.json"),
];

/// The lab's results for every server it measured.
#[derive(Debug)]
pub struct Knowledge {
    servers: Vec<ServerResults>,
}

impl Knowledge {
    /// The results built into this program.
    ///
    /// # Panics
    ///
    /// If a built-in file is not valid. The tests read every one of them,
    /// so that cannot reach a user.
    pub fn built_in() -> &'static Knowledge {
        static KNOWLEDGE: OnceLock<Knowledge> = OnceLock::new();
        KNOWLEDGE.get_or_init(|| Knowledge {
            servers: BUILT_IN
                .iter()
                .map(|json| ServerResults::from_json(json).expect("built-in lab results are valid"))
                .collect(),
        })
    }

    pub fn servers(&self) -> &[ServerResults] {
        &self.servers
    }

    /// The results for a server named like `mysql-8.4.13`.
    pub fn server(&self, name: &str) -> Option<&ServerResults> {
        self.servers
            .iter()
            .find(|results| results.server.name() == name)
    }
}

/// Everything the lab measured on one server.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
pub struct ServerResults {
    pub server: Server,
    scenarios: Vec<ScenarioResult>,
}

impl ServerResults {
    /// Reads a result file written by kilit-lab.
    pub fn from_json(json: &str) -> Result<ServerResults, String> {
        serde_json::from_str(json).map_err(|err| err.to_string())
    }

    pub fn scenarios(&self) -> &[ScenarioResult] {
        &self.scenarios
    }

    pub fn scenario(&self, id: &str) -> Option<&ScenarioResult> {
        self.scenarios.iter().find(|scenario| scenario.id == id)
    }
}

/// One scenario's results on one server.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
pub struct ScenarioResult {
    pub id: String,
    pub title: String,
    /// Statements that prepared the tables before the measurement.
    #[serde(default)]
    pub setup: Vec<String>,
    /// Statements run in the same session right before the ALTER.
    #[serde(default)]
    pub session: Vec<String>,
    /// `MyISAM` when the lab created `orders` with that engine; `None` for
    /// the InnoDB tables of `scenarios/tables.sql`.
    #[serde(default)]
    pub engine: Option<String>,
    /// The ALTER statement, as run without forcing anything.
    pub statement: String,
    /// What the server does with `statement`.
    pub verdict: Verdict,
    /// One per variant the lab ran.
    pub observations: Vec<Observation>,
}

/// The `ALGORITHM=...` / `LOCK=NONE` errors that mean "not possible for
/// this change", as opposed to a problem with the data.
const NOT_SUPPORTED: [u16; 2] = [1845, 1846];

impl ScenarioResult {
    /// What the server does with this scenario's statement run as
    /// `variant`, or `None` if the lab did not run that variant.
    ///
    /// For the server default this is the lab's verdict. For a forced
    /// variant it is built from what the lab saw for that variant: whether
    /// the server accepted it, rebuilt the table and copied rows. Whether
    /// writes continue is only known when the lab saw it, never guessed.
    pub fn verdict_for(&self, variant: Variant) -> Option<Verdict> {
        if variant == Variant::SERVER_DEFAULT {
            return Some(self.verdict.clone());
        }
        let outcome = self.outcome(variant)?;
        let (copied_rows, rebuilt) = match outcome {
            Outcome::Rejected { code, message } => {
                return Some(Verdict::Fails {
                    code: *code,
                    message: message.clone(),
                });
            }
            Outcome::Accepted {
                copied_rows,
                rebuilt,
                ..
            } => (*copied_rows, *rebuilt),
        };
        let Verdict::Runs {
            algorithm: default_algorithm,
            writes: default_writes,
            ..
        } = &self.verdict
        else {
            // The server default failed but a forced variant ran: the lab
            // has nothing to compare it with.
            return None;
        };

        let algorithm = variant.algorithm.or(*default_algorithm);
        let writes = if variant.lock_none {
            // The server accepted LOCK=NONE: it keeps the table writable.
            Writes::Allowed
        } else if algorithm == *default_algorithm {
            // Forcing what the server picks anyway changes nothing.
            *default_writes
        } else if algorithm == Some(Algorithm::Inplace) {
            self.writes_with(Variant {
                algorithm: Some(Algorithm::Inplace),
                lock_none: true,
            })
        } else {
            Writes::Unknown
        };
        Some(Verdict::Runs {
            algorithm,
            rebuilds_table: rebuilt,
            copied_rows,
            writes,
            consistent: true,
        })
    }

    fn outcome(&self, variant: Variant) -> Option<&Outcome> {
        self.observations
            .iter()
            .find(|observation| observation.variant == variant)
            .map(|observation| &observation.outcome)
    }

    /// Whether writes continue, judged by a variant that asks for `LOCK=NONE`.
    fn writes_with(&self, lock_none_variant: Variant) -> Writes {
        match self.outcome(lock_none_variant) {
            Some(Outcome::Accepted { .. }) => Writes::Allowed,
            Some(Outcome::Rejected { code, .. }) if NOT_SUPPORTED.contains(code) => Writes::Blocked,
            _ => Writes::Unknown,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_built_in_file_is_valid() {
        let knowledge = Knowledge::built_in();
        assert_eq!(knowledge.servers().len(), BUILT_IN.len());
        for results in knowledge.servers() {
            assert!(!results.scenarios().is_empty(), "{}", results.server.name());
        }
    }

    #[test]
    fn finds_servers_by_name() {
        let knowledge = Knowledge::built_in();
        assert!(knowledge.server("mysql-8.4.13").is_some());
        assert!(knowledge.server("mysql-8.4").is_none());
    }

    fn results(id: &str) -> ScenarioResult {
        Knowledge::built_in()
            .server("mysql-8.4.13")
            .unwrap()
            .scenario(id)
            .unwrap_or_else(|| panic!("no scenario {id}"))
            .clone()
    }

    #[test]
    fn the_server_default_is_the_verdict() {
        let scenario = results("int-to-bigint");
        assert_eq!(
            scenario.verdict_for(Variant::SERVER_DEFAULT),
            Some(scenario.verdict.clone())
        );
    }

    #[test]
    fn a_refused_variant_fails() {
        // A type change cannot be done in place.
        let verdict = results("int-to-bigint").verdict_for(Variant::forced(Algorithm::Inplace));
        assert!(matches!(verdict, Some(Verdict::Fails { code: 1846, .. })));
    }

    #[test]
    fn forcing_inplace_on_an_instant_change_rebuilds_the_table() {
        let verdict =
            results("add-nullable-column").verdict_for(Variant::forced(Algorithm::Inplace));
        assert_eq!(
            verdict,
            Some(Verdict::Runs {
                algorithm: Some(Algorithm::Inplace),
                rebuilds_table: true,
                copied_rows: Some(0),
                // From the INPLACE + LOCK=NONE run, which was accepted.
                writes: Writes::Allowed,
                consistent: true,
            })
        );
    }

    #[test]
    fn writes_are_not_guessed_for_a_forced_copy() {
        // The lab never ran COPY with LOCK=NONE on a column add.
        let verdict = results("add-nullable-column").verdict_for(Variant::forced(Algorithm::Copy));
        assert!(matches!(
            verdict,
            Some(Verdict::Runs {
                algorithm: Some(Algorithm::Copy),
                writes: Writes::Unknown,
                ..
            })
        ));
    }

    #[test]
    fn variants_the_lab_did_not_run_have_no_verdict() {
        let instant_lock_none = Variant {
            algorithm: Some(Algorithm::Instant),
            lock_none: true,
        };
        assert_eq!(results("add-index").verdict_for(instant_lock_none), None);
    }
}
