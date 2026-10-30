//! Gate tests (real tools on fixtures) and structure tests (the files that configure the checks).

use std::collections::BTreeSet;
use std::env;
use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::Mutex;

use syn::buffer::{Cursor, TokenBuffer};
use syn::{Attribute, ImplItem, Item, ItemImpl, ItemMod};
use tempfile::TempDir;

/// The environment variables that a nested tool gets. The coverage settings of an outer `cargo llvm-cov` would change its result.
const KEPT_VARIABLES: [&str; 5] = [
    "PATH",
    "HOME",
    "CARGO_HOME",
    "RUSTUP_HOME",
    "RUSTUP_TOOLCHAIN",
];

/// A fixture crate with 10 lines, of which its test covers 5.
const HALF_COVERED: &str = "//! Fixture.

/// Adds one.
pub fn covered(value: u32) -> u32 {
    let next = value + 1;
    next
}

/// Adds the double and the triple.
pub fn uncovered(value: u32) -> u32 {
    let doubled = value * 2;
    let tripled = value * 3;
    doubled + tripled
}

#[test]
fn covers() { assert_eq!(covered(1), 2); }
";

/// A test that covers the lines of [`HALF_COVERED`] that its own test does not cover.
const REST_COVERED: &str = "
#[test]
fn covers_rest() { assert_eq!(uncovered(1), 5); }
";

/// A fixture crate with a function that is not used.
const UNUSED_FUNCTION: &str = "//! Fixture.

/// Returns one.
fn unused() -> u32 {
    1
}
";

/// A fixture crate that calls `unwrap` outside a test.
const UNWRAP_OUTSIDE_TEST: &str = "//! Fixture.

/// Parses a number.
///
/// # Panics
///
/// When `text` is not a number.
#[must_use]
pub fn parse(text: &str) -> u32 {
    text.parse().unwrap()
}
";

/// A fixture crate that calls `unwrap` inside a test.
const UNWRAP_INSIDE_TEST: &str = "//! Fixture.

/// Tests of the fixture.
#[cfg(test)]
mod tests {
    /// Parses a number.
    #[test]
    fn parses() {
        let number: u32 = \"1\".parse().unwrap();
        assert_eq!(number, 1);
    }
}
";

/// A fixture crate with a private function that has no doc comment.
const UNDOCUMENTED_FUNCTION: &str = "//! Fixture.

/// Returns `value`.
#[must_use]
pub fn public(value: u32) -> u32 {
    private(value)
}

fn private(value: u32) -> u32 {
    value
}
";

/// A fixture crate with a private struct that has no doc comment.
const UNDOCUMENTED_STRUCT: &str = "//! Fixture.

/// Returns `value`.
#[must_use]
pub fn public(value: u32) -> u32 {
    Private { value }.value
}

struct Private {
    /// The value.
    value: u32,
}
";

/// A fixture crate with an `unsafe` block.
const UNSAFE_BLOCK: &str = "//! Fixture.

/// Reads `value` through a raw pointer.
#[must_use]
pub fn read(value: &u32) -> u32 {
    // SAFETY: a reference is valid to read.
    unsafe { *std::ptr::from_ref(value) }
}
";

/// A fixture crate that is not formatted.
const UNFORMATTED: &str = "//! Fixture.

/// Returns one.
#[must_use]
pub fn one( ) -> u32 { 1 }
";

/// A fixture file with let chains in two functions.
const LET_CHAINS: &str = "fn first(a: Option<u32>, b: Option<u32>) -> u32 {
    if let Some(x) = a && let Some(y) = b && x > y { x } else { 0 }
}

fn second(a: Option<u32>) -> bool {
    while let Some(x) = a && x > 1 { return true; }
    false
}
";

/// The paths that Git must ignore: build output, models, the virtual environment, the local configuration and the data files.
const IGNORED: [&str; 11] = [
    "target/",
    "models/",
    ".venv/",
    "jarvis.local.toml",
    "habits.json",
    "voices.json",
    "jarvis.db",
    "generated.json",
    "training.json",
    "pools.json",
    "learned.toml",
];

/// Holds the gate tests that run `quint verify` one at a time: each starts an Apalache server on the same port.
static APALACHE: Mutex<()> = Mutex::new(());

/// The check boxes of the PR template that are not CI jobs.
const OTHER_BOXES: [&str; 3] = [
    "Started in a fresh context",
    "Codex review threads resolved",
    "`[GPU host]` criteria pass in `gpu-test`",
];

/// The repository root.
fn root() -> &'static Path {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("xtask is a folder of the repository root")
}

/// Runs `command` with only the kept environment variables.
fn run(command: &mut Command) -> Output {
    let kept: Vec<(OsString, OsString)> = env::vars_os()
        .filter(|(name, _)| {
            name.to_str()
                .is_some_and(|name| KEPT_VARIABLES.contains(&name))
        })
        .collect();
    command
        .env_clear()
        .envs(kept)
        .output()
        .expect("the command starts")
}

/// Runs `program` with `arguments` in the repository root and returns its standard output.
fn stdout_in_root(program: &str, arguments: &[&str]) -> String {
    let output = run(Command::new(program).args(arguments).current_dir(root()));
    assert!(
        output.status.success(),
        "{program} {arguments:?} fails: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).expect("the output is UTF-8")
}

/// Reads a file of the repository.
fn read(path: &str) -> String {
    fs::read_to_string(root().join(path)).expect("the file exists")
}

/// Parses a YAML file of the repository.
fn yaml(path: &str) -> serde_yaml::Value {
    serde_yaml::from_str(&read(path)).expect("the file is YAML")
}

/// Writes a temporary crate with `source` as `src/lib.rs` and the workspace lint settings.
fn fixture_crate(source: &str) -> TempDir {
    let workspace: toml::Table = read("Cargo.toml").parse().expect("Cargo.toml is TOML");
    let mut lints = toml::Table::new();
    if let Some(workspace_lints) = workspace
        .get("workspace")
        .and_then(|table| table.get("lints"))
    {
        lints.insert("lints".to_owned(), workspace_lints.clone());
    }
    let folder = TempDir::new().expect("a temporary folder");
    let manifest = format!(
        "[package]\nname = \"fixture\"\nversion = \"0.0.0\"\nedition = \"2024\"\n\n{lints}"
    );
    fs::write(folder.path().join("Cargo.toml"), manifest).expect("the manifest is written");
    fs::create_dir(folder.path().join("src")).expect("the source folder is made");
    fs::write(folder.path().join("src/lib.rs"), source).expect("the source is written");
    fs::copy(
        root().join("clippy.toml"),
        folder.path().join("clippy.toml"),
    )
    .expect("clippy.toml is copied");
    folder
}

/// A fixture crate with one function whose body has `lines` lines.
fn long_function(lines: usize) -> String {
    let additions = "    total += 1;\n".repeat(lines - 2);
    format!(
        "//! Fixture.\n\n/// Adds.\n#[must_use]\npub fn long(value: u32) -> u32 {{\n    let mut total = value;\n{additions}    total\n}}\n"
    )
}

/// Runs the `justfile` recipe `recipe` in `folder`.
fn recipe_in(folder: &Path, recipe: &str) -> Output {
    run(Command::new("just")
        .arg("--justfile")
        .arg(root().join("justfile"))
        .arg("--working-directory")
        .arg(folder)
        .arg(recipe))
}

/// Runs the `justfile` recipe `recipe` on a fixture crate with `source`.
fn recipe_on_fixture(recipe: &str, source: &str) -> Output {
    recipe_in(fixture_crate(source).path(), recipe)
}

/// Checks that `output` is a failure that names `lint`. Rustc writes lint names with `-`, Clippy with `_`.
fn assert_fails_with(output: &Output, lint: &str) {
    let stderr = String::from_utf8_lossy(&output.stderr).replace('-', "_");
    assert!(
        !output.status.success() && stderr.contains(lint),
        "the command fails with {lint}, its output is: {stderr}"
    );
}

/// Checks that `output` is a success.
fn assert_passes(output: &Output) {
    assert!(
        output.status.success(),
        "the command fails: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

/// Runs the coverage gate on a fixture crate with `source`.
fn coverage_gate(source: &str) -> Output {
    let fixture = fixture_crate(source);
    run(Command::new("cargo")
        .args(["llvm-cov", "--fail-under-lines", "80.01"])
        .current_dir(fixture.path()))
}

/// The `justfile` as JSON.
fn justfile() -> serde_json::Value {
    let dump = stdout_in_root("just", &["--dump", "--dump-format", "json"]);
    serde_json::from_str(&dump).expect("the dump is JSON")
}

/// The names of the recipes that `recipe` depends on.
fn dependencies(justfile: &serde_json::Value, recipe: &str) -> Vec<String> {
    let dependencies = justfile["recipes"][recipe]["dependencies"]
        .as_array()
        .expect("the recipe exists");
    dependencies
        .iter()
        .map(|dependency| {
            dependency["recipe"]
                .as_str()
                .expect("a recipe name")
                .to_owned()
        })
        .collect()
}

/// The steps of all jobs of a workflow.
fn steps(workflow: &serde_yaml::Value) -> Vec<&serde_yaml::Value> {
    let jobs = workflow["jobs"]
        .as_mapping()
        .expect("the workflow has jobs");
    jobs.values()
        .flat_map(|job| job["steps"].as_sequence().expect("the job has steps"))
        .collect()
}

/// The `run` commands of the steps of `job`.
fn runs<'workflow>(workflow: &'workflow serde_yaml::Value, job: &str) -> Vec<&'workflow str> {
    let steps = workflow["jobs"][job]["steps"]
        .as_sequence()
        .expect("the job exists");
    steps
        .iter()
        .filter_map(|step| step["run"].as_str())
        .collect()
}

/// Checks that a CI step installs a tool only with its `install-` recipe or with the version of its variable.
fn assert_installs_pinned(
    step: &serde_yaml::Value,
    justfile: &serde_json::Value,
    installed: &BTreeSet<String>,
) {
    if let Some(command) = step["run"]
        .as_str()
        .filter(|command| command.contains("install"))
    {
        let recipe = command.strip_prefix("just install-").unwrap_or(command);
        assert!(
            installed.contains(recipe),
            "the step runs an install- recipe: {command}"
        );
    }
    let action = step["uses"]
        .as_str()
        .and_then(|uses| uses.split_once('/'))
        .and_then(|(_, action)| action.strip_prefix("setup-"));
    if let Some((tool, _)) = action.and_then(|action| action.split_once('@')) {
        let variable = format!("{}_version", tool.replace('-', "_"));
        let version = justfile["assignments"][&variable]["value"]
            .as_str()
            .expect("the tool has a version variable");
        assert_eq!(
            step["with"][format!("{tool}-version").as_str()].as_str(),
            Some(version),
            "the {tool} step uses the version of {variable}"
        );
    }
}

/// The paths of the file table of `01-architecture.md` section 8.
fn file_table() -> Vec<String> {
    let architecture = read("docs/spec/01-architecture.md");
    let (_, section) = architecture
        .split_once("## 8. Files")
        .expect("section 8 exists");
    let (section, _) = section.split_once("\n## ").expect("section 9 follows");
    section
        .lines()
        .filter_map(|line| line.strip_prefix("| `"))
        .filter_map(|line| line.split_once('`'))
        .map(|(path, _)| path.to_owned())
        .collect()
}

/// Whether `path` matches a row of the file table. A `*` in a row matches any text.
fn in_table(table: &[String], path: &str) -> bool {
    table.iter().any(|row| match row.split_once('*') {
        Some((prefix, suffix)) => path.starts_with(prefix) && path.ends_with(suffix),
        None => row == path,
    })
}

/// A fixture file with a function that has 11 `if` branches: cyclomatic complexity 12.
fn branchy_function() -> String {
    let branches: Vec<String> = (1..=11)
        .map(|branch| format!("    if value == {branch} {{ return {branch}; }}\n"))
        .collect();
    format!(
        "fn branchy(value: u32) -> u32 {{\n{}    0\n}}\n",
        branches.concat()
    )
}

/// Writes `source` to a temporary folder as `name`.
fn fixture_file(name: &str, source: &str) -> TempDir {
    let folder = TempDir::new().expect("a temporary folder");
    fs::write(folder.path().join(name), source).expect("the source is written");
    folder
}

/// Checks that `check` runs `recipe` after `before`, and that the CI job `recipe` runs `just <recipe>`.
fn assert_check_and_ci_run_after(before: &str, recipe: &str) {
    let check = dependencies(&justfile(), "check");
    let position = |name: &str| check.iter().position(|dependency| dependency == name);
    assert!(
        matches!((position(before), position(recipe)), (Some(first), Some(second)) if first < second),
        "check runs {recipe} after {before}, it runs {check:?}"
    );
    let command = format!("just {recipe}");
    assert!(
        runs(&yaml(".github/workflows/ci.yml"), recipe).contains(&command.as_str()),
        "{recipe} runs {command}"
    );
}

/// A fixture file with a function of 40 lines, of which no two are the same.
fn distinct_function() -> String {
    let lines: Vec<String> = (1..=37)
        .map(|line| format!("    let total = total * {line} + 1;\n"))
        .collect();
    format!(
        "fn distinct(total: u32) -> u32 {{\n{}    total\n}}\n",
        lines.concat()
    )
}

/// Runs `jscpd` with the limits of the `duplication` recipe on `folder`.
fn duplication_gate(folder: &Path) -> Output {
    run(Command::new("jscpd")
        .args([
            "--threshold",
            "3",
            "--min-tokens",
            "50",
            "--format",
            "rust,python",
        ])
        .arg(folder))
}

/// The workspace builds.
#[test]
fn ac_0_1_1_workspace_builds() {
    let target = TempDir::new().expect("a temporary folder");
    let output = run(Command::new("cargo")
        .args(["build", "--workspace", "--target-dir"])
        .arg(target.path())
        .current_dir(root()));
    assert!(
        output.status.success(),
        "cargo build --workspace fails: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

/// The coverage gate fails 50% line coverage and passes 100%.
#[test]
fn ac_0_1_3_coverage_gate() {
    let half = coverage_gate(HALF_COVERED);
    let report = String::from_utf8_lossy(&half.stdout);
    let total: Vec<&str> = report
        .lines()
        .find(|line| line.starts_with("TOTAL"))
        .expect("the report has a total")
        .split_whitespace()
        .collect();
    assert!(
        total.windows(3).any(|lines| lines == ["10", "5", "50.00%"]),
        "the fixture has 10 lines with 5 missed, the total is {total:?}"
    );
    assert!(!half.status.success(), "the gate passes 50% line coverage");
    let full = coverage_gate(&format!("{HALF_COVERED}{REST_COVERED}"));
    assert!(
        full.status.success(),
        "the gate fails 100% line coverage: {}",
        String::from_utf8_lossy(&full.stderr)
    );
}

/// `check` runs the workflow lint and the coverage gate with its limit.
#[test]
fn ac_0_1_4_check_runs_coverage_gate() {
    let justfile = justfile();
    let check = dependencies(&justfile, "check");
    assert!(
        check.contains(&"workflow-lint".to_owned()) && check.contains(&"cov".to_owned()),
        "check runs workflow-lint and cov, it runs {check:?}"
    );
    let cov = justfile["recipes"]["cov"]["body"].to_string();
    assert!(
        cov.contains("--fail-under-lines 80.01"),
        "cov has the 80.01 limit: {cov}"
    );
}

/// The pre-commit hook runs `just check` on each commit.
#[test]
fn ac_0_1_5_hook_runs_check() {
    let config = yaml(".pre-commit-config.yaml");
    let repos = config["repos"].as_sequence().expect("the config has repos");
    let hooks: Vec<&serde_yaml::Value> = repos
        .iter()
        .flat_map(|repo| repo["hooks"].as_sequence().expect("the repo has hooks"))
        .collect();
    let [hook] = hooks.as_slice() else {
        panic!("the config has one hook, it has {}", hooks.len());
    };
    let expected: serde_yaml::Value = serde_yaml::from_str(
        "{entry: just check, stages: [pre-commit], always_run: true, pass_filenames: false}",
    )
    .expect("the expected hook is YAML");
    let expected = expected
        .as_mapping()
        .expect("the expected hook is a mapping");
    for (key, value) in expected {
        assert_eq!(&hook[key], value, "hook field {key:?}");
    }
}

/// CI has the triggers, with `push` on `main` only, and runs the checks of this sub-phase.
#[test]
fn ac_0_1_6_ci_runs_checks() {
    let ci = yaml(".github/workflows/ci.yml");
    let expected: serde_yaml::Value =
        serde_yaml::from_str("{push: {branches: [main]}, pull_request: null, workflow_call: null}")
            .expect("the expected triggers are YAML");
    assert_eq!(ci["on"], expected, "the triggers");
    assert!(
        runs(&ci, "rust-test").contains(&"just cov"),
        "rust-test runs just cov"
    );
    assert!(
        runs(&ci, "workflow-lint").contains(&"just workflow-lint"),
        "workflow-lint runs just workflow-lint"
    );
}

/// Git ignores build output, models and data files.
#[test]
fn ac_0_1_7_git_ignores_generated_files() {
    let mut arguments = vec!["-c", "core.excludesFile=/dev/null", "check-ignore"];
    arguments.extend(IGNORED);
    let ignored = stdout_in_root("git", &arguments);
    let lines: Vec<&str> = ignored.lines().collect();
    assert_eq!(lines, IGNORED);
}

/// The toolchain is one fixed version with the required components.
#[test]
fn ac_0_1_8_toolchain_is_fixed() {
    let toolchain: toml::Table = read("rust-toolchain.toml")
        .parse()
        .expect("the file is TOML");
    let channel = toolchain["toolchain"]["channel"]
        .as_str()
        .expect("a channel");
    let parts: Result<Vec<u32>, _> = channel.split('.').map(str::parse).collect();
    assert!(
        matches!(parts.as_deref(), Ok([_, _, _])),
        "the channel is a version x.y.z: {channel}"
    );
    let components: BTreeSet<&str> = toolchain["toolchain"]["components"]
        .as_array()
        .expect("components")
        .iter()
        .map(|component| component.as_str().expect("a component name"))
        .collect();
    assert_eq!(
        components,
        BTreeSet::from(["clippy", "rustfmt", "llvm-tools-preview"])
    );
}

/// Each tool has a fixed version that `setup` and CI install.
#[test]
fn ac_0_1_9_tools_have_fixed_versions() {
    let justfile = justfile();
    let variables = justfile["assignments"].as_object().expect("assignments");
    let versioned: BTreeSet<String> = variables
        .keys()
        .filter_map(|name| name.strip_suffix("_version"))
        .map(|tool| tool.replace('_', "-"))
        .collect();
    let recipes = justfile["recipes"].as_object().expect("recipes");
    let installed: BTreeSet<String> = recipes
        .keys()
        .filter_map(|name| name.strip_prefix("install-"))
        .map(str::to_owned)
        .collect();
    assert_eq!(
        versioned, installed,
        "each tool has a version variable and an install- recipe"
    );
    let setup: BTreeSet<String> = dependencies(&justfile, "setup")
        .into_iter()
        .filter_map(|name| name.strip_prefix("install-").map(str::to_owned))
        .collect();
    assert_eq!(setup, installed, "setup runs every install- recipe");
    let ci = yaml(".github/workflows/ci.yml");
    for step in steps(&ci) {
        assert_installs_pinned(step, &justfile, &installed);
    }
}

/// Each tracked file is in the file table.
#[test]
fn ac_0_1_10_tracked_files_are_in_table() {
    let table = file_table();
    let tracked = stdout_in_root("git", &["ls-files"]);
    let unlisted: Vec<&str> = tracked
        .lines()
        .filter(|path| !in_table(&table, path))
        .collect();
    assert!(
        unlisted.is_empty(),
        "files not in 01-architecture.md section 8: {unlisted:?}"
    );
}

/// `lint` fails a function with 51 lines and passes 50.
#[test]
fn ac_0_2_1_lint_fails_long_function() {
    let long = recipe_on_fixture("lint", &long_function(51));
    assert_fails_with(&long, "too_many_lines");
    assert_fails_with(&long, "(51/50)");
    assert_passes(&recipe_on_fixture("lint", &long_function(50)));
}

/// `lint` fails a function that is not used.
#[test]
fn ac_0_2_2_lint_fails_dead_code() {
    assert_fails_with(&recipe_on_fixture("lint", UNUSED_FUNCTION), "dead_code");
}

/// `lint` fails `unwrap` outside a test and passes it inside a test.
#[test]
fn ac_0_2_3_lint_fails_unwrap_outside_test() {
    assert_fails_with(
        &recipe_on_fixture("lint", UNWRAP_OUTSIDE_TEST),
        "unwrap_used",
    );
    assert_passes(&recipe_on_fixture("lint", UNWRAP_INSIDE_TEST));
}

/// `lint` fails a private function or a private struct without a doc comment.
#[test]
fn ac_0_2_4_lint_fails_missing_private_docs() {
    for source in [UNDOCUMENTED_FUNCTION, UNDOCUMENTED_STRUCT] {
        assert_fails_with(
            &recipe_on_fixture("lint", source),
            "missing_docs_in_private_items",
        );
    }
}

/// The build fails an `unsafe` block.
#[test]
fn ac_0_2_5_build_fails_unsafe_code() {
    let fixture = fixture_crate(UNSAFE_BLOCK);
    let output = run(Command::new("cargo")
        .arg("build")
        .current_dir(fixture.path()));
    assert_fails_with(&output, "unsafe_code");
}

/// `format` fails a file that is not formatted.
#[test]
fn ac_0_2_6_format_fails_unformatted_file() {
    let output = recipe_on_fixture("format", UNFORMATTED);
    let diff = String::from_utf8_lossy(&output.stdout);
    assert!(
        !output.status.success() && diff.contains("Diff in"),
        "format fails with a diff, its output is: {diff}"
    );
}

/// `check` runs the lint gates before the coverage gate, and CI runs them.
#[test]
fn ac_0_2_7_check_and_ci_run_lint_gates() {
    let gates = ["workflow-lint", "format", "lint", "cov"];
    let check: Vec<String> = dependencies(&justfile(), "check")
        .into_iter()
        .filter(|recipe| gates.contains(&recipe.as_str()))
        .collect();
    assert_eq!(check, gates, "check runs the gates in this order");
    let ci = yaml(".github/workflows/ci.yml");
    for (job, command) in [("rust-format", "just format"), ("rust-lint", "just lint")] {
        assert!(runs(&ci, job).contains(&command), "{job} runs {command}");
    }
}

/// `xtask complexity` fails a function with 11 `if` branches and names it.
#[test]
fn ac_0_3_8_complexity_fails_branchy_function() {
    let fixture = fixture_file("branchy.rs", &branchy_function());
    let target = TempDir::new().expect("a temporary folder");
    let output = run(Command::new("cargo")
        .args(["run", "--quiet", "--package", "xtask", "--target-dir"])
        .arg(target.path())
        .args(["--", "complexity"])
        .arg(fixture.path().join("branchy.rs"))
        .current_dir(root()));
    let report = String::from_utf8_lossy(&output.stdout);
    assert_eq!(
        output.status.code(),
        Some(1),
        "the exit code, output: {report}"
    );
    assert!(
        report.contains("branchy"),
        "the output names the function: {report}"
    );
}

/// `rust-code-analysis-cli` gives metrics for each function of a file with let chains.
#[test]
fn ac_0_3_9_metrics_for_let_chains() {
    let fixture = fixture_file("chains.rs", LET_CHAINS);
    let path = fixture.path().join("chains.rs");
    let units = crate::complexity::units(path.to_str().expect("the path is UTF-8"));
    let [unit] = units.as_slice() else {
        panic!("one unit for one file, there are {}", units.len());
    };
    let functions: Vec<(&str, Option<f64>)> = unit
        .spaces
        .iter()
        .map(|space| (space.name.as_str(), space.metrics["cyclomatic"]["sum"]))
        .collect();
    assert_eq!(functions, [("first", Some(4.0)), ("second", Some(3.0))]);
}

/// `xtask complexity` passes a file without functions, whose unit has metrics without a value.
#[test]
fn complexity_passes_file_without_functions() {
    let fixture = fixture_file("constant.rs", "/// One.\nconst ONE: u32 = 1;\n");
    let path = fixture.path().join("constant.rs");
    let units = crate::complexity::units(path.to_str().expect("the path is UTF-8"));
    let [unit] = units.as_slice() else {
        panic!("one unit for one file, there are {}", units.len());
    };
    assert!(crate::complexity::violations(unit, &crate::complexity::LIMITS).is_empty());
}

/// `check` runs the complexity gate after the lint gate, and CI runs it.
#[test]
fn ac_0_3_10_check_and_ci_run_complexity_gate() {
    assert_check_and_ci_run_after("lint", "complexity");
}

/// `jscpd` fails two copies of a 40-line function and names both files.
#[test]
fn ac_0_4_1_duplication_fails_two_copies() {
    let fixture = fixture_file("first.rs", &distinct_function());
    fs::write(fixture.path().join("second.rs"), distinct_function()).expect("the copy is written");
    let output = duplication_gate(fixture.path());
    let report = String::from_utf8_lossy(&output.stdout);
    assert!(
        !output.status.success() && report.contains("first.rs") && report.contains("second.rs"),
        "jscpd fails and names both files, its output is: {report}"
    );
}

/// `jscpd` passes one copy of the function.
#[test]
fn ac_0_4_2_duplication_passes_one_copy() {
    let fixture = fixture_file("first.rs", &distinct_function());
    assert_passes(&duplication_gate(fixture.path()));
}

/// `check` runs the duplication gate after the complexity gate, and CI runs it.
#[test]
fn ac_0_4_3_check_and_ci_run_duplication_gate() {
    assert_check_and_ci_run_after("complexity", "duplication");
}

/// The PR template has the sections `Summary`, `Changes` and `Checks`, in that order.
#[test]
fn ac_0_5_1_template_has_sections() {
    let template = read(".github/pull_request_template.md");
    let headings: Vec<&str> = template
        .lines()
        .filter_map(|line| line.strip_prefix("## "))
        .collect();
    assert_eq!(headings, ["Summary", "Changes", "Checks"]);
}

/// The PR template has one check box for each CI job and the three other boxes.
#[test]
fn ac_0_5_2_template_has_box_for_each_job() {
    let template = read(".github/pull_request_template.md");
    let boxes: Vec<&str> = template
        .lines()
        .filter_map(|line| line.strip_prefix("- [ ] "))
        .collect();
    let labels: BTreeSet<String> = boxes.iter().map(ToString::to_string).collect();
    assert_eq!(
        boxes.len(),
        labels.len(),
        "each box is in the template once: {boxes:?}"
    );
    let ci = yaml(".github/workflows/ci.yml");
    let jobs = ci["jobs"].as_mapping().expect("the workflow has jobs");
    let mut expected: BTreeSet<String> = jobs
        .keys()
        .map(|job| format!("`{}`", job.as_str().expect("a job name")))
        .collect();
    expected.extend(OTHER_BOXES.map(str::to_owned));
    assert_eq!(labels, expected);
}

/// `setup` installs the `gh-stack` extension.
#[test]
fn ac_0_5_3_setup_installs_gh_stack() {
    let setup = justfile()["recipes"]["setup"]["body"].to_string();
    assert!(
        setup.contains("gh extension install github/gh-stack"),
        "setup installs github/gh-stack: {setup}"
    );
}

/// The shell line that reads the PR base. In a native stack, `github.base_ref` is the trunk.
const READ_BASE: &str = "base=$(gh pr view \"$PR\" --json baseRefName --jq .baseRefName)";

/// Checks that the CI job `job` runs only on PRs, with the full history, reads the PR base, and
/// that a command has `command`. Returns the job.
fn assert_pull_request_job<'workflow>(
    ci: &'workflow serde_yaml::Value,
    job: &str,
    command: &str,
) -> &'workflow serde_yaml::Value {
    let steps = &ci["jobs"][job];
    assert_eq!(
        steps["if"].as_str(),
        Some("github.event_name == 'pull_request'"),
        "{job} runs only on pull_request"
    );
    let checkout = steps["steps"]
        .as_sequence()
        .expect("the job has steps")
        .iter()
        .find(|step| {
            step["uses"]
                .as_str()
                .is_some_and(|uses| uses.starts_with("actions/checkout@"))
        })
        .expect("the job checks out");
    assert_eq!(checkout["with"]["fetch-depth"].as_u64(), Some(0));
    let commands = runs(ci, job);
    assert!(
        commands.iter().any(|run| run.contains(READ_BASE)),
        "{job} reads the PR base, it runs {commands:?}"
    );
    assert!(
        commands.iter().any(|run| run.contains(command)),
        "{job} runs {command}, it runs {commands:?}"
    );
    steps
}

/// CI runs `pr-size` only on PRs, with the full history and the PR base.
#[test]
fn ac_0_6_6_ci_runs_pr_size_on_pull_requests() {
    let ci = yaml(".github/workflows/ci.yml");
    let job = assert_pull_request_job(&ci, "pr-size", "pr-size --base \"origin/$base\"");
    assert_eq!(
        job["permissions"]["pull-requests"].as_str(),
        Some("read"),
        "pr-size can read the PR base"
    );
}

/// `check` runs the architecture gate after the duplication gate, and CI runs it.
#[test]
fn ac_0_8_8_check_and_ci_run_architecture_gate() {
    assert_check_and_ci_run_after("duplication", "architecture");
}

/// CI posts the PR diagram against the PR base in one comment with the marker.
#[test]
fn ac_0_10_7_ci_posts_diagram_on_pull_requests() {
    let ci = yaml(".github/workflows/ci.yml");
    let job = assert_pull_request_job(&ci, "diagram", "diagram --base \"origin/$base\"");
    assert_eq!(
        job["permissions"]["pull-requests"].as_str(),
        Some("write"),
        "diagram can write PR comments"
    );
    let marker = "<!-- jarvis-architecture -->";
    let step = steps(&ci)
        .into_iter()
        .find(|step| {
            step["run"]
                .as_str()
                .is_some_and(|run| run.contains("diagram --base"))
        })
        .expect("a step draws the diagram");
    let run = step["run"].as_str().unwrap_or_default();
    assert_eq!(step["env"]["MARKER"].as_str(), Some(marker));
    for command in [
        "$GITHUB_STEP_SUMMARY",
        "startswith(\\\"$MARKER\\\")",
        "--method PATCH",
    ] {
        assert!(run.contains(command), "diagram runs {command}: {run}");
    }
}

/// The script of the `architecture.yml` step that commits the diagram.
fn commit_script(workflow: &serde_yaml::Value) -> &str {
    steps(workflow)
        .into_iter()
        .filter_map(|step| step["run"].as_str())
        .find(|run| run.contains("git commit"))
        .expect("a step commits the diagram")
}

/// `architecture.yml` runs on `push` to `main` only, draws the diagram and commits only `docs/architecture.md`.
#[test]
fn ac_0_11_1_workflow_commits_diagram_on_main() {
    let workflow = yaml(".github/workflows/architecture.yml");
    let expected: serde_yaml::Value =
        serde_yaml::from_str("{push: {branches: [main]}}").expect("the expected trigger is YAML");
    assert_eq!(workflow["on"], expected, "the triggers");
    assert_eq!(workflow["permissions"]["contents"].as_str(), Some("write"));
    assert_eq!(
        workflow["concurrency"]["group"].as_str(),
        Some("architecture")
    );
    assert_eq!(
        workflow["concurrency"]["cancel-in-progress"].as_bool(),
        Some(false)
    );
    assert!(
        runs(&workflow, "diagram").contains(&"just diagram"),
        "the job runs just diagram"
    );
    let script = commit_script(&workflow);
    let commits: Vec<&str> = script
        .lines()
        .filter(|line| line.starts_with("git commit") || line.starts_with("git add"))
        .collect();
    assert!(
        matches!(commits.as_slice(), [commit] if commit.ends_with(" -- docs/architecture.md")),
        "one commit of docs/architecture.md only, the script runs {commits:?}"
    );
}

/// Runs `git` with `arguments` in `repository`, with a fixed identity and no signing.
fn git(repository: &Path, arguments: &[&str]) -> Output {
    run(Command::new("git")
        .args(["-c", "user.name=Test", "-c", "user.email=test@example.com"])
        .args(["-c", "commit.gpgsign=false"])
        .args(arguments)
        .current_dir(repository))
}

/// The commit step makes no commit on a clean working tree.
#[test]
fn ac_0_11_2_clean_tree_makes_no_commit() {
    let repository = TempDir::new().expect("a temporary folder");
    fs::create_dir(repository.path().join("docs")).expect("the docs folder is made");
    fs::write(repository.path().join("docs/architecture.md"), "graph TD\n")
        .expect("the diagram is written");
    for arguments in [
        &["init", "--quiet"][..],
        &["add", "docs/architecture.md"],
        &["commit", "--quiet", "-m", "Initial"],
    ] {
        assert_passes(&git(repository.path(), arguments));
    }
    let workflow = yaml(".github/workflows/architecture.yml");
    assert_passes(&run(Command::new("bash")
        .args(["-eo", "pipefail", "-c", commit_script(&workflow)])
        .current_dir(repository.path())));
    let count = git(repository.path(), &["rev-list", "--count", "HEAD"]);
    assert_eq!(String::from_utf8_lossy(&count.stdout).trim(), "1");
}

/// A fixture Quint model with a counter that `next` updates, the invariant `count < 3` and one `run` test.
fn counter_model(next: &str) -> String {
    format!(
        "module fixture {{
  var count: int

  action init = count' = 0

  action step = count' = {next}

  val allInvariants = count < 3

  run ac_counts = init.then(step).expect(count == 1)
}}
"
    )
}

/// Runs the `quint` recipe in a fixture folder whose `quint/` folder has `model`, if any.
fn quint_gate(model: Option<&str>) -> Output {
    let fixture = TempDir::new().expect("a temporary folder");
    let models = fixture.path().join("quint");
    fs::create_dir(&models).expect("the model folder is made");
    if let Some(model) = model {
        fs::write(models.join("fixture.qnt"), model).expect("the model is written");
    }
    let _apalache = APALACHE.lock().expect("no test panics while it runs Quint");
    recipe_in(fixture.path(), "quint")
}

/// The exit code and the result lines of a `quint` output: test results, trace states and verdicts, without timings.
fn quint_results(output: &Output) -> (Option<i32>, Vec<String>) {
    let stdout = String::from_utf8_lossy(&output.stdout);
    let lines: Vec<String> = stdout
        .lines()
        .filter(|line| line.starts_with('[') || line.contains("ac_"))
        .map(|line| {
            line.split_once(" (")
                .map_or(line, |(result, _)| result)
                .trim()
                .to_owned()
        })
        .collect();
    (output.status.code(), lines)
}

/// `quint` passes a fixture folder with no model.
#[test]
fn ac_0_12_1_quint_passes_no_model() {
    assert_passes(&quint_gate(None));
}

/// `quint` fails a model whose invariant a reachable state breaks, and prints the trace.
#[test]
fn ac_0_12_2_quint_fails_broken_invariant() {
    let output = quint_gate(Some(&counter_model("count + 1")));
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        !output.status.success() && stdout.contains("[State 3] { count: 3 }"),
        "quint fails and prints the trace to count 3, its output is: {stdout}"
    );
}

/// `quint` passes a model whose invariant holds in every reachable state.
#[test]
fn ac_0_12_3_quint_passes_true_invariant() {
    assert_passes(&quint_gate(Some(&counter_model("(count + 1) % 3"))));
}

/// Two runs of `quint` on the same model give the same exit code and results.
#[test]
fn ac_0_12_4_quint_is_deterministic() {
    for next in ["count + 1", "(count + 1) % 3"] {
        let model = counter_model(next);
        let first = quint_results(&quint_gate(Some(&model)));
        assert!(!first.1.is_empty(), "quint prints results for {next}");
        assert_eq!(
            first,
            quint_results(&quint_gate(Some(&model))),
            "two runs with {next}"
        );
    }
}

/// `check` runs the Quint gate after the architecture gate, CI runs it, and `rust-test` installs the Quint CLI.
#[test]
fn ac_0_12_5_check_and_ci_run_quint_gate() {
    assert_check_and_ci_run_after("architecture", "quint");
    assert!(
        runs(&yaml(".github/workflows/ci.yml"), "rust-test").contains(&"just install-quint"),
        "rust-test runs just install-quint"
    );
}

/// CI runs `gpu-test` on the self-hosted GPU runner.
#[test]
fn ac_0_13_1_ci_runs_gpu_test_on_gpu_runner() {
    let ci = yaml(".github/workflows/ci.yml");
    let labels: serde_yaml::Value =
        serde_yaml::from_str("[self-hosted, linux, gpu]").expect("the expected labels are YAML");
    assert_eq!(
        ci["jobs"]["gpu-test"]["runs-on"], labels,
        "the runner labels"
    );
    assert!(
        runs(&ci, "gpu-test").contains(&"just gpu-test"),
        "gpu-test runs just gpu-test"
    );
}

/// `gpu-test` runs the ignored tests on CUDA, and `check` runs it only when `JARVIS_DEVICE=cuda` is set.
#[test]
fn ac_0_13_2_check_runs_gpu_test_on_cuda() {
    let justfile = justfile();
    let gpu_test = justfile["recipes"]["gpu-test"]["body"].to_string();
    for part in ["JARVIS_DEVICE=cuda", "-- --ignored"] {
        assert!(gpu_test.contains(part), "gpu-test has {part}: {gpu_test}");
    }
    let condition = serde_json::json!([
        "if",
        ["==", ["call", "env", "JARVIS_DEVICE", ""], "cuda"],
        "just gpu-test",
        ""
    ]);
    assert_eq!(
        justfile["recipes"]["check"]["body"],
        serde_json::json!([[[condition]]]),
        "check runs gpu-test only on CUDA"
    );
}

/// `gpu-test` passes a crate with no ignored tests.
#[test]
fn ac_0_13_3_gpu_test_passes_no_ignored_tests() {
    assert_passes(&recipe_on_fixture("gpu-test", HALF_COVERED));
}

/// The `gpu-test` job stops `jarvis` before the tests and always starts it again after them.
#[test]
fn ac_0_13_4_gpu_test_job_pauses_jarvis() {
    let ci = yaml(".github/workflows/ci.yml");
    let steps = ci["jobs"]["gpu-test"]["steps"]
        .as_sequence()
        .expect("the job exists");
    let position = |command: &str| {
        steps
            .iter()
            .position(|step| step["run"].as_str() == Some(command))
    };
    let order = [
        position("sudo systemctl stop jarvis"),
        position("just gpu-test"),
        position("sudo systemctl start jarvis"),
    ];
    assert!(
        matches!(order, [Some(stop), Some(test), Some(start)]
            if stop < test && test < start && steps[start]["if"].as_str() == Some("always()")),
        "gpu-test stops jarvis, runs the tests, then always starts jarvis: {order:?}"
    );
}

/// `release.yml` runs on `v*` tags, calls CI first and builds each target on its runner.
#[test]
fn ac_0_14_1_release_builds_targets_after_ci() {
    let release = yaml(".github/workflows/release.yml");
    let expected: serde_yaml::Value =
        serde_yaml::from_str("{push: {tags: ['v*']}}").expect("the expected trigger is YAML");
    assert_eq!(release["on"], expected, "the triggers");
    let jobs = &release["jobs"];
    assert_eq!(
        jobs["ci"]["uses"].as_str(),
        Some("./.github/workflows/ci.yml")
    );
    assert_eq!(jobs["build"]["needs"].as_str(), Some("ci"));
    assert_eq!(
        jobs["build"]["runs-on"].as_str(),
        Some("${{ matrix.runner }}")
    );
    let targets: serde_yaml::Value = serde_yaml::from_str(
        "[{target: x86_64-unknown-linux-gnu, runner: ubuntu-latest}, \
         {target: x86_64-apple-darwin, runner: macos-15-intel}]",
    )
    .expect("the expected targets are YAML");
    assert_eq!(
        jobs["build"]["strategy"]["matrix"]["include"], targets,
        "the targets and their runners"
    );
}

/// Each build verifies its archive before the upload, and the release needs both builds.
#[test]
fn ac_0_14_2_release_verifies_archives() {
    let release = yaml(".github/workflows/release.yml");
    let steps = release["jobs"]["build"]["steps"]
        .as_sequence()
        .expect("the build job exists");
    let verify = steps.iter().position(|step| {
        step["run"]
            .as_str()
            .is_some_and(|run| run.contains("file jarvis") && run.contains("./jarvis"))
    });
    let upload = steps.iter().position(|step| {
        step["uses"]
            .as_str()
            .is_some_and(|uses| uses.starts_with("actions/upload-artifact@"))
    });
    assert!(
        matches!((verify, upload), (Some(verify), Some(upload)) if verify < upload),
        "the build verifies the archive before the upload: {verify:?}, {upload:?}"
    );
    assert_eq!(release["jobs"]["release"]["needs"].as_str(), Some("build"));
    assert!(
        runs(&release, "release")
            .iter()
            .any(|run| run.starts_with("gh release create")),
        "the release job runs gh release create"
    );
}

/// `merge` runs `xtask merge` with the PR number.
#[test]
fn ac_0_16_4_merge_recipe_runs_xtask_merge() {
    let merge = justfile()["recipes"]["merge"]["body"].to_string();
    assert!(
        merge.contains("cargo run -p xtask -- merge "),
        "merge runs xtask merge: {merge}"
    );
}

/// `setup` writes `.git/hooks/pre-push`, which runs `xtask guard-push`.
#[test]
fn ac_0_17_2_setup_writes_pre_push_hook() {
    let justfile = justfile();
    let setup = dependencies(&justfile, "setup");
    assert!(
        setup.contains(&"pre-push-hook".to_owned()),
        "setup runs pre-push-hook, it runs {setup:?}"
    );
    let hook = justfile["recipes"]["pre-push-hook"]["body"].to_string();
    assert!(
        hook.contains("> .git/hooks/pre-push") && hook.contains("-p xtask -- guard-push"),
        "pre-push-hook writes a hook that runs xtask guard-push: {hook}"
    );
}

/// A repository with the branches `main` and `2.3-ha-requests` and the pre-push hook of `setup`.
fn repository_with_hook() -> TempDir {
    let repository = TempDir::new().expect("a temporary folder");
    for arguments in [
        &["init", "--quiet", "--initial-branch=main"][..],
        &["commit", "--quiet", "--allow-empty", "-m", "Initial"],
        &["branch", "2.3-ha-requests"],
    ] {
        assert_passes(&git(repository.path(), arguments));
    }
    assert_passes(&recipe_in(repository.path(), "pre-push-hook"));
    repository
}

/// A push of a branch and `main` through the hook fails, and the remote gets no `main`.
#[test]
fn ac_0_17_3_hook_refuses_push_to_main() {
    let remote = TempDir::new().expect("a temporary folder");
    assert_passes(&git(remote.path(), &["init", "--quiet", "--bare"]));
    let repository = repository_with_hook();
    let url = remote.path().to_str().expect("a UTF-8 path");
    let push = git(repository.path(), &["push", url, "2.3-ha-requests", "main"]);
    let stderr = String::from_utf8_lossy(&push.stderr);
    assert!(
        !push.status.success() && stderr.contains("A push to main is not permitted."),
        "the guard refuses the push: {stderr}"
    );
    let refs = git(remote.path(), &["for-each-ref", "--format=%(refname)"]);
    let refs = String::from_utf8_lossy(&refs.stdout);
    assert!(!refs.contains("refs/heads/main"), "the remote has {refs}");
}

/// A fixture function that returns one.
const ONE: &str = "one() -> u32 { 1 }";

/// A fixture function that adds two `u32` parameters.
const SUM: &str = "sum(a: u32, b: u32) -> u32 { a + b }";

/// A fixture crate with `function` and the Verus attribute `attribute`.
fn verus_function(attribute: &str, function: &str) -> String {
    format!("use vstd::prelude::*;\n\n#[{attribute}]\npub fn {function}\n")
}

/// A fixture domain file with one function without a Verus attribute.
const UNVERIFIED_DOMAIN: &str = "use vstd::prelude::*;

pub fn unverified() {}

pub struct Unit;

#[verus_verify]
impl Unit {
    pub fn method() {}
}

verus! {
    pub open spec fn double(x: int) -> int { 2 * x }
}

#[cfg(test)]
mod tests {
    fn helper() {}
}
";

/// A fixture domain file with three trusted items.
const TRUSTED_DOMAIN: &str = "use vstd::prelude::*;

verus! {
    pub assume_specification[ core::cmp::max::<u32> ](a: u32, b: u32) -> u32;

    proof fn lemma() {
        admit();
    }
}

#[verus_verify(external_body)]
pub fn trusted() {}
";

/// The trusted code file of the domain.
const CONTRACTS: &str = "src/domain/contracts.rs";

/// A function without a Verus attribute, or a trusted item outside [`CONTRACTS`].
#[derive(Debug, PartialEq)]
struct Finding {
    /// The file of the item.
    file: PathBuf,
    /// The function name, or the token of the trusted item.
    item: String,
}

/// True when `attributes` has `#[verus_spec]` or `#[verus_verify]`.
fn verified(attributes: &[Attribute]) -> bool {
    attributes.iter().any(|attribute| {
        attribute.path().is_ident("verus_spec") || attribute.path().is_ident("verus_verify")
    })
}

/// True for `#[cfg(test)]`.
fn is_cfg_test(attribute: &Attribute) -> bool {
    attribute.path().is_ident("cfg")
        && attribute
            .parse_args::<syn::Ident>()
            .is_ok_and(|argument| argument == "test")
}

/// The names of the functions of `items`, outside test modules and `verus!` blocks, without a Verus attribute on them or on their `impl` block.
fn unverified(items: &[Item]) -> Vec<String> {
    items.iter().flat_map(unverified_item).collect()
}

/// The names of the functions of `item` without a Verus attribute, as [`unverified`] gives them.
fn unverified_item(item: &Item) -> Vec<String> {
    match item {
        Item::Fn(function) if !verified(&function.attrs) => vec![function.sig.ident.to_string()],
        Item::Impl(block) if !verified(&block.attrs) => unverified_methods(block),
        Item::Mod(ItemMod {
            attrs,
            content: Some((_, items)),
            ..
        }) if !attrs.iter().any(is_cfg_test) => unverified(items),
        _ => Vec::new(),
    }
}

/// The names of the methods of `block` without a Verus attribute.
fn unverified_methods(block: &ItemImpl) -> Vec<String> {
    let methods = block.items.iter().filter_map(|item| match item {
        ImplItem::Fn(method) if !verified(&method.attrs) => Some(&method.sig.ident),
        _ => None,
    });
    methods.map(ToString::to_string).collect()
}

/// The inside of the attribute `#[...]` or `#![...]` at `cursor`, and the cursor after it.
fn attribute(cursor: Cursor<'_>) -> Option<(Cursor<'_>, Cursor<'_>)> {
    let (_, next) = cursor.punct().filter(|(hash, _)| hash.as_char() == '#')?;
    let next = next
        .punct()
        .filter(|(bang, _)| bang.as_char() == '!')
        .map_or(next, |(_, after)| after);
    let (inside, _, _, after) = next.any_group()?;
    Some((inside, after))
}

/// Adds the trusted items of the tokens from `cursor` to `found`, also inside groups and `verus!` blocks.
fn add_trusted(mut cursor: Cursor<'_>, in_attribute: bool, found: &mut Vec<String>) {
    while let Some((_, mut next)) = cursor.token_tree() {
        if let Some((inside, after)) = attribute(cursor) {
            add_trusted(inside, true, found);
            next = after;
        } else if let Some((inside, ..)) = cursor.any_group() {
            add_trusted(inside, in_attribute, found);
        } else if let Some((ident, after)) = cursor.ident() {
            let name = ident.to_string();
            let call = after.any_group().is_some() && matches!(name.as_str(), "assume" | "admit");
            if call
                || name == "assume_specification"
                || in_attribute && name.starts_with("external")
            {
                found.push(name);
            }
        }
        cursor = next;
    }
}

/// The functions without a Verus attribute and the trusted items outside [`CONTRACTS`] of `files`.
fn verus_findings(files: &[PathBuf]) -> Vec<Finding> {
    let mut findings = Vec::new();
    for file in files {
        let source = fs::read_to_string(file).expect("the file exists");
        let mut items = unverified(&syn::parse_file(&source).expect("the file is Rust").items);
        if !file.ends_with(CONTRACTS) {
            let tokens = TokenBuffer::new2(source.parse().expect("the file is Rust tokens"));
            add_trusted(tokens.begin(), false, &mut items);
        }
        findings.extend(items.into_iter().map(|item| Finding {
            file: file.clone(),
            item,
        }));
    }
    findings
}

/// Writes `source` as `path` in a fixture folder and returns the folder and the file.
fn domain_file(path: &str, source: &str) -> (TempDir, PathBuf) {
    let folder = TempDir::new().expect("a temporary folder");
    let file = folder.path().join(path);
    fs::create_dir_all(file.parent().expect("the path has a folder")).expect("the folder is made");
    fs::write(&file, source).expect("the source is written");
    (folder, file)
}

/// Holds the gate tests that run `verus` one at a time: each cleans the fixture crate of the shared target folder first.
static VERUS: Mutex<()> = Mutex::new(());

/// Runs the `verus` recipe on a fixture crate `jarvis` with `source`. The fixtures share one target folder, so `vstd` is verified once.
fn verus_gate(source: &str) -> Output {
    let workspace: toml::Table = read("Cargo.toml").parse().expect("Cargo.toml is TOML");
    let vstd = workspace["dependencies"]["vstd"]
        .as_str()
        .expect("a vstd version");
    let target = root().join("target/verus-fixtures");
    let (fixture, _) = domain_file("src/lib.rs", source);
    let manifest = format!(
        "[package]\nname = \"jarvis\"\nversion = \"0.0.0\"\nedition = \"2024\"\n\n[dependencies]\nvstd = \"{vstd}\"\n\n[package.metadata.verus]\nverify = true\n"
    );
    fs::write(fixture.path().join("Cargo.toml"), manifest).expect("the manifest is written");
    fs::create_dir(fixture.path().join(".cargo")).expect("the config folder is made");
    fs::write(
        fixture.path().join(".cargo/config.toml"),
        format!("[build]\ntarget-dir = {:?}\n", target.display().to_string()),
    )
    .expect("the config is written");
    let _verus = VERUS.lock().expect("no test panics while it runs Verus");
    // Cargo reuses a passing build of the same source, which then prints no results.
    assert_passes(&run(Command::new("cargo")
        .args(["clean", "--quiet", "-p", "jarvis"])
        .current_dir(fixture.path())));
    recipe_in(fixture.path(), "verus")
}

/// The exit code and the result lines of a `verus` output: the errors and their places, and the counts of the fixture crate.
fn verus_results(output: &Output) -> (Option<i32>, Vec<String>) {
    let stderr = String::from_utf8_lossy(&output.stderr);
    let mut lines: Vec<String> = stderr
        .lines()
        .map(str::trim)
        .filter(|line| line.starts_with("error") || line.starts_with("-->"))
        .map(str::to_owned)
        .collect();
    // Verus prints the counts of `vstd` before the counts of the fixture crate.
    let stdout = String::from_utf8_lossy(&output.stdout);
    lines.extend(stdout.lines().last().map(str::to_owned));
    (output.status.code(), lines)
}

/// Checks that `output` is a failure whose output has `text`.
fn assert_verus_fails_with(output: &Output, text: &str) {
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !output.status.success() && stderr.contains(text),
        "verus fails with {text}, its output is: {stderr}"
    );
}

/// `cargo verus verify` fails an `ensures` clause that does not hold and names the function.
#[test]
fn ac_0_18_1_verus_fails_broken_ensures() {
    let broken = verus_function("verus_spec(result => ensures result == 2)", ONE);
    assert_verus_fails_with(&verus_gate(&broken), "fn one");
    let holds = verus_function("verus_spec(result => ensures result == 1)", ONE);
    assert_passes(&verus_gate(&holds));
}

/// `cargo verus verify` fails a sum that can overflow and passes it with a `requires` clause.
#[test]
fn ac_0_18_2_verus_fails_overflow() {
    assert_verus_fails_with(&verus_gate(&verus_function("verus_verify", SUM)), "fn sum");
    let bounded = verus_function("verus_spec(requires a < 1000, b < 1000)", SUM);
    assert_passes(&verus_gate(&bounded));
}

/// Two runs of `verus` on the same code give the same exit code and results.
#[test]
fn ac_0_18_3_verus_is_deterministic() {
    for ensured in [2, 1] {
        let attribute = format!("verus_spec(result => ensures result == {ensured})");
        let source = verus_function(&attribute, ONE);
        let first = verus_results(&verus_gate(&source));
        assert!(!first.1.is_empty(), "verus prints results for {source}");
        assert_eq!(
            first,
            verus_results(&verus_gate(&source)),
            "two runs of {source}"
        );
    }
}

/// `verus_findings` gives only the function without a Verus attribute.
#[test]
fn ac_0_18_4_findings_unverified_function() {
    let (_folder, file) = domain_file("src/domain/fixture.rs", UNVERIFIED_DOMAIN);
    let expected = Finding {
        file: file.clone(),
        item: "unverified".to_owned(),
    };
    assert_eq!(verus_findings(&[file]), [expected]);
}

/// `verus_findings` gives each trusted item outside `src/domain/contracts.rs` and none inside it.
#[test]
fn ac_0_18_5_findings_trusted_items() {
    let (_folder, file) = domain_file("src/domain/fixture.rs", TRUSTED_DOMAIN);
    let expected: Vec<Finding> = ["assume_specification", "admit", "external_body"]
        .map(|item| Finding {
            file: file.clone(),
            item: item.to_owned(),
        })
        .into();
    assert_eq!(verus_findings(&[file]), expected);
    let (_folder, contracts) = domain_file(CONTRACTS, TRUSTED_DOMAIN);
    assert_eq!(verus_findings(&[contracts]), []);
}

/// The domain has no function without a Verus attribute and no trusted item outside `src/domain/contracts.rs`.
#[test]
fn ac_0_18_6_domain_has_no_findings() {
    let entries = fs::read_dir(root().join("src/domain")).expect("the domain folder exists");
    let files: Vec<PathBuf> = entries
        .map(|entry| entry.expect("a folder entry").path())
        .filter(|path| path.extension().is_some_and(|extension| extension == "rs"))
        .collect();
    assert!(!files.is_empty(), "the domain has files");
    assert_eq!(verus_findings(&files), []);
}

/// The toolchain of the installed Verus release is the channel of `rust-toolchain.toml`.
#[test]
fn ac_0_18_7_verus_toolchain_is_channel() {
    let version = justfile()["assignments"]["verus_version"]["value"]
        .as_str()
        .expect("a Verus version")
        .to_owned();
    let home = env::var("HOME").expect("HOME is set");
    let verus = Path::new(&home).join(".verus").join(version).join("verus");
    let output = run(Command::new(verus).arg("--version"));
    let stdout = String::from_utf8_lossy(&output.stdout);
    let toolchain = stdout
        .lines()
        .find_map(|line| line.trim().strip_prefix("Toolchain: "))
        .and_then(|toolchain| toolchain.split_once('-'))
        .map(|(version, _)| version);
    let channels: toml::Table = read("rust-toolchain.toml")
        .parse()
        .expect("the file is TOML");
    assert_eq!(
        toolchain,
        channels["toolchain"]["channel"].as_str(),
        "{stdout}"
    );
}

/// `check` runs `verus` after `quint`, `verus` runs `cargo verus verify -p jarvis`, CI installs Verus, and the PR template has its box.
#[test]
fn ac_0_18_8_check_and_ci_run_verus_gate() {
    assert_check_and_ci_run_after("quint", "verus");
    let verus = justfile()["recipes"]["verus"]["body"].to_string();
    assert!(
        verus.contains("cargo verus verify -p jarvis"),
        "verus runs cargo verus verify -p jarvis: {verus}"
    );
    let ci = yaml(".github/workflows/ci.yml");
    for job in ["verus", "rust-test"] {
        assert!(
            runs(&ci, job).contains(&"just install-verus"),
            "{job} runs just install-verus"
        );
    }
    let template = read(".github/pull_request_template.md");
    assert!(
        template.lines().any(|line| line == "- [ ] `verus`"),
        "the PR template has a box for verus"
    );
}
