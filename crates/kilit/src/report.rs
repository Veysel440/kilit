//! What kilit prints: a report for people, or JSON for other programs.

use std::fmt::Write as _;

use kilit_core::{
    Algorithm, Impact, Prediction, Schema, ServerResults, Statement, StatementKind, Verdict,
    Writes, check, impacts, metadata_lock_warning,
};
use serde::Serialize;

use crate::input::{Input, SourceKind, Unreadable};

/// What kilit says about the input.
pub struct Report<'a> {
    server: String,
    schema: Option<SchemaSummary>,
    sources: Vec<SourceReport<'a>>,
    unreadable: &'a [Unreadable],
    from_laravel: bool,
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
}

impl<'a> Report<'a> {
    /// Checks every statement of `input` as one migration, in order,
    /// starting from the tables in `schema`.
    pub fn new(
        results: &ServerResults,
        schema: Option<&(String, Schema)>,
        input: &'a Input,
    ) -> Report<'a> {
        let statements: Vec<Statement> = input
            .sources
            .iter()
            .flat_map(|source| source.statements.iter().cloned())
            .collect();
        let no_tables = Schema::new();
        let tables = schema.map_or(&no_tables, |(_, schema)| schema);
        let findings = check(&statements, tables, results);
        let impacts = impacts(&statements, &findings);

        let mut checked = findings.into_iter().zip(impacts);
        let sources: Vec<SourceReport> = input
            .sources
            .iter()
            .map(|source| SourceReport {
                name: &source.name,
                kind: source.kind,
                statements: source
                    .statements
                    .iter()
                    .map(|statement| {
                        let (finding, impact) = checked
                            .next()
                            .expect("check gives one finding per statement");
                        Checked {
                            statement,
                            prediction: finding.prediction,
                            impact,
                        }
                    })
                    .collect(),
            })
            .collect();

        let mut locked_tables: Vec<String> = Vec::new();
        for checked in sources.iter().flat_map(|source| &source.statements) {
            if matches!(checked.impact, Impact::NewTable | Impact::NotChecked) {
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
            sources,
            unreadable: &input.unreadable,
            from_laravel: input.from_laravel,
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

    /// 0 if nothing fails or makes writes wait, 1 if something does, 2 if
    /// kilit could not read all of the input.
    pub fn exit_code(&self, strict: bool) -> u8 {
        if !self.unreadable.is_empty() {
            return 2;
        }
        let summary = self.summary();
        if summary.fails > 0 || summary.writes_wait > 0 || (strict && summary.unknown > 0) {
            1
        } else {
            0
        }
    }

    /// The report for people.
    pub fn text(&self) -> String {
        let mut out = String::new();
        let _ = self.write_text(&mut out);
        out
    }

    fn write_text(&self, out: &mut String) -> std::fmt::Result {
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
                match source.kind {
                    SourceKind::Sql => writeln!(out, "  line {}: {sql}", checked.statement.line)?,
                    SourceKind::LaravelMigration => writeln!(out, "  {sql}")?,
                }
                write_prediction(out, checked)?;
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
        } else if self.from_laravel && self.sources.is_empty() {
            writeln!(out, "nothing to check: Laravel has no pending migration")?;
        } else {
            writeln!(out, "nothing to check: no SQL statement")?;
        }
        Ok(())
    }

    /// The report for other programs, as one JSON document.
    pub fn json(&self, strict: bool) -> String {
        let report = JsonReport {
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
            warnings: if self.locked_tables.is_empty() {
                Vec::new()
            } else {
                vec![JsonWarning {
                    id: "metadata-lock",
                    tables: &self.locked_tables,
                    message: metadata_lock_warning(),
                }]
            },
            summary: self.summary(),
            exit_code: self.exit_code(strict),
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

fn write_prediction(out: &mut String, checked: &Checked) -> std::fmt::Result {
    match &checked.prediction {
        Prediction::Measured {
            case,
            verdict,
            evidence,
            notes,
        } => {
            match verdict {
                // The lab's error message names the lab's own tables and
                // keys; the notes say why this statement fails.
                Verdict::Fails { code, .. } => writeln!(out, "    fails with error {code}")?,
                runs => writeln!(out, "    {runs}")?,
            }
            let plural = if evidence.len() == 1 { "" } else { "s" };
            writeln!(
                out,
                "    measured as {case} ({} lab scenario{plural})",
                evidence.len()
            )?;
            for note in notes {
                writeln!(out, "    - {note}")?;
            }
        }
        Prediction::Unknown { reason } => {
            for (i, line) in reason.lines().enumerate() {
                let label = if i == 0 { "UNKNOWN: " } else { "  " };
                writeln!(out, "    {label}{line}")?;
            }
        }
        Prediction::NotChecked { reason } => writeln!(out, "    not checked: {reason}")?,
    }
    if checked.impact == Impact::NewTable {
        let table = changed_table(checked.statement).unwrap_or("the table");
        writeln!(
            out,
            "    - {table} is created earlier in these migrations: nothing else uses it yet, \
             so nothing waits for it"
        )?;
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
fn wrap(text: &str, width: usize) -> Vec<String> {
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

#[derive(Serialize)]
struct JsonReport<'a> {
    kilit: &'static str,
    server: &'a str,
    schema: Option<JsonSchema<'a>>,
    sources: Vec<JsonSource<'a>>,
    unreadable: Vec<JsonUnreadable<'a>>,
    warnings: Vec<JsonWarning<'a>>,
    summary: Summary,
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
    notes: &'a [String],
    #[serde(skip_serializing_if = "Option::is_none")]
    reason: Option<&'a str>,
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
            notes: &[],
            reason: None,
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
                json.notes = notes;
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
    message: String,
}

#[cfg(test)]
mod tests {
    use super::*;

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
