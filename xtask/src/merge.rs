//! The merge rules, the merge command and the pre-push guard.

use std::collections::BTreeMap;
use std::ffi::OsStr;
use std::process::{Command, ExitCode};

use serde_json::Value;

/// The login of the Codex GitHub app (field log of sub-phase 0.5).
const CODEX_LOGIN: &str = "chatgpt-codex-connector";

/// The CI workflow. Each of its jobs runs on `pull_request`, so each is a required check.
const CI: &str = include_str!("../../.github/workflows/ci.yml");

/// The fields that `gh pr view` reads.
const VIEW_FIELDS: &str = "title,headRefName,baseRefName,files,body,statusCheckRollup,headRefOid";

/// The connections of a PR with the logins of the reviews, comments and reactions, each read as `author { login }`.
const LOGIN_CONNECTIONS: [(&str, &str); 3] = [
    ("reviews", "author { login }"),
    ("comments", "author { login }"),
    ("reactions", "author: user { login }"),
];

/// The PR data that the merge rules read.
#[derive(Clone, Debug, Default)]
pub struct PullRequest {
    /// The PR title.
    pub title: String,
    /// The head branch.
    pub branch: String,
    /// The base branch.
    pub base: String,
    /// The paths of the changed files.
    pub files: Vec<String>,
    /// The PR description.
    pub body: String,
    /// The conclusion of each check, by check name.
    pub checks: BTreeMap<String, String>,
    /// The number of review threads that are not resolved.
    pub unresolved_threads: usize,
    /// The logins of the reviews, comments and reactions.
    pub logins: Vec<String>,
}

/// A failed merge condition.
#[must_use]
#[derive(Clone, Debug, PartialEq)]
pub enum Reason {
    /// A required check that has no `SUCCESS` conclusion.
    Check(String),
    /// The number of review threads that are not resolved.
    UnresolvedThreads(usize),
    /// No review, comment or reaction by `CODEX_LOGIN`.
    NoCodexReview,
    /// The label of a check box that is not checked.
    Unchecked(String),
    /// A base branch other than `main`.
    Base(String),
    /// The title and the branch follow no kind of PR.
    Kind,
    /// A file outside `docs/spec/` and `CLAUDE.md` in a spec PR.
    SpecFile(String),
    /// The number of sentences in the `Summary` section, when it is not two or three.
    Sentences(usize),
    /// A `Co-Authored-By` or `Generated with` line.
    Attribution,
}

/// `Ok` when `pr` meets every merge rule, else each failed condition.
///
/// # Errors
///
/// When a condition fails. The list has one reason for each failure.
pub fn merge_allowed(pr: &PullRequest, required: &[&str]) -> Result<(), Vec<Reason>> {
    let reasons: Vec<Reason> = [
        failed_checks(pr, required),
        pr_reasons(pr),
        description_reasons(&pr.body),
    ]
    .concat();
    if reasons.is_empty() {
        Ok(())
    } else {
        Err(reasons)
    }
}

/// A reason for each required check without a `SUCCESS` conclusion.
fn failed_checks(pr: &PullRequest, required: &[&str]) -> Vec<Reason> {
    required
        .iter()
        .filter(|name| pr.checks.get(**name).map(String::as_str) != Some("SUCCESS"))
        .map(|name| Reason::Check((*name).to_owned()))
        .collect()
}

/// The reasons about the review, the base, the title, the branch and the files of a spec PR.
fn pr_reasons(pr: &PullRequest) -> Vec<Reason> {
    let spec = pr.title.starts_with("Spec: ");
    let spec_files = pr
        .files
        .iter()
        .filter(|file| spec && !file.starts_with("docs/spec/") && *file != "CLAUDE.md")
        .map(|file| Reason::SpecFile(file.clone()));
    let threads = pr.unresolved_threads;
    let single = [
        (threads > 0).then_some(Reason::UnresolvedThreads(threads)),
        (!pr.logins.iter().any(|login| is_codex(login))).then_some(Reason::NoCodexReview),
        (pr.base != "main").then(|| Reason::Base(pr.base.clone())),
        (!follows_kind(&pr.title, &pr.branch)).then_some(Reason::Kind),
    ];
    single.into_iter().flatten().chain(spec_files).collect()
}

/// The reasons about the check boxes, the summary and the attribution lines of `body`.
fn description_reasons(body: &str) -> Vec<Reason> {
    let body = &without_comments(body);
    let unchecked = body
        .lines()
        .filter_map(|line| {
            line.trim_start()
                .strip_prefix(['-', '*', '+'])?
                .strip_prefix(" [ ] ")
        })
        .map(|label| Reason::Unchecked(label.to_owned()));
    let count = sentences(body);
    let single = [
        (!(2..=3).contains(&count)).then_some(Reason::Sentences(count)),
        has_attribution(body).then_some(Reason::Attribution),
    ];
    unchecked.chain(single.into_iter().flatten()).collect()
}

/// `body` without its HTML comments, which GitHub does not render.
fn without_comments(body: &str) -> String {
    match body.split_once("<!--") {
        Some((before, rest)) => {
            let after = rest.split_once("-->").map_or("", |(_, after)| after);
            format!("{before}{}", without_comments(after))
        }
        None => body.to_owned(),
    }
}

/// True for `CODEX_LOGIN`. GraphQL gives the login of a bot reaction the `[bot]` suffix.
fn is_codex(login: &str) -> bool {
    login.strip_suffix("[bot]").unwrap_or(login) == CODEX_LOGIN
}

/// True when the title and the branch follow one kind of PR (`02-rules.md` section 7.1).
fn follows_kind(title: &str, branch: &str) -> bool {
    let prefix = match title.split_once(' ').filter(|(_, rest)| !rest.is_empty()) {
        Some(("Spec:", _)) => "spec-".to_owned(),
        Some(("Fix:", _)) => "fix-".to_owned(),
        Some((number, _)) if is_sub_phase(number) => format!("{number}-"),
        _ => return false,
    };
    branch
        .strip_prefix(&prefix)
        .is_some_and(|short_title| !short_title.is_empty())
}

/// True for `<phase>.<n>`, where `<n>` is a number with an optional letter.
fn is_sub_phase(number: &str) -> bool {
    let Some((phase, part)) = number.split_once('.') else {
        return false;
    };
    let digits = part
        .strip_suffix(|letter: char| letter.is_ascii_lowercase())
        .unwrap_or(part);
    is_number(phase) && is_number(digits)
}

/// True for one or more ASCII digits.
fn is_number(text: &str) -> bool {
    !text.is_empty() && text.bytes().all(|byte| byte.is_ascii_digit())
}

/// The text of the `Summary` section of `body`.
fn summary(body: &str) -> &str {
    body.split("## ")
        .find_map(|section| section.strip_prefix("Summary"))
        .unwrap_or_default()
        .trim()
}

/// The number of words that end a sentence in the `Summary` section of `body`.
fn sentences(body: &str) -> usize {
    summary(body)
        .split_whitespace()
        .filter(|word| word.ends_with(['.', '!', '?']))
        .count()
}

/// True when a line of `body` has a `Co-Authored-By` or `Generated with` attribution.
fn has_attribution(body: &str) -> bool {
    body.lines().any(|line| {
        let line = line.to_ascii_lowercase();
        line.contains("co-authored-by") || line.contains("generated with")
    })
}

/// Merges PR `number` when it meets the merge rules, else prints each reason. `path` is the `PATH` that finds `gh`.
#[must_use]
pub fn run(number: &str, path: &OsStr) -> ExitCode {
    match merge(number, path) {
        Ok(()) => ExitCode::SUCCESS,
        Err(errors) => {
            for error in errors {
                eprintln!("{error}");
            }
            ExitCode::FAILURE
        }
    }
}

/// Exits with code 1 when a line of the pre-push input `input` has the remote ref `refs/heads/main`, also for a deletion.
#[must_use]
pub fn guard_push(input: &str) -> ExitCode {
    if input
        .lines()
        .any(|line| line.split(' ').nth(2) == Some("refs/heads/main"))
    {
        eprintln!("A push to main is not permitted. Merge with `just merge`.");
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    }
}

/// Reads PR `number`, applies the merge rules and squash merges at the head commit that was read.
fn merge(number: &str, path: &OsStr) -> Result<(), Vec<String>> {
    let (pr, head) = read(number, path).map_err(|error| vec![error])?;
    if let Err(reasons) = merge_allowed(&pr, &jobs(CI)) {
        return Err(reasons.iter().map(|reason| format!("{reason:?}")).collect());
    }
    let body = without_comments(&pr.body);
    let arguments = [
        "pr",
        "merge",
        number,
        "--squash",
        "--match-head-commit",
        &head,
        "--subject",
        &pr.title,
        "--body",
        summary(&body),
    ];
    gh(path, &arguments).map_err(|error| vec![error])?;
    Ok(())
}

/// The PR data and the head commit of PR `number`.
fn read(number: &str, path: &OsStr) -> Result<(PullRequest, String), String> {
    let view = json(&gh(path, &["pr", "view", number, "--json", VIEW_FIELDS])?)?;
    let threads = nodes(path, number, "reviewThreads", "isResolved")?;
    let pr = PullRequest {
        title: text(&view, "/title"),
        branch: text(&view, "/headRefName"),
        base: text(&view, "/baseRefName"),
        files: list(&view, "/files")
            .iter()
            .map(|file| text(file, "/path"))
            .collect(),
        body: text(&view, "/body"),
        checks: conclusions(list(&view, "/statusCheckRollup")),
        unresolved_threads: threads
            .iter()
            .filter(|thread| thread["isResolved"] != true)
            .count(),
        logins: logins(path, number)?,
    };
    Ok((pr, text(&view, "/headRefOid")))
}

/// The logins of the reviews, comments and reactions of PR `number`.
fn logins(path: &OsStr, number: &str) -> Result<Vec<String>, String> {
    let mut logins = Vec::new();
    for (connection, fields) in LOGIN_CONNECTIONS {
        let nodes = nodes(path, number, connection, fields)?;
        logins.extend(nodes.iter().map(|node| text(node, "/author/login")));
    }
    Ok(logins)
}

/// The nodes of `connection` of PR `number` over all pages, with `fields` for each node.
fn nodes(path: &OsStr, number: &str, connection: &str, fields: &str) -> Result<Vec<Value>, String> {
    let query = format!(
        "query($owner: String!, $repo: String!, $number: Int!, $endCursor: String) {{ repository(owner: $owner, name: $repo) {{ pullRequest(number: $number) {{ connection: {connection}(first: 100, after: $endCursor) {{ nodes {{ {fields} }} pageInfo {{ hasNextPage endCursor }} }} }} }} }}"
    );
    let number = format!("number={number}");
    let query = format!("query={query}");
    let arguments = [
        "api",
        "graphql",
        "--paginate",
        "--slurp",
        "-F",
        "owner={owner}",
        "-F",
        "repo={repo}",
        "-F",
        &number,
        "-f",
        &query,
    ];
    let pages = json(&gh(path, &arguments)?)?;
    let nodes = list(&pages, "")
        .iter()
        .flat_map(|page| list(page, "/data/repository/pullRequest/connection/nodes"));
    Ok(nodes.cloned().collect())
}

/// The conclusion of each check over its runs. A skipped run does not count. A run without success outranks a success.
fn conclusions(runs: &[Value]) -> BTreeMap<String, String> {
    let mut checks = BTreeMap::new();
    for run in runs {
        let conclusion = text(run, "/conclusion");
        if conclusion == "SKIPPED" {
            continue;
        }
        let current = checks
            .entry(text(run, "/name"))
            .or_insert_with(|| conclusion.clone());
        if *current == "SUCCESS" {
            *current = conclusion;
        }
    }
    checks
}

/// The standard output of `gh` with `arguments`, else its standard error. `path` is the `PATH` that finds `gh`.
fn gh(path: &OsStr, arguments: &[&str]) -> Result<String, String> {
    let output = Command::new("gh")
        .env("PATH", path)
        .args(arguments)
        .output()
        .map_err(|error| format!("gh does not start: {error}"))?;
    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    } else {
        Err(String::from_utf8_lossy(&output.stderr).trim().to_owned())
    }
}

/// `text` parsed as JSON.
fn json(text: &str) -> Result<Value, String> {
    serde_json::from_str(text).map_err(|error| error.to_string())
}

/// The string at `pointer` in `value`, or an empty string.
fn text(value: &Value, pointer: &str) -> String {
    value
        .pointer(pointer)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned()
}

/// The array at `pointer` in `value`, or an empty slice.
fn list<'value>(value: &'value Value, pointer: &str) -> &'value [Value] {
    value
        .pointer(pointer)
        .and_then(Value::as_array)
        .map_or(&[], Vec::as_slice)
}

/// The job names of `workflow`: the keys with two spaces of indent after `jobs:`.
fn jobs(workflow: &str) -> Vec<&str> {
    let (_, jobs) = workflow.split_once("\njobs:\n").unwrap_or_default();
    jobs.lines()
        .filter_map(|line| line.strip_prefix("  ")?.strip_suffix(':'))
        .filter(|name| !name.starts_with(' '))
        .collect()
}

/// Tests of the merge rules, the merge command and the pre-push guard.
#[cfg(test)]
mod tests {
    use std::fs;
    use std::os::unix::fs::PermissionsExt;

    use serde_json::Value;
    use serde_json::json;
    use tempfile::TempDir;

    use super::*;

    /// The required checks of the fixtures.
    const REQUIRED: [&str; 2] = ["rust-lint", "rust-test"];

    /// The description of a PR that meets every rule.
    const BODY: &str = "## Summary\n\nAdds the merge rules. They decide whether a PR can merge.\n\n## Changes\n\n- `merge.rs`.\n\n## Checks\n\n- [x] `rust-lint`\n- [x] Started in a fresh context\n";

    /// A PR that meets every rule.
    fn allowed() -> PullRequest {
        PullRequest {
            title: "0.15 Merge Rules".to_owned(),
            branch: "0.15-merge-rules".to_owned(),
            base: "main".to_owned(),
            files: vec!["docs/spec/phase-00.md".to_owned()],
            body: BODY.to_owned(),
            checks: BTreeMap::from(REQUIRED.map(|name| (name.to_owned(), "SUCCESS".to_owned()))),
            unresolved_threads: 0,
            logins: vec!["chatgpt-codex-connector".to_owned()],
        }
    }

    /// `allowed()` with `title` and `branch`.
    fn titled(title: &str, branch: &str) -> PullRequest {
        PullRequest {
            title: title.to_owned(),
            branch: branch.to_owned(),
            ..allowed()
        }
    }

    /// `allowed()` with the summary `summary`.
    fn summarized(summary: &str) -> PullRequest {
        PullRequest {
            body: format!("## Summary\n\n{summary}\n\n## Checks\n\n- [x] `rust-lint`\n"),
            ..allowed()
        }
    }

    /// A failed check is a reason that names it.
    #[test]
    fn ac_0_15_1_failed_check() {
        let mut pr = allowed();
        *pr.checks.get_mut("rust-lint").expect("a fixture check") = "FAILURE".to_owned();
        assert_eq!(
            merge_allowed(&pr, &REQUIRED),
            Err(vec![Reason::Check("rust-lint".to_owned())])
        );
    }

    /// A missing check is a reason that names it.
    #[test]
    fn ac_0_15_2_missing_check() {
        let mut pr = allowed();
        pr.checks.remove("rust-test");
        assert_eq!(
            merge_allowed(&pr, &REQUIRED),
            Err(vec![Reason::Check("rust-test".to_owned())])
        );
    }

    /// One unresolved thread is a reason.
    #[test]
    fn ac_0_15_3_unresolved_thread() {
        let pr = PullRequest {
            unresolved_threads: 1,
            ..allowed()
        };
        assert_eq!(
            merge_allowed(&pr, &REQUIRED),
            Err(vec![Reason::UnresolvedThreads(1)])
        );
    }

    /// No Codex login is a reason. One reaction login, with the `[bot]` suffix of GraphQL reactions, is enough.
    #[test]
    fn ac_0_15_4_codex_login() {
        let none = PullRequest {
            logins: vec!["github-actions".to_owned()],
            ..allowed()
        };
        let reaction = PullRequest {
            logins: vec!["chatgpt-codex-connector[bot]".to_owned()],
            ..allowed()
        };
        assert_eq!(
            merge_allowed(&none, &REQUIRED),
            Err(vec![Reason::NoCodexReview])
        );
        assert_eq!(merge_allowed(&reaction, &REQUIRED), Ok(()));
    }

    /// An unchecked `-` or `*` box is a reason that names its label.
    #[test]
    fn ac_0_15_5_unchecked_box() {
        let pr = PullRequest {
            body: BODY
                .replace("- [x] `rust", "* [ ] `rust")
                .replace("[x]", "[ ]"),
            ..allowed()
        };
        let labels = ["`rust-lint`", "Started in a fresh context"];
        assert_eq!(
            merge_allowed(&pr, &REQUIRED),
            Err(labels
                .map(|label| Reason::Unchecked(label.to_owned()))
                .to_vec())
        );
    }

    /// A base other than `main` is a reason.
    #[test]
    fn ac_0_15_6_base() {
        let pr = PullRequest {
            base: "0.14-release-workflow".to_owned(),
            ..allowed()
        };
        assert_eq!(
            merge_allowed(&pr, &REQUIRED),
            Err(vec![Reason::Base("0.14-release-workflow".to_owned())])
        );
    }

    /// A PR that meets every rule can merge.
    #[test]
    fn ac_0_15_7_all_conditions_hold() {
        assert_eq!(merge_allowed(&allowed(), &REQUIRED), Ok(()));
    }

    /// A title of no kind, or a branch of another kind or sub-phase, is a reason.
    #[test]
    fn ac_0_15_8_title_and_branch() {
        let pairs = [
            ("Merge Rules", "merge-rules"),
            ("0.15", "0.15-merge-rules"),
            ("0.15 Merge Rules", "0.16-merge-rules"),
            ("0.15 Merge Rules", "0.15-"),
            ("0.15bb Merge Rules", "0.15bb-merge-rules"),
            ("Spec: Title", "fix-title"),
            ("Fix: Title", "fix"),
        ];
        for (title, branch) in pairs {
            assert_eq!(
                merge_allowed(&titled(title, branch), &REQUIRED),
                Err(vec![Reason::Kind]),
                "title {title:?}, branch {branch:?}"
            );
        }
    }

    /// One sentence and four sentences are reasons. An HTML comment has no sentences.
    #[test]
    fn ac_0_15_9_summary_sentences() {
        assert_eq!(
            merge_allowed(&summarized("One."), &REQUIRED),
            Err(vec![Reason::Sentences(1)])
        );
        assert_eq!(
            merge_allowed(&summarized("One. Two `a.rs`. Three? Four!"), &REQUIRED),
            Err(vec![Reason::Sentences(4)])
        );
        assert_eq!(
            merge_allowed(&summarized("<!-- One. Two. -->One."), &REQUIRED),
            Err(vec![Reason::Sentences(1)])
        );
    }

    /// A `Co-Authored-By` line is a reason.
    #[test]
    fn ac_0_15_10_co_authored_by() {
        let pr = PullRequest {
            body: format!("{BODY}\nCo-Authored-By: Claude <noreply@anthropic.com>\n"),
            ..allowed()
        };
        assert_eq!(
            merge_allowed(&pr, &REQUIRED),
            Err(vec![Reason::Attribution])
        );
    }

    /// A split sub-phase, a spec PR and a fix PR follow their kind.
    #[test]
    fn ac_0_15_11_kinds() {
        let pairs = [
            ("4.5b Title", "4.5b-title"),
            ("Spec: Title", "spec-title"),
            ("Fix: Title", "fix-title"),
        ];
        for (title, branch) in pairs {
            assert_eq!(
                merge_allowed(&titled(title, branch), &REQUIRED),
                Ok(()),
                "title {title:?}, branch {branch:?}"
            );
        }
    }

    /// A spec PR that changes a code file is a reason that names the file.
    #[test]
    fn ac_0_15_12_spec_file() {
        let pr = PullRequest {
            files: vec![
                "docs/spec/phase-00.md".to_owned(),
                "CLAUDE.md".to_owned(),
                "xtask/src/merge.rs".to_owned(),
            ],
            ..titled("Spec: Title", "spec-title")
        };
        assert_eq!(
            merge_allowed(&pr, &REQUIRED),
            Err(vec![Reason::SpecFile("xtask/src/merge.rs".to_owned())])
        );
    }

    /// The `gh pr view` output of a PR that meets every rule, with a push run of each check and a skipped one.
    fn view() -> Value {
        let mut runs: Vec<Value> = jobs(CI)
            .iter()
            .map(|name| json!({"name": name, "conclusion": "SUCCESS"}))
            .collect();
        runs.push(json!({"name": "pr-size", "conclusion": "SKIPPED"}));
        json!({
            "title": "0.16 Merge Command",
            "headRefName": "0.16-merge-command",
            "baseRefName": "main",
            "files": [{"path": "xtask/src/merge.rs"}],
            "body": BODY,
            "headRefOid": "0123abc",
            "statusCheckRollup": runs,
        })
    }

    /// A shell command that prints `gh api graphql --paginate --slurp` output with one page for each list of nodes.
    fn print_pages(pages: &[Value]) -> String {
        let pages: Vec<Value> = pages
            .iter()
            .map(|nodes| json!({"data": {"repository": {"pullRequest": {"connection": {"nodes": nodes}}}}}))
            .collect();
        format!("printf '%s' '{}'", Value::from(pages))
    }

    /// A folder with a fake `gh` that records each call in `gh.calls`, prints `view()` and a Codex login, and runs
    /// `threads` for the review thread query.
    fn fake_gh(threads: &str) -> TempDir {
        let folder = TempDir::new().expect("a temporary folder");
        let logins = print_pages(&[json!([{"author": {"login": "chatgpt-codex-connector"}}])]);
        let script = format!(
            "#!/bin/sh\nprintf '[%s]' \"$@\" >> \"$0.calls\"\necho >> \"$0.calls\"\ncase \"$*\" in\n\"pr view\"*) printf '%s' '{}' ;;\n*reviewThreads*) {threads} ;;\n*graphql*) {logins} ;;\nesac\n",
            view()
        );
        let gh = folder.path().join("gh");
        fs::write(&gh, script).expect("the fake gh is written");
        fs::set_permissions(&gh, fs::Permissions::from_mode(0o755))
            .expect("the fake gh is executable");
        folder
    }

    /// The calls of the fake `gh` in `folder`, one line each, with each argument in brackets.
    fn calls(folder: &TempDir) -> String {
        fs::read_to_string(folder.path().join("gh.calls")).expect("gh was called")
    }

    /// The merge has the head commit, the title and the summary that were read.
    #[test]
    fn ac_0_16_1_merge_at_read_head() {
        let gh = fake_gh(&print_pages(&[json!([{"isResolved": true}])]));
        assert_eq!(merge("16", gh.path().as_os_str()), Ok(()));
        let merge_call = "[pr][merge][16][--squash][--match-head-commit][0123abc][--subject][0.16 Merge Command][--body][Adds the merge rules. They decide whether a PR can merge.]\n";
        assert!(
            calls(&gh).ends_with(merge_call),
            "the last call merges at the head: {}",
            calls(&gh)
        );
    }

    /// An unresolved thread on the second page refuses the merge.
    #[test]
    fn ac_0_16_2_threads_on_two_pages() {
        let gh = fake_gh(&print_pages(&[
            json!([{"isResolved": true}]),
            json!([{"isResolved": false}]),
        ]));
        assert_eq!(
            merge("16", gh.path().as_os_str()),
            Err(vec!["UnresolvedThreads(1)".to_owned()])
        );
        assert!(!calls(&gh).contains("[merge]"), "no merge: {}", calls(&gh));
    }

    /// A failed `gh` call exits with code 1, gives its error and runs no merge.
    #[test]
    fn ac_0_16_3_failed_gh_call() {
        let gh = fake_gh("echo 'HTTP 502: Bad Gateway' >&2; exit 1");
        assert_eq!(
            merge("16", gh.path().as_os_str()),
            Err(vec!["HTTP 502: Bad Gateway".to_owned()])
        );
        assert_eq!(run("16", gh.path().as_os_str()), ExitCode::FAILURE);
        assert!(!calls(&gh).contains("[merge]"), "no merge: {}", calls(&gh));
    }

    /// A line with the remote ref `refs/heads/main` exits with code 1, also as the second line or a deletion.
    #[test]
    fn ac_0_17_1_guard_push() {
        let branch = "refs/heads/main 1111111 refs/heads/2.3-ha-requests 2222222\n";
        let main = "refs/heads/2.3-ha-requests 1111111 refs/heads/main 2222222\n";
        let deletion = "(delete) 0000000 refs/heads/main 2222222\n";
        assert_eq!(guard_push(&format!("{branch}{main}")), ExitCode::FAILURE);
        assert_eq!(guard_push(deletion), ExitCode::FAILURE);
        assert_eq!(guard_push(&branch.repeat(2)), ExitCode::SUCCESS);
    }
}
