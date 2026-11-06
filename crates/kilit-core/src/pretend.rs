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
//!
//! When a migration throws, Laravel prints the migration's name, none of its
//! statements, and an error screen, and stops:
//!
//! ```text
//!   2026_11_01_000002_widen_amount_on_orders_table .............................
//!
//!    RuntimeException
//!
//!   boom from a migration
//!
//!   at database/migrations/2026_11_01_000002_widen_amount_on_orders_table.php:15
//! ```
//!
//! An error in a migration's PHP syntax stops Laravel before it prints
//! anything else, so the error screen is the whole output.

use crate::parse::parse;
use crate::statement::Statement;

/// The mark Laravel puts before each statement.
const STATEMENT_MARK: char = '⇂';

/// The mark after Windows PowerShell before 7.4 passed it on in a pipe. It
/// decodes a program's output with the console's code page and encodes it
/// again for the next program: as `?` or `???` when it encodes ASCII, or as
/// the mark's three UTF-8 bytes read in one of the single-byte code pages
/// Windows consoles and files use. (869 and 874 have no character for some
/// of the bytes; what PowerShell writes there, and in the double-byte code
/// pages of East Asian Windows, is not known.)
const MANGLED_MARKS: [&str; 16] = [
    "?",   // decoded right, encoded as ASCII
    "???", // decoded byte by byte, encoded as ASCII
    "Γçé", // 437, 860, 861, 863, 865
    "ظçé", // 720
    "έΘΓ", // 737
    "Ōćé", // 775
    "Ôçé", // 850, 852, 857, 858
    "РЄѓ", // 855
    "Γחג", // 862
    "ﻗ┼∙", // 864
    "тЗВ", // 866
    "â‡‚", // 1250, 1252, 1254, 1256, 1258
    "в‡‚", // 1251
    "β‡‚", // 1253
    "ג‡‚", // 1255
    "ā‡‚", // 1257
];

/// What kilit says about a migration that reads the database.
pub const PRETEND_READ_NOTE: &str = "this migration reads the database: under --pretend \
     every read returns nothing, so statements that depend on what it reads may be missing \
     here, or may appear although they would not run";

/// One migration in the output.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LaravelMigration {
    /// The migration's file name without `.php`, or its class name for a
    /// migration that is not an anonymous class.
    pub name: String,
    /// What the migration would run, in order. Their lines are lines of the
    /// output.
    pub statements: Vec<Statement>,
    /// Which of `statements` is the first that asks the database something,
    /// such as the `select exists (...)` of `Schema::hasTable()`.
    pub first_read: Option<usize>,
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
    /// Laravel stopped with an error. Nothing on its error screen is read.
    pub error: Option<LaravelError>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UnreadableLine {
    /// Counting from 1.
    pub line: usize,
    pub text: String,
}

/// Laravel's error screen, which ends the output.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LaravelError {
    /// The line the error screen starts at, counting from 1.
    pub line: usize,
    /// The migration Laravel stopped in, when the output names it. It is not
    /// among the migrations, as Laravel printed none of its statements.
    pub migration: Option<String>,
    /// What kilit says about it.
    pub message: String,
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
    let error_screen = error_screen(&lines);
    let before_error = &lines[..error_screen.map_or(lines.len(), |screen| screen.title)];
    if error_screen.is_none() && !before_error.iter().any(|line| is_from_laravel(line)) {
        return None;
    }

    let mut pretend = Pretend::default();
    for (i, line) in before_error.iter().enumerate() {
        let number = i + 1;
        match read_line(line) {
            Line::Blank | Line::Info | Line::Task => {}
            Line::Migration(name) => pretend.migrations.push(LaravelMigration {
                name: name.to_owned(),
                statements: Vec::new(),
                first_read: None,
            }),
            Line::Statement(sql) => match pretend.migrations.last_mut() {
                Some(migration) => {
                    for mut statement in parse(sql) {
                        statement.line = number;
                        if migration.first_read.is_none() && asks_the_database(&statement.sql) {
                            migration.first_read = Some(migration.statements.len());
                        }
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
    if let Some(screen) = error_screen {
        pretend.error = Some(stopped(&mut pretend.migrations, &screen));
    }
    Some(pretend)
}

/// Where Laravel's error screen is in the output.
#[derive(Clone, Copy)]
struct ErrorScreen<'a> {
    /// The first line: the exception's class, such as `RuntimeException`,
    /// or Symfony's `In <file> line <line>:`.
    title: usize,
    /// The file the exception was thrown in, from `at <file>:<line>` or
    /// `In <file> line <line>:`.
    file: &'a str,
}

/// Laravel's error screen. With nunomaduro/collision, which new Laravel
/// applications install for development: the exception's class alone on a
/// line, a blank line, the message, then `at <file>:<line>`. Without it,
/// Symfony's: `In <file> line <line>:`, then the message in a block of
/// spaces.
///
/// ```text
///    RuntimeException
///
///   boom from a migration
///
///   at database/migrations/2026_11_01_000002_widen_amount_on_orders_table.php:15
///
/// In 2026_11_01_000002_widen_amount_on_orders_table.php line 15:
///
///   boom from a migration
/// ```
fn error_screen(lines: &[String]) -> Option<ErrorScreen<'_>> {
    let followed_by_blank = |i: usize| lines.get(i + 1).is_some_and(|line| line.is_empty());
    (0..lines.len()).find_map(|i| {
        if let Some(file) = symfony_header(&lines[i]) {
            return followed_by_blank(i).then_some(ErrorScreen { title: i, file });
        }
        if is_class_name(&lines[i]) && (i == 0 || lines[i - 1].is_empty()) && followed_by_blank(i) {
            let file = lines[i + 2..].iter().find_map(|line| thrown_in(line))?;
            return Some(ErrorScreen { title: i, file });
        }
        None
    })
}

/// The file in Symfony's `In <file> line <line>:`.
fn symfony_header(line: &str) -> Option<&str> {
    let (file, number) = line
        .strip_prefix("In ")?
        .strip_suffix(':')?
        .rsplit_once(" line ")?;
    php_file(file, number)
}

/// A PHP class name such as `Illuminate\Database\QueryException`.
fn is_class_name(line: &str) -> bool {
    let line = line.strip_prefix('\\').unwrap_or(line);
    !line.is_empty()
        && line.split('\\').all(|part| {
            part.starts_with(|c: char| c.is_ascii_alphabetic() || c == '_')
                && part.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
        })
}

/// The file in `at <file>:<line>`.
fn thrown_in(line: &str) -> Option<&str> {
    let (file, number) = line.strip_prefix("at ")?.rsplit_once(':')?;
    php_file(file, number)
}

/// `file`, when it is a PHP file and `number` a line number.
fn php_file<'a>(file: &'a str, number: &str) -> Option<&'a str> {
    let file = file.trim();
    let is_number = !number.is_empty() && number.chars().all(|c| c.is_ascii_digit());
    (is_number && file.len() > 4 && file.ends_with(".php")).then_some(file)
}

/// The error, and the migration Laravel stopped in. Laravel prints a
/// migration's name before it runs it and its statements after, so the
/// migration it stopped in is the last name, with no statements; it is taken
/// out of `migrations`. An error in a migration's syntax stops Laravel
/// before any migration, and only the file names it.
fn stopped(migrations: &mut Vec<LaravelMigration>, screen: &ErrorScreen) -> LaravelError {
    let line = screen.title + 1;
    if migrations
        .last()
        .is_some_and(|migration| migration.statements.is_empty())
    {
        let name = migrations.pop().map(|migration| migration.name);
        let message = format!(
            "Laravel stopped with an error in {}; its statements and the migrations after it \
             were not seen",
            name.as_deref().unwrap_or_default()
        );
        return LaravelError {
            line,
            migration: name,
            message,
        };
    }
    if let Some(last) = migrations.last() {
        return LaravelError {
            line,
            migration: None,
            message: format!(
                "Laravel stopped with an error after {}; the migrations after it were not seen",
                last.name
            ),
        };
    }
    let file = screen.file.rsplit(['/', '\\']).next().unwrap_or_default();
    let migration = file
        .strip_suffix(".php")
        .filter(|name| is_migration_file_name(name))
        .map(str::to_owned);
    let message = match &migration {
        Some(name) => format!(
            "Laravel stopped with an error in {name} before it printed any migration; \
             no statement was seen"
        ),
        None => "Laravel stopped with an error before it printed any migration; \
                 no statement was seen"
            .to_owned(),
    };
    LaravelError {
        line,
        migration,
        message,
    }
}

/// A statement that asks the database something rather than changing it.
/// Under --pretend Laravel prints it and returns nothing.
fn asks_the_database(sql: &str) -> bool {
    let sql = sql.trim_start_matches(|c: char| c == '(' || c.is_whitespace());
    let word: String = sql
        .chars()
        .take_while(|c| c.is_ascii_alphabetic())
        .collect();
    [
        "select", "show", "describe", "desc", "explain", "with", "table", "values",
    ]
    .iter()
    .any(|read| word.eq_ignore_ascii_case(read))
}

/// A statement that only reads the database, as far as its words tell: one
/// that asks the database something, but does not start with `WITH`, which
/// may lead to an UPDATE or a DELETE, and has none of the words INSERT,
/// UPDATE, DELETE, REPLACE and INTO.
pub fn only_reads(sql: &str) -> bool {
    let first = sql
        .trim_start_matches(|c: char| c == '(' || c.is_whitespace())
        .get(..4);
    asks_the_database(sql)
        && !first.is_some_and(|word| word.eq_ignore_ascii_case("with"))
        && !sql
            .split(|c: char| !(c.is_alphanumeric() || c == '_' || c == '$'))
            .any(|word| {
                ["insert", "update", "delete", "replace", "into"]
                    .iter()
                    .any(|write| word.eq_ignore_ascii_case(write))
            })
}

/// The name Laravel gives a migration file: `2026_11_01_000001_` and a name,
/// `YYYY_MM_DD_HHMMSS_`.
fn is_migration_file_name(name: &str) -> bool {
    let mut parts = name.splitn(5, '_');
    [4, 2, 2, 6].iter().all(|&digits| {
        parts
            .next()
            .is_some_and(|part| part.len() == digits && part.bytes().all(|b| b.is_ascii_digit()))
    }) && parts.next().is_some_and(|rest| !rest.is_empty())
        && !name.contains(char::is_whitespace)
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
    // The dots follow a space. Without them a line is a name only in the
    // form Laravel gives migration files; a migration that is not an
    // anonymous class is named by its class, always with dots here.
    let name = line.trim_end_matches('.');
    let dots = name.len() < line.len() && name.ends_with(' ');
    let name = name.trim_end();
    if !name.is_empty()
        && !name.contains(char::is_whitespace)
        && (dots || is_migration_file_name(name))
    {
        return Line::Migration(name);
    }
    if line.contains(" ..") {
        return Line::Task;
    }
    Line::Other
}

/// The SQL after Laravel's mark, `⇂ alter table ...`, or after the mark as
/// Windows PowerShell may pass it on.
fn statement(line: &str) -> Option<&str> {
    let (mark, sql) = line.split_once(' ').unwrap_or((line, ""));
    let is_mark = mark.chars().eq([STATEMENT_MARK]) || MANGLED_MARKS.contains(&mark);
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

    /// The error screen of Laravel 13.40 with nunomaduro/collision, piped,
    /// for a migration that throws after an ALTER, cut after the first frame.
    const THROWS: &str = "
   INFO  Running migrations.

  2026_02_01_000041_first_alter_orders_table .................................
  ⇂ alter table `orders` add `qty` int null
  2026_02_01_000042_second_throws_orders_table ...............................

   RuntimeException

  boom from a migration

  at database/migrations/2026_02_01_000042_second_throws_orders_table.php:15
     11▕     {
     12▕         Schema::table('orders', function (Blueprint $table) {
     13▕             $table->integer('total')->nullable();
     14▕         });
  ➜  15▕         throw new RuntimeException('boom from a migration');
     16▕     }
     17▕

      \u{1b}[2m+27 vendor frames \u{1b}[22m

  28  artisan:16
      Illuminate\\Foundation\\Application::handleCommand(Object(Symfony\\Component\\Console\\Input\\ArgvInput))
";

    #[test]
    fn nothing_on_laravel_s_error_screen_is_read() {
        let pretend = read_pretend(THROWS).unwrap();
        assert_eq!(
            names(&pretend),
            ["2026_02_01_000041_first_alter_orders_table"]
        );
        assert_eq!(
            sql(&pretend.migrations[0]),
            ["alter table `orders` add `qty` int null"]
        );
        assert!(pretend.unreadable.is_empty());
        assert_eq!(
            pretend.error,
            Some(LaravelError {
                line: 8,
                migration: Some("2026_02_01_000042_second_throws_orders_table".into()),
                message: "Laravel stopped with an error in \
                          2026_02_01_000042_second_throws_orders_table; its statements and the \
                          migrations after it were not seen"
                    .into(),
            })
        );
    }

    #[test]
    fn the_error_screen_without_collision_is_read_the_same() {
        let pretend = read_pretend(
            "
   INFO  Running migrations.

  2026_02_01_000041_first_alter_orders_table .................................
  ⇂ alter table `orders` add `qty` int null
  2026_02_01_000042_second_throws_orders_table ...............................

In 2026_02_01_000042_second_throws_orders_table.php line 15:
                         
  boom from a migration  
                         

",
        )
        .unwrap();
        assert_eq!(
            names(&pretend),
            ["2026_02_01_000041_first_alter_orders_table"]
        );
        assert!(pretend.unreadable.is_empty());
        let error = pretend.error.unwrap();
        assert_eq!(error.line, 8);
        assert_eq!(
            error.migration.as_deref(),
            Some("2026_02_01_000042_second_throws_orders_table")
        );
    }

    #[test]
    fn a_syntax_error_stops_laravel_before_any_migration() {
        for output in [
            "
   ParseError

  syntax error, unexpected token \"}\"

  at database/migrations/2026_02_01_000003_syntax_error_in_orders_table.php:13
",
            "
In 2026_02_01_000003_syntax_error_in_orders_table.php line 13:
                                      
  syntax error, unexpected token \"}\"  
                                      
",
        ] {
            let pretend = read_pretend(output).expect("Laravel's output, not SQL");
            assert!(pretend.migrations.is_empty());
            assert!(pretend.unreadable.is_empty());
            assert_eq!(
                pretend.error.unwrap().message,
                "Laravel stopped with an error in 2026_02_01_000003_syntax_error_in_orders_table \
                 before it printed any migration; no statement was seen"
            );
        }
    }

    #[test]
    fn an_error_in_a_file_that_is_not_a_migration_names_no_migration() {
        let pretend = read_pretend(
            "
   Illuminate\\Database\\QueryException

  SQLSTATE[HY000] [2002] Connection refused

  at vendor/laravel/framework/src/Illuminate/Database/Connection.php:824
",
        )
        .unwrap();
        let error = pretend.error.unwrap();
        assert_eq!(error.migration, None);
        assert_eq!(
            error.message,
            "Laravel stopped with an error before it printed any migration; no statement was seen"
        );
    }

    #[test]
    fn an_error_after_a_migration_with_statements_names_the_migration_before_it() {
        let pretend = read_pretend(&THROWS.replace(
            "  2026_02_01_000042_second_throws_orders_table ...............................\n",
            "",
        ))
        .unwrap();
        assert_eq!(
            names(&pretend),
            ["2026_02_01_000041_first_alter_orders_table"]
        );
        let error = pretend.error.unwrap();
        assert_eq!(error.migration, None);
        assert_eq!(
            error.message,
            "Laravel stopped with an error after 2026_02_01_000041_first_alter_orders_table; \
             the migrations after it were not seen"
        );
    }

    #[test]
    fn sql_with_a_word_alone_and_a_time_is_not_an_error_screen() {
        for sql in [
            "/*\n\nRollout\n\nat 10:30\n*/\nALTER TABLE orders DROP COLUMN note;\n",
            "/*\nIn production line 2:\n\n*/\nALTER TABLE orders DROP COLUMN note;\n",
        ] {
            assert!(read_pretend(sql).is_none(), "{sql}");
        }
    }

    #[test]
    fn symfony_s_header_starts_an_error_screen_only_before_a_blank_line() {
        let output = "
   INFO  Running migrations.

  2026_02_01_000041_first_alter_orders_table .................................
In 2026_02_01_000041_first_alter_orders_table.php line 15:
  ⇂ alter table `orders` add `qty` int null
";
        let pretend = read_pretend(output).unwrap();
        assert_eq!(pretend.error, None);
        assert_eq!(pretend.migrations[0].statements.len(), 1);
        assert_eq!(pretend.unreadable[0].line, 5);
    }

    #[test]
    fn a_word_alone_is_not_an_error_screen_without_its_file() {
        let pretend = read_pretend(
            "
   INFO  Running migrations.

  2026_02_01_000041_first_alter_orders_table .................................

   RuntimeException

  boom from a migration
",
        )
        .unwrap();
        assert_eq!(pretend.error, None);
        let unreadable: Vec<&str> = pretend
            .unreadable
            .iter()
            .map(|line| line.text.as_str())
            .collect();
        assert_eq!(unreadable, ["RuntimeException", "boom from a migration"]);
    }

    #[test]
    fn a_line_without_dots_is_a_migration_only_with_laravel_s_file_name() {
        let pretend = read_pretend(
            "
   INFO  Running migrations.

  0001_01_01_000000_create_users_table
  ⇂ select 1
  RuntimeException
  12▕
  Illuminate\\Foundation\\Application::handleCommand()
  2026_1_01_000000_short_month
  AddQtyToOrdersTable ......
  ⇂ select 2
",
        )
        .unwrap();
        assert_eq!(
            names(&pretend),
            [
                "0001_01_01_000000_create_users_table",
                "AddQtyToOrdersTable"
            ]
        );
        let unreadable: Vec<usize> = pretend.unreadable.iter().map(|line| line.line).collect();
        assert_eq!(unreadable, [6, 7, 8, 9]);
    }

    #[test]
    fn dots_without_a_space_before_them_do_not_make_a_name() {
        let pretend = read_pretend("   INFO  Running migrations.\n\n  Done.\n").unwrap();
        assert!(pretend.migrations.is_empty());
        assert_eq!(pretend.unreadable[0].text, "Done.");
    }

    #[test]
    fn only_laravel_s_mark_and_its_mangled_forms_start_a_statement() {
        for mark in MANGLED_MARKS.iter().copied().chain(["⇂"]) {
            assert_eq!(
                statement(&format!("{mark} select 1")),
                Some("select 1"),
                "{mark}"
            );
        }
        for word in ["➜", "▕", "12▕", "??", "????", "→", "é"] {
            assert_eq!(statement(&format!("{word} select 1")), None, "{word}");
        }
    }

    #[test]
    fn the_mangled_marks_are_the_mark_s_bytes_in_windows_code_pages() {
        // Each form is three characters, one per UTF-8 byte of the mark.
        for mark in &MANGLED_MARKS[2..] {
            assert_eq!(mark.chars().count(), 3, "{mark}");
        }
        assert_eq!("⇂".as_bytes(), [0xE2, 0x87, 0x82]);
    }

    #[test]
    fn the_first_read_of_each_migration_is_found() {
        let pretend = read_pretend(
            "
   INFO  Running migrations.

  2026_02_01_000051_guard_has_table_orders_table ..............................
  ⇂ alter table `orders` add `a` int null
  ⇂ select exists (select 1 from information_schema.tables where table_name = 'orders') as `exists`
  ⇂ show index from orders
  2026_02_01_000052_plain .....................................................
  ⇂ alter table `orders` add `b` int null
  2026_02_01_000053_reads_first ...............................................
  ⇂ (SELECT 1)
",
        )
        .unwrap();
        let reads: Vec<Option<usize>> = pretend
            .migrations
            .iter()
            .map(|migration| migration.first_read)
            .collect();
        assert_eq!(reads, [Some(1), None, Some(0)]);
    }

    #[test]
    fn reads_are_told_from_changes_by_their_first_word() {
        for sql in [
            "select 1",
            "SELECT count(*) as aggregate from `orders`",
            "show index from orders",
            "describe orders",
            "desc orders",
            "explain select 1",
            "with t as (select 1) select * from t",
            "table orders",
            "values row(1)",
            "( select 1 )",
        ] {
            assert!(asks_the_database(sql), "{sql}");
        }
        for sql in [
            "alter table `orders` add `a` int null",
            "insert into orders select * from old_orders",
            "update orders set note = (select 1)",
            "selected",
            "set foreign_key_checks = 0",
        ] {
            assert!(!asks_the_database(sql), "{sql}");
        }
    }

    #[test]
    fn a_read_that_may_write_is_not_only_a_read() {
        for sql in [
            "select 1",
            "select exists (select 1 from information_schema.tables where table_name = 'users') as `exists`",
            "SHOW INDEX FROM orders",
            "table orders",
        ] {
            assert!(only_reads(sql), "{sql}");
        }
        for sql in [
            "with t as (select 1) select * from t",
            "WITH t AS (SELECT id FROM orders) UPDATE orders SET note = 1",
            "select * from orders for update",
            "select 1 into @a",
            "select * from orders into outfile '/tmp/o'",
            "explain delete from orders",
            "insert into orders select * from old_orders",
            "alter table `orders` add `a` int null",
        ] {
            assert!(!only_reads(sql), "{sql}");
        }
    }

    #[test]
    fn strips_color_codes() {
        assert_eq!(plain("\u{1b}[90m⇂ x\u{1b}[39m"), "⇂ x");
        assert_eq!(plain("\u{1b}[37;44m INFO \u{1b}[39;49m"), " INFO ");
        assert_eq!(plain("no colors"), "no colors");
    }
}
