//! kilit-lab runs schema-change scenarios against real MySQL servers and
//! prints what each server actually did.
//!
//! Start the servers with `docker compose up -d --wait`, then run
//! `cargo run -p kilit-lab`, or pass your own `mysql://` URLs.

mod probe;
mod report;
mod scenario;

use std::process::ExitCode;

use mysql_async::prelude::Queryable;
use mysql_async::{Conn, Opts, OptsBuilder};

use crate::probe::Observation;
use crate::scenario::{SCENARIOS, Variant};

/// The lab's errors: the server or the network failed us, not a measurement.
pub type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

/// The servers from `compose.yaml`.
const DEFAULT_TARGETS: [&str; 2] = [
    "mysql://root:kilit@127.0.0.1:3380", // MySQL 8.0
    "mysql://root:kilit@127.0.0.1:3384", // MySQL 8.4
];

const USAGE: &str = "\
usage: kilit-lab [mysql://user:password@host:port ...]

Runs every scenario against each server and prints what the server did.
Without arguments it uses the servers from compose.yaml.

Warning: the lab drops and recreates a database named `kilit_lab`.
Only point it at throwaway servers.";

#[tokio::main(flavor = "current_thread")]
async fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.iter().any(|arg| arg == "-h" || arg == "--help") {
        println!("{USAGE}");
        return ExitCode::SUCCESS;
    }
    let targets = if args.is_empty() {
        DEFAULT_TARGETS.map(String::from).to_vec()
    } else {
        args
    };

    let mut failed = false;
    for url in &targets {
        if let Err(err) = run_target(url).await {
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

/// Runs every scenario, in every variant, on one server.
async fn run_target(url: &str) -> Result<()> {
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
    report::print_server(&version, &address);

    for scenario in SCENARIOS {
        let mut observations: Vec<Observation> = Vec::with_capacity(Variant::ALL.len());
        for variant in Variant::ALL {
            observations.push(probe::run(&mut conn, scenario, variant).await?);
        }
        report::print_scenario(scenario, &observations);
    }
    println!();

    probe::clean_up(&mut conn).await?;
    conn.disconnect().await?;
    Ok(())
}
