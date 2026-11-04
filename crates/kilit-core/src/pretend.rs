//! The output of `php artisan migrate --pretend`: the SQL each pending
//! Laravel migration would run, without running it.
//!
//! Laravel prints it like this, with colors in a terminal:
//!
//! ```text
//!    INFO  Running migrations.
//!
//!   2026_11_01_000001_add_coupon_to_orders_table ...............................
//!   ⇂ alter table `orders` add `coupon` varchar(255) null after `amount`
//! ```
//!
//! The code that prints it has not changed since Laravel 9.21. Seen on
//! Laravel 10, 11, 12 and 13, piped into a file and in terminals down to 60
//! columns:
//!
//! - every statement is on one line, however long;
//! - a migration name too wide for the terminal gets no dots;
//! - every run of spaces, tabs and line breaks becomes one space, even inside
//!   quotes, so `DEFAULT 'a  b'` reads as `DEFAULT 'a b'`;
//! - a statement that ends with `.` or `?` loses that character;
//! - `<`, `>` and `&` come through as they are.

use crate::parse::parse;
use crate::statement::Statement;

/// The mark Laravel puts before each statement.
const STATEMENT_MARK: char = '⇂';

/// One migration in the output.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LaravelMigration {
    /// The migration's file name without `.php`, or its class name for a
    /// migration that is not an anonymous class.
    pub name: String,
    /// What the migration would run, in order. Their lines are lines of the
    /// output.
    pub statements: Vec<Statement>,
}

/// What kilit read from the output of `migrate --pretend`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Pretend {
    /// The pending migrations, in the order Laravel would run them. Empty
    /// when Laravel says there is nothing to migrate.
    pub migrations: Vec<LaravelMigration>,
    /// Lines that are neither a migration nor a statement, such as an error
    /// Laravel printed half way. Whatever they hide, kilit did not check.
    pub unreadable: Vec<UnreadableLine>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UnreadableLine {
    /// Counting from 1.
    pub line: usize,
    pub text: String,
}

/// Reads `text` as the output of `php artisan migrate --pretend`, or returns
/// `None` if it does not look like that output, so it can be read as SQL.
///
/// ```
/// let pretend = kilit_core::read_pretend(
///     "   INFO  Running migrations.\n\n  \
///      2026_11_01_000001_add_coupon_to_orders_table .........\n  \
///      ⇂ alter table `orders` add `coupon` varchar(255) null\n",
/// )
/// .unwrap();
/// assert_eq!(pretend.migrations[0].name, "2026_11_01_000001_add_coupon_to_orders_table");
/// assert_eq!(pretend.migrations[0].statements[0].line, 4);
/// assert!(kilit_core::read_pretend("alter table orders drop note;").is_none());
/// ```
pub fn read_pretend(text: &str) -> Option<Pretend> {
    let lines: Vec<String> = text
        .lines()
        .map(|line| plain(line).trim().to_owned())
        .collect();
    if !lines.iter().any(|line| is_from_laravel(line)) {
        return None;
    }

    let mut pretend = Pretend::default();
    for (i, line) in lines.iter().enumerate() {
        let number = i + 1;
        match read_line(line) {
            Line::Blank | Line::Info | Line::Task => {}
            Line::Migration(name) => pretend.migrations.push(LaravelMigration {
                name: name.to_owned(),
                statements: Vec::new(),
            }),
            Line::Statement(sql) => match pretend.migrations.last_mut() {
                Some(migration) => {
                    for mut statement in parse(sql) {
                        statement.line = number;
                        migration.statements.push(statement);
                    }
                }
                // Laravel names the migration first; without the name the
                // output is cut or mixed up.
                None => pretend.unreadable.push(UnreadableLine {
                    line: number,
                    text: line.clone(),
                }),
            },
            Line::Other => pretend.unreadable.push(UnreadableLine {
                line: number,
                text: line.clone(),
            }),
        }
    }
    Some(pretend)
}

/// A line of the output, without colors and surrounding spaces.
#[derive(Debug, PartialEq, Eq)]
enum Line<'a> {
    Blank,
    /// `INFO  Running migrations.` and the like.
    Info,
    /// The start of a migration: its name, then a row of dots.
    Migration(&'a str),
    /// Laravel's own work, such as `Creating migration table .... 15.11ms DONE`.
    Task,
    Statement(&'a str),
    Other,
}

fn read_line(line: &str) -> Line<'_> {
    if line.is_empty() {
        return Line::Blank;
    }
    if let Some(sql) = statement(line) {
        return Line::Statement(sql);
    }
    if line.starts_with("INFO ") {
        return Line::Info;
    }
    // A migration's name has no spaces. Laravel fills the rest of the line
    // with dots, as many as fit: none for a name as wide as the terminal,
    // two when it detects an AI agent and shortens its output.
    let name = line.trim_end_matches('.').trim_end();
    if !name.is_empty() && !name.contains(char::is_whitespace) {
        return Line::Migration(name);
    }
    if line.contains(" ..") {
        return Line::Task;
    }
    Line::Other
}

/// The SQL after Laravel's mark, `⇂ alter table ...`.
///
/// PowerShell before 7.4 decodes a program's output with the
/// console's code page and encodes it again for the next program in a pipe,
/// so the mark can arrive as `???` or `Γçé`. Any first word of up to three
/// characters that are `?` or not ASCII counts as the mark: Laravel's other
/// lines start with ASCII words.
fn statement(line: &str) -> Option<&str> {
    let (mark, sql) = line.split_once(' ').unwrap_or((line, ""));
    let is_mark =
        (1..=3).contains(&mark.chars().count()) && mark.chars().all(|c| c == '?' || !c.is_ascii());
    is_mark.then(|| sql.trim())
}

/// Lines that only Laravel's pretend output has.
fn is_from_laravel(line: &str) -> bool {
    line.starts_with(STATEMENT_MARK)
        || (line.starts_with("INFO ")
            && (line.ends_with("Running migrations.") || line.ends_with("Nothing to migrate.")))
}

/// `line` without the color codes a terminal would turn into colors: `ESC [`,
/// then parameters, then one letter.
fn plain(line: &str) -> String {
    let mut plain = String::with_capacity(line.len());
    let mut chars = line.chars();
    while let Some(c) = chars.next() {
        if c != '\u{1b}' {
            plain.push(c);
            continue;
        }
        if chars.next() == Some('[') {
            for c in chars.by_ref() {
                if ('\u{40}'..='\u{7e}').contains(&c) {
                    break;
                }
            }
        }
    }
    plain
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::statement::StatementKind;

    /// Lines of `php artisan migrate --pretend > pretend.txt` on Laravel
    /// 13.40. The second migration calls DB::statement() and DB::update(),
    /// the third does nothing.
    const PIPED: &str = "
   INFO  Running migrations.

  2026_11_01_000001_add_coupon_to_orders_table ...............................
  ⇂ alter table `orders` add `coupon` varchar(255) null after `amount`
  2026_11_01_000005_tidy_orders_table ........................................
  ⇂ ALTER TABLE orders ADD FULLTEXT orders_note_fulltext (note)
  ⇂ UPDATE orders SET note = '-' WHERE note = ''
  2026_11_01_000007_does_nothing .............................................


";

    /// The same in an 80-column terminal, recorded with `script`: colors and
    /// carriage returns.
    const TERMINAL: &str = "\r
  \u{1b}[37;44m INFO \u{1b}[39;49m Running migrations.  \r
\r
  2026_11_01_000001_add_coupon_to_orders_table \u{1b}[90m...............................\u{1b}[39m  \r
  \u{1b}[90m⇂ alter table `orders` add `coupon` varchar(255) null after `amount`\u{1b}[39m  \r
  2026_11_01_000008_brackets \u{1b}[90m.................................................\u{1b}[39m  \r
  \u{1b}[90m⇂ ALTER TABLE orders ADD COLUMN first_item VARCHAR(20) AS (JSON_UNQUOTE(JSON_EXTRACT(note, '$.items\u{1b}[39m\u{1b}[1m[0]\u{1b}[22m\u{1b}[90m'))) VIRTUAL\u{1b}[39m  \r
\r
";

    fn names(pretend: &Pretend) -> Vec<&str> {
        pretend
            .migrations
            .iter()
            .map(|migration| migration.name.as_str())
            .collect()
    }

    fn sql(migration: &LaravelMigration) -> Vec<&str> {
        migration
            .statements
            .iter()
            .map(|statement| statement.sql.as_str())
            .collect()
    }

    #[test]
    fn reads_migrations_and_their_statements() {
        let pretend = read_pretend(PIPED).unwrap();
        assert_eq!(
            names(&pretend),
            [
                "2026_11_01_000001_add_coupon_to_orders_table",
                "2026_11_01_000005_tidy_orders_table",
                "2026_11_01_000007_does_nothing",
            ]
        );
        assert_eq!(
            sql(&pretend.migrations[1]),
            [
                "ALTER TABLE orders ADD FULLTEXT orders_note_fulltext (note)",
                "UPDATE orders SET note = '-' WHERE note = ''",
            ]
        );
        assert!(pretend.migrations[2].statements.is_empty());
        assert!(pretend.unreadable.is_empty());
    }

    #[test]
    fn statements_keep_their_line_in_the_output() {
        let pretend = read_pretend(PIPED).unwrap();
        let lines: Vec<usize> = pretend
            .migrations
            .iter()
            .flat_map(|migration| &migration.statements)
            .map(|statement| statement.line)
            .collect();
        assert_eq!(lines, [5, 7, 8]);
    }

    #[test]
    fn statements_are_read_like_any_sql() {
        let pretend = read_pretend(PIPED).unwrap();
        let StatementKind::Alter(alter) = &pretend.migrations[0].statements[0].kind else {
            panic!("expected an ALTER TABLE");
        };
        assert_eq!(alter.table, "orders");
        assert_eq!(
            pretend.migrations[1].statements[1].kind,
            StatementKind::Other
        );
    }

    #[test]
    fn colors_and_carriage_returns_make_no_difference() {
        let pretend = read_pretend(TERMINAL).unwrap();
        assert_eq!(
            sql(&pretend.migrations[0]),
            ["alter table `orders` add `coupon` varchar(255) null after `amount`"]
        );
        assert_eq!(
            sql(&pretend.migrations[1]),
            ["ALTER TABLE orders ADD COLUMN first_item VARCHAR(20) \
                 AS (JSON_UNQUOTE(JSON_EXTRACT(note, '$.items[0]'))) VIRTUAL"]
        );
        assert!(pretend.unreadable.is_empty());
    }

    #[test]
    fn reads_a_mark_that_windows_powershell_mangled() {
        for mark in ["???", "Γçé"] {
            let pretend = read_pretend(&format!(
                "   INFO  Running migrations.\n\n  \
                 2026_11_01_000001_add_coupon_to_orders_table .........\n  \
                 {mark} alter table `orders` add `coupon` varchar(255) null\n"
            ))
            .unwrap();
            assert_eq!(
                sql(&pretend.migrations[0]),
                ["alter table `orders` add `coupon` varchar(255) null"],
                "{mark}"
            );
        }
    }

    #[test]
    fn nothing_to_migrate_is_no_migration() {
        let pretend = read_pretend("\n   INFO  Nothing to migrate.  \n\n").unwrap();
        assert_eq!(pretend, Pretend::default());
    }

    #[test]
    fn preparing_a_new_database_is_laravel_s_own_work() {
        let pretend = read_pretend(
            "
   INFO  Preparing database.

  Creating migration table ...................................... 15.11ms DONE

   INFO  Running migrations.

  2026_01_10_000000_create_shops_table .......................................
  ⇂ create table `shops` (`id` bigint unsigned not null auto_increment primary key, `name` varchar(255) not null, `created_at` timestamp null, `updated_at` timestamp null) default character set utf8mb4 collate 'utf8mb4_unicode_ci'
",
        )
        .unwrap();
        assert_eq!(names(&pretend), ["2026_01_10_000000_create_shops_table"]);
        assert!(matches!(
            pretend.migrations[0].statements[0].kind,
            StatementKind::CreateTable { .. }
        ));
        assert!(pretend.unreadable.is_empty());
    }

    #[test]
    fn the_shorter_output_for_ai_agents_reads_the_same() {
        // Laravel 13 prints this when it detects an AI agent, such as
        // Claude Code, through environment variables.
        let pretend = read_pretend(
            "
 INFO Running migrations.

 2026_11_01_000001_add_coupon_to_orders_table ..
 ⇂ alter table `orders` add `coupon` varchar(255) null after `amount`
",
        )
        .unwrap();
        assert_eq!(
            names(&pretend),
            ["2026_11_01_000001_add_coupon_to_orders_table"]
        );
        assert_eq!(pretend.migrations[0].statements.len(), 1);
    }

    #[test]
    fn a_name_too_wide_for_the_terminal_has_no_dots() {
        let pretend = read_pretend(
            "
   INFO  Running migrations.

  2026_11_01_000009_add_a_very_long_descriptive_column_name_for_the_orders_table_that_goes_past_eighty
  ⇂ select 1
",
        )
        .unwrap();
        assert_eq!(
            names(&pretend),
            [
                "2026_11_01_000009_add_a_very_long_descriptive_column_name_for_the_orders_table_that_goes_past_eighty"
            ]
        );
        assert_eq!(sql(&pretend.migrations[0]), ["select 1"]);
    }

    #[test]
    fn lines_it_does_not_know_are_reported() {
        // An error half way, like the one Laravel 9.21 to 10 print when
        // ->change() needs doctrine/dbal.
        let pretend = read_pretend(
            "
   INFO  Running migrations.

  2026_11_01_000002_widen_amount_on_orders_table .............................

   ERROR  [2026_11_01_000002_widen_amount_on_orders_table] failed to dump queries.
",
        )
        .unwrap();
        assert_eq!(pretend.migrations[0].statements.len(), 0);
        assert_eq!(
            pretend.unreadable,
            [UnreadableLine {
                line: 6,
                text: "ERROR  [2026_11_01_000002_widen_amount_on_orders_table] \
                       failed to dump queries."
                    .into(),
            }]
        );
    }

    #[test]
    fn a_statement_before_any_migration_is_reported() {
        let pretend = read_pretend("⇂ alter table `orders` drop `note`").unwrap();
        assert!(pretend.migrations.is_empty());
        assert_eq!(pretend.unreadable[0].line, 1);
    }

    #[test]
    fn sql_is_not_pretend_output() {
        assert!(read_pretend("ALTER TABLE orders ADD note TEXT;\n").is_none());
        // A row of dots in SQL does not make it Laravel's output.
        assert!(read_pretend("-- step 1 ..........\nALTER TABLE orders DROP note;").is_none());
        assert!(read_pretend("").is_none());
    }

    #[test]
    fn strips_color_codes() {
        assert_eq!(plain("\u{1b}[90m⇂ x\u{1b}[39m"), "⇂ x");
        assert_eq!(plain("\u{1b}[37;44m INFO \u{1b}[39;49m"), " INFO ");
        assert_eq!(plain("no colors"), "no colors");
    }
}
