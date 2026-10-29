//! Prints observations as plain-text tables.

use crate::probe::{FIXTURE_ROWS, Observation, Outcome};
use crate::scenario::Scenario;

pub fn print_server(version: &str, address: &str) {
    println!("== MySQL {version} at {address}");
}

pub fn print_scenario(scenario: &Scenario, observations: &[Observation]) {
    println!();
    println!("{}: {}", scenario.id, scenario.title);
    println!(
        "ALTER TABLE orders {}  ({FIXTURE_ROWS} rows)",
        scenario.operation
    );
    println!();
    println!(
        "  {:<30} {:<10} {:>11}  {:<7}  row versions",
        "variant", "result", "copied rows", "rebuilt"
    );
    for observation in observations {
        let variant = observation.variant.to_string();
        match &observation.outcome {
            Outcome::Accepted {
                copied_rows,
                rebuilt,
                row_versions: (before, after),
            } => {
                let copied = copied_rows.map_or_else(|| "?".to_owned(), |rows| rows.to_string());
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
