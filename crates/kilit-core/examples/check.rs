//! Prints what kilit says about each statement of a SQL file, from the lab
//! results built into kilit-core.
//!
//!     cargo run -p kilit-core --example check -- [--schema schema.sql] [migration.sql] [server]
//!
//! Without arguments it checks `examples/sample.sql` against the tables in
//! `examples/mysql-schema.sql` on mysql-8.4.13. Without `--schema` kilit
//! knows no table, so it cannot judge a MODIFY or CHANGE.

use std::path::Path;
use std::process::ExitCode;

use kilit_core::{Knowledge, Prediction, Schema, Verdict};

const USAGE: &str = "usage: check [--schema schema.sql] [migration.sql] [server]";

fn main() -> ExitCode {
    let mut schema_path = None;
    let mut positional = Vec::new();
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--schema" => match args.next() {
                Some(path) => schema_path = Some(path),
                None => {
                    eprintln!("error: --schema needs a file\n{USAGE}");
                    return ExitCode::FAILURE;
                }
            },
            option if option.starts_with('-') => {
                eprintln!("error: unknown option {option}\n{USAGE}");
                return ExitCode::FAILURE;
            }
            _ => positional.push(arg),
        }
    }
    let mut positional = positional.into_iter();
    let path = positional.next().unwrap_or_else(|| {
        // The demo: the sample migration on the sample tables.
        let examples = concat!(env!("CARGO_MANIFEST_DIR"), "/examples");
        schema_path.get_or_insert_with(|| format!("{examples}/mysql-schema.sql"));
        format!("{examples}/sample.sql")
    });
    let server = positional
        .next()
        .unwrap_or_else(|| "mysql-8.4.13".to_owned());

    let knowledge = Knowledge::built_in();
    let Some(results) = knowledge.server(&server) else {
        let known: Vec<String> = knowledge
            .servers()
            .iter()
            .map(|results| results.server.name())
            .collect();
        eprintln!(
            "error: no lab results for {server}; kilit knows {}",
            known.join(", ")
        );
        return ExitCode::FAILURE;
    };
    let Some(sql) = read(&path) else {
        return ExitCode::FAILURE;
    };
    let schema = match &schema_path {
        Some(path) => match read(path) {
            Some(sql) => Schema::from_sql(&sql),
            None => return ExitCode::FAILURE,
        },
        None => Schema::new(),
    };

    println!("checked against {server}, as measured by kilit-lab");
    match &schema_path {
        Some(path) => {
            let names: Vec<&str> = schema.tables().map(|table| table.name.as_str()).collect();
            println!("tables from {}: {}", file_name(path), names.join(", "));
            for (table, reason) in schema.lost() {
                println!("  could not follow {table}: {reason}");
            }
        }
        None => println!("no schema given: kilit knows no table"),
    }
    println!();

    for finding in kilit_core::check(&kilit_core::parse(&sql), &schema, results) {
        println!("line {}: {}", finding.line, finding.sql);
        match finding.prediction {
            Prediction::Measured {
                case,
                verdict,
                evidence,
                notes,
            } => {
                match verdict {
                    // The lab's error message names the lab's own tables
                    // and keys; the notes say why this statement fails.
                    Verdict::Fails { code, .. } => println!("  fails with error {code}"),
                    runs => println!("  {runs}"),
                }
                let plural = if evidence.len() == 1 { "" } else { "s" };
                println!(
                    "  measured as {case} ({} lab scenario{plural})",
                    evidence.len()
                );
                for note in notes {
                    println!("  - {note}");
                }
            }
            Prediction::Unknown { reason } => {
                for (i, line) in reason.lines().enumerate() {
                    let label = if i == 0 { "UNKNOWN: " } else { "  " };
                    println!("  {label}{line}");
                }
            }
            Prediction::NotChecked { reason } => println!("  not checked: {reason}"),
        }
        println!();
    }
    ExitCode::SUCCESS
}

fn read(path: &str) -> Option<String> {
    match std::fs::read_to_string(path) {
        Ok(sql) => Some(sql),
        Err(err) => {
            eprintln!("error: {path}: {err}");
            None
        }
    }
}

fn file_name(path: &str) -> String {
    Path::new(path).file_name().map_or_else(
        || path.to_owned(),
        |name| name.to_string_lossy().into_owned(),
    )
}
