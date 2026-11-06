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
}

/// The engines a scenario can give `orders`, besides the InnoDB of
/// `scenarios/tables.sql`.
const ENGINES: [&str; 1] = ["MyISAM"];

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
}

impl Scenario {
    /// The ALTER statement for one variant.
    pub fn statement(&self, variant: Variant) -> String {
        format!("ALTER TABLE orders {}{}", self.operation, variant.clause())
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
    Ok(Scenario {
        id: id.to_owned(),
        title: file.title,
        setup: file.setup,
        session: file.session,
        operation: file.operation,
        engine,
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
