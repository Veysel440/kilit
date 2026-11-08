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

/// The server kilit checks against when none is named: the newest release
/// the lab measured in this series, MySQL's 8.4 LTS. Measuring another
/// series does not move it; moving it is a decision of its own.
pub const DEFAULT_SERIES: &str = "mysql-8.4";

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
        KNOWLEDGE.get_or_init(|| {
            let mut servers: Vec<ServerResults> = BUILT_IN
                .iter()
                .map(|json| ServerResults::from_json(json).expect("built-in lab results are valid"))
                .collect();
            compare(&mut servers);
            Knowledge { servers }
        })
    }

    pub fn servers(&self) -> &[ServerResults] {
        &self.servers
    }

    /// The results for a server named like `mysql-8.4.13`, or for the
    /// newest release the lab measured in a series named like `mysql-8.4`.
    pub fn server(&self, name: &str) -> Option<&ServerResults> {
        if let Some(results) = self
            .servers
            .iter()
            .find(|results| results.server.name() == name)
        {
            return Some(results);
        }
        self.servers
            .iter()
            .filter(|results| results.server.series() == name)
            .max_by_key(|results| release(&results.server.version))
    }

    /// The results for [`DEFAULT_SERIES`].
    pub fn default_server(&self) -> Option<&ServerResults> {
        self.server(DEFAULT_SERIES)
    }
}

/// The numbers of a version like `8.4.13`, to order releases of a series.
fn release(version: &str) -> Vec<u32> {
    version
        .split('.')
        .map(|number| number.parse().unwrap_or(0))
        .collect()
}

/// Marks, on each server, the scenarios whose results differ on another
/// one: their verdict, or what the lab saw in any variant, which is every
/// field an answer shows. A scenario that another server lacks differs too.
fn compare(servers: &mut [ServerResults]) {
    let varies: Vec<Vec<bool>> = servers
        .iter()
        .map(|results| {
            results
                .scenarios
                .iter()
                .map(|scenario| {
                    servers
                        .iter()
                        .any(|other| match other.scenario(&scenario.id) {
                            Some(theirs) => {
                                theirs.verdict != scenario.verdict
                                    || theirs.observations != scenario.observations
                            }
                            None => true,
                        })
                })
                .collect()
        })
        .collect();
    for (results, varies) in servers.iter_mut().zip(varies) {
        results.varies = varies;
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
    /// For each of `scenarios`, whether another server the lab measured
    /// gives other results for it. `true` until compared: results read
    /// alone may differ anywhere.
    varies: Vec<bool>,
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
            varies: vec![true; file.scenarios.len()],
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
        self.position(id).map(|at| &self.scenarios[at])
    }

    /// Whether another server the lab measured gives other results for the
    /// scenario with this id, in any variant, or lacks it. Also `true` for
    /// an id this server lacks, and for results not compared with any other
    /// server's ([`ServerResults::from_json`] alone).
    pub fn varies(&self, id: &str) -> bool {
        self.position(id).is_none_or(|at| self.varies[at])
    }

    fn position(&self, id: &str) -> Option<usize> {
        let first = self
            .by_id
            .partition_point(|at| self.scenarios[*at].id.as_str() < id);
        let at = *self.by_id.get(first)?;
        (self.scenarios[at].id == id).then_some(at)
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
    /// `COMPRESSED`, `COMPACT` or `REDUNDANT` when the lab created `orders`
    /// with that `ROW_FORMAT`; `None` for the server's default, DYNAMIC.
    #[serde(default)]
    pub row_format: Option<String>,
    /// The `KEY_BLOCK_SIZE` the lab created `orders` with, if any.
    #[serde(default)]
    pub key_block_size: Option<u8>,
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
    fn finds_servers_by_name_and_by_series() {
        let knowledge = Knowledge::built_in();
        let name = |name: &str| knowledge.server(name).map(|results| results.server.name());
        assert_eq!(name("mysql-8.4.13").as_deref(), Some("mysql-8.4.13"));
        assert_eq!(name("mysql-8.4").as_deref(), Some("mysql-8.4.13"));
        assert_eq!(name("mysql-8.0").as_deref(), Some("mysql-8.0.46"));
        for unknown in [
            "mysql-8",
            "mysql-8.4.",
            "8.4",
            "mysql-8.4.1",
            "MySQL-8.4",
            "mariadb-8.4",
        ] {
            assert_eq!(name(unknown), None, "{unknown}");
        }
    }

    #[test]
    fn a_series_means_its_newest_measured_release() {
        let json = |version: &str| {
            format!(
                r#"{{"server": {{"product": "mysql", "version": "{version}"}}, "scenarios": []}}"#
            )
        };
        let knowledge = Knowledge {
            servers: ["8.4.9", "8.4.13", "8.4.10", "8.0.46"]
                .map(|version| ServerResults::from_json(&json(version)).unwrap())
                .into(),
        };
        let found = knowledge.server("mysql-8.4").unwrap();
        assert_eq!(found.server.name(), "mysql-8.4.13");
        assert_eq!(
            knowledge.default_server().unwrap().server.name(),
            "mysql-8.4.13"
        );
    }

    #[test]
    fn the_default_server_is_the_newest_measured_8_4() {
        let knowledge = Knowledge::built_in();
        assert_eq!(DEFAULT_SERIES, "mysql-8.4");
        assert_eq!(
            knowledge.default_server().unwrap().server.name(),
            "mysql-8.4.13"
        );
    }

    #[test]
    fn the_scenarios_that_differ_between_the_servers_are_marked() {
        let knowledge = Knowledge::built_in();
        let [mysql_80, mysql_84] =
            ["mysql-8.0.46", "mysql-8.4.13"].map(|name| knowledge.server(name).unwrap());
        fn marked(results: &ServerResults) -> Vec<&str> {
            results
                .scenarios()
                .iter()
                .map(|scenario| scenario.id.as_str())
                .filter(|id| results.varies(id))
                .collect()
        }
        let mut expected = vec![
            "add-foreign-key-no-parent-index",
            "add-foreign-key-parent-column-prefix-index",
            "add-foreign-key-parent-longer-index",
            "add-foreign-key-parent-longer-unique-index",
            "add-foreign-key-parent-plain-desc-index",
            "add-foreign-key-parent-plain-index",
            "add-foreign-key-parent-second-column-index",
            "add-foreign-key-parent-unique-column-prefix-index",
        ];
        expected.sort_unstable();
        for results in [mysql_80, mysql_84] {
            let mut marked = marked(results);
            marked.sort_unstable();
            assert_eq!(marked, expected, "{}", results.server.name());
        }
        // Checked against the files themselves, every variant.
        for scenario in mysql_84.scenarios() {
            let theirs = mysql_80.scenario(&scenario.id).unwrap();
            let same = Variant::ALL
                .iter()
                .all(|variant| theirs.verdict_for(*variant) == scenario.verdict_for(*variant));
            assert_eq!(!same, mysql_84.varies(&scenario.id), "{}", scenario.id);
        }
        assert!(mysql_84.varies("no-such-scenario"));
    }

    #[test]
    fn results_read_alone_may_differ_anywhere() {
        let results = ServerResults::from_json(
            r#"{"server": {"product": "mysql", "version": "8.4.13"},
                "scenarios": [{"id": "a", "title": "a", "statement": "ALTER TABLE orders DROP note",
                    "verdict": {"result": "runs", "algorithm": "COPY", "rebuilds_table": true,
                                "copied_rows": 0, "writes": "blocked", "consistent": true},
                    "observations": []}]}"#,
        )
        .unwrap();
        assert!(results.varies("a"));
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
