//! Prints the lab's results as plain-text tables.

use kilit_core::{Outcome, Server, Variant, Verdict, Writes};

use crate::fixture::ORDERS_ROWS;
use crate::probe::Measurement;

/// One line per scenario: what the server does when nothing is forced.
pub fn print_summary(server: &Server, address: &str, measurements: &[Measurement]) {
    let width = measurements
        .iter()
        .map(|measurement| measurement.scenario.id.len())
        .max()
        .unwrap_or(0)
        .max("scenario".len());

    println!("== {} {} at {address}", server.product, server.version);
    println!();
    println!(
        "  {:<width$}  {:<9}  {:<8}  {:>11}  writes",
        "scenario", "algorithm", "rebuilds", "copied rows"
    );

    let mut inconsistent = false;
    for measurement in measurements {
        let id = &measurement.scenario.id;
        match &measurement.verdict {
            Verdict::Runs {
                algorithm,
                rebuilds_table,
                copied_rows,
                writes,
                consistent,
            } => {
                let algorithm = algorithm.map_or_else(|| "?".to_owned(), |a| a.to_string());
                let rebuilds = if *rebuilds_table { "yes" } else { "no" };
                let copied = copied_rows.map_or_else(|| "?".to_owned(), |rows| rows.to_string());
                let writes = match writes {
                    Writes::Allowed => "allowed",
                    Writes::Blocked => "blocked",
                    Writes::Unknown => "unknown",
                };
                let flag = if *consistent { "" } else { "  (!)" };
                inconsistent |= !consistent;
                println!(
                    "  {id:<width$}  {algorithm:<9}  {rebuilds:<8}  {copied:>11}  {writes}{flag}"
                );
            }
            Verdict::Fails { code, message } => {
                println!("  {id:<width$}  fails with error {code}: {message}");
            }
        }
    }
    if inconsistent {
        println!();
        println!("  (!) the server-default run differs from the forced run of that algorithm;");
        println!("      run with --details and look closer");
    }
}

/// Every variant of every scenario.
pub fn print_details(measurements: &[Measurement]) {
    for measurement in measurements {
        let scenario = measurement.scenario;
        println!();
        println!("{}: {}", scenario.id, scenario.title);
        for statement in &scenario.setup {
            println!("{statement};  -- setup");
        }
        for statement in &scenario.session {
            println!("{statement};");
        }
        println!(
            "{}  ({ORDERS_ROWS} rows)",
            scenario.statement(Variant::SERVER_DEFAULT)
        );
        println!();
        println!(
            "  {:<30} {:<10} {:>11}  {:<7}  row versions",
            "variant", "result", "copied rows", "rebuilt"
        );
        for observation in &measurement.observations {
            let variant = observation.variant.to_string();
            match &observation.outcome {
                Outcome::Accepted {
                    copied_rows,
                    rebuilt,
                    row_versions: (before, after),
                } => {
                    let copied =
                        copied_rows.map_or_else(|| "?".to_owned(), |rows| rows.to_string());
                    let rebuilt = if *rebuilt { "yes" } else { "no" };
                    println!(
                        "  {variant:<30} {:<10} {copied:>11}  {rebuilt:<7}  {before} -> {after}",
                        "accepted"
                    );
                }
                Outcome::Rejected { code, message } => {
                    let result = format!("error {code}");
                    println!("  {variant:<30} {result:<10} {message}");
                }
            }
        }
    }
}
