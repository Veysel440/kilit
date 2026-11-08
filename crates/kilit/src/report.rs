//! What kilit prints: a report for people, or JSON for other programs.

use std::fmt::Write as _;

use clap::ValueEnum;
use kilit_core::{
    Algorithm, Impact, PRETEND_READ_NOTE, Prediction, Schema, ServerResults, Statement,
    StatementKind, Verdict, Writes, check_sessions, depends_on_rows, impacts,
    metadata_lock_warning, on_empty_tables, on_new_tables, only_reads,
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
    /// Tables that exist before the migration and that an ALTER changes.
    locked_tables: Vec<String>,
}

struct SchemaSummary {
    file: String,
    tables: Vec<String>,
    lost: Vec<(String, String)>,
}

struct SourceReport<'a> {
    name: &'a str,
    kind: SourceKind,
    statements: Vec<Checked<'a>>,
}

struct Checked<'a> {
    statement: &'a Statement,
    prediction: Prediction,
    impact: Impact,
    /// The statement changes only tables that the input created earlier.
    on_new_table: bool,
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
        let sessions: Vec<&[Statement]> = lengths
            .into_iter()
            .map(|(_, len)| {
                let (session, after) = rest.split_at(len);
                rest = after;
                session
            })
            .collect();
        let findings = check_sessions(&sessions, tables, results);
        let impacts = impacts(&statements, &findings);
        let mut others_said: Vec<Vec<OtherServer>> = vec![Vec::new(); statements.len()];
        // When no answer used a result or a rule that differs between the
        // servers, every server answers every statement the same way.
        let server_dependent = findings.iter().any(|finding| finding.server_dependent);
        if server_dependent {
            for other in others {
                let theirs = check_sessions(&sessions, tables, other);
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
        let on_new_tables = on_new_tables(&statements);
        // Rows may come from lines kilit could not read, or from a statement
        // inside a `/*! */` comment, which kilit does not see.
        let rows_unseen = !input.unreadable.is_empty() || input.runs_comments;
        let on_empty_tables = on_empty_tables(&statements)
            .into_iter()
            .map(|on_empty_table| on_empty_table && !rows_unseen);

        let mut checked = findings
            .into_iter()
            .zip(impacts)
            .zip(on_new_tables)
            .zip(on_empty_tables)
            .zip(others_said);
        let sources: Vec<SourceReport> = input
            .sources
            .iter()
            .map(|source| SourceReport {
                name: &source.name,
                kind: source.kind,
                statements: source
                    .statements
                    .iter()
                    .enumerate()
                    .map(|(i, statement)| {
                        let ((((finding, impact), on_new_table), on_empty_table), others) = checked
                            .next()
                            .expect("check gives one finding per statement");
                        Checked {
                            statement,
                            prediction: finding.prediction,
                            impact,
                            on_new_table,
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

        let mut locked_tables: Vec<String> = Vec::new();
        for checked in sources.iter().flat_map(|source| &source.statements) {
            // No transaction can be using a table the input created, whatever
            // kilit says about the statement.
            if checked.on_new_table || checked.impact == Impact::NotChecked {
                continue;
            }
            if let Some(table) = altered_table(checked.statement)
                && !locked_tables
                    .iter()
                    .any(|known| known.eq_ignore_ascii_case(table))
            {
                locked_tables.push(table.to_owned());
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
            None => writeln!(
                out,
                "no --schema given: kilit knows no table, so it cannot judge MODIFY and CHANGE"
            )?,
        }

        for source in &self.sources {
            writeln!(out)?;
            writeln!(out, "{}", source.name)?;
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
                })
                .chain(
                    self.laravel_errors
                        .iter()
                        .map(|(input, error)| JsonWarning {
                            id: "laravel-error",
                            tables: &[],
                            file: Some(input),
                            message: (*error).to_owned(),
                        }),
                )
                .chain((!self.locked_tables.is_empty()).then(|| JsonWarning {
                    id: "metadata-lock",
                    tables: &self.locked_tables,
                    file: None,
                    message: metadata_lock_warning(),
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

/// The table an ALTER TABLE changes, also when kilit could not read the rest.
fn altered_table(statement: &Statement) -> Option<&str> {
    match &statement.kind {
        StatementKind::Alter(alter) => Some(&alter.table),
        StatementKind::Unparsed {
            table: Some(table), ..
        } if statement
            .sql
            .get(..5)
            .is_some_and(|word| word.eq_ignore_ascii_case("alter")) =>
        {
            Some(table)
        }
        _ => None,
    }
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
            Prediction::Unknown { reason } => {
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
            let findings = check_sessions(sessions, tables, server);
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
        Prediction::Unknown { reason } => {
            writeln!(out, "    {}", AnswerLine(&checked.prediction))?;
            for line in reason.lines().skip(1) {
                writeln!(out, "      {line}")?;
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
const FORMAT_VERSION: &str = "1.0";

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
            Prediction::Unknown { reason } | Prediction::NotChecked { reason } => {
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
