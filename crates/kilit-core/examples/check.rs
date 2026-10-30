//! Prints what kilit says about each statement of a SQL file, from the lab
//! results built into kilit-core.
//!
//!     cargo run -p kilit-core --example check -- [file.sql] [server]
//!
//! Without arguments it checks `examples/sample.sql` against mysql-8.4.13.

use std::process::ExitCode;

use kilit_core::{Knowledge, Prediction};

fn main() -> ExitCode {
    let mut args = std::env::args().skip(1);
    let path = args
        .next()
        .unwrap_or_else(|| concat!(env!("CARGO_MANIFEST_DIR"), "/examples/sample.sql").to_owned());
    let server = args.next().unwrap_or_else(|| "mysql-8.4.13".to_owned());

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
    let sql = match std::fs::read_to_string(&path) {
        Ok(sql) => sql,
        Err(err) => {
            eprintln!("error: {path}: {err}");
            return ExitCode::FAILURE;
        }
    };

    println!("checked against {server}, as measured by kilit-lab\n");
    for finding in kilit_core::check(&kilit_core::parse(&sql), results) {
        println!("line {}: {}", finding.line, finding.sql);
        match finding.prediction {
            Prediction::Measured {
                case,
                verdict,
                evidence,
                notes,
            } => {
                println!("  {verdict}");
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
