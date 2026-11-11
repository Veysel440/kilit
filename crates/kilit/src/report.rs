//! What kilit prints: a report for people, JSON for other programs, or the
//! report with GitHub Actions annotations.

use std::fmt::Write as _;
use std::path::{Component, Path, PathBuf};

use clap::ValueEnum;
use kilit_core::{
    Algorithm, Impact, PRETEND_READ_NOTE, Prediction, Schema, ServerResults, Shared, Statement,
    StatementKind, TableSeen, UnreadableLine, Verdict, Writes, check_files, depends_on_rows,
    impacts, locked_tables as locked_tables_of, metadata_lock_warning, on_empty_tables,
    on_new_tables, only_reads, table_of,
};
use serde::Serialize;

use crate::input::{Input, SourceKind, Unreadable};

/// Nothing that the exit status counts was found.
pub const EXIT_PASSED: u8 = 0;
/// A statement fails or makes writes wait, and that is counted.
pub const EXIT_FOUND: u8 = 1;
/// kilit could not check everything.
pub const EXIT_INCOMPLETE: u8 = 2;
/// The only counted statements are ones kilit cannot tell what they do.
pub const EXIT_UNKNOWN: u8 = 3;

/// What the exit status counts, as `--fail-on` names it.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, ValueEnum, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Count {
    /// Statements that fail
    Fails,
    /// Statements that make writes wait
    Waits,
    /// Statements kilit cannot tell what they do
    Unknown,
}

/// `counted` in a fixed order, each once.
fn in_order(counted: &[Count]) -> Vec<Count> {
    let mut counted = counted.to_vec();
    counted.sort_unstable();
    counted.dedup();
    counted
}

/// What kilit says about the input.
pub struct Report<'a> {
    server: String,
    schema: Option<SchemaSummary>,
    /// Why the schema's server may not be the one checked against.
    server_warnings: Vec<String>,
    sources: Vec<SourceReport<'a>>,
    unreadable: &'a [Unreadable],
    from_laravel: bool,
    /// Each error screen of Laravel in the input: the input it is in, and
    /// what kilit says about it.
    laravel_errors: Vec<(&'a str, &'a str)>,
    /// More than one input was read: messages name theirs.
    several_inputs: bool,
    /// Tables that may exist before the migration and that an ALTER, DROP
    /// TABLE, RENAME TABLE or TRUNCATE TABLE waits for.
    locked_tables: Vec<String>,
    /// How many statements, but for CREATE TABLE, are on a table kilit
    /// does not know: one no `--schema` and no statement before shows, or
    /// one it lost track of.
    on_unknown_tables: usize,
}

struct SchemaSummary {
    file: String,
    tables: Vec<String>,
    lost: Vec<(String, String)>,
}

struct SourceReport<'a> {
    name: &'a str,
    /// The SQL file, as given.
    file: Option<&'a Path>,
    kind: SourceKind,
    /// Text a Laravel migration printed itself.
    printed: &'a [UnreadableLine],
    statements: Vec<Checked<'a>>,
}

struct Checked<'a> {
    statement: &'a Statement,
    /// The table the statement is on, as written, when it names one.
    table: Option<&'a str>,
    /// What kilit knew of that table right before the statement: `known`,
    /// `created` (by an earlier statement), `lost` or `unseen`.
    table_state: Option<&'static str>,
    prediction: Prediction,
    impact: Impact,
    /// The statement changes a table that the input created earlier and
    /// that no statement since may have put rows in.
    on_empty_table: bool,
    /// The first statement of a Laravel migration that reads the database.
    first_read: bool,
    /// A statement that only reads, after the first read of its Laravel
    /// migration: the text gives it one line.
    later_read: bool,
    /// What other measured servers do with the statement, where it is not
    /// what the checked server does.
    others: Vec<OtherServer>,
}

/// Another measured server's answer to a statement.
#[derive(Clone)]
struct OtherServer {
    server: String,
    impact: Impact,
    /// As the report would say it with `--server` naming that server.
    answer: String,
}

impl<'a> Report<'a> {
    /// Checks every statement of `input` as one migration, in order,
    /// starting from the tables in `schema`. Each input runs in a session
    /// of its own. Where an answer may depend on the server, `others` are
    /// checked too, and each statement they answer differently says so.
    pub fn new(
        results: &ServerResults,
        others: &[&ServerResults],
        schema: Option<&(String, Schema)>,
        input: &'a Input,
        server_warnings: Vec<String>,
    ) -> Report<'a> {
        let statements: Vec<Statement> = input
            .sources
            .iter()
            .flat_map(|source| source.statements.iter().cloned())
            .collect();
        let no_tables = Schema::new();
        let tables = schema.map_or(&no_tables, |(_, schema)| schema);
        // How many statements each session has, in order.
        let mut lengths: Vec<(usize, usize)> = Vec::new();
        for source in &input.sources {
            match lengths.last_mut() {
                Some((session, len)) if *session == source.session => {
                    *len += source.statements.len();
                }
                _ => lengths.push((source.session, source.statements.len())),
            }
        }
        let mut rest = statements.as_slice();
        let file_names: Vec<&str> = lengths
            .iter()
            .map(|(session, _)| input.session_names[*session].as_str())
            .collect();
        let sessions: Vec<&[Statement]> = lengths
            .into_iter()
            .map(|(_, len)| {
                let (session, after) = rest.split_at(len);
                rest = after;
                session
            })
            .collect();
        let findings = check_files(&sessions, &file_names, tables, results);
        let impacts = impacts(&statements, &findings);
        let mut others_said: Vec<Vec<OtherServer>> = vec![Vec::new(); statements.len()];
        // When no answer used a result or a rule that differs between the
        // servers, every server answers every statement the same way.
        let server_dependent = findings.iter().any(|finding| finding.server_dependent);
        if server_dependent {
            for other in others {
                let theirs = check_files(&sessions, &file_names, tables, other);
                let their_impacts = kilit_core::impacts(&statements, &theirs);
                for (at, (finding, impact)) in theirs.iter().zip(their_impacts).enumerate() {
                    if (impact, Shown::of(&finding.prediction))
                        != (impacts[at], Shown::of(&findings[at].prediction))
                    {
                        others_said[at].push(OtherServer {
                            server: other.server.name(),
                            impact,
                            answer: other_answer(&finding.prediction, impact, &statements[at]),
                        });
                    }
                }
            }
        }
        #[cfg(debug_assertions)]
        same_everywhere(
            &sessions,
            &statements,
            tables,
            results,
            others,
            server_dependent,
        );
        // Rows may come from lines kilit could not read, or from a statement
        // inside a `/*! */` comment, which kilit does not see.
        let rows_unseen = !input.unreadable.is_empty() || input.runs_comments;
        let on_empty_tables = on_empty_tables(&statements, &findings)
            .into_iter()
            .map(|on_empty_table| on_empty_table && !rows_unseen);

        let locked_in_order = locked_tables_of(&statements, &findings);
        let on_new_tables = on_new_tables(&statements, &findings);
        // Statements on tables kilit does not know, but for a CREATE TABLE.
        let unseen = findings
            .iter()
            .zip(&statements)
            .filter(|(finding, statement)| {
                matches!(
                    finding.table_state,
                    Some(TableSeen::Unseen | TableSeen::Lost)
                ) && !matches!(statement.kind, StatementKind::CreateTable { .. })
            })
            .count();
        let table_states: Vec<Option<&'static str>> = findings
            .iter()
            .zip(on_new_tables)
            .map(|(finding, on_new_table)| {
                finding.table_state.map(|state| match state {
                    TableSeen::Known if on_new_table => "created",
                    TableSeen::Known => "known",
                    TableSeen::Lost => "lost",
                    TableSeen::Unseen => "unseen",
                })
            })
            .collect();
        let mut table_states = table_states.into_iter();
        let mut checked = findings
            .into_iter()
            .zip(impacts)
            .zip(on_empty_tables)
            .zip(others_said);
        let sources: Vec<SourceReport> = input
            .sources
            .iter()
            .map(|source| SourceReport {
                name: &source.name,
                file: source.file.as_deref(),
                kind: source.kind,
                printed: &source.printed,
                statements: source
                    .statements
                    .iter()
                    .enumerate()
                    .map(|(i, statement)| {
                        let (((finding, impact), on_empty_table), others) = checked
                            .next()
                            .expect("check gives one finding per statement");
                        let table_state = table_states
                            .next()
                            .expect("check gives one finding per statement");
                        Checked {
                            statement,
                            table: table_of(statement),
                            table_state,
                            prediction: finding.prediction,
                            impact,
                            on_empty_table,
                            first_read: source.first_read == Some(i),
                            later_read: source.first_read.is_some_and(|first| first < i)
                                && impact == Impact::NotChecked
                                && matches!(statement.kind, StatementKind::Other)
                                && only_reads(&statement.sql),
                            others,
                        }
                    })
                    .collect(),
            })
            .collect();

        // No transaction can be using a table the input created, whatever
        // kilit says about the statement; a DROP TABLE waits although kilit
        // does not check it. Names compare exactly: `orders` and `ORDERS`
        // may be two tables.
        let mut locked_tables: Vec<String> = Vec::new();
        for table in locked_in_order.into_iter().flatten() {
            if !locked_tables.contains(&table) {
                locked_tables.push(table);
            }
        }

        Report {
            server: results.server.name(),
            schema: schema.map(|(file, schema)| SchemaSummary {
                file: file.clone(),
                tables: schema.tables().map(|table| table.name.clone()).collect(),
                lost: schema
                    .lost()
                    .map(|(table, reason)| (table.to_owned(), reason.to_owned()))
                    .collect(),
            }),
            server_warnings,
            sources,
            unreadable: &input.unreadable,
            from_laravel: input.from_laravel,
            laravel_errors: input
                .laravel_errors
                .iter()
                .map(|stop| (stop.input.as_str(), stop.error.message.as_str()))
                .collect(),
            several_inputs: input.inputs() > 1,
            locked_tables,
            on_unknown_tables: unseen,
        }
    }

    fn checked(&self) -> impl Iterator<Item = &Checked<'a>> {
        self.sources.iter().flat_map(|source| &source.statements)
    }

    fn summary(&self) -> Summary {
        let mut summary = Summary::default();
        for checked in self.checked() {
            summary.statements += 1;
            *match checked.impact {
                Impact::Fails => &mut summary.fails,
                Impact::WritesWait => &mut summary.writes_wait,
                Impact::WritesContinue => &mut summary.writes_continue,
                Impact::NewTable => &mut summary.new_table,
                Impact::Unknown => &mut summary.unknown,
                Impact::NotChecked => &mut summary.not_checked,
            } += 1;
        }
        summary
    }

    /// [`EXIT_INCOMPLETE`] if kilit could not read all of the input or
    /// Laravel stopped with an error; else [`EXIT_FOUND`] if a statement
    /// fails or makes writes wait and `counted` counts it; else
    /// [`EXIT_UNKNOWN`] if `counted` counts unknown statements and there
    /// are some; else [`EXIT_PASSED`].
    pub fn exit_code(&self, counted: &[Count]) -> u8 {
        if !self.unreadable.is_empty() || !self.laravel_errors.is_empty() {
            return EXIT_INCOMPLETE;
        }
        let summary = self.summary();
        if (counted.contains(&Count::Fails) && summary.fails > 0)
            || (counted.contains(&Count::Waits) && summary.writes_wait > 0)
        {
            EXIT_FOUND
        } else if counted.contains(&Count::Unknown) && summary.unknown > 0 {
            EXIT_UNKNOWN
        } else {
            EXIT_PASSED
        }
    }

    /// The report for people. `counted` is what the exit status counts.
    pub fn text(&self, counted: &[Count]) -> String {
        let mut out = String::new();
        let _ = self.write_text(&mut out, counted);
        out
    }

    fn write_text(&self, out: &mut String, counted: &[Count]) -> std::fmt::Result {
        writeln!(
            out,
            "checked against {}, as measured by kilit-lab",
            self.server
        )?;
        match &self.schema {
            Some(schema) => {
                let count = schema.tables.len();
                if count == 0 || count > 10 {
                    writeln!(out, "tables from {}: {count}", schema.file)?;
                } else {
                    writeln!(
                        out,
                        "tables from {}: {count} ({})",
                        schema.file,
                        schema.tables.join(", ")
                    )?;
                }
                for (table, reason) in &schema.lost {
                    writeln!(out, "  could not follow {table}: {reason}")?;
                }
                for warning in &self.server_warnings {
                    writeln!(out, "  {warning}")?;
                }
            }
            None if self.on_unknown_tables == 0 => writeln!(
                out,
                "no --schema given: kilit knows only the tables these statements create"
            )?,
            None => {
                let count = self.on_unknown_tables;
                let (statements, get) = if count == 1 {
                    ("statement", "gets")
                } else {
                    ("statements", "get")
                };
                writeln!(
                    out,
                    "no --schema given: {count} {statements} on tables kilit does not know {get} \
                     no answer the table could change"
                )?;
                writeln!(
                    out,
                    "  give kilit the tables with --schema: php artisan schema:dump, or \
                     mysqldump --no-data"
                )?;
            }
        }

        for source in &self.sources {
            writeln!(out)?;
            writeln!(out, "{}", source.name)?;
            if !source.printed.is_empty() {
                writeln!(out, "  note: this migration {PRINTED_NOTE}:")?;
                for printed in source.printed {
                    writeln!(out, "    line {}: {}", printed.line, printed.text)?;
                }
            }
            if source.statements.is_empty() {
                writeln!(out, "  no statements")?;
            }
            for checked in &source.statements {
                let sql = shown_sql(checked);
                // Step 10's note on the first read says what reads mean
                // under --pretend; the later ones need no more words.
                if checked.later_read {
                    writeln!(out, "  read (not checked): {sql}")?;
                    continue;
                }
                match source.kind {
                    SourceKind::Sql => writeln!(out, "  line {}: {sql}", checked.statement.line)?,
                    SourceKind::LaravelMigration => writeln!(out, "  {sql}")?,
                }
                write_prediction(out, checked)?;
            }
        }

        for (input, error) in &self.laravel_errors {
            writeln!(out)?;
            if self.several_inputs {
                writeln!(out, "{input}: {error}")?;
            } else {
                writeln!(out, "{error}")?;
            }
        }

        if !self.unreadable.is_empty() {
            writeln!(out)?;
            writeln!(
                out,
                "kilit could not read these lines, so it did not check what they hide:"
            )?;
            for unreadable in self.unreadable {
                writeln!(
                    out,
                    "  {}, line {}: {}",
                    unreadable.input, unreadable.line.line, unreadable.line.text
                )?;
            }
        }

        if !self.locked_tables.is_empty() {
            writeln!(out)?;
            writeln!(out, "metadata lock on {}:", self.locked_tables.join(", "))?;
            for line in wrap(&metadata_lock_warning(), 76) {
                writeln!(out, "  {line}")?;
            }
            writeln!(out, "  measured by kilit-lab on {}", self.server)?;
        }

        writeln!(out)?;
        let summary = self.summary();
        if summary.statements > 0 {
            writeln!(out, "{}", summary.sentence())?;
            if summary.unknown > 0
                && !counted.contains(&Count::Unknown)
                && self.exit_code(counted) == EXIT_PASSED
            {
                let (statements, them) = if summary.unknown == 1 {
                    ("statement is", "it")
                } else {
                    ("statements are", "them")
                };
                let mut with_unknown = in_order(counted);
                with_unknown.push(Count::Unknown);
                writeln!(
                    out,
                    "{} unknown {statements} not counted in the exit status; \
                     --fail-on {} counts {them}",
                    summary.unknown,
                    names(&with_unknown)
                )?;
            }
        } else if !self.laravel_errors.is_empty() {
            writeln!(
                out,
                "nothing checked: Laravel stopped with an error before it printed a statement"
            )?;
        } else if self.from_laravel && self.sources.is_empty() {
            writeln!(out, "nothing to check: Laravel has no pending migration")?;
        } else {
            writeln!(out, "nothing to check: no SQL statement")?;
        }
        Ok(())
    }

    /// The report for people, then a GitHub Actions annotation for each
    /// statement or warning that needs one. The report echoes the input, so
    /// GitHub reads no workflow command in it: it is printed between
    /// `::stop-commands::` and a token new on every run. `places` says
    /// where the annotations point.
    pub fn github(&self, counted: &[Count], places: &Places) -> String {
        let token = stop_token();
        let mut out = format!("::stop-commands::{token}\n");
        out.push_str(&self.text(counted));
        let _ = writeln!(out, "::{token}::");
        for annotation in self.annotations(counted, places) {
            let _ = writeln!(out, "{annotation}");
        }
        out
    }

    /// The annotations, in the order of the text report. An error is
    /// exactly what makes the exit status other than 0.
    fn annotations(&self, counted: &[Count], places: &Places) -> Vec<Annotation> {
        let mut annotations: Vec<Annotation> = self
            .server_warnings
            .iter()
            .map(|warning| Annotation {
                level: "warning",
                file: None,
                line: None,
                title: "kilit: the schema's server".to_owned(),
                message: warning.clone(),
            })
            .collect();

        for source in &self.sources {
            // A statement of an SQL file points at its line; a migration's
            // file at its first line, as kilit knows no line in it.
            let file = match source.kind {
                SourceKind::Sql => source.file.map(|file| places.shown(file, None)),
                SourceKind::LaravelMigration => places.migration(source.name),
            };
            for checked in &source.statements {
                let Some(level) = level(checked, counted) else {
                    continue;
                };
                let answer = AnswerLine(&checked.prediction).to_string();
                let sql = shown_sql(checked);
                let mut message = format!("{}\n", source.name);
                match source.kind {
                    SourceKind::Sql => {
                        let _ = write!(message, "line {}: {sql}", checked.statement.line);
                    }
                    SourceKind::LaravelMigration => message.push_str(&sql),
                }
                let mut said = String::new();
                let _ = write_prediction(&mut said, checked);
                let rest = said
                    .strip_prefix(&format!("    {answer}\n"))
                    .expect("what the report says about a statement starts with its answer");
                for more in rest.lines() {
                    message.push('\n');
                    message.push_str(more.trim_start());
                }
                annotations.push(Annotation {
                    level,
                    line: file.as_ref().map(|_| match source.kind {
                        SourceKind::Sql => checked.statement.line,
                        SourceKind::LaravelMigration => 1,
                    }),
                    file: file.clone(),
                    title: format!("kilit: {answer}"),
                    message,
                });
            }
        }

        for (input, error) in &self.laravel_errors {
            annotations.push(Annotation {
                level: "error",
                file: None,
                line: None,
                title: "kilit: Laravel stopped with an error".to_owned(),
                message: if self.several_inputs {
                    format!("{input}: {error}")
                } else {
                    (*error).to_owned()
                },
            });
        }

        for unreadable in self.unreadable {
            let file = unreadable
                .file
                .as_deref()
                .map(|file| places.shown(file, None));
            annotations.push(Annotation {
                level: "error",
                line: file.as_ref().map(|_| unreadable.line.line),
                file,
                title: "kilit: could not read this line".to_owned(),
                message: format!(
                    "{}, line {}: {}",
                    unreadable.input, unreadable.line.line, unreadable.line.text
                ),
            });
        }

        if !self.locked_tables.is_empty() {
            annotations.push(Annotation {
                level: "notice",
                file: None,
                line: None,
                title: format!("kilit: metadata lock on {}", self.locked_tables.join(", ")),
                message: format!(
                    "{}\nmeasured by kilit-lab on {}",
                    metadata_lock_warning(),
                    self.server
                ),
            });
        }
        annotations
    }

    /// The report for other programs, as one JSON document. `counted` is
    /// what the exit status counts.
    pub fn json(&self, counted: &[Count]) -> String {
        let report = JsonReport {
            format_version: FORMAT_VERSION,
            kilit: env!("CARGO_PKG_VERSION"),
            server: &self.server,
            schema: self.schema.as_ref().map(|schema| JsonSchema {
                file: &schema.file,
                tables: &schema.tables,
                lost: schema
                    .lost
                    .iter()
                    .map(|(table, reason)| JsonLost { table, reason })
                    .collect(),
            }),
            sources: self
                .sources
                .iter()
                .map(|source| JsonSource {
                    name: source.name,
                    kind: source.kind,
                    statements: source.statements.iter().map(JsonStatement::from).collect(),
                })
                .collect(),
            unreadable: self
                .unreadable
                .iter()
                .map(|unreadable| JsonUnreadable {
                    input: &unreadable.input,
                    line: unreadable.line.line,
                    text: &unreadable.line.text,
                })
                .collect(),
            warnings: self
                .server_warnings
                .iter()
                .map(|warning| JsonWarning {
                    id: "server-mismatch",
                    tables: &[],
                    file: None,
                    message: warning.clone(),
                    migration: None,
                    lines: Vec::new(),
                })
                .chain(
                    self.sources
                        .iter()
                        .filter(|source| !source.printed.is_empty())
                        .map(|source| JsonWarning {
                            id: "migration-output",
                            tables: &[],
                            file: None,
                            message: format!("{} {PRINTED_NOTE}", source.name),
                            migration: Some(source.name),
                            lines: source
                                .printed
                                .iter()
                                .map(|printed| JsonLine {
                                    line: printed.line,
                                    text: &printed.text,
                                })
                                .collect(),
                        }),
                )
                .chain(
                    self.laravel_errors
                        .iter()
                        .map(|(input, error)| JsonWarning {
                            id: "laravel-error",
                            tables: &[],
                            file: Some(input),
                            message: (*error).to_owned(),
                            migration: None,
                            lines: Vec::new(),
                        }),
                )
                .chain((!self.locked_tables.is_empty()).then(|| JsonWarning {
                    id: "metadata-lock",
                    tables: &self.locked_tables,
                    file: None,
                    message: metadata_lock_warning(),
                    migration: None,
                    lines: Vec::new(),
                }))
                .collect(),
            summary: self.summary(),
            fail_on: in_order(counted),
            exit_code: self.exit_code(counted),
        };
        let mut json = serde_json::to_string_pretty(&report).expect("the report is plain data");
        json.push('\n');
        json
    }
}

/// The annotation a statement gets, if any: an error when the exit status
/// counts it, a warning when it would count it with another `--fail-on`,
/// and a notice when it runs but another measured server answers it
/// otherwise.
fn level(checked: &Checked, counted: &[Count]) -> Option<&'static str> {
    let count = match checked.impact {
        Impact::Fails => Count::Fails,
        Impact::WritesWait => Count::Waits,
        Impact::Unknown => Count::Unknown,
        Impact::WritesContinue | Impact::NewTable => {
            return (!checked.others.is_empty()).then_some("notice");
        }
        Impact::NotChecked => return None,
    };
    Some(if counted.contains(&count) {
        "error"
    } else {
        "warning"
    })
}

/// A token for `::stop-commands::` that no input can know: the standard
/// library seeds every hasher's keys at random.
fn stop_token() -> String {
    use std::hash::BuildHasher;
    let random = || std::collections::hash_map::RandomState::new().hash_one(0u8);
    format!("kilit-{:016x}{:016x}", random(), random())
}

/// One GitHub Actions workflow command that annotates the run, or a file.
struct Annotation {
    level: &'static str,
    file: Option<String>,
    line: Option<usize>,
    title: String,
    message: String,
}

/// The longest title GitHub keeps, in characters.
const LONGEST_TITLE: usize = 255;

impl std::fmt::Display for Annotation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "::{} ", self.level)?;
        if let Some(file) = &self.file {
            write!(f, "file={},", escape_property(file))?;
        }
        if let Some(line) = self.line {
            write!(f, "line={line},")?;
        }
        // A title too long for GitHub is cut, and the message starts with
        // all of it.
        if self.title.chars().count() > LONGEST_TITLE {
            let short: String = self.title.chars().take(LONGEST_TITLE - 3).collect();
            write!(
                f,
                "title={}...::{}",
                escape_property(&short),
                escape_data(&format!("{}\n{}", self.title, self.message))
            )
        } else {
            write!(
                f,
                "title={}::{}",
                escape_property(&self.title),
                escape_data(&self.message)
            )
        }
    }
}

/// An error that stops kilit before it can check, as an annotation of the
/// run: the message names input, such as a file or a dump's server line.
pub fn github_error(message: &str) -> String {
    Annotation {
        level: "error",
        file: None,
        line: None,
        title: "kilit: could not check".to_owned(),
        message: message.to_owned(),
    }
    .to_string()
}

/// A workflow command's message as GitHub's runner reads it back: `%`
/// first, then the line breaks, so no input can end the command's line.
fn escape_data(text: &str) -> String {
    text.replace('%', "%25")
        .replace('\r', "%0D")
        .replace('\n', "%0A")
}

/// A workflow command's property value: as a message, and also `:` and
/// `,`, which would end the value.
fn escape_property(text: &str) -> String {
    escape_data(text).replace(':', "%3A").replace(',', "%2C")
}

/// Where annotations point: the current directory, and the repository's
/// folder on the runner (`GITHUB_WORKSPACE`).
pub struct Places {
    current_dir: Option<PathBuf>,
    workspace: Option<PathBuf>,
}

impl Places {
    pub fn new(current_dir: Option<PathBuf>, workspace: Option<PathBuf>) -> Places {
        Places {
            current_dir,
            workspace: workspace.filter(|workspace| !workspace.as_os_str().is_empty()),
        }
    }

    /// `path` relative to the workspace, with `/` between its parts, when
    /// it is inside it; else `as_found`, or `path` as given. The folders
    /// are compared as the system resolves them, links and `..` included,
    /// and by their text only when the system cannot resolve them.
    fn shown(&self, path: &Path, as_found: Option<&str>) -> String {
        if let (Some(current_dir), Some(workspace)) = (&self.current_dir, &self.workspace) {
            let file = current_dir.join(path);
            let workspace = current_dir.join(workspace);
            let inside = match (resolved(&file), std::fs::canonicalize(&workspace)) {
                (Some(file), Ok(workspace)) => relative(&file, &workspace),
                _ => relative(&lexical(&file), &lexical(&workspace)),
            };
            if let Some(inside) = inside {
                return inside;
            }
        }
        as_found.map_or_else(|| path.display().to_string(), str::to_owned)
    }

    /// The file of the Laravel migration `name`, where `make:migration`
    /// puts it: `database/migrations/<name>.php` under the current
    /// directory, if it is there. kilit looks nowhere else and reads
    /// nothing in it.
    fn migration(&self, name: &str) -> Option<String> {
        let plain = !name.is_empty()
            && !name.contains(['/', '\\', ':'])
            && matches!(
                Path::new(name).components().collect::<Vec<_>>().as_slice(),
                [Component::Normal(part)] if *part == name
            );
        if !plain {
            return None;
        }
        let file = Path::new("database")
            .join("migrations")
            .join(format!("{name}.php"));
        let current_dir = self.current_dir.as_ref()?;
        current_dir
            .join(&file)
            .is_file()
            .then(|| self.shown(&file, Some(&format!("database/migrations/{name}.php"))))
    }
}

/// `file` in its folder as the system resolves that folder. The file's own
/// name is kept: a link in the repository is the repository's file.
fn resolved(file: &Path) -> Option<PathBuf> {
    let name = file.file_name()?;
    let folder = std::fs::canonicalize(file.parent()?).ok()?;
    Some(folder.join(name))
}

/// `file` relative to `folder` with `/` between its parts, if it is inside.
fn relative(file: &Path, folder: &Path) -> Option<String> {
    let inside = file.strip_prefix(folder).ok()?;
    let parts: Option<Vec<&str>> = inside
        .components()
        .map(|part| match part {
            Component::Normal(part) => part.to_str(),
            _ => None,
        })
        .collect();
    parts
        .filter(|parts| !parts.is_empty())
        .map(|parts| parts.join("/"))
}

/// `path` with `.` and `..` taken out by its text alone.
fn lexical(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for part in path.components() {
        match part {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            part => out.push(part),
        }
    }
    out
}

/// The statement as printed: in full when kilit has something to say about
/// it, shortened when it does not check it, such as a long CREATE TABLE.
fn shown_sql(checked: &Checked) -> String {
    const LONGEST: usize = 100;
    let sql = &checked.statement.sql;
    if checked.impact == Impact::NotChecked && sql.chars().count() > LONGEST {
        let short: String = sql.chars().take(LONGEST - 3).collect();
        format!("{short}...")
    } else {
        sql.clone()
    }
}

/// The first line of what the report says about a statement: what it does,
/// or why kilit cannot say.
struct AnswerLine<'p>(&'p Prediction);

impl std::fmt::Display for AnswerLine<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.0 {
            // The lab's error message names the lab's own tables and keys;
            // the notes say why this statement fails.
            Prediction::Measured {
                verdict: Verdict::Fails { code, .. },
                ..
            } => write!(f, "fails with error {code}"),
            Prediction::Measured { verdict, .. } => write!(f, "{verdict}"),
            Prediction::DependsOnTable {
                shared: Shared::WritesWait,
                ..
            } => f.write_str("writes wait; the algorithm and the rebuild depend on the table"),
            Prediction::DependsOnTable {
                shared: Shared::Fails,
                ..
            } => f.write_str("fails; the error depends on the table"),
            Prediction::Unknown { reason, .. } => {
                write!(f, "UNKNOWN: {}", reason.lines().next().unwrap_or_default())
            }
            Prediction::NotChecked { reason } => write!(f, "not checked: {reason}"),
        }
    }
}

/// What another server's report says first about a statement: its answer
/// line, and for a table the input created earlier, that nothing waits.
fn other_answer(prediction: &Prediction, impact: Impact, statement: &Statement) -> String {
    let answer = AnswerLine(prediction).to_string();
    if impact == Impact::NewTable {
        let table = changed_table(statement).unwrap_or("the table");
        format!("{answer}; {table} is created earlier in these migrations, so nothing waits for it")
    } else {
        answer
    }
}

/// What an answer shows, to compare two servers' answers: notes and reasons
/// left out.
#[derive(PartialEq, Eq)]
enum Shown {
    Fails(u16),
    Runs {
        algorithm: Option<Algorithm>,
        rebuilds_table: bool,
        copies_rows: bool,
        writes: Writes,
    },
    DependsOnTable(Shared),
    Unknown,
    NotChecked,
}

impl Shown {
    fn of(prediction: &Prediction) -> Shown {
        match prediction {
            Prediction::Measured {
                verdict: Verdict::Fails { code, .. },
                ..
            } => Shown::Fails(*code),
            Prediction::Measured {
                verdict:
                    Verdict::Runs {
                        algorithm,
                        rebuilds_table,
                        copied_rows,
                        writes,
                        ..
                    },
                ..
            } => Shown::Runs {
                algorithm: *algorithm,
                rebuilds_table: *rebuilds_table,
                copies_rows: copied_rows.is_some_and(|rows| rows > 0),
                writes: *writes,
            },
            Prediction::DependsOnTable { shared, .. } => Shown::DependsOnTable(*shared),
            Prediction::Unknown { .. } => Shown::Unknown,
            Prediction::NotChecked { .. } => Shown::NotChecked,
        }
    }
}

/// In debug builds, which the tests run: checks that every server whose
/// answers depend on no server-specific result answers each statement as
/// every other server does, so skipping the other servers hides nothing.
#[cfg(debug_assertions)]
fn same_everywhere(
    sessions: &[&[Statement]],
    statements: &[Statement],
    tables: &Schema,
    results: &ServerResults,
    others: &[&ServerResults],
    server_dependent: bool,
) {
    let all: Vec<&ServerResults> = std::iter::once(results)
        .chain(others.iter().copied())
        .collect();
    let said: Vec<(Vec<kilit_core::Finding>, Vec<Impact>)> = all
        .iter()
        .map(|server| {
            let findings = check_files(sessions, &[], tables, server);
            let impacts = impacts(statements, &findings);
            (findings, impacts)
        })
        .collect();
    for (at, (findings, impacts)) in said.iter().enumerate() {
        let skips = if at == 0 {
            !server_dependent
        } else {
            !findings.iter().any(|finding| finding.server_dependent)
        };
        if !skips {
            continue;
        }
        for (other, (their_findings, their_impacts)) in said.iter().enumerate() {
            for i in 0..statements.len() {
                assert!(
                    (impacts[i], Shown::of(&findings[i].prediction))
                        == (their_impacts[i], Shown::of(&their_findings[i].prediction)),
                    "{} skips the other servers, but {} answers line {} otherwise",
                    all[at].server.name(),
                    all[other].server.name(),
                    statements[i].line
                );
            }
        }
    }
}

fn write_prediction(out: &mut String, checked: &Checked) -> std::fmt::Result {
    match &checked.prediction {
        Prediction::Measured {
            case,
            evidence,
            notes,
            ..
        } => {
            writeln!(out, "    {}", AnswerLine(&checked.prediction))?;
            let plural = if evidence.len() == 1 { "" } else { "s" };
            writeln!(
                out,
                "    measured as {case} ({} lab scenario{plural})",
                evidence.len()
            )?;
            // A table with no rows is in no situation that depends on rows.
            for note in notes
                .iter()
                .filter(|note| !(checked.on_empty_table && depends_on_rows(note)))
            {
                writeln!(out, "    - {note}")?;
            }
        }
        Prediction::DependsOnTable { reason, notes, .. } => {
            writeln!(out, "    {}", AnswerLine(&checked.prediction))?;
            writeln!(out, "    {reason}")?;
            for note in notes
                .iter()
                .filter(|note| !(checked.on_empty_table && depends_on_rows(note)))
            {
                writeln!(out, "    - {note}")?;
            }
        }
        Prediction::Unknown { reason, notes } => {
            writeln!(out, "    {}", AnswerLine(&checked.prediction))?;
            for line in reason.lines().skip(1) {
                writeln!(out, "      {line}")?;
            }
            for note in notes {
                writeln!(out, "    - {note}")?;
            }
        }
        Prediction::NotChecked { .. } => {
            writeln!(out, "    {}", AnswerLine(&checked.prediction))?;
        }
    }
    if checked.impact == Impact::NewTable {
        let table = changed_table(checked.statement).unwrap_or("the table");
        writeln!(
            out,
            "    - {table} is created earlier in these migrations: nothing else uses it yet, \
             so nothing waits for it"
        )?;
    }
    if checked.first_read {
        writeln!(out, "    - {PRETEND_READ_NOTE}")?;
    }
    for other in &checked.others {
        writeln!(out, "    - on {}: {}", other.server, other.answer)?;
    }
    Ok(())
}

/// The table a statement changes, by the name it has before it runs.
fn changed_table(statement: &Statement) -> Option<&str> {
    match &statement.kind {
        StatementKind::Alter(alter) => Some(&alter.table),
        StatementKind::RenameTable { renames } => {
            renames.first().map(|rename| rename.from.as_str())
        }
        StatementKind::Unparsed { table, .. } => table.as_deref(),
        _ => None,
    }
}

/// `text` in lines of at most `width` characters, broken at spaces.
pub fn wrap(text: &str, width: usize) -> Vec<String> {
    let mut lines = Vec::new();
    let mut line = String::new();
    for word in text.split_whitespace() {
        if !line.is_empty() && line.chars().count() + 1 + word.chars().count() > width {
            lines.push(std::mem::take(&mut line));
        }
        if !line.is_empty() {
            line.push(' ');
        }
        line.push_str(word);
    }
    if !line.is_empty() {
        lines.push(line);
    }
    lines
}

/// How many statements have each impact.
#[derive(Default, Serialize)]
struct Summary {
    statements: usize,
    fails: usize,
    writes_wait: usize,
    writes_continue: usize,
    new_table: usize,
    unknown: usize,
    not_checked: usize,
}

impl Summary {
    /// `5 statements: 1 fails, 2 make writes wait, ...`, worst first.
    fn sentence(&self) -> String {
        let parts: Vec<String> = [
            (self.fails, "fails", "fail"),
            (self.writes_wait, "makes writes wait", "make writes wait"),
            (self.unknown, "unknown", "unknown"),
            (
                self.writes_continue,
                "runs while writes continue",
                "run while writes continue",
            ),
            (
                self.new_table,
                "changes a table created here",
                "change tables created here",
            ),
            (self.not_checked, "not checked", "not checked"),
        ]
        .into_iter()
        .filter(|(count, ..)| *count > 0)
        .map(|(count, one, many)| format!("{count} {}", if count == 1 { one } else { many }))
        .collect();
        let plural = if self.statements == 1 { "" } else { "s" };
        format!(
            "{} statement{plural}: {}",
            self.statements,
            parts.join(", ")
        )
    }
}

/// The version of the JSON format: a new minor version adds fields, a new
/// major version removes, renames or changes one.
const FORMAT_VERSION: &str = "1.1";

/// What kilit says about text a Laravel migration printed itself.
const PRINTED_NOTE: &str = "printed these lines itself, between its name and its statements; \
     kilit read them as text, not as SQL";

/// The names of `counted` as `--fail-on` takes them.
fn names(counted: &[Count]) -> String {
    counted
        .iter()
        .filter_map(|count| count.to_possible_value())
        .map(|value| value.get_name().to_owned())
        .collect::<Vec<_>>()
        .join(",")
}

#[derive(Serialize)]
struct JsonReport<'a> {
    format_version: &'static str,
    kilit: &'static str,
    server: &'a str,
    schema: Option<JsonSchema<'a>>,
    sources: Vec<JsonSource<'a>>,
    unreadable: Vec<JsonUnreadable<'a>>,
    warnings: Vec<JsonWarning<'a>>,
    summary: Summary,
    fail_on: Vec<Count>,
    exit_code: u8,
}

#[derive(Serialize)]
struct JsonSchema<'a> {
    file: &'a str,
    tables: &'a [String],
    lost: Vec<JsonLost<'a>>,
}

#[derive(Serialize)]
struct JsonLost<'a> {
    table: &'a str,
    reason: &'a str,
}

#[derive(Serialize)]
struct JsonSource<'a> {
    name: &'a str,
    kind: SourceKind,
    statements: Vec<JsonStatement<'a>>,
}

/// One statement. Which fields are there depends on what kilit says.
#[derive(Serialize)]
struct JsonStatement<'a> {
    line: usize,
    sql: &'a str,
    /// The table the statement is on, as written, or null.
    table: Option<&'a str>,
    /// `known`, `created`, `lost`, `unseen`, or null.
    table_state: Option<&'static str>,
    impact: Impact,
    #[serde(skip_serializing_if = "Option::is_none")]
    case: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    algorithm: Option<Algorithm>,
    #[serde(skip_serializing_if = "Option::is_none")]
    rebuilds_table: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    copies_rows: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    writes: Option<Writes>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<u16>,
    #[serde(skip_serializing_if = "<[_]>::is_empty")]
    evidence: &'a [&'static str],
    #[serde(skip_serializing_if = "<[_]>::is_empty")]
    notes: Vec<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    reason: Option<&'a str>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    other_servers: Vec<JsonOtherServer<'a>>,
}

#[derive(Serialize)]
struct JsonOtherServer<'a> {
    server: &'a str,
    impact: Impact,
    answer: &'a str,
}

impl<'a> From<&'a Checked<'a>> for JsonStatement<'a> {
    fn from(checked: &'a Checked<'a>) -> JsonStatement<'a> {
        let mut json = JsonStatement {
            line: checked.statement.line,
            sql: &checked.statement.sql,
            table: checked.table,
            table_state: checked.table_state,
            impact: checked.impact,
            case: None,
            algorithm: None,
            rebuilds_table: None,
            copies_rows: None,
            writes: None,
            error: None,
            evidence: &[],
            notes: Vec::new(),
            reason: None,
            other_servers: checked
                .others
                .iter()
                .map(|other| JsonOtherServer {
                    server: &other.server,
                    impact: other.impact,
                    answer: &other.answer,
                })
                .collect(),
        };
        match &checked.prediction {
            Prediction::Measured {
                case,
                verdict,
                evidence,
                notes,
            } => {
                json.case = Some(case);
                json.evidence = evidence;
                json.notes = notes.iter().map(String::as_str).collect();
                match verdict {
                    Verdict::Runs {
                        algorithm,
                        rebuilds_table,
                        copied_rows,
                        writes,
                        ..
                    } => {
                        json.algorithm = *algorithm;
                        json.rebuilds_table = Some(*rebuilds_table);
                        json.copies_rows = Some(copied_rows.is_some_and(|rows| rows > 0));
                        json.writes = Some(*writes);
                    }
                    Verdict::Fails { code, .. } => json.error = Some(*code),
                }
            }
            Prediction::DependsOnTable {
                case,
                shared,
                evidence,
                reason,
                notes,
            } => {
                json.case = Some(case);
                json.evidence = evidence;
                json.notes = notes.iter().map(String::as_str).collect();
                json.reason = Some(reason);
                if *shared == Shared::WritesWait {
                    json.writes = Some(Writes::Blocked);
                }
            }
            Prediction::Unknown { reason, notes } => {
                json.reason = Some(reason);
                json.notes = notes.iter().map(String::as_str).collect();
            }
            Prediction::NotChecked { reason } => {
                json.reason = Some(reason);
            }
        }
        if checked.first_read {
            json.notes.push(PRETEND_READ_NOTE);
        }
        json
    }
}

#[derive(Serialize)]
struct JsonUnreadable<'a> {
    input: &'a str,
    line: usize,
    text: &'a str,
}

#[derive(Serialize)]
struct JsonWarning<'a> {
    id: &'static str,
    tables: &'a [String],
    /// The input a Laravel error screen is in.
    #[serde(skip_serializing_if = "Option::is_none")]
    file: Option<&'a str>,
    message: String,
    /// The Laravel migration that printed text of its own.
    #[serde(skip_serializing_if = "Option::is_none")]
    migration: Option<&'a str>,
    /// The lines it printed.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    lines: Vec<JsonLine<'a>>,
}

#[derive(Serialize)]
struct JsonLine<'a> {
    line: usize,
    text: &'a str,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_what_is_counted_in_a_fixed_order_once() {
        let counted = in_order(&[Count::Unknown, Count::Waits, Count::Fails, Count::Waits]);
        assert_eq!(names(&counted), "fails,waits,unknown");
        assert_eq!(names(&in_order(&[Count::Waits])), "waits");
    }

    #[test]
    fn wraps_at_spaces() {
        assert_eq!(wrap("one two three four", 9), ["one two", "three", "four"]);
        assert_eq!(wrap("", 9), Vec::<String>::new());
    }

    #[test]
    fn escapes_what_ends_a_workflow_command() {
        assert_eq!(escape_data("50% a::b,c\r\nd"), "50%25 a::b,c%0D%0Ad");
        assert_eq!(
            escape_property("50% a::b,c\r\nd"),
            "50%25 a%3A%3Ab%2Cc%0D%0Ad"
        );
        assert_eq!(escape_data("%0A"), "%250A");
    }

    #[test]
    fn an_annotation_names_its_file_and_line_only_when_it_has_them() {
        let mut annotation = Annotation {
            level: "error",
            file: Some("db/a,b.sql".to_owned()),
            line: Some(4),
            title: "kilit: fails".to_owned(),
            message: "one\ntwo".to_owned(),
        };
        assert_eq!(
            annotation.to_string(),
            "::error file=db/a%2Cb.sql,line=4,title=kilit%3A fails::one%0Atwo"
        );
        annotation.file = None;
        annotation.line = None;
        assert_eq!(
            annotation.to_string(),
            "::error title=kilit%3A fails::one%0Atwo"
        );
    }

    #[test]
    fn the_stop_token_is_new_on_every_run() {
        let token = stop_token();
        let hex = token.strip_prefix("kilit-").unwrap();
        assert_eq!(hex.len(), 32);
        assert!(
            hex.bytes()
                .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
        );
        assert_ne!(token, stop_token());
    }

    #[cfg(unix)]
    #[test]
    fn a_path_inside_the_workspace_is_shown_relative_to_it() {
        let places = Places::new(Some(PathBuf::from("/w/app")), Some(PathBuf::from("/w/./")));
        assert_eq!(places.shown(Path::new("db/a.sql"), None), "app/db/a.sql");
        assert_eq!(places.shown(Path::new("./../x/a.sql"), None), "x/a.sql");
        assert_eq!(places.shown(Path::new("/w/b.sql"), None), "b.sql");
        assert_eq!(places.shown(Path::new("../../o.sql"), None), "../../o.sql");
        assert_eq!(places.shown(Path::new("/o/a.sql"), None), "/o/a.sql");
        assert_eq!(places.shown(Path::new("/w"), Some("found")), "found");
        let outside = Places::new(Some(PathBuf::from("/w/app")), Some(PathBuf::new()));
        assert_eq!(outside.shown(Path::new("./a.sql"), None), "./a.sql");
    }

    #[cfg(windows)]
    #[test]
    fn a_windows_path_inside_the_workspace_is_shown_with_slashes() {
        let places = Places::new(
            Some(PathBuf::from(r"D:\a\kilit\kilit\demo")),
            Some(PathBuf::from(r"D:\a\kilit\kilit")),
        );
        assert_eq!(
            places.shown(Path::new(r"database\migrations\m.php"), None),
            "demo/database/migrations/m.php"
        );
    }

    #[test]
    fn only_a_plain_migration_name_gets_a_file() {
        let places = Places::new(Some(std::env::temp_dir()), None);
        for name in ["", ".", "..", "../x", "a/b", "a\\b", "c:x"] {
            assert_eq!(places.migration(name), None, "{name}");
        }
    }

    #[test]
    fn the_summary_puts_the_worst_first() {
        let summary = Summary {
            statements: 6,
            fails: 1,
            writes_wait: 2,
            writes_continue: 1,
            not_checked: 2,
            ..Summary::default()
        };
        assert_eq!(
            summary.sentence(),
            "6 statements: 1 fails, 2 make writes wait, 1 runs while writes continue, \
             2 not checked"
        );
    }
}
