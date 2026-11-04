//! kilit says what MySQL will do with a schema migration: whether writes
//! wait, whether the table is copied, whether the statement fails. Every
//! answer comes from what kilit-lab measured on real servers.
//!
//! ```text
//! php artisan migrate --pretend | kilit check --schema database/schema/mysql-schema.sql
//! kilit check --schema schema.sql migration.sql
//! kilit servers
//! ```

mod input;
mod report;

use std::io::IsTerminal;
use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Args, Parser, Subcommand, ValueEnum};
use kilit_core::{Knowledge, Schema, ServerResults};

use crate::input::Input;
use crate::report::Report;

const EXIT_STATUS: &str = "\
Exit status:
  0  no statement fails and none makes writes wait
  1  a statement fails or makes writes wait; with --strict, also when kilit
     cannot tell what a statement does
  2  kilit could not check everything: a bad option, a file it cannot read,
     an unknown server, or lines of the input it does not understand";

#[derive(Parser)]
#[command(
    name = "kilit",
    version,
    about = "Says what MySQL will do with a schema migration, from measurements on real servers"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Check a migration: SQL, or the output of `php artisan migrate --pretend`
    #[command(after_help = EXIT_STATUS)]
    Check(CheckArgs),
    /// List the servers kilit has lab results for
    Servers,
}

#[derive(Args)]
struct CheckArgs {
    /// SQL files or saved `migrate --pretend` output, checked in order as one
    /// migration. Without a file, or with `-`, kilit reads standard input
    #[arg(value_name = "FILE")]
    files: Vec<PathBuf>,

    /// The tables before the migration, as `php artisan schema:dump` or
    /// `mysqldump --no-data` writes them. Without it kilit cannot judge
    /// MODIFY and CHANGE
    #[arg(long, value_name = "FILE")]
    schema: Option<PathBuf>,

    /// The server to check against, as `kilit servers` lists them
    /// [default: the newest]
    #[arg(long, value_name = "NAME")]
    server: Option<String>,

    /// How to print the result
    #[arg(long, value_enum, default_value_t = Format::Text)]
    format: Format,

    /// Exit with 1 also when kilit cannot tell what a statement does
    #[arg(long)]
    strict: bool,
}

#[derive(Clone, Copy, ValueEnum)]
enum Format {
    Text,
    Json,
}

fn main() -> ExitCode {
    // clap prints help and errors itself; a bad option exits with 2.
    match Cli::parse().command {
        Command::Check(args) => match check(&args) {
            Ok(code) => ExitCode::from(code),
            Err(message) => {
                eprintln!("kilit: {message}");
                ExitCode::from(2)
            }
        },
        Command::Servers => {
            servers();
            ExitCode::SUCCESS
        }
    }
}

/// Checks the input and prints the report. Returns the exit status, or why
/// kilit could not check at all.
fn check(args: &CheckArgs) -> Result<u8, String> {
    let results = server(args.server.as_deref())?;
    let schema = match &args.schema {
        Some(path) => Some((
            path.display().to_string(),
            Schema::from_sql(&input::read(path)?),
        )),
        None => None,
    };

    let files = if args.files.is_empty() {
        if std::io::stdin().is_terminal() {
            return Err(
                "nothing to check: pipe `php artisan migrate --pretend` into \
                        `kilit check`, or give it a file (see `kilit check --help`)"
                    .into(),
            );
        }
        vec![PathBuf::from("-")]
    } else {
        args.files.clone()
    };
    let input = Input::read(&files)?;

    let report = Report::new(results, schema.as_ref(), &input);
    let output = match args.format {
        Format::Text => report.text(),
        Format::Json => report.json(args.strict),
    };
    print!("{output}");
    Ok(report.exit_code(args.strict))
}

/// The lab results for the server named `name`, or for the newest one.
fn server(name: Option<&str>) -> Result<&'static ServerResults, String> {
    let knowledge = Knowledge::built_in();
    let found = match name {
        Some(name) => knowledge.server(name),
        // The built-in results are listed oldest first.
        None => knowledge.servers().last(),
    };
    found.ok_or_else(|| {
        let known: Vec<String> = knowledge
            .servers()
            .iter()
            .map(|results| results.server.name())
            .collect();
        format!(
            "no lab results for {}; kilit has results for {}",
            name.unwrap_or("any server"),
            known.join(", ")
        )
    })
}

fn servers() {
    for results in Knowledge::built_in().servers() {
        println!(
            "{}  {} scenarios",
            results.server.name(),
            results.scenarios().len()
        );
    }
}
