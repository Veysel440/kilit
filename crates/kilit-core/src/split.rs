//! Splits SQL text into statements at semicolons, as the mysql client does:
//! a semicolon inside quotes or a comment does not end a statement.
//!
//! Splitting first means one statement the parser cannot read does not hide
//! the others.
//!
//! Known limits: `DELIMITER` (used for stored procedures and triggers) is a
//! mysql client command, not SQL, and is not supported. Backslashes are
//! treated as escapes inside strings, which is MySQL's default; servers
//! running with `NO_BACKSLASH_ESCAPES` treat them as plain characters.

/// One statement's text and the line it starts on.
#[derive(Debug, PartialEq, Eq)]
pub struct Piece<'a> {
    /// The line of the statement's first character, counting from 1.
    pub line: usize,
    /// The statement without the final semicolon and surrounding whitespace.
    /// Comments before the statement are left out.
    pub sql: &'a str,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum State {
    Code,
    /// Inside a string or a quoted name; holds the closing quote: `'`, `"` or `` ` ``.
    Quoted(char),
    /// From `#` or `-- ` to the end of the line.
    LineComment,
    /// Between `/*` and `*/`.
    BlockComment,
}

pub fn split(input: &str) -> Vec<Piece<'_>> {
    let mut pieces = Vec::new();
    let mut state = State::Code;
    let mut line = 1;
    // The byte offset and line where the current statement starts.
    let mut start: Option<(usize, usize)> = None;
    let mut chars = input.char_indices().peekable();

    while let Some((i, c)) = chars.next() {
        match state {
            State::Code => match c {
                ';' => {
                    if let Some((begin, begin_line)) = start.take() {
                        pieces.push(Piece {
                            line: begin_line,
                            sql: input[begin..i].trim_end(),
                        });
                    }
                }
                '#' => state = State::LineComment,
                '-' if starts_dash_comment(&input[i..]) => state = State::LineComment,
                '/' if chars.peek().is_some_and(|&(_, next)| next == '*') => {
                    chars.next();
                    state = State::BlockComment;
                }
                c if c.is_whitespace() => {}
                c => {
                    start.get_or_insert((i, line));
                    if matches!(c, '\'' | '"' | '`') {
                        state = State::Quoted(c);
                    }
                }
            },
            State::Quoted(quote) => {
                if c == '\\' && quote != '`' {
                    // The backslash escapes the next character, even a quote.
                    if let Some((_, escaped)) = chars.next()
                        && escaped == '\n'
                    {
                        line += 1;
                    }
                } else if c == quote {
                    // A doubled quote ('it''s') closes and reopens the
                    // string, which leaves us inside it: no special case needed.
                    state = State::Code;
                }
            }
            State::LineComment => {
                if c == '\n' {
                    state = State::Code;
                }
            }
            State::BlockComment => {
                if c == '*' && chars.peek().is_some_and(|&(_, next)| next == '/') {
                    chars.next();
                    state = State::Code;
                }
            }
        }
        if c == '\n' {
            line += 1;
        }
    }

    // The last statement may have no semicolon.
    if let Some((begin, begin_line)) = start {
        pieces.push(Piece {
            line: begin_line,
            sql: input[begin..].trim_end(),
        });
    }
    pieces
}

/// MySQL starts a comment with `--` only when whitespace or the end of the
/// text follows: `1--1` is one minus minus one.
fn starts_dash_comment(rest: &str) -> bool {
    rest.strip_prefix("--")
        .is_some_and(|after| after.chars().next().is_none_or(char::is_whitespace))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The statements' text, without line numbers.
    fn texts(input: &str) -> Vec<&str> {
        split(input).into_iter().map(|piece| piece.sql).collect()
    }

    #[test]
    fn splits_at_semicolons() {
        assert_eq!(
            texts("ALTER TABLE a DROP x; ALTER TABLE b DROP y;"),
            ["ALTER TABLE a DROP x", "ALTER TABLE b DROP y"]
        );
    }

    #[test]
    fn the_last_statement_needs_no_semicolon() {
        assert_eq!(texts("SELECT 1;\nSELECT 2\n"), ["SELECT 1", "SELECT 2"]);
    }

    #[test]
    fn empty_statements_are_skipped() {
        assert_eq!(texts(";; \n ;SELECT 1;;"), ["SELECT 1"]);
        assert!(texts("").is_empty());
        assert!(texts("  \n\t").is_empty());
    }

    #[test]
    fn semicolons_in_strings_do_not_split() {
        assert_eq!(
            texts("INSERT INTO t VALUES ('a;b', \"c;d\"); SELECT 1"),
            ["INSERT INTO t VALUES ('a;b', \"c;d\")", "SELECT 1"]
        );
    }

    #[test]
    fn escaped_and_doubled_quotes_stay_inside_the_string() {
        assert_eq!(
            texts(r"SELECT 'it\'s;'; SELECT 'it''s;'; SELECT 'a\\'; SELECT 3"),
            [
                r"SELECT 'it\'s;'",
                "SELECT 'it''s;'",
                r"SELECT 'a\\'",
                "SELECT 3"
            ]
        );
    }

    #[test]
    fn semicolons_in_quoted_names_do_not_split() {
        assert_eq!(
            texts("ALTER TABLE `we;ird` DROP `x``;y`; SELECT 1"),
            ["ALTER TABLE `we;ird` DROP `x``;y`", "SELECT 1"]
        );
    }

    #[test]
    fn backslashes_do_not_escape_in_quoted_names() {
        // In MySQL `a\` is a name ending in a backslash.
        assert_eq!(
            texts(r"SELECT `a\`; SELECT 2"),
            [r"SELECT `a\`", "SELECT 2"]
        );
    }

    #[test]
    fn semicolons_in_comments_do_not_split() {
        let input = "\
ALTER TABLE a -- first; second
  DROP x # third; fourth
  /* fifth; sixth */ ;
SELECT 1";
        let pieces = texts(input);
        assert_eq!(pieces.len(), 2);
        assert!(pieces[0].ends_with("/* fifth; sixth */"));
        assert_eq!(pieces[1], "SELECT 1");
    }

    #[test]
    fn a_comment_needs_whitespace_after_the_dashes() {
        assert_eq!(texts("SELECT 1--1; SELECT 2"), ["SELECT 1--1", "SELECT 2"]);
        assert_eq!(texts("SELECT 1 --"), ["SELECT 1 --"]);
    }

    #[test]
    fn comments_before_a_statement_are_left_out() {
        let pieces = split("-- add a coupon\n/* note */\n\nALTER TABLE a ADD c INT;");
        assert_eq!(
            pieces,
            [Piece {
                line: 4,
                sql: "ALTER TABLE a ADD c INT"
            }]
        );
    }

    #[test]
    fn comment_only_input_has_no_statements() {
        assert!(texts("-- nothing here\n# or here\n/* or; here */").is_empty());
    }

    #[test]
    fn lines_are_counted_from_one() {
        let input = "SELECT 1;\n\nSELECT\n  2;\nSELECT 'a\nb';\nSELECT 4";
        let lines: Vec<usize> = split(input).iter().map(|piece| piece.line).collect();
        assert_eq!(lines, [1, 3, 5, 7]);
    }

    #[test]
    fn escaped_newlines_are_counted() {
        let input = "SELECT 'a\\\nb';\nSELECT 2";
        assert_eq!(split(input)[1].line, 3);
    }

    #[test]
    fn windows_line_endings_are_trimmed() {
        assert_eq!(texts("SELECT 1;\r\nSELECT 2\r\n"), ["SELECT 1", "SELECT 2"]);
    }

    #[test]
    fn an_unclosed_string_runs_to_the_end() {
        // The parser reports the error; the splitter only must not lose text.
        assert_eq!(texts("SELECT 'abc; SELECT 2"), ["SELECT 'abc; SELECT 2"]);
    }
}
