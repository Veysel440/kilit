//! What the lab runs: scenarios read from `scenarios/*.toml`, and the
//! variants each one is tried with.

use std::fs;
use std::path::Path;

use kilit_core::Variant;
use serde::Deserialize;

use crate::Result;

/// One schema change to measure, read from `scenarios/<id>.toml`.
pub struct Scenario {
    /// The file name without `.toml`, e.g. `add-index`. Two files in one
    /// directory cannot share a name, so ids are unique by construction.
    pub id: String,
    pub title: String,
    /// Statements that prepare the tables before anything is measured,
    /// e.g. adding the index that the scenario then drops.
    pub setup: Vec<String>,
    /// Statements run in the same session right before the ALTER,
    /// e.g. `SET foreign_key_checks = 0`.
    pub session: Vec<String>,
    /// Everything that follows `ALTER TABLE orders`.
    pub operation: String,
    /// `MyISAM` to create `orders` with that engine instead of InnoDB.
    /// Nothing else about the tables changes.
    pub engine: Option<&'static str>,
    /// `COMPRESSED`, `COMPACT` or `REDUNDANT` to create `orders` with that
    /// `ROW_FORMAT` instead of the server's default, DYNAMIC.
    pub row_format: Option<&'static str>,
    /// `8` to create `orders` with that `KEY_BLOCK_SIZE`, which makes an
    /// InnoDB table COMPRESSED even without `ROW_FORMAT`.
    pub key_block_size: Option<u8>,
}

/// The engines a scenario can give `orders`, besides the InnoDB of
/// `scenarios/tables.sql`.
const ENGINES: [&str; 1] = ["MyISAM"];

/// The row formats a scenario can give `orders`, besides the server's
/// default.
const ROW_FORMATS: [&str; 3] = ["COMPRESSED", "COMPACT", "REDUNDANT"];

/// The key block sizes a scenario can give `orders`.
const KEY_BLOCK_SIZES: [u8; 1] = [8];

/// The contents of a scenario file. Unknown keys are an error, so a typo
/// such as `operaton` cannot silently drop a field.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ScenarioFile {
    title: String,
    #[serde(default)]
    setup: Vec<String>,
    #[serde(default)]
    session: Vec<String>,
    operation: String,
    engine: Option<String>,
    row_format: Option<String>,
    key_block_size: Option<u8>,
}

impl Scenario {
    /// The ALTER statement for one variant.
    pub fn statement(&self, variant: Variant) -> String {
        format!("ALTER TABLE orders {}{}", self.operation, variant.clause())
    }

    /// The table options `orders` gets besides those of `tables.sql`, as
    /// mysqldump writes them after the character set:
    /// ` ROW_FORMAT=COMPRESSED KEY_BLOCK_SIZE=8`. Empty when there are none.
    pub fn table_options(&self) -> String {
        let mut options = String::new();
        if let Some(row_format) = self.row_format {
            options.push_str(&format!(" ROW_FORMAT={row_format}"));
        }
        if let Some(size) = self.key_block_size {
            options.push_str(&format!(" KEY_BLOCK_SIZE={size}"));
        }
        options
    }

    /// The row format InnoDB reports for `orders`, as `INNODB_TABLES`
    /// spells it. A `KEY_BLOCK_SIZE` makes the table compressed.
    pub fn innodb_row_format(&self) -> &'static str {
        match (self.row_format, self.key_block_size) {
            (Some("COMPACT"), None) => "Compact",
            (Some("REDUNDANT"), None) => "Redundant",
            (Some(_), _) | (None, Some(_)) => "Compressed",
            (None, None) => "Dynamic",
        }
    }

    /// Whether the scenario sets `old_alter_table` in its session. Without
    /// an `ALGORITHM` clause such a session copies the table, while the
    /// variants that force an algorithm get that algorithm, so the lab
    /// reads its verdict differently: see [`crate::verdict::decide_as_run`].
    pub fn sets_old_alter_table(&self) -> bool {
        self.session
            .iter()
            .any(|statement| statement.to_ascii_lowercase().contains("old_alter_table"))
    }
}

/// Reads every `*.toml` file in `dir`, sorted by id.
pub fn load_all(dir: &Path) -> Result<Vec<Scenario>> {
    let entries = fs::read_dir(dir).map_err(|err| format!("{}: {err}", dir.display()))?;
    let mut scenarios = Vec::new();
    for entry in entries {
        let path = entry?.path();
        if path
            .extension()
            .is_some_and(|extension| extension == "toml")
        {
            let scenario = load(&path).map_err(|err| format!("{}: {err}", path.display()))?;
            scenarios.push(scenario);
        }
    }
    if scenarios.is_empty() {
        return Err(format!("no scenario files in {}", dir.display()).into());
    }
    scenarios.sort_by(|a, b| a.id.cmp(&b.id));
    Ok(scenarios)
}

fn load(path: &Path) -> Result<Scenario> {
    let id = path
        .file_stem()
        .and_then(|stem| stem.to_str())
        .ok_or("the file name is not valid UTF-8")?;
    parse(id, &fs::read_to_string(path)?)
}

fn parse(id: &str, text: &str) -> Result<Scenario> {
    check_id(id)?;
    let file: ScenarioFile = toml::from_str(text)?;
    let engine = match file.engine {
        None => None,
        Some(engine) => Some(
            ENGINES
                .into_iter()
                .find(|known| *known == engine)
                .ok_or_else(|| {
                    format!("`engine = \"{engine}\"`: the lab knows only {ENGINES:?}")
                })?,
        ),
    };
    let row_format = match file.row_format {
        None => None,
        Some(row_format) => Some(
            ROW_FORMATS
                .into_iter()
                .find(|known| *known == row_format)
                .ok_or_else(|| {
                    format!("`row_format = \"{row_format}\"`: the lab knows only {ROW_FORMATS:?}")
                })?,
        ),
    };
    if let Some(size) = file.key_block_size
        && !KEY_BLOCK_SIZES.contains(&size)
    {
        return Err(
            format!("`key_block_size = {size}`: the lab knows only {KEY_BLOCK_SIZES:?}").into(),
        );
    }
    // KEY_BLOCK_SIZE needs a COMPRESSED table: MySQL refuses it with the
    // other row formats.
    if file.key_block_size.is_some() && row_format.is_some_and(|format| format != "COMPRESSED") {
        return Err("`key_block_size` works only with `row_format = \"COMPRESSED\"`".into());
    }
    if engine.is_some() && (row_format.is_some() || file.key_block_size.is_some()) {
        return Err("`row_format` and `key_block_size` are for InnoDB tables only".into());
    }
    Ok(Scenario {
        id: id.to_owned(),
        title: file.title,
        setup: file.setup,
        session: file.session,
        operation: file.operation,
        engine,
        row_format,
        key_block_size: file.key_block_size,
    })
}

/// Ids appear in reports and result files: lowercase letters, digits and
/// single hyphens, like `add-unique-index`.
fn check_id(id: &str) -> Result<()> {
    let valid = !id.is_empty()
        && !id.starts_with('-')
        && !id.ends_with('-')
        && !id.contains("--")
        && id
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-');
    if valid {
        Ok(())
    } else {
        Err(format!(
            "`{id}` is not a valid scenario id: use lowercase letters, digits and single hyphens"
        )
        .into())
    }
}

#[cfg(test)]
mod tests {
    use kilit_core::Algorithm;

    use super::*;

    #[test]
    fn statement_appends_the_variant() {
        let scenario = parse(
            "add-index",
            r#"title = "t"
operation = "ADD INDEX i (amount)""#,
        )
        .unwrap();
        assert_eq!(
            scenario.statement(Variant::forced(Algorithm::Copy)),
            "ALTER TABLE orders ADD INDEX i (amount), ALGORITHM=COPY"
        );
    }

    #[test]
    fn setup_is_optional() {
        let scenario = parse("x", "title = \"t\"\noperation = \"DROP INDEX i\"").unwrap();
        assert!(scenario.setup.is_empty());

        let scenario = parse(
            "x",
            "title = \"t\"\nsetup = [\"ALTER TABLE orders ADD INDEX i (amount)\"]\noperation = \"DROP INDEX i\"",
        )
        .unwrap();
        assert_eq!(scenario.setup, ["ALTER TABLE orders ADD INDEX i (amount)"]);
    }

    #[test]
    fn session_is_optional() {
        let scenario = parse("x", "title = \"t\"\noperation = \"ADD INDEX i (amount)\"").unwrap();
        assert!(scenario.session.is_empty());

        let scenario = parse(
            "x",
            "title = \"t\"\nsession = [\"SET foreign_key_checks = 0\"]\noperation = \"o\"",
        )
        .unwrap();
        assert_eq!(scenario.session, ["SET foreign_key_checks = 0"]);
    }

    #[test]
    fn engine_is_optional_and_only_myisam() {
        let scenario = parse("x", "title = \"t\"\noperation = \"o\"").unwrap();
        assert_eq!(scenario.engine, None);

        let scenario = parse("x", "title = \"t\"\nengine = \"MyISAM\"\noperation = \"o\"").unwrap();
        assert_eq!(scenario.engine, Some("MyISAM"));

        for other in ["InnoDB", "myisam", "MEMORY"] {
            let text = format!("title = \"t\"\nengine = \"{other}\"\noperation = \"o\"");
            assert!(parse("x", &text).is_err(), "{other} should be refused");
        }
    }

    #[test]
    fn row_format_and_key_block_size_are_optional_and_only_the_measured_ones() {
        let scenario = parse("x", "title = \"t\"\noperation = \"o\"").unwrap();
        assert_eq!(scenario.table_options(), "");
        assert_eq!(scenario.innodb_row_format(), "Dynamic");

        for (keys, options, row_format) in [
            (
                "row_format = \"COMPRESSED\"",
                " ROW_FORMAT=COMPRESSED",
                "Compressed",
            ),
            ("row_format = \"COMPACT\"", " ROW_FORMAT=COMPACT", "Compact"),
            (
                "row_format = \"REDUNDANT\"",
                " ROW_FORMAT=REDUNDANT",
                "Redundant",
            ),
            ("key_block_size = 8", " KEY_BLOCK_SIZE=8", "Compressed"),
            (
                "row_format = \"COMPRESSED\"\nkey_block_size = 8",
                " ROW_FORMAT=COMPRESSED KEY_BLOCK_SIZE=8",
                "Compressed",
            ),
        ] {
            let text = format!("title = \"t\"\n{keys}\noperation = \"o\"");
            let scenario = parse("x", &text).unwrap();
            assert_eq!(scenario.table_options(), options, "{keys}");
            assert_eq!(scenario.innodb_row_format(), row_format, "{keys}");
        }

        for refused in [
            "row_format = \"DYNAMIC\"",
            "row_format = \"compact\"",
            "row_format = \"FIXED\"",
            "key_block_size = 4",
            "key_block_size = 0",
            "row_format = \"COMPACT\"\nkey_block_size = 8",
            "engine = \"MyISAM\"\nrow_format = \"COMPACT\"",
            "engine = \"MyISAM\"\nkey_block_size = 8",
        ] {
            let text = format!("title = \"t\"\n{refused}\noperation = \"o\"");
            assert!(parse("x", &text).is_err(), "{refused} should be refused");
        }
    }

    #[test]
    fn a_session_that_names_old_alter_table_sets_it() {
        let scenario = parse("x", "title = \"t\"\noperation = \"o\"").unwrap();
        assert!(!scenario.sets_old_alter_table());
        let scenario = parse(
            "x",
            "title = \"t\"\nsession = [\"SET foreign_key_checks = 0\"]\noperation = \"o\"",
        )
        .unwrap();
        assert!(!scenario.sets_old_alter_table());
        let scenario = parse(
            "x",
            "title = \"t\"\nsession = [\"SET SESSION old_alter_table = ON\"]\noperation = \"o\"",
        )
        .unwrap();
        assert!(scenario.sets_old_alter_table());
    }

    #[test]
    fn unknown_keys_are_rejected() {
        let typo = parse("x", "title = \"t\"\noperaton = \"ADD INDEX i (amount)\"");
        assert!(typo.is_err());
    }

    #[test]
    fn ids_are_lowercase_words_joined_by_hyphens() {
        for good in ["add-index", "varchar-50-to-60", "x"] {
            assert!(check_id(good).is_ok(), "{good} should be valid");
        }
        for bad in [
            "",
            "Add-Index",
            "add_index",
            "-add",
            "add-",
            "add--index",
            "add index",
        ] {
            assert!(check_id(bad).is_err(), "{bad:?} should be invalid");
        }
    }

    #[test]
    fn every_scenario_file_in_the_repository_loads() {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../scenarios");
        let scenarios = load_all(&dir).unwrap();
        assert!(scenarios.len() >= 5);
    }

    /// kilit can only use a measurement if it can read the statement that
    /// was measured: every statement the lab sends must come back from
    /// kilit-core as exactly what the lab meant.
    #[test]
    fn kilit_core_reads_every_statement_the_lab_runs() {
        use kilit_core::{Lock, Operation, Statement, StatementKind};

        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../scenarios");
        for scenario in load_all(&dir).unwrap() {
            for sql in &scenario.session {
                let statements = kilit_core::parse(sql);
                assert!(
                    matches!(
                        statements.as_slice(),
                        [Statement {
                            kind: StatementKind::Set { .. },
                            ..
                        }]
                    ),
                    "{}: {statements:?}",
                    scenario.id
                );
            }

            for variant in Variant::ALL {
                let sql = scenario.statement(variant);
                let statements = kilit_core::parse(&sql);
                let [
                    Statement {
                        kind: StatementKind::Alter(alter),
                        ..
                    },
                ] = statements.as_slice()
                else {
                    panic!("{}: not one ALTER TABLE: {statements:?}", scenario.id);
                };
                assert_eq!(alter.table, "orders", "{sql}");
                assert_eq!(alter.algorithm, variant.algorithm, "{sql}");
                assert_eq!(alter.lock, variant.lock_none.then_some(Lock::None), "{sql}");
                assert!(!alter.operations.is_empty(), "{sql}");
                for operation in &alter.operations {
                    // An `Other` operation prints as "not understood: ...".
                    assert!(
                        !matches!(operation, Operation::Other { .. }),
                        "{}: {operation}",
                        scenario.id
                    );
                }
            }
        }
    }
}
