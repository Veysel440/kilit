//! kilit-lab runs schema-change scenarios against real MySQL servers,
//! prints what each server did and saves the results under `matrix/`.
//!
//! Start the servers with `docker compose up -d --wait`, then run
//! `cargo run -p kilit-lab`, or pass your own `mysql://` URLs.

mod fixture;
mod matrix;
mod probe;
mod report;
mod scenario;
mod verdict;

use std::path::Path;
use std::process::ExitCode;

use mysql_async::prelude::Queryable;
use mysql_async::{Conn, Opts, OptsBuilder};

use crate::matrix::Server;
use crate::scenario::Scenario;

/// The lab's errors: the server or the network failed us, not a measurement.
pub type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

/// The servers from `compose.yaml`.
const DEFAULT_TARGETS: [&str; 2] = [
    "mysql://root:kilit@127.0.0.1:3380", // MySQL 8.0
    "mysql://root:kilit@127.0.0.1:3384", // MySQL 8.4
];

const USAGE: &str = "\
usage: kilit-lab [--details] [mysql://user:password@host:port ...]

Runs every scenario in scenarios/ against each server, prints a summary
and writes the results to matrix/<product>-<version>.json.
Without URLs it uses the servers from compose.yaml.

  --details   also print every variant of every scenario

Warning: the lab drops and recreates a database named `kilit_lab`.
Only point it at throwaway servers.";

#[tokio::main(flavor = "current_thread")]
async fn main() -> ExitCode {
    let mut details = false;
    let mut targets = Vec::new();
    for arg in std::env::args().skip(1) {
        match arg.as_str() {
            "-h" | "--help" => {
                println!("{USAGE}");
                return ExitCode::SUCCESS;
            }
            "--details" => details = true,
            option if option.starts_with('-') => {
                eprintln!("error: unknown option {option}\n\n{USAGE}");
                return ExitCode::FAILURE;
            }
            _ => targets.push(arg),
        }
    }
    if targets.is_empty() {
        targets = DEFAULT_TARGETS.map(String::from).to_vec();
    }

    // The workspace root, so the lab finds its folders from any directory.
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .expect("crates/kilit-lab lives two levels below the workspace root");

    let scenarios = match scenario::load_all(&root.join("scenarios")) {
        Ok(scenarios) => scenarios,
        Err(err) => {
            eprintln!("error: {err}");
            return ExitCode::FAILURE;
        }
    };

    let mut failed = false;
    for url in &targets {
        if let Err(err) = run_target(url, &scenarios, root, details).await {
            eprintln!("error: {err}");
            failed = true;
        }
    }
    if failed {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    }
}

/// Measures every scenario on one server, prints the results and saves them.
async fn run_target(url: &str, scenarios: &[Scenario], root: &Path, details: bool) -> Result<()> {
    let opts = Opts::from_url(url)?;
    // Printed instead of the URL, which contains the password.
    let address = format!("{}:{}", opts.ip_or_hostname(), opts.tcp_port());

    // After connecting to 127.0.0.1, mysql_async would by default reconnect
    // through the server's unix socket path, looked up on *this* machine.
    // A local MySQL listening there would silently take the container's place.
    let opts = OptsBuilder::from_opts(opts).prefer_socket(false);
    let mut conn = Conn::new(opts).await.map_err(|err| {
        // A network error usually means the servers are not running.
        let hint = if matches!(err, mysql_async::Error::Io(_)) {
            "\n       is the lab running? try `docker compose up -d --wait`"
        } else {
            ""
        };
        format!("{address}: cannot connect: {err}{hint}")
    })?;

    let version: String = conn
        .query_first("SELECT VERSION()")
        .await?
        .unwrap_or_default();
    let server = Server::from_version(&version);
    if server.product != "mysql" {
        return Err(format!("{address}: {version} is not supported yet, only MySQL is").into());
    }

    let mut measurements = Vec::with_capacity(scenarios.len());
    for scenario in scenarios {
        measurements.push(probe::measure(&mut conn, scenario).await?);
    }
    fixture::drop(&mut conn).await?;
    conn.disconnect().await?;

    report::print_summary(&server, &address, &measurements);
    if details {
        report::print_details(&measurements);
    }
    let path = matrix::write(&root.join("matrix"), &server, &measurements)?;
    println!();
    println!(
        "  saved {}",
        path.strip_prefix(root).unwrap_or(&path).display()
    );
    println!();
    Ok(())
}
