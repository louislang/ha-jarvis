//! Developer tools of Jarvis.

use std::env;
use std::process::ExitCode;

mod complexity;
#[cfg(test)]
mod gates;
mod graph;
mod mermaid;
mod pr_size;
mod rules;

/// Entry point of the `xtask` command line.
fn main() -> ExitCode {
    let arguments: Vec<String> = env::args().skip(1).collect();
    let words: Vec<&str> = arguments.iter().map(String::as_str).collect();
    match words.as_slice() {
        ["complexity", ..] => complexity::run(arguments.get(1..).unwrap_or_default()),
        ["pr-size", "--base", base, "--limit", limit] => pr_size::run(base, limit),
        ["rules"] => rules::run(),
        ["diagram", "--base", reference] => mermaid::run_base(reference),
        ["diagram", file] => mermaid::run(file),
        _ => {
            eprintln!(
                "usage: xtask complexity <paths> | xtask pr-size --base <ref> --limit <n> | xtask rules | xtask diagram <file> | xtask diagram --base <ref>"
            );
            ExitCode::FAILURE
        }
    }
}

/// Tests of the entry point.
#[cfg(test)]
mod tests {
    /// AC-0.1-2.
    #[test]
    fn ac_0_1_2_main_returns() {
        let _ = super::main();
    }
}
