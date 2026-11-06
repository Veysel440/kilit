//! What the lab measured, built into kilit when it is compiled.
//!
//! The result files in `matrix/` are the evidence behind every answer kilit
//! gives. They are compiled into the program, so kilit needs no files and
//! no database at run time.

use std::fmt;
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
#[derive(Clone, PartialEq, Eq, Deserialize)]
#[serde(from = "file::ServerResults")]
pub struct ServerResults {
    pub server: Server,
    scenarios: Vec<ScenarioResult>,
    /// Positions in `scenarios`, ordered by id and, for one id, by
    /// position, so that a lookup is a binary search.
    by_id: Vec<usize>,
}

mod file {
    use serde::Deserialize;

    use super::{ScenarioResult, Server};

    /// A result file as written: the public `ServerResults` without its
    /// index. Same name, so that errors read as before.
    #[derive(Deserialize)]
    pub(super) struct ServerResults {
        pub(super) server: Server,
        pub(super) scenarios: Vec<ScenarioResult>,
    }
}

impl From<file::ServerResults> for ServerResults {
    fn from(file: file::ServerResults) -> ServerResults {
        let mut by_id: Vec<usize> = (0..file.scenarios.len()).collect();
        // Stable: the same id keeps the order of the file.
        by_id.sort_by(|a, b| file.scenarios[*a].id.cmp(&file.scenarios[*b].id));
        ServerResults {
            server: file.server,
            scenarios: file.scenarios,
            by_id,
        }
    }
}

/// The index is left out: it only repeats `scenarios`.
impl fmt::Debug for ServerResults {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ServerResults")
            .field("server", &self.server)
            .field("scenarios", &self.scenarios)
            .finish()
    }
}

impl ServerResults {
    /// Reads a result file written by kilit-lab.
    pub fn from_json(json: &str) -> Result<ServerResults, String> {
        serde_json::from_str(json).map_err(|err| err.to_string())
    }

    pub fn scenarios(&self) -> &[ScenarioResult] {
        &self.scenarios
    }

    /// The scenario with this id; the first one in the file if the id is
    /// there twice.
    pub fn scenario(&self, id: &str) -> Option<&ScenarioResult> {
        let first = self
            .by_id
            .partition_point(|at| self.scenarios[*at].id.as_str() < id);
        let scenario = &self.scenarios[*self.by_id.get(first)?];
        (scenario.id == id).then_some(scenario)
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
    fn scenario_ids_are_unique_on_each_server() {
        for results in Knowledge::built_in().servers() {
            let mut ids: Vec<&str> = results.scenarios().iter().map(|s| s.id.as_str()).collect();
            ids.sort_unstable();
            for pair in ids.windows(2) {
                assert_ne!(pair[0], pair[1], "{}", results.server.name());
            }
        }
    }

    #[test]
    fn every_scenario_is_found_by_its_id() {
        for results in Knowledge::built_in().servers() {
            for scenario in results.scenarios() {
                assert_eq!(results.scenario(&scenario.id), Some(scenario));
            }
            assert_eq!(results.scenario(""), None);
            assert_eq!(results.scenario("add-colum"), None);
            assert_eq!(results.scenario("zzz"), None);
        }
    }

    #[test]
    fn a_repeated_id_finds_the_first_scenario_in_the_file() {
        let scenario = |id: &str, title: &str| {
            format!(
                r#"{{"id": "{id}", "title": "{title}", "statement": "ALTER TABLE orders DROP note",
                    "verdict": {{"result": "runs", "algorithm": "COPY", "rebuilds_table": true,
                                 "copied_rows": 0, "writes": "blocked", "consistent": true}},
                    "observations": []}}"#
            )
        };
        let json = format!(
            r#"{{"server": {{"product": "mysql", "version": "8.4.13"}},
                "scenarios": [{}, {}, {}]}}"#,
            scenario("b", "first"),
            scenario("a", "other"),
            scenario("b", "second"),
        );
        let results = ServerResults::from_json(&json).unwrap();
        assert_eq!(results.scenario("b").unwrap().title, "first");
        assert_eq!(results.scenario("a").unwrap().title, "other");
        assert_eq!(results.scenario("c"), None);
    }

    #[test]
    fn errors_and_debug_output_name_the_server_results() {
        let error = ServerResults::from_json("1").unwrap_err();
        assert_eq!(
            error,
            "invalid type: integer `1`, expected struct ServerResults at line 1 column 1"
        );
        let error = ServerResults::from_json("[]").unwrap_err();
        assert_eq!(
            error,
            "invalid length 0, expected struct ServerResults with 2 elements at line 1 column 2"
        );
        let results = ServerResults::from_json(
            r#"{"server": {"product": "mysql", "version": "8.4.13"}, "scenarios": []}"#,
        )
        .unwrap();
        assert_eq!(
            format!("{results:?}"),
            r#"ServerResults { server: Server { product: "mysql", version: "8.4.13" }, scenarios: [] }"#
        );
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
