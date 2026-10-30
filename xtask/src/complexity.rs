//! Reads `rust-code-analysis-cli` JSON and applies the limits.

use std::collections::BTreeMap;
use std::fmt;
use std::path::Path;
use std::process::{Command, ExitCode};

use serde::Deserialize;

/// The limits of `02-rules.md` R4, R8 and R9.
pub const LIMITS: Limits = Limits {
    cyclomatic: 10.0,
    volume: 1000.0,
    difficulty: 30.0,
    python_lines: 50.0,
};

/// The highest permitted value of each metric of a function.
pub struct Limits {
    /// Cyclomatic complexity.
    pub cyclomatic: f64,
    /// Halstead volume.
    pub volume: f64,
    /// Halstead difficulty.
    pub difficulty: f64,
    /// Code lines of a Python function.
    pub python_lines: f64,
}

/// A node of the space tree: a unit (file), an `impl`, a function or another scope.
#[derive(Deserialize)]
pub struct Space {
    /// The file path for a unit, else the item name.
    pub name: String,
    /// The first line.
    pub start_line: u64,
    /// `unit`, `function`, `impl` or another scope kind.
    pub kind: String,
    /// The nested spaces.
    pub spaces: Vec<Space>,
    /// The metrics of the space and its nested spaces, by group (`halstead`) and name (`volume`).
    /// An average over no functions has no value.
    pub metrics: BTreeMap<String, BTreeMap<String, Option<f64>>>,
}

/// One metric of one function above its limit.
pub struct Violation {
    /// The file of the function.
    pub file: String,
    /// The first line of the function.
    pub line: u64,
    /// The function name.
    pub function: String,
    /// The metric as `<group>.<name>`.
    pub metric: &'static str,
    /// The measured value.
    pub value: f64,
    /// The limit.
    pub limit: f64,
}

impl fmt::Display for Violation {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "{}:{} {} {} {} > {}",
            self.file, self.line, self.function, self.metric, self.value, self.limit
        )
    }
}

/// The violations of every function in the space tree of `unit`.
#[must_use]
pub fn violations(unit: &Space, limits: &Limits) -> Vec<Violation> {
    let python = Path::new(&unit.name)
        .extension()
        .is_some_and(|extension| extension == "py");
    space_violations(unit, &unit.name, python, limits)
}

/// The violations of `space` and its nested spaces.
fn space_violations(space: &Space, file: &str, python: bool, limits: &Limits) -> Vec<Violation> {
    let own = if space.kind == "function" {
        function_violations(space, file, python, limits)
    } else {
        Vec::new()
    };
    let nested = space
        .spaces
        .iter()
        .flat_map(|child| space_violations(child, file, python, limits));
    own.into_iter().chain(nested).collect()
}

/// The metrics of the function `space` that are strictly above their limits.
fn function_violations(space: &Space, file: &str, python: bool, limits: &Limits) -> Vec<Violation> {
    let mut checked = vec![
        ("cyclomatic.sum", limits.cyclomatic),
        ("halstead.volume", limits.volume),
        ("halstead.difficulty", limits.difficulty),
    ];
    if python {
        checked.push(("loc.ploc", limits.python_lines));
    }
    checked
        .into_iter()
        .map(|(metric, limit)| (metric, value(space, metric), limit))
        .filter(|(_, value, limit)| value > limit)
        .map(|(metric, value, limit)| Violation {
            file: file.to_owned(),
            line: space.start_line,
            function: space.name.clone(),
            metric,
            value,
            limit,
        })
        .collect()
}

/// The value of `metric` (`<group>.<name>`) of `space`.
///
/// # Panics
///
/// When `space` has no value for `metric`.
fn value(space: &Space, metric: &str) -> f64 {
    let (group, name) = metric.split_once('.').expect("a metric is <group>.<name>");
    space.metrics[group][name].expect("a function has a value for each metric")
}

/// The units of the files in `path`, from `rust-code-analysis-cli`.
///
/// # Panics
///
/// When `rust-code-analysis-cli` does not start, fails or writes output that is not its JSON.
#[must_use]
pub fn units(path: &str) -> Vec<Space> {
    let output = Command::new("rust-code-analysis-cli")
        .args(["-m", "-O", "json", "-p", path])
        .output()
        .expect("rust-code-analysis-cli starts");
    assert!(
        output.status.success(),
        "rust-code-analysis-cli fails on {path}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::Deserializer::from_slice(&output.stdout)
        .into_iter()
        .collect::<Result<_, _>>()
        .expect("the output is a JSON unit for each file")
}

/// Prints each violation in `paths`. Fails when there is one or more.
#[must_use]
pub fn run(paths: &[String]) -> ExitCode {
    let found: Vec<Violation> = paths
        .iter()
        .flat_map(|path| units(path))
        .flat_map(|unit| violations(&unit, &LIMITS))
        .collect();
    for violation in &found {
        println!("{violation}");
    }
    if found.is_empty() {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}

/// Tests of the limits.
#[cfg(test)]
mod tests {
    use super::*;

    /// No report lines.
    const NONE: [&str; 0] = [];

    /// A function space with the given metrics.
    fn function(name: &str, cyclomatic: f64, volume: f64, difficulty: f64, ploc: f64) -> Space {
        serde_json::from_value(serde_json::json!({
            "name": name, "start_line": 7, "kind": "function", "spaces": [],
            "metrics": {
                "cyclomatic": {"sum": cyclomatic},
                "halstead": {"volume": volume, "difficulty": difficulty},
                "loc": {"ploc": ploc},
            },
        }))
        .expect("the space is valid")
    }

    /// A space of `kind` with `spaces` and metrics far above the limits.
    fn scope(name: &str, kind: &str, spaces: Vec<Space>) -> Space {
        Space {
            kind: kind.to_owned(),
            spaces,
            ..function(name, 100.0, 9000.0, 90.0, 900.0)
        }
    }

    /// The report lines of a unit `file` with `spaces`.
    fn report(file: &str, spaces: Vec<Space>) -> Vec<String> {
        let unit = scope(file, "unit", spaces);
        let found = violations(&unit, &LIMITS);
        found.iter().map(ToString::to_string).collect()
    }

    /// A cyclomatic complexity of 11 is one violation with its location and metric.
    #[test]
    fn ac_0_3_1_cyclomatic_above_limit() {
        let found = report("src/a.rs", vec![function("f", 11.0, 1.0, 1.0, 1.0)]);
        assert_eq!(found, ["src/a.rs:7 f cyclomatic.sum 11 > 10"]);
    }

    /// A cyclomatic complexity of 10 passes.
    #[test]
    fn ac_0_3_2_cyclomatic_at_limit() {
        let found = report("src/a.rs", vec![function("f", 10.0, 1.0, 1.0, 1.0)]);
        assert_eq!(found, NONE);
    }

    /// A Halstead volume of 1000.1 is one violation, 1000.0 passes.
    #[test]
    fn ac_0_3_3_volume_limit() {
        let above = report("src/a.rs", vec![function("f", 1.0, 1000.1, 1.0, 1.0)]);
        assert_eq!(above, ["src/a.rs:7 f halstead.volume 1000.1 > 1000"]);
        let at = report("src/a.rs", vec![function("f", 1.0, 1000.0, 1.0, 1.0)]);
        assert_eq!(at, NONE);
    }

    /// A Halstead difficulty of 30.1 is one violation, 30.0 passes.
    #[test]
    fn ac_0_3_4_difficulty_limit() {
        let above = report("src/a.rs", vec![function("f", 1.0, 1.0, 30.1, 1.0)]);
        assert_eq!(above, ["src/a.rs:7 f halstead.difficulty 30.1 > 30"]);
        let at = report("src/a.rs", vec![function("f", 1.0, 1.0, 30.0, 1.0)]);
        assert_eq!(at, NONE);
    }

    /// 51 code lines fail a Python function and pass a Rust function.
    #[test]
    fn ac_0_3_5_python_lines_limit() {
        let python = report("train/a.py", vec![function("f", 1.0, 1.0, 1.0, 51.0)]);
        assert_eq!(python, ["train/a.py:7 f loc.ploc 51 > 50"]);
        let rust = report("src/a.rs", vec![function("f", 1.0, 1.0, 1.0, 51.0)]);
        assert_eq!(rust, NONE);
    }

    /// A function inside an `impl` inside a unit is checked.
    #[test]
    fn ac_0_3_6_function_in_impl() {
        let method = function("m", 11.0, 1.0, 1.0, 1.0);
        let found = report("src/a.rs", vec![scope("S", "impl", vec![method])]);
        assert_eq!(found, ["src/a.rs:7 m cyclomatic.sum 11 > 10"]);
    }

    /// A space that is not a function gives no violation.
    #[test]
    fn ac_0_3_7_other_spaces_not_checked() {
        assert_eq!(
            report("src/a.rs", vec![scope("S", "impl", Vec::new())]),
            NONE
        );
    }
}
