//! Writes the lab's results to `matrix/<product>-<version>.json`.
//!
//! The files are committed: they are the evidence behind what kilit says.
//! They hold no timestamps and no table ids, so running the lab again
//! changes a file only when the server behaved differently. Before writing,
//! the lab compares its results with the file it replaces and reports what
//! changed.

use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};

use kilit_core::{Observation, Server, ServerResults, Variant, Verdict};
use serde::Serialize;

use crate::Result;
use crate::fixture::ORDERS_ROWS;
use crate::probe::Measurement;

#[derive(Serialize)]
struct Matrix<'a> {
    server: &'a Server,
    orders_rows: u32,
    scenarios: Vec<ScenarioResult<'a>>,
}

#[derive(Serialize)]
struct ScenarioResult<'a> {
    id: &'a str,
    title: &'a str,
    #[serde(skip_serializing_if = "<[String]>::is_empty")]
    setup: &'a [String],
    session: &'a [String],
    #[serde(skip_serializing_if = "Option::is_none")]
    engine: Option<&'a str>,
    statement: String,
    verdict: &'a Verdict,
    observations: &'a [Observation],
}

/// How the new results differ from the file they replace, scenario by scenario.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Changes {
    pub added: Vec<String>,
    pub changed: Vec<String>,
    pub removed: Vec<String>,
    pub unchanged: usize,
}

/// `2 new, 1 changed, 25 unchanged`
impl fmt::Display for Changes {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut parts = Vec::new();
        if !self.added.is_empty() {
            parts.push(format!("{} new", self.added.len()));
        }
        if !self.changed.is_empty() {
            parts.push(format!("{} changed", self.changed.len()));
        }
        if !self.removed.is_empty() {
            parts.push(format!("{} removed", self.removed.len()));
        }
        parts.push(format!("{} unchanged", self.unchanged));
        f.write_str(&parts.join(", "))
    }
}

/// Writes one server's results into `dir`. Returns the file's path and
/// what changed, or `None` if there was no earlier file to compare with.
pub fn write(
    dir: &Path,
    server: &Server,
    measurements: &[Measurement],
) -> Result<(PathBuf, Option<Changes>)> {
    let matrix = Matrix {
        server,
        orders_rows: ORDERS_ROWS,
        scenarios: measurements
            .iter()
            .map(|measurement| ScenarioResult {
                id: &measurement.scenario.id,
                title: &measurement.scenario.title,
                setup: &measurement.scenario.setup,
                session: &measurement.scenario.session,
                engine: measurement.scenario.engine,
                statement: measurement.scenario.statement(Variant::SERVER_DEFAULT),
                verdict: &measurement.verdict,
                observations: &measurement.observations,
            })
            .collect(),
    };
    let mut json = serde_json::to_string_pretty(&matrix)?;
    json.push('\n');

    // Read the new results back the way kilit will: if kilit-core cannot
    // read what the lab writes, stop here rather than save a file it can't use.
    let new = ServerResults::from_json(&json)
        .map_err(|err| format!("kilit-core cannot read the new results: {err}"))?;

    fs::create_dir_all(dir)?;
    let path = dir.join(format!("{}.json", server.name()));
    let changes = match fs::read_to_string(&path) {
        Ok(text) => ServerResults::from_json(&text)
            .ok()
            .map(|old| compare(&old, &new)),
        Err(_) => None,
    };
    fs::write(&path, json)?;
    Ok((path, changes))
}

fn compare(old: &ServerResults, new: &ServerResults) -> Changes {
    let mut changes = Changes::default();
    for scenario in new.scenarios() {
        match old.scenario(&scenario.id) {
            None => changes.added.push(scenario.id.clone()),
            Some(previous) if previous == scenario => changes.unchanged += 1,
            Some(_) => changes.changed.push(scenario.id.clone()),
        }
    }
    for scenario in old.scenarios() {
        if new.scenario(&scenario.id).is_none() {
            changes.removed.push(scenario.id.clone());
        }
    }
    changes
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A result file with one scenario per `(id, copied_rows)`.
    fn results(scenarios: &[(&str, u64)]) -> ServerResults {
        let scenarios: Vec<String> = scenarios
            .iter()
            .map(|(id, rows)| {
                format!(
                    r#"{{"id": "{id}", "title": "t", "session": [], "statement": "ALTER TABLE orders DROP note",
                        "verdict": {{"result": "runs", "algorithm": "COPY", "rebuilds_table": true,
                                     "copied_rows": {rows}, "writes": "blocked", "consistent": true}},
                        "observations": []}}"#
                )
            })
            .collect();
        let json = format!(
            r#"{{"server": {{"product": "mysql", "version": "8.4.13"}}, "orders_rows": 1000,
                "scenarios": [{}]}}"#,
            scenarios.join(",")
        );
        ServerResults::from_json(&json).unwrap()
    }

    #[test]
    fn reports_new_changed_removed_and_unchanged_scenarios() {
        let old = results(&[("a", 1000), ("b", 1000), ("c", 1000)]);
        let new = results(&[("a", 1000), ("b", 999), ("d", 1000)]);
        let changes = compare(&old, &new);
        assert_eq!(
            changes,
            Changes {
                added: vec!["d".into()],
                changed: vec!["b".into()],
                removed: vec!["c".into()],
                unchanged: 1,
            }
        );
        assert_eq!(
            changes.to_string(),
            "1 new, 1 changed, 1 removed, 1 unchanged"
        );
    }

    #[test]
    fn identical_results_are_all_unchanged() {
        let results = results(&[("a", 1000), ("b", 0)]);
        assert_eq!(compare(&results, &results).to_string(), "2 unchanged");
    }
}
