//! Developer tools of Jarvis.

use std::env;
use std::process::ExitCode;

mod complexity;
#[cfg(test)]
mod gates;
mod pr_size;
// `rules` (sub-phase 0.8) uses the graph outside the tests.
#[cfg(test)]
mod graph;

/// Entry point of the `xtask` command line.
fn main() -> ExitCode {
    let arguments: Vec<String> = env::args().skip(1).collect();
    match arguments.as_slice() {
        [command, paths @ ..] if command == "complexity" => complexity::run(paths),
        [command, base_flag, base, limit_flag, limit]
            if command == "pr-size" && base_flag == "--base" && limit_flag == "--limit" =>
        {
            pr_size::run(base, limit)
        }
        _ => {
            eprintln!("usage: xtask complexity <paths> | xtask pr-size --base <ref> --limit <n>");
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
