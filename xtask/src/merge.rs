//! The merge rules: whether a PR can merge.

use std::collections::BTreeMap;

/// The login of the Codex GitHub app (field log of sub-phase 0.5).
const CODEX_LOGIN: &str = "chatgpt-codex-connector";

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
        review_reasons(pr),
        branch_reasons(pr),
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
        .filter(|name| {
            pr.checks
                .get(**name)
                .is_none_or(|conclusion| conclusion != "SUCCESS")
        })
        .map(|name| Reason::Check((*name).to_owned()))
        .collect()
}

/// The reasons about the review threads and the Codex review.
fn review_reasons(pr: &PullRequest) -> Vec<Reason> {
    let codex = pr.logins.iter().any(|login| is_codex(login));
    [
        (pr.unresolved_threads > 0).then_some(Reason::UnresolvedThreads(pr.unresolved_threads)),
        (!codex).then_some(Reason::NoCodexReview),
    ]
    .into_iter()
    .flatten()
    .collect()
}

/// The reasons about the base, the title, the branch and the files of a spec PR.
fn branch_reasons(pr: &PullRequest) -> Vec<Reason> {
    let spec = pr.title.starts_with("Spec: ");
    let spec_files = pr
        .files
        .iter()
        .filter(|file| spec && !file.starts_with("docs/spec/") && *file != "CLAUDE.md")
        .map(|file| Reason::SpecFile(file.clone()));
    let single = [
        (pr.base != "main").then(|| Reason::Base(pr.base.clone())),
        (!follows_kind(&pr.title, &pr.branch)).then_some(Reason::Kind),
    ];
    single.into_iter().flatten().chain(spec_files).collect()
}

/// The reasons about the check boxes, the summary and the attribution lines of `body`.
fn description_reasons(body: &str) -> Vec<Reason> {
    let unchecked = body
        .lines()
        .filter_map(|line| line.trim_start().strip_prefix("- [ ] "))
        .map(|label| Reason::Unchecked(label.to_owned()));
    let count = sentences(body);
    let single = [
        (!(2..=3).contains(&count)).then_some(Reason::Sentences(count)),
        has_attribution(body).then_some(Reason::Attribution),
    ];
    unchecked.chain(single.into_iter().flatten()).collect()
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

/// The number of words that end a sentence in the `Summary` section of `body`.
fn sentences(body: &str) -> usize {
    body.split("## ")
        .find_map(|section| section.strip_prefix("Summary"))
        .unwrap_or_default()
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

/// Tests of the merge rules.
#[cfg(test)]
mod tests {
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
            checks: REQUIRED
                .iter()
                .map(|name| ((*name).to_owned(), "SUCCESS".to_owned()))
                .collect(),
            unresolved_threads: 0,
            logins: vec![
                "github-actions".to_owned(),
                "chatgpt-codex-connector".to_owned(),
            ],
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
        pr.checks
            .insert("rust-lint".to_owned(), "FAILURE".to_owned());
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

    /// An unchecked box is a reason that names its label.
    #[test]
    fn ac_0_15_5_unchecked_box() {
        let pr = PullRequest {
            body: BODY.replace("- [x] `rust-lint`", "- [ ] `rust-lint`"),
            ..allowed()
        };
        assert_eq!(
            merge_allowed(&pr, &REQUIRED),
            Err(vec![Reason::Unchecked("`rust-lint`".to_owned())])
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

    /// One sentence and four sentences are reasons.
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
}
