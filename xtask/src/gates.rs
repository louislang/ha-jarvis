//! Gate tests (real tools on fixtures) and structure tests (the files that configure the checks).

use std::collections::BTreeSet;
use std::env;
use std::ffi::OsString;
use std::fs;
use std::path::Path;
use std::process::{Command, Output};

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

/// Runs the `justfile` recipe `recipe` on a fixture crate with `source`.
fn recipe_on_fixture(recipe: &str, source: &str) -> Output {
    let fixture = fixture_crate(source);
    run(Command::new("just")
        .arg("--justfile")
        .arg(root().join("justfile"))
        .arg("--working-directory")
        .arg(fixture.path())
        .arg(recipe))
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

/// CI has the triggers and runs the checks of this sub-phase.
#[test]
fn ac_0_1_6_ci_runs_checks() {
    let ci = yaml(".github/workflows/ci.yml");
    for trigger in ["push", "pull_request", "workflow_call"] {
        assert!(
            ci["on"].get(trigger).is_some(),
            "ci.yml has the trigger {trigger}"
        );
    }
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
    let functions: Vec<(&str, f64)> = unit
        .spaces
        .iter()
        .map(|space| (space.name.as_str(), space.metrics["cyclomatic"]["sum"]))
        .collect();
    assert_eq!(functions, [("first", 4.0), ("second", 3.0)]);
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

/// Checks that the CI job `job` runs only on PRs, with the full history, and that a command has
/// `command`. Returns the job.
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
        commands.iter().any(|run| run.contains(command)),
        "{job} runs {command}, it runs {commands:?}"
    );
    steps
}

/// CI runs `pr-size` only on PRs, with the full history and the PR base.
#[test]
fn ac_0_6_6_ci_runs_pr_size_on_pull_requests() {
    assert_pull_request_job(
        &yaml(".github/workflows/ci.yml"),
        "pr-size",
        "pr-size --base origin/${{ github.base_ref }}",
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
    let job = assert_pull_request_job(
        &ci,
        "diagram",
        "diagram --base origin/${{ github.base_ref }}",
    );
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
