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
use kilit_core::{DEFAULT_SERIES, Knowledge, Schema, ServerResults};

use crate::input::Input;
use crate::report::{
    Count, EXIT_FOUND, EXIT_INCOMPLETE, EXIT_PASSED, EXIT_UNKNOWN, Places, Report,
};

/// What each exit status means. `kilit check --help` and the README's table
/// say exactly this; a test holds both to it.
const EXIT_STATUSES: [(u8, &str); 4] = [
    (EXIT_PASSED, "nothing that --fail-on counts was found"),
    (
        EXIT_FOUND,
        "a statement fails or makes writes wait, and --fail-on counts it",
    ),
    (
        EXIT_INCOMPLETE,
        "kilit could not check everything: a bad option, a file it cannot read, an unknown \
         server, a MariaDB schema without --server, lines of the input it does not understand, \
         or Laravel stopping with an error",
    ),
    (
        EXIT_UNKNOWN,
        "the only statements --fail-on counts are unknown: kilit cannot tell what they do",
    ),
];

/// Which status wins when several apply.
const EXIT_PRECEDENCE: &str = "When several apply, 2 wins over 1, and 1 over 3.";

/// The exit status part of `kilit check --help`.
fn exit_status_help() -> String {
    let mut help = String::from("Exit status:\n");
    for (code, meaning) in EXIT_STATUSES {
        for (i, line) in report::wrap(meaning, 72).iter().enumerate() {
            if i == 0 {
                help.push_str(&format!("  {code}  {line}\n"));
            } else {
                help.push_str(&format!("     {line}\n"));
            }
        }
    }
    help.push_str(&format!("  {EXIT_PRECEDENCE}"));
    help
}

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
    #[command(after_help = exit_status_help())]
    Check(CheckArgs),
    /// List the servers kilit has lab results for
    Servers,
}

#[derive(Args)]
struct CheckArgs {
    /// SQL files or saved `migrate --pretend` output, checked in order as one
    /// migration, each file in a session of its own. Without a file, or with
    /// `-`, kilit reads standard input
    #[arg(value_name = "FILE")]
    files: Vec<PathBuf>,

    /// The tables before the migration, as `php artisan schema:dump` or
    /// `mysqldump --no-data` writes them. Without it kilit does not know the
    /// tables the migration does not create, and gives no answer a table
    /// could change
    #[arg(long, value_name = "FILE")]
    schema: Option<PathBuf>,

    /// The server to check against: a release as `kilit servers` lists them,
    /// such as mysql-8.4.13, or a series, such as mysql-8.4, for the newest
    /// release the lab measured in it [default: mysql-8.4]
    #[arg(long, value_name = "NAME")]
    server: Option<String>,

    /// How to print the result: `github` prints the text report, then a
    /// GitHub Actions annotation for each statement that needs one
    #[arg(long, value_enum, default_value_t = Format::Text)]
    format: Format,

    /// What makes kilit exit with a status other than 0, comma-separated:
    /// statements that fail, that make writes wait, or that kilit cannot tell
    /// what they do
    #[arg(
        long,
        value_name = "LIST",
        value_enum,
        value_delimiter = ',',
        default_value = "fails,waits"
    )]
    fail_on: Vec<Count>,

    /// Count unknown statements too: the same as --fail-on fails,waits,unknown
    #[arg(long, conflicts_with = "fail_on")]
    strict: bool,
}

impl CheckArgs {
    /// What the exit status counts.
    fn counted(&self) -> Vec<Count> {
        if self.strict {
            vec![Count::Fails, Count::Waits, Count::Unknown]
        } else {
            self.fail_on.clone()
        }
    }
}

#[derive(Clone, Copy, ValueEnum)]
enum Format {
    Text,
    Json,
    Github,
}

fn main() -> ExitCode {
    // clap prints help and errors itself; a bad option exits with 2.
    match Cli::parse().command {
        Command::Check(args) => match check(&args) {
            Ok(code) => ExitCode::from(code),
            Err(message) => {
                match args.format {
                    // GitHub reads workflow commands on standard error too.
                    Format::Github => eprintln!("{}", report::github_error(&message)),
                    Format::Text | Format::Json => eprintln!("kilit: {message}"),
                }
                ExitCode::from(EXIT_INCOMPLETE)
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
    let mut warnings = Vec::new();
    let schema = match &args.schema {
        Some(path) => {
            let text = input::read(path)?;
            warnings = schema_server(&text, results, args.server.is_some())?;
            Some((path.display().to_string(), Schema::from_dump(&text)))
        }
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

    let others: Vec<&ServerResults> = Knowledge::built_in()
        .servers()
        .iter()
        .filter(|other| other.server != results.server)
        .collect();
    let counted = args.counted();
    let report = Report::new(results, &others, schema.as_ref(), &input, warnings);
    let output = match args.format {
        Format::Text => report.text(&counted),
        Format::Json => report.json(&counted),
        Format::Github => report.github(
            &counted,
            &Places::new(
                std::env::current_dir().ok(),
                std::env::var_os("GITHUB_WORKSPACE").map(PathBuf::from),
            ),
        ),
    };
    print!("{output}");
    Ok(report.exit_code(&counted))
}

/// The lab results for the server or series named `name`, or for
/// [`DEFAULT_SERIES`].
fn server(name: Option<&str>) -> Result<&'static ServerResults, String> {
    let knowledge = Knowledge::built_in();
    let found = match name {
        Some(name) => knowledge.server(name),
        None => knowledge.default_server(),
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
    let knowledge = Knowledge::built_in();
    let default = knowledge.default_server().map(|results| &results.server);
    for results in knowledge.servers() {
        let mark = if Some(&results.server) == default {
            format!(" (default: {DEFAULT_SERIES})")
        } else {
            String::new()
        };
        println!(
            "{}  {} scenarios{mark}",
            results.server.name(),
            results.scenarios().len()
        );
    }
}

/// Warnings about the server a schema dump names in its `-- Server version`
/// lines, or why kilit will not check against it. kilit never picks the
/// server from the dump: it says when the dump comes from another series.
/// A MariaDB dump stops the check unless `--server` was given (`explicit`).
fn schema_server(
    text: &str,
    results: &ServerResults,
    explicit: bool,
) -> Result<Vec<String>, String> {
    let checked = results.server.name();
    let mut warnings = Vec::new();
    for version in input::server_versions(text) {
        if version.to_ascii_lowercase().contains("mariadb") {
            if !explicit {
                return Err(format!(
                    "the schema comes from MariaDB {version}, and kilit has no MariaDB \
                     measurements; give --server to check it against MySQL anyway"
                ));
            }
            warnings.push(format!(
                "the schema comes from MariaDB {version}, and kilit has no MariaDB \
                 measurements; answers are for {checked}"
            ));
            continue;
        }
        match mysql_series(version) {
            Some(series) if series == results.server.series() => {}
            Some(_) => warnings.push(format!(
                "the schema comes from MySQL {version}; answers are for {checked}"
            )),
            None => warnings.push(format!(
                "the schema names its server version as \"{version}\", which kilit cannot \
                 read; answers are for {checked}"
            )),
        }
    }
    Ok(warnings)
}

/// `mysql-8.0` for a version like `8.0.36` or `8.0.36-0ubuntu0.22.04.1`.
fn mysql_series(version: &str) -> Option<String> {
    let mut numbers = version.split(['.', '-']);
    let major = numbers.next()?;
    let minor = numbers.next()?;
    let digits = |number: &str| !number.is_empty() && number.bytes().all(|b| b.is_ascii_digit());
    (digits(major) && digits(minor)).then(|| format!("mysql-{major}.{minor}"))
}

#[cfg(test)]
mod tests {
    use clap::CommandFactory;

    use super::*;

    /// Each status of a help text's exit status part on one line, its
    /// wrapped lines joined, and the line on precedence.
    fn statuses(help: &str) -> Vec<String> {
        let mut lines: Vec<String> = Vec::new();
        for line in help
            .lines()
            .skip_while(|line| *line != "Exit status:")
            .skip(1)
        {
            match (line.strip_prefix("     "), lines.last_mut()) {
                (Some(more), Some(last)) => {
                    last.push(' ');
                    last.push_str(more);
                }
                _ => lines.push(line.trim().to_owned()),
            }
        }
        lines
    }

    #[test]
    fn the_help_says_what_each_exit_status_means() {
        let mut expected: Vec<String> = EXIT_STATUSES
            .iter()
            .map(|(code, meaning)| format!("{code}  {meaning}"))
            .collect();
        expected.push(EXIT_PRECEDENCE.to_owned());
        let help = Cli::command()
            .find_subcommand_mut("check")
            .expect("kilit has a check command")
            .render_long_help()
            .to_string();
        assert_eq!(statuses(&help), expected, "{help}");
    }

    #[test]
    fn the_readme_says_what_each_exit_status_means() {
        let readme = include_str!("../../../README.md");
        for (code, meaning) in EXIT_STATUSES {
            let row = format!("| {code} | {meaning} |");
            assert!(readme.contains(&row), "the README has no row {row}");
        }
        assert!(readme.contains(EXIT_PRECEDENCE));
    }

    #[test]
    fn every_exit_status_means_one_thing() {
        let codes: Vec<u8> = EXIT_STATUSES.iter().map(|(code, _)| *code).collect();
        assert_eq!(codes, [0, 1, 2, 3]);
    }

    #[test]
    fn the_help_names_the_default_server() {
        let help = Cli::command()
            .find_subcommand_mut("check")
            .expect("kilit has a check command")
            .render_long_help()
            .to_string();
        assert!(
            help.contains(&format!("[default: {DEFAULT_SERIES}]")),
            "{help}"
        );
    }

    #[test]
    fn reads_the_series_of_a_mysql_version() {
        assert_eq!(mysql_series("8.0.36").as_deref(), Some("mysql-8.0"));
        assert_eq!(mysql_series("8.4.13").as_deref(), Some("mysql-8.4"));
        assert_eq!(mysql_series("9.7.4").as_deref(), Some("mysql-9.7"));
        assert_eq!(
            mysql_series("8.0.36-0ubuntu0.22.04.1").as_deref(),
            Some("mysql-8.0")
        );
        assert_eq!(mysql_series("5.7.44-log").as_deref(), Some("mysql-5.7"));
        for unreadable in ["8", "8.", ".4", "v8.4.13", "8.x", "weird", ""] {
            assert_eq!(mysql_series(unreadable), None, "{unreadable}");
        }
    }

    fn mysql_84() -> &'static ServerResults {
        Knowledge::built_in().server("mysql-8.4.13").unwrap()
    }

    #[test]
    fn a_schema_from_the_checked_series_gets_no_warning() {
        let warnings = schema_server("-- Server version\t8.4.2\n", mysql_84(), false);
        assert_eq!(warnings, Ok(Vec::new()));
        assert_eq!(
            schema_server("CREATE TABLE t (id int);", mysql_84(), false),
            Ok(Vec::new())
        );
    }

    #[test]
    fn a_schema_from_another_series_gets_a_warning() {
        let warnings = schema_server("-- Server version\t8.0.36\n", mysql_84(), false);
        assert_eq!(
            warnings,
            Ok(vec![
                "the schema comes from MySQL 8.0.36; answers are for mysql-8.4.13".to_owned()
            ])
        );
        let warnings = schema_server("-- Server version\tweird\n", mysql_84(), true);
        assert_eq!(
            warnings,
            Ok(vec![
                "the schema names its server version as \"weird\", which kilit cannot read; \
                 answers are for mysql-8.4.13"
                    .to_owned()
            ])
        );
    }

    #[test]
    fn a_schema_from_mariadb_stops_the_check_unless_a_server_is_named() {
        let dump = "/*M!999999\\- enable the sandbox mode */ \n\
                    -- MariaDB dump 10.19  Distrib 10.11.6-MariaDB, for debian-linux-gnu (x86_64)\n\
                    -- Server version\t10.11.6-MariaDB-0ubuntu0.23.04.1\n";
        assert_eq!(
            schema_server(dump, mysql_84(), false),
            Err(
                "the schema comes from MariaDB 10.11.6-MariaDB-0ubuntu0.23.04.1, and kilit has \
                 no MariaDB measurements; give --server to check it against MySQL anyway"
                    .to_owned()
            )
        );
        assert_eq!(
            schema_server(dump, mysql_84(), true),
            Ok(vec![
                "the schema comes from MariaDB 10.11.6-MariaDB-0ubuntu0.23.04.1, and kilit has \
                 no MariaDB measurements; answers are for mysql-8.4.13"
                    .to_owned()
            ])
        );
    }
}
