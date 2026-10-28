//! Counts the changed lines of a PR without the excluded files.

use std::process::{Command, ExitCode};

/// The files whose changed lines do not count (`02-rules.md` section 7.1, rule 8).
pub const EXCLUDED: [&str; 3] = [
    "Cargo.lock",
    "docs/architecture.md",
    "src/testing/house.json",
];

/// The sum of `added + deleted` over the `git diff --numstat` lines that are not binary or in `excluded`.
///
/// # Errors
///
/// When a line is not `<added>\t<deleted>\t<path>`. The error contains the line.
pub fn counted_lines(numstat: &str, excluded: &[&str]) -> Result<usize, String> {
    numstat
        .lines()
        .try_fold(0, |total, line| Ok(total + line_count(line, excluded)?))
}

/// The count of one numstat line: 0 for a binary or excluded file, else `added + deleted`.
fn line_count(line: &str, excluded: &[&str]) -> Result<usize, String> {
    let fields: Vec<&str> = line.split('\t').collect();
    let count = match fields.as_slice() {
        [_, _, path] if excluded.contains(path) => Some(0),
        ["-", "-", _] => Some(0),
        [added, deleted, _] => added
            .parse::<usize>()
            .ok()
            .zip(deleted.parse::<usize>().ok())
            .map(|(added, deleted)| added + deleted),
        _ => None,
    };
    count.ok_or_else(|| format!("not a numstat line: {line}"))
}

/// Fails with `<count> > <limit>` when `count` is more than `limit`.
///
/// # Errors
///
/// When `count` is more than `limit`.
pub fn check(count: usize, limit: usize) -> Result<(), String> {
    if count > limit {
        Err(format!("{count} > {limit}"))
    } else {
        Ok(())
    }
}

/// Checks the counted lines of `HEAD` against `base`. Fails above `limit`.
///
/// # Panics
///
/// When `git` does not start or fails, or when `limit` is not a number.
#[must_use]
pub fn run(base: &str, limit: &str) -> ExitCode {
    let limit: usize = limit.parse().expect("the limit is a number");
    let output = Command::new("git")
        .args(["diff", "--numstat", &format!("{base}...HEAD")])
        .output()
        .expect("git starts");
    assert!(
        output.status.success(),
        "git diff fails: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let numstat = String::from_utf8_lossy(&output.stdout);
    match counted_lines(&numstat, &EXCLUDED).and_then(|count| check(count, limit)) {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => {
            eprintln!("{message}");
            ExitCode::FAILURE
        }
    }
}

/// Tests of the line count.
#[cfg(test)]
mod tests {
    use super::*;

    /// Lines of two text files count `added + deleted`.
    #[test]
    fn ac_0_6_1_counts_added_and_deleted() {
        assert_eq!(
            counted_lines("300\t0\ta.rs\n40\t10\tb.rs\n", &EXCLUDED),
            Ok(350)
        );
    }

    /// A count at the limit passes. One more fails with `<count> > <limit>`.
    #[test]
    fn ac_0_6_2_limit() {
        assert_eq!(check(400, 400), Ok(()));
        assert_eq!(check(401, 400), Err("401 > 400".to_owned()));
    }

    /// The lines of `Cargo.lock` do not count.
    #[test]
    fn ac_0_6_3_excluded_file() {
        assert_eq!(
            counted_lines("2500\t2500\tCargo.lock\n1\t2\ta.rs\n", &EXCLUDED),
            Ok(3)
        );
    }

    /// A binary file counts 0.
    #[test]
    fn ac_0_6_4_binary_file() {
        assert_eq!(counted_lines("-\t-\ta.png\n1\t2\ta.rs\n", &EXCLUDED), Ok(3));
    }

    /// A line with two fields is an error that contains the line.
    #[test]
    fn ac_0_6_5_line_with_two_fields() {
        let result = counted_lines("1\t2\ta.rs\n3\tb.rs\n", &EXCLUDED);
        assert!(
            result
                .as_ref()
                .is_err_and(|error| error.contains("3\tb.rs")),
            "an error with the line, the result is {result:?}"
        );
    }
}
