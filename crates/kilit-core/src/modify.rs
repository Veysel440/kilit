//! What a `MODIFY` or `CHANGE` changes, found by comparing a column's
//! current definition with the new one.
//!
//! MySQL replaces the whole definition: whatever the statement leaves out,
//! such as a default or a comment, is gone afterwards. So kilit compares
//! every part of the column, not only the parts the statement writes.

use std::fmt;

use crate::charset::Charset;
use crate::schema::names_text_settings;
use crate::statement::{Column, DataType, Position, Table};

/// One thing that a MODIFY or CHANGE changes about a column.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Change {
    /// A new name, given with CHANGE.
    Name {
        to: String,
    },
    /// A new data type, other than a VARCHAR's new length.
    Type {
        from: DataType,
        to: DataType,
    },
    /// A VARCHAR gets longer or shorter.
    Length {
        from: u64,
        to: u64,
        charset: &'static Charset,
    },
    /// `implicit` when the statement names no character set, so the
    /// column takes the table's.
    Charset {
        from: &'static str,
        to: &'static str,
        implicit: bool,
    },
    Collation {
        from: String,
        to: String,
        implicit: bool,
    },
    Nullable {
        to: bool,
    },
    /// The defaults as written; `None` for no default.
    Default {
        from: Option<String>,
        to: Option<String>,
    },
    Comment {
        to: Option<String>,
    },
    /// `FIRST` or `AFTER` puts the column somewhere else.
    Position,
    /// A change kilit does not model, described.
    Other(String),
}

impl fmt::Display for Change {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Change::Name { to } => write!(f, "renames the column to {to}"),
            Change::Type { from, to } => write!(f, "changes the type from {from} to {to}"),
            Change::Length { from, to, charset } => write!(
                f,
                "changes VARCHAR({from}) to VARCHAR({to}), {} to {} bytes in {}",
                from * charset.max_bytes,
                to * charset.max_bytes,
                charset.name
            ),
            Change::Charset { from, to, implicit } => {
                write!(f, "changes the character set from {from} to {to}")?;
                write_implicit(f, *implicit)
            }
            Change::Collation { from, to, implicit } => {
                write!(f, "changes the collation from {from} to {to}")?;
                write_implicit(f, *implicit)
            }
            Change::Nullable { to: true } => f.write_str("makes the column nullable"),
            Change::Nullable { to: false } => f.write_str("makes the column NOT NULL"),
            Change::Default {
                from: Some(from),
                to: Some(to),
            } => write!(f, "changes the default from {from} to {to}"),
            Change::Default {
                from: None,
                to: Some(to),
            } => write!(f, "gives the column the default {to}"),
            Change::Default {
                from: Some(from),
                to: None,
            } => write!(f, "drops the default {from}"),
            Change::Default {
                from: None,
                to: None,
            } => f.write_str("changes the default"),
            Change::Comment { to: Some(_) } => f.write_str("changes the comment"),
            Change::Comment { to: None } => f.write_str("drops the comment"),
            Change::Position => f.write_str("moves the column"),
            Change::Other(what) => f.write_str(what),
        }
    }
}

fn write_implicit(f: &mut fmt::Formatter<'_>, implicit: bool) -> fmt::Result {
    if implicit {
        f.write_str(", the table's default, because the statement names none")?;
    }
    Ok(())
}

/// Everything that a MODIFY or CHANGE giving `new` (and `position`) changes
/// about `old`, a column of `table`. An error says what kilit cannot work
/// out, such as a character set it does not know.
pub(crate) fn changes(
    table: &Table,
    old: &Column,
    new: &Column,
    position: Option<&Position>,
) -> Result<Vec<Change>, String> {
    let mut changes = Vec::new();

    if !old.name.eq_ignore_ascii_case(&new.name) {
        changes.push(Change::Name {
            to: new.name.clone(),
        });
    }

    let text = old.data_type.is_text() && new.data_type.is_text();
    match (&old.data_type, &new.data_type) {
        (DataType::VarChar { length: from }, DataType::VarChar { length: to }) if from != to => {
            changes.push(Change::Length {
                from: *from,
                to: *to,
                charset: table.text_settings(new)?.charset,
            });
        }
        (from, to) if from != to => changes.push(Change::Type {
            from: from.clone(),
            to: to.clone(),
        }),
        _ => {}
    }
    // When neither definition names a character set or a collation, both
    // take the table's: nothing changes, whatever the table's are.
    if text && (names_text_settings(old) || names_text_settings(new)) {
        let (from, to) = (table.text_settings(old)?, table.text_settings(new)?);
        if from.charset != to.charset {
            changes.push(Change::Charset {
                from: from.charset.name,
                to: to.charset.name,
                implicit: to.from_table,
            });
        } else if from.collation != to.collation {
            changes.push(Change::Collation {
                from: from.collation,
                to: to.collation,
                implicit: to.from_table,
            });
        }
    }

    let nullable = new
        .nullable
        .unwrap_or_else(|| !table.in_primary_key(&old.name));
    if nullable != table.is_nullable(old) {
        changes.push(Change::Nullable { to: nullable });
    }

    let is_text = new.data_type.is_text();
    if default_key(old.default.as_deref(), is_text) != default_key(new.default.as_deref(), is_text)
    {
        changes.push(Change::Default {
            from: written_default(old),
            to: written_default(new),
        });
    }

    let comment = |column: &Column| column.comment.clone().filter(|text| !text.is_empty());
    if comment(old) != comment(new) {
        changes.push(Change::Comment { to: comment(new) });
    }

    if old.auto_increment != new.auto_increment {
        changes.push(Change::Other(
            if new.auto_increment {
                "adds AUTO_INCREMENT"
            } else {
                "drops AUTO_INCREMENT"
            }
            .to_owned(),
        ));
    }
    if default_key(old.on_update.as_deref(), false) != default_key(new.on_update.as_deref(), false)
    {
        changes.push(Change::Other("changes ON UPDATE".to_owned()));
    }
    if old.generated != new.generated {
        changes.push(Change::Other("changes the generated expression".to_owned()));
    }
    let options = |column: &Column| {
        let mut options: Vec<String> = column
            .other_options
            .iter()
            .map(|option| option.to_ascii_uppercase())
            .collect();
        options.sort();
        options
    };
    if options(old) != options(new) {
        changes.push(Change::Other(format!(
            "changes options kilit does not model: {} to {}",
            listed(&old.other_options),
            listed(&new.other_options)
        )));
    }

    if let Some(position) = position {
        let at = table.position_of(&old.name);
        let moves = match position {
            Position::First => at != Some(0),
            Position::After(name) => {
                let before = table
                    .position_of(name)
                    .ok_or_else(|| format!("{} has no column {name}", table.name))?;
                at != Some(before + 1)
            }
        };
        if moves {
            changes.push(Change::Position);
        }
    }

    Ok(changes)
}

fn listed(options: &[String]) -> String {
    if options.is_empty() {
        "none".to_owned()
    } else {
        options.join(" ")
    }
}

/// The default as written, or `None` for no default or `DEFAULT NULL`.
fn written_default(column: &Column) -> Option<String> {
    column
        .default
        .clone()
        .filter(|default| !default.trim().eq_ignore_ascii_case("NULL"))
}

/// A default in a form where equal defaults are equal: no default and
/// `NULL`; `now()` and `CURRENT_TIMESTAMP`; and for a column that does not
/// hold text, `'0'`, `0` and `'0.00'`, as MySQL stores them alike.
fn default_key(default: Option<&str>, text: bool) -> Option<String> {
    let value = default?.trim();
    if value.eq_ignore_ascii_case("NULL") {
        return None;
    }
    let upper = value.to_ascii_uppercase();
    for name in ["CURRENT_TIMESTAMP", "LOCALTIMESTAMP", "LOCALTIME", "NOW"] {
        let Some(rest) = upper.strip_prefix(name) else {
            continue;
        };
        let rest = rest.trim();
        if rest.is_empty() || rest == "()" {
            return Some("CURRENT_TIMESTAMP".to_owned());
        }
        if let Some(precision) = rest.strip_prefix('(').and_then(|r| r.strip_suffix(')')) {
            return Some(format!("CURRENT_TIMESTAMP({})", precision.trim()));
        }
    }
    let unquoted = unquote(value);
    if !text && let Ok(number) = unquoted.parse::<f64>() {
        return Some(format!("{number}"));
    }
    Some(unquoted)
}

/// `'it''s'` becomes `it's`; anything not quoted stays as it is.
fn unquote(value: &str) -> String {
    for quote in ['\'', '"'] {
        if let Some(inner) = value
            .strip_prefix(quote)
            .and_then(|rest| rest.strip_suffix(quote))
        {
            return inner.replace(&format!("{quote}{quote}"), &quote.to_string());
        }
    }
    value.to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parse::parse;
    use crate::statement::{Operation, StatementKind};

    /// `orders` from a Laravel app, as `php artisan schema:dump` writes it.
    const ORDERS: &str = "CREATE TABLE `orders` (
  `id` bigint unsigned NOT NULL AUTO_INCREMENT,
  `amount` int NOT NULL DEFAULT '0',
  `status` varchar(20) COLLATE utf8mb4_unicode_ci NOT NULL DEFAULT 'new',
  `note` varchar(50) COLLATE utf8mb4_unicode_ci DEFAULT NULL COMMENT 'from the customer',
  `code` varchar(20) CHARACTER SET latin1 COLLATE latin1_swedish_ci DEFAULT NULL,
  `created_at` timestamp NULL DEFAULT CURRENT_TIMESTAMP,
  PRIMARY KEY (`id`)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_unicode_ci";

    /// What the MODIFY or CHANGE in `sql` changes in `orders`, described.
    fn changed(sql: &str) -> Result<Vec<String>, String> {
        let statements = parse(&format!("{ORDERS};\n{sql}"));
        let StatementKind::CreateTable { table, .. } = &statements[0].kind else {
            panic!("expected CREATE TABLE");
        };
        let StatementKind::Alter(alter) = &statements[1].kind else {
            panic!("{sql}: expected ALTER TABLE");
        };
        let (old_name, new, position) = match &alter.operations[..] {
            [Operation::ModifyColumn { column, position }] => (&column.name, column, position),
            [
                Operation::ChangeColumn {
                    old_name,
                    column,
                    position,
                },
            ] => (old_name, column, position),
            other => panic!("{sql}: {other:?}"),
        };
        let old = table.column(old_name).unwrap();
        let changes = changes(table, old, new, position.as_ref())?;
        Ok(changes.iter().map(ToString::to_string).collect())
    }

    #[test]
    fn the_same_definition_changes_nothing() {
        for sql in [
            "alter table `orders` modify `amount` int not null default '0'",
            "ALTER TABLE orders MODIFY amount INT(11) NOT NULL DEFAULT 0",
            "alter table `orders` modify `note` varchar(50) null comment 'from the customer'",
            "alter table `orders` modify `note` varchar(50) collate 'utf8mb4_unicode_ci' \
             default null comment 'from the customer'",
            "alter table `orders` modify `created_at` timestamp null default current_timestamp()",
            "alter table `orders` modify `status` varchar(20) not null default 'new' after `amount`",
        ] {
            assert_eq!(changed(sql), Ok(vec![]), "{sql}");
        }
    }

    #[test]
    fn finds_each_kind_of_change() {
        for (sql, change) in [
            (
                "alter table `orders` modify `amount` bigint not null default '0'",
                "changes the type from INT to BIGINT",
            ),
            (
                "alter table `orders` modify `status` varchar(64) not null default 'new'",
                "changes VARCHAR(20) to VARCHAR(64), 80 to 256 bytes in utf8mb4",
            ),
            (
                "alter table `orders` modify `amount` int null default '0'",
                "makes the column nullable",
            ),
            (
                "alter table `orders` modify `amount` int not null default '1'",
                "changes the default from '0' to '1'",
            ),
            (
                "alter table `orders` modify `amount` int not null",
                "drops the default '0'",
            ),
            (
                "alter table `orders` modify `note` varchar(50) null",
                "drops the comment",
            ),
            (
                "alter table `orders` modify `amount` int not null default '0' first",
                "moves the column",
            ),
            (
                "alter table `orders` change `note` `memo` varchar(50) null comment 'from the customer'",
                "renames the column to memo",
            ),
            (
                "alter table `orders` modify `status` varchar(20) collate utf8mb4_bin not null default 'new'",
                "changes the collation from utf8mb4_unicode_ci to utf8mb4_bin",
            ),
            (
                "alter table `orders` modify `id` bigint unsigned not null",
                "drops AUTO_INCREMENT",
            ),
        ] {
            assert_eq!(changed(sql), Ok(vec![change.to_owned()]), "{sql}");
        }
    }

    #[test]
    fn a_statement_without_a_character_set_takes_the_tables() {
        // `code` is latin1 in a utf8mb4 table: a MODIFY that names no
        // character set converts it.
        assert_eq!(
            changed("alter table `orders` modify `code` varchar(20) null"),
            Ok(vec![
                "changes the character set from latin1 to utf8mb4, the table's default, \
                 because the statement names none"
                    .to_owned()
            ])
        );
        assert_eq!(
            changed("alter table `orders` modify `code` varchar(20) character set latin1 null"),
            Ok(vec![])
        );
    }

    #[test]
    fn several_changes_are_all_found() {
        // Laravel 11 and later: $table->string('status', 30)->change()
        // drops every modifier it is not given again.
        assert_eq!(
            changed("alter table `orders` modify `status` varchar(30) not null"),
            Ok(vec![
                "changes VARCHAR(20) to VARCHAR(30), 80 to 120 bytes in utf8mb4".to_owned(),
                "drops the default 'new'".to_owned(),
            ])
        );
    }

    #[test]
    fn text_defaults_are_compared_as_text() {
        assert_eq!(default_key(Some("'1.0'"), true), Some("1.0".into()));
        assert_eq!(
            default_key(Some("'1.0'"), false),
            default_key(Some("1"), false)
        );
        assert_eq!(default_key(Some("NULL"), false), None);
        assert_eq!(
            default_key(Some("now(3)"), false),
            default_key(Some("CURRENT_TIMESTAMP(3)"), false)
        );
        assert_eq!(default_key(Some("'it''s'"), true), Some("it's".into()));
    }

    #[test]
    fn an_unknown_position_is_an_error() {
        assert!(changed("alter table `orders` modify `amount` int not null after `nope`").is_err());
    }
}
