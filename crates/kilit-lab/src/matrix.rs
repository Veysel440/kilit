//! Writes the lab's results to `matrix/<product>-<version>.json`.
//!
//! The files are committed: they are the evidence behind what kilit says.
//! They hold no timestamps and no table ids, so running the lab again
//! changes a file only when the server behaved differently.

use std::fs;
use std::path::{Path, PathBuf};

use serde::Serialize;

use crate::Result;
use crate::fixture::ORDERS_ROWS;
use crate::probe::{Measurement, Observation};
use crate::scenario::Variant;
use crate::verdict::Verdict;

/// The server a matrix file describes.
#[derive(Debug, PartialEq, Eq, Serialize)]
pub struct Server {
    /// `mysql` or `mariadb`.
    pub product: &'static str,
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
        let version = raw.split('-').next().unwrap_or(raw).to_owned();
        Server { product, version }
    }

    /// The matrix file name without `.json`, e.g. `mysql-8.4.13`.
    pub fn name(&self) -> String {
        format!("{}-{}", self.product, self.version)
    }
}

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
    session: &'a [String],
    statement: String,
    verdict: &'a Verdict,
    observations: &'a [Observation],
}

/// Writes one server's results into `dir` and returns the file's path.
pub fn write(dir: &Path, server: &Server, measurements: &[Measurement]) -> Result<PathBuf> {
    let matrix = Matrix {
        server,
        orders_rows: ORDERS_ROWS,
        scenarios: measurements
            .iter()
            .map(|measurement| ScenarioResult {
                id: &measurement.scenario.id,
                title: &measurement.scenario.title,
                session: &measurement.scenario.session,
                statement: measurement.scenario.statement(Variant::SERVER_DEFAULT),
                verdict: &measurement.verdict,
                observations: &measurement.observations,
            })
            .collect(),
    };
    let mut json = serde_json::to_string_pretty(&matrix)?;
    json.push('\n');

    fs::create_dir_all(dir)?;
    let path = dir.join(format!("{}.json", server.name()));
    fs::write(&path, json)?;
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::Server;

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
}
