//! The bytes a row of a table takes, as MySQL counts them against its
//! limit of 65,535 bytes for a row (error 1118).
//!
//! The lab measured the line on MySQL 8.0.46 and 8.4.13, for each family of
//! types: a table at exactly 65,535 bytes is accepted, one byte more is
//! refused, in every SQL mode, whatever `innodb_strict_mode` says. TEXT,
//! BLOB, JSON and GEOMETRY count only the few bytes of their pointer.
//! kilit counts only the types the lab measured; any other makes the row
//! one it cannot count.
//!
//! InnoDB has a second limit, for the record it keeps in a 16K page, which
//! each row format counts its own way (error 1118, "> 8126"). MySQL 8.0.46
//! and 8.4.13 count some columns differently against it, and an added
//! column may go in place instead, so kilit gives no answer past it: see
//! [`page`].

use crate::schema::TextSettings;
use crate::statement::{Column, DataType, IndexKind, IntegerSize, Table};

/// The most bytes MySQL allows in a row, not counting what TEXT and BLOB
/// columns keep outside it.
pub const MAX_ROW_BYTES: u64 = 65_535;

/// InnoDB's row formats, which keep a record in a page each their own way.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Format {
    Dynamic,
    Compact,
    Redundant,
    /// `ROW_FORMAT=COMPRESSED`, or `KEY_BLOCK_SIZE=8` alone: the same.
    Compressed,
}

/// What kilit counted of a row, against each limit.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Counts {
    /// Against MySQL's limit of 65,535 bytes: each column's bytes, and a
    /// bit for each nullable column, one more when no column is of
    /// variable length, rounded up to whole bytes.
    pub row: u64,
    /// The record in a page of an InnoDB table of the format asked for,
    /// and the most bytes it may take there: see [`page`].
    pub page: Option<(u64, u64)>,
}

/// The bytes of a row of `columns` in `table`'s settings, against MySQL's
/// limit for a row and, with a `format`, InnoDB's for a record in a page;
/// or why kilit cannot count them.
pub fn count(table: &Table, columns: &[&Column], format: Option<Format>) -> Result<Counts, String> {
    // Most columns take the table's character set: it is looked up once.
    let mut table_bytes: Option<Result<u64, String>> = None;
    let mut row: u64 = 0;
    let mut null_bits: u64 = 0;
    let mut variable = false;
    let key = clustered_key(table, columns);
    let in_key = |name: &str, prefix: bool| {
        key.as_ref().is_some_and(|key| {
            key.iter()
                .any(|(column, prefixed)| *prefixed == prefix && column.eq_ignore_ascii_case(name))
        })
    };
    let mut fields = Vec::with_capacity(columns.len() + 2);
    for column in columns {
        if column.generated.is_some() {
            return Err(format!(
                "{} is a generated column, and the lab has not measured how one counts in a row",
                column.name
            ));
        }
        let counted = column_bytes(table, column, &mut table_bytes)?;
        let nullable = table.is_nullable(column);
        row = row.saturating_add(counted.bytes);
        variable |= counted.variable;
        null_bits += u64::from(nullable);
        // A key on a prefix of a column holds the prefix as a field of
        // its own, before the whole column: kilit counts it whole, the
        // larger.
        if in_key(&column.name, true) {
            fields.push(Field {
                page: counted.page.clone(),
                nullable: false,
                key: true,
            });
        }
        fields.push(Field {
            page: counted.page,
            nullable,
            key: in_key(&column.name, false),
        });
    }
    // A FULLTEXT index keeps a hidden FTS_DOC_ID of 8 bytes in each row.
    if table.fulltext_index().is_some() {
        fields.push(Field {
            page: Page::Fixed(8),
            nullable: false,
            key: false,
        });
    }
    let nullable = null_bits;
    if !variable {
        null_bits += 1;
    }
    Ok(Counts {
        row: row.saturating_add(null_bits.div_ceil(8)),
        page: format.map(|format| page(format, &fields, nullable, key.is_some())),
    })
}

/// The columns InnoDB keeps a table's rows in order of, each with whether
/// the key takes only a prefix of it: the primary key, or without one the
/// first UNIQUE index whose columns are all among `columns` and NOT NULL.
/// `None` when there is neither, and InnoDB adds a row id.
fn clustered_key(table: &Table, columns: &[&Column]) -> Option<Vec<(String, bool)>> {
    let entries = |entries: &[String]| -> Option<Vec<(String, bool)>> {
        entries
            .iter()
            .map(|entry| {
                let entry = entry.trim();
                if entry.starts_with('(') {
                    return None;
                }
                let (name, prefixed) = match entry.split_once('(') {
                    Some((name, _)) => (name.trim(), true),
                    None => (entry, false),
                };
                Some((name.trim_matches('`').to_owned(), prefixed))
            })
            .collect()
    };
    if !table.primary_key.is_empty() {
        return entries(&table.primary_key);
    }
    let not_null = |name: &str| {
        columns
            .iter()
            .any(|column| column.name.eq_ignore_ascii_case(name) && !table.is_nullable(column))
    };
    table
        .indexes
        .iter()
        .filter(|index| index.kind == IndexKind::Unique)
        .filter_map(|index| entries(&index.columns))
        .find(|key| key.iter().all(|(name, _)| not_null(name)))
}

/// A column as InnoDB keeps it in a record.
struct Field {
    page: Page,
    nullable: bool,
    /// In the primary key.
    key: bool,
}

/// The record of `fields` in a page of a table of `format`, and its limit,
/// as the lab measured them on MySQL 8.0.46 and 8.4.13 (16K pages, strict
/// modes). Where the two servers count apart, this is the larger count, so
/// past the limit kilit gives no answer, never a wrong one. A record holds
/// the key's columns (see [`clustered_key`]; a key on a prefix holds the
/// prefix, then the whole column again), or a row id of 6 bytes, a
/// transaction id of 6 and a roll pointer of 7, then the other columns, and
/// FTS_DOC_ID (8) with a FULLTEXT index; "var" below is a VARCHAR,
/// VARBINARY or multibyte CHAR of up to L bytes.
///
/// - DYNAMIC: 5 + a bit per nullable column + the fields; a var of up to
///   255 bytes counts L + 1, a longer one and TEXT, BLOB, JSON or GEOMETRY
///   41 (in the key, MySQL 8.0.46 caps none: L + 2); at most 8,125.
/// - COMPRESSED: as DYNAMIC, with 2 for 5, and at most 8,069 less 2 for
///   each field.
/// - COMPACT: 5 + a bit per nullable column + the fields; a var counts
///   L + 1 up to 255 bytes, L + 2 up to 788, 790 beyond, and so does each
///   TEXT or BLOB; at most 8,125.
/// - REDUNDANT: 6 + 2 for each field + the fields, with no bits and no
///   lengths; a var counts L up to 788, a multibyte CHAR L below 768 and
///   788 beyond, a TEXT or BLOB 788; at most 8,122.
fn page(format: Format, fields: &[Field], nullable: u64, clustered: bool) -> (u64, u64) {
    let dynamic = |field: &Field| match field.page {
        Page::Fixed(size) => size,
        Page::Variable(length) | Page::Char(length) if length <= 255 => length.saturating_add(1),
        Page::Variable(length) | Page::Char(length) if field.key => length.saturating_add(2),
        Page::Variable(_) | Page::Char(_) | Page::Off => 41,
    };
    let compact = |field: &Field| match field.page {
        Page::Fixed(size) => size,
        Page::Variable(length) | Page::Char(length) if length <= 255 => length.saturating_add(1),
        Page::Variable(length) | Page::Char(length) if length <= 788 => length.saturating_add(2),
        Page::Variable(_) | Page::Char(_) | Page::Off => 790,
    };
    let redundant = |field: &Field| match field.page {
        Page::Fixed(size) => size,
        Page::Variable(length) => length.min(788),
        Page::Char(length) if length < 768 => length,
        Page::Char(_) | Page::Off => 788,
    };
    // The row id (6), or nothing, and the transaction id (6) and roll
    // pointer (7).
    let hidden: u64 = if clustered { 13 } else { 19 };
    let count = u64::try_from(fields.len())
        .unwrap_or(u64::MAX)
        .saturating_add(if clustered { 2 } else { 3 });
    let bitmap = nullable.div_ceil(8);
    debug_assert!(fields.iter().all(|field| !(field.key && field.nullable)));
    let sum = |each: &dyn Fn(&Field) -> u64| {
        fields
            .iter()
            .map(each)
            .fold(hidden, |total: u64, bytes| total.saturating_add(bytes))
    };
    // Every sum saturates, and so does the COMPRESSED limit: a column kilit
    // reads may be longer, and a table wider, than any MySQL takes.
    match format {
        Format::Dynamic => (
            5_u64.saturating_add(bitmap).saturating_add(sum(&dynamic)),
            8_125,
        ),
        Format::Compressed => (
            2_u64.saturating_add(bitmap).saturating_add(sum(&dynamic)),
            8_069_u64.saturating_sub(count.saturating_mul(2)),
        ),
        Format::Compact => (
            5_u64.saturating_add(bitmap).saturating_add(sum(&compact)),
            8_125,
        ),
        Format::Redundant => (
            6_u64
                .saturating_add(count.saturating_mul(2))
                .saturating_add(sum(&redundant)),
            8_122,
        ),
    }
}

/// Why a new column is no row too long but one MySQL refuses in strict SQL
/// mode (error 1074) and makes TEXT or BLOB in the others: a VARCHAR or
/// VARBINARY of more than 65,535 bytes, or a CHAR or BINARY of more than
/// 255 characters. `None` for any other column, or one kilit cannot read.
/// Without the table, only what is too long in any character set counts.
pub fn too_long(table: Option<&Table>, column: &Column) -> Option<String> {
    let (limit, length, what) = match &column.data_type {
        DataType::VarChar { length } => {
            let per_character = match table {
                Some(table) => table.text_settings(column).ok()?.charset.max_bytes,
                None => 1,
            };
            (MAX_ROW_BYTES, length.saturating_mul(per_character), "bytes")
        }
        DataType::Text(name) | DataType::Other(name) => {
            let written = name.trim().to_ascii_uppercase();
            let (base, rest) = written.split_once('(')?;
            let length: u64 = rest.strip_suffix(')')?.trim().parse().ok()?;
            match base.trim() {
                "VARBINARY" => (MAX_ROW_BYTES, length, "bytes"),
                "CHAR" | "CHARACTER" | "BINARY" => (255, length, "characters"),
                _ => return None,
            }
        }
        DataType::Integer { .. } => return None,
    };
    (length > limit).then(|| {
        format!(
            "{} takes up to {length} {what}, more than its type holds ({limit}): MySQL refuses \
             it in strict SQL mode and changes its type in the others, which the lab has not \
             measured",
            column.name
        )
    })
}

/// What one column adds to a row.
struct Counted {
    /// Against the limit of 65,535 bytes.
    bytes: u64,
    /// The column stores a length with its value: VARCHAR, VARBINARY,
    /// TEXT, BLOB, JSON and GEOMETRY.
    variable: bool,
    /// How InnoDB keeps it in the record of a DYNAMIC table.
    page: Page,
}

#[derive(Clone)]
enum Page {
    /// This many bytes, always.
    Fixed(u64),
    /// Up to this many bytes, with a length.
    Variable(u64),
    /// A CHAR of this many bytes in a multibyte character set.
    Char(u64),
    /// TEXT, BLOB, JSON and GEOMETRY.
    Off,
}

fn fixed(bytes: u64) -> Result<Counted, String> {
    Ok(Counted {
        bytes,
        variable: false,
        page: Page::Fixed(bytes),
    })
}

/// TEXT, BLOB, JSON or GEOMETRY, with the bytes of its pointer in the row.
fn blob(bytes: u64) -> Result<Counted, String> {
    Ok(Counted {
        bytes,
        variable: true,
        page: Page::Off,
    })
}

/// A VARCHAR or VARBINARY of `bytes` bytes at most, with its 1 or 2
/// length bytes.
fn with_length(bytes: u64) -> Result<Counted, String> {
    Ok(Counted {
        bytes: bytes.saturating_add(if bytes <= 255 { 1 } else { 2 }),
        variable: true,
        page: Page::Variable(bytes),
    })
}

/// A CHAR of `bytes` bytes: fixed in a row, but InnoDB keeps it in a
/// multibyte character set as a variable column.
fn char_bytes(bytes: u64, per_character: u64) -> Result<Counted, String> {
    Ok(Counted {
        bytes,
        variable: false,
        page: if per_character > 1 {
            Page::Char(bytes)
        } else {
            Page::Fixed(bytes)
        },
    })
}

fn column_bytes(
    table: &Table,
    column: &Column,
    table_bytes: &mut Option<Result<u64, String>>,
) -> Result<Counted, String> {
    let mut per_character = || -> Result<u64, String> {
        if column.charset.is_none() && column.collation.is_none() {
            return table_bytes
                .get_or_insert_with(|| {
                    let TextSettings { charset, .. } = table.text_settings(column)?;
                    Ok(charset.max_bytes)
                })
                .clone();
        }
        let TextSettings { charset, .. } = table.text_settings(column)?;
        Ok(charset.max_bytes)
    };
    let not_counted = |name: &str| {
        Err(format!(
            "the lab has not measured how a {name} column counts in a row"
        ))
    };
    let name = match &column.data_type {
        DataType::Integer { size, .. } => {
            return fixed(match size {
                IntegerSize::Tiny => 1,
                IntegerSize::Small => 2,
                IntegerSize::Medium => 3,
                IntegerSize::Int => 4,
                IntegerSize::Big => 8,
            });
        }
        DataType::VarChar { length } => {
            return with_length(length.saturating_mul(per_character()?));
        }
        DataType::Text(name) | DataType::Other(name) => name,
    };
    // A sign written with the type changes none of its bytes.
    let mut written = name.trim().to_ascii_uppercase();
    if written.contains(char::is_whitespace) {
        written = written
            .split_whitespace()
            .filter(|word| !matches!(*word, "UNSIGNED" | "SIGNED" | "ZEROFILL"))
            .collect::<Vec<_>>()
            .join(" ");
    }
    let (base, arguments) = match written.split_once('(') {
        Some((base, rest)) => (
            base.trim(),
            Some(
                rest.strip_suffix(')')
                    .ok_or_else(|| format!("kilit cannot read the type {name}"))?,
            ),
        ),
        None => (written.as_str(), None),
    };
    let numbers = || -> Result<Vec<u64>, String> {
        arguments
            .unwrap_or_default()
            .split(',')
            .filter(|part| !part.trim().is_empty())
            .map(|part| {
                part.trim()
                    .parse::<u64>()
                    .map_err(|_| format!("kilit cannot read the type {name}"))
            })
            .collect()
    };
    let one = |default: u64| -> Result<u64, String> {
        match numbers()?.as_slice() {
            [] => Ok(default),
            [n] => Ok(*n),
            _ => Err(format!("kilit cannot read the type {name}")),
        }
    };
    match base {
        "CHAR" | "CHARACTER" => {
            let per_character = per_character()?;
            char_bytes(one(1)?.saturating_mul(per_character), per_character)
        }
        "BINARY" => fixed(one(1)?),
        "VARBINARY" => with_length(one(0)?),
        "TINYTEXT" | "TINYBLOB" => blob(9),
        "TEXT" | "BLOB" if arguments.is_none() => blob(10),
        "MEDIUMTEXT" | "MEDIUMBLOB" => blob(11),
        "LONGTEXT" | "LONGBLOB" | "JSON" | "GEOMETRY" | "POINT" => blob(12),
        "ENUM" if values(arguments) <= 255 => fixed(1),
        "SET" if values(arguments) <= 8 => fixed(1),
        "BIT" => match one(1)? {
            1..=8 => fixed(1),
            9..=16 => fixed(2),
            _ => not_counted(name),
        },
        "YEAR" if arguments.is_none() => fixed(1),
        "DATE" if arguments.is_none() => fixed(3),
        "TIME" => fixed(3 + fraction_bytes(one(0)?)?),
        "DATETIME" => fixed(5 + fraction_bytes(one(0)?)?),
        "TIMESTAMP" => fixed(4 + fraction_bytes(one(0)?)?),
        // FLOAT(p) is a FLOAT up to 24 digits of precision, a DOUBLE up
        // to 53; FLOAT(m,d) and DOUBLE(m,d) keep their size.
        "FLOAT" => match numbers()?.as_slice() {
            [] | [_, _] => fixed(4),
            [0..=24] => fixed(4),
            [25..=53] => fixed(8),
            _ => Err(format!("kilit cannot read the type {name}")),
        },
        "DOUBLE" | "REAL" | "DOUBLE PRECISION" => fixed(8),
        "DECIMAL" | "NUMERIC" | "DEC" => match numbers()?.as_slice() {
            [] => fixed(decimal_bytes(10, 0)),
            [precision] => fixed(decimal_bytes(*precision, 0)),
            [precision, scale] if scale <= precision => fixed(decimal_bytes(*precision, *scale)),
            _ => Err(format!("kilit cannot read the type {name}")),
        },
        _ => not_counted(name),
    }
}

/// The values of an ENUM or the members of a SET, as written between its
/// parentheses: the commas outside quotes, plus one.
fn values(arguments: Option<&str>) -> u64 {
    let Some(arguments) = arguments else {
        return 0;
    };
    let mut count = 1;
    let mut quoted = false;
    for c in arguments.chars() {
        match c {
            '\'' => quoted = !quoted,
            ',' if !quoted => count += 1,
            _ => {}
        }
    }
    count
}

/// The bytes of the fractional seconds of TIME, DATETIME and TIMESTAMP.
fn fraction_bytes(digits: u64) -> Result<u64, String> {
    match digits {
        0 => Ok(0),
        1 | 2 => Ok(1),
        3 | 4 => Ok(2),
        5 | 6 => Ok(3),
        _ => Err(format!(
            "fractional seconds of {digits} digits do not exist"
        )),
    }
}

/// A DECIMAL stores its integer part and its fraction apart: 4 bytes for
/// each 9 digits, and 1 to 4 bytes for the digits left over.
fn decimal_bytes(precision: u64, scale: u64) -> u64 {
    let part = |digits: u64| {
        let leftover = match digits % 9 {
            0 => 0,
            1 | 2 => 1,
            3 | 4 => 2,
            5 | 6 => 3,
            _ => 4,
        };
        digits / 9 * 4 + leftover
    };
    part(precision - scale) + part(scale)
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::schema::Schema;

    /// The table `name` of `sql`.
    fn known<'a>(schema: &'a Schema, name: &str) -> &'a Table {
        let crate::schema::TableState::Known(table) = schema.table(name) else {
            panic!("{name} is not known");
        };
        table
    }

    fn counts(sql: &str, name: &str, format: Option<Format>) -> Counts {
        let schema = Schema::from_sql(sql);
        let table = known(&schema, name);
        let columns: Vec<&Column> = table.columns.iter().collect();
        count(table, &columns, format).unwrap()
    }

    #[test]
    fn a_row_of_65535_bytes_is_the_most_mysql_allows() {
        // The lab's hand test C1: accepted at 65,532, refused at 65,533.
        let table = |length: u64| {
            format!(
                "CREATE TABLE r (f VARCHAR({length}) CHARACTER SET latin1 NOT NULL, \
                 x TINYINT NOT NULL) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4"
            )
        };
        assert_eq!(counts(&table(65532), "r", None).row, MAX_ROW_BYTES);
        assert_eq!(counts(&table(65533), "r", None).row, MAX_ROW_BYTES + 1);
    }

    #[test]
    fn the_record_of_the_lab_s_orders_takes_281_bytes_by_8_0_46_s_count() {
        let sql = include_str!("../../../scenarios/tables.sql");
        let page = counts(sql, "orders", Some(Format::Dynamic)).page;
        assert_eq!(page, Some((281, 8_125)));
    }

    /// The lab's table q: a BIGINT key and `bytes` of NOT NULL latin1
    /// CHAR(255) columns, the last one shorter.
    fn filler(bytes: u64) -> String {
        let mut columns = vec!["id BIGINT PRIMARY KEY".to_owned()];
        let mut left = bytes;
        let mut at = 0;
        while left > 0 {
            let length = left.min(255);
            columns.push(format!(
                "f{at} CHAR({length}) CHARACTER SET latin1 NOT NULL"
            ));
            left -= length;
            at += 1;
        }
        format!(
            "CREATE TABLE q ({}) DEFAULT CHARSET=utf8mb4",
            columns.join(", ")
        )
    }

    #[test]
    fn each_row_format_keeps_the_record_the_lab_measured_in_a_page() {
        // The largest filler each format accepted, by bisection on both
        // servers; one byte more is refused.
        for (format, accepted) in [
            (Format::Dynamic, 8_099),
            (Format::Compact, 8_099),
            (Format::Redundant, 8_025),
            (Format::Compressed, 7_976),
        ] {
            let fits = |bytes| {
                let (record, limit) = counts(&filler(bytes), "q", Some(format)).page.unwrap();
                record <= limit
            };
            assert!(fits(accepted), "{format:?}");
            assert!(!fits(accepted + 1), "{format:?}");
        }
    }

    /// Whether the record of `sql`'s table q fits a page of `format`.
    fn fits(sql: &str, format: Format) -> bool {
        let (record, limit) = counts(sql, "q", Some(format)).page.unwrap();
        record <= limit
    }

    #[test]
    fn a_long_column_counts_what_each_row_format_keeps_in_the_page() {
        // The lab's boundary pairs: a BIGINT key, x VARCHAR(255) utf8mb4,
        // and a filler; COMPACT keeps 790 bytes of x, REDUNDANT 788.
        let with_x = |bytes| {
            filler(bytes).replace(
                "id BIGINT PRIMARY KEY",
                "id BIGINT PRIMARY KEY, x VARCHAR(255) NOT NULL",
            )
        };
        for (format, accepted) in [(Format::Compact, 7_309), (Format::Redundant, 7_241)] {
            assert!(fits(&with_x(accepted), format), "{format:?}");
            assert!(!fits(&with_x(accepted + 1), format), "{format:?}");
        }
    }

    #[test]
    fn a_table_without_a_key_has_a_row_id_of_6_bytes() {
        // The lab: without a primary key the filler may be 2 bytes longer
        // than with a BIGINT one.
        let without_key = |bytes| filler(bytes).replace("id BIGINT PRIMARY KEY, ", "");
        assert!(fits(&without_key(8_101), Format::Dynamic));
        assert!(!fits(&without_key(8_102), Format::Dynamic));
    }

    #[test]
    fn mysql_8_0_46_caps_no_column_of_the_key() {
        // By hand: a VARCHAR(768) utf8mb4 key counts 3,074 bytes on 8.0.46.
        let sql = "CREATE TABLE q (id VARCHAR(768) NOT NULL PRIMARY KEY) DEFAULT CHARSET=utf8mb4";
        assert_eq!(
            counts(sql, "q", Some(Format::Dynamic)).page,
            Some((5 + 3_074 + 13, 8_125))
        );
    }

    #[test]
    fn a_key_on_a_prefix_and_a_unique_key_without_a_primary_key_are_counted_as_keys() {
        // A key on a prefix holds the prefix and the whole column; without a
        // primary key, InnoDB keys the rows on the first UNIQUE index of NOT
        // NULL columns, with no row id. kilit counts the larger.
        let prefix = "CREATE TABLE q (name VARCHAR(255) CHARACTER SET latin1 NOT NULL, \
                      PRIMARY KEY (name(200)))";
        assert_eq!(
            counts(prefix, "q", Some(Format::Dynamic)).page,
            Some((5 + 256 + 13 + 256, 8_125))
        );
        let unique = "CREATE TABLE q (u VARCHAR(700) NOT NULL, n INT NULL, UNIQUE KEY un (n), \
                      UNIQUE KEY uk (u)) DEFAULT CHARSET=utf8mb4";
        assert_eq!(
            counts(unique, "q", Some(Format::Dynamic)).page,
            Some((5 + 1 + 2_802 + 13 + 4, 8_125))
        );
    }

    #[test]
    fn a_fulltext_index_adds_8_bytes_to_the_record() {
        let sql = "CREATE TABLE q (id BIGINT PRIMARY KEY, t TEXT NULL, FULLTEXT KEY ft (t)) \
                   DEFAULT CHARSET=utf8mb4";
        assert_eq!(
            counts(sql, "q", Some(Format::Dynamic)).page,
            Some((5 + 1 + 8 + 13 + 41 + 8, 8_125))
        );
    }

    #[test]
    fn columns_whose_bytes_overflow_are_counted_without_wrapping() {
        // No such table exists in MySQL; kilit must still not wrap or panic.
        let sql = "CREATE TABLE q (id INT PRIMARY KEY,
                   v VARCHAR(4611686018427387904) CHARACTER SET utf8mb3,
                   w VARCHAR(18446744073709551615) CHARACTER SET utf8mb3)";
        let counts = counts(sql, "q", Some(Format::Dynamic));
        assert_eq!(counts.row, u64::MAX);
        // DYNAMIC keeps such long columns out of the page: 41 bytes each.
        assert_eq!(counts.page, Some((5 + 1 + 4 + 13 + 41 + 41, 8_125)));
    }

    #[test]
    fn a_key_column_whose_bytes_overflow_is_counted_without_wrapping_in_every_format() {
        // No such table exists in MySQL; DYNAMIC and COMPRESSED count a key
        // column whole, so its bytes reach the record and must saturate.
        let sql = "CREATE TABLE q (id VARCHAR(4611686018427387904) NOT NULL PRIMARY KEY) \
                   DEFAULT CHARSET=utf8mb4";
        for (format, page) in [
            (Format::Dynamic, (u64::MAX, 8_125)),
            (Format::Compressed, (u64::MAX, 8_069 - 2 * 3)),
            (Format::Compact, (5 + 13 + 790, 8_125)),
            (Format::Redundant, (6 + 2 * 3 + 13 + 788, 8_122)),
        ] {
            assert_eq!(
                counts(sql, "q", Some(format)).page,
                Some(page),
                "{format:?}"
            );
        }
    }

    #[test]
    fn a_record_of_more_fields_than_a_compressed_page_allows_has_a_limit_of_zero() {
        // No such table exists in MySQL (InnoDB takes at most 1,017
        // columns); kilit must still not wrap or panic: 4,100 fields leave
        // no room in a COMPRESSED page.
        let columns: Vec<String> = (1..=4_100)
            .map(|n| format!("c{n} TINYINT NOT NULL"))
            .collect();
        let sql = format!(
            "CREATE TABLE q (id INT PRIMARY KEY, {})",
            columns.join(", ")
        );
        let (record, limit) = counts(&sql, "q", Some(Format::Compressed)).page.unwrap();
        assert_eq!(limit, 0);
        assert!(record > limit);
    }

    #[test]
    fn a_decimal_counts_its_integer_part_and_its_fraction_apart() {
        // The lab's measured lines: (8,2) 4 bytes, (10,2) 5, (65,30) 30.
        assert_eq!(decimal_bytes(8, 2), 4);
        assert_eq!(decimal_bytes(10, 2), 5);
        assert_eq!(decimal_bytes(65, 30), 30);
        assert_eq!(decimal_bytes(10, 0), 5);
    }

    #[test]
    fn enum_values_and_set_members_are_counted_outside_quotes() {
        assert_eq!(values(Some("'a','b'")), 2);
        assert_eq!(values(Some("'a,b','c'")), 2);
        assert_eq!(values(None), 0);
    }
}
