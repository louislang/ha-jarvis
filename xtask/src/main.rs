//! Developer tools of Jarvis.

use std::env;
use std::process::ExitCode;

mod complexity;
#[cfg(test)]
mod gates;

/// Entry point of the `xtask` command line.
fn main() -> ExitCode {
    let arguments: Vec<String> = env::args().skip(1).collect();
    match arguments.split_first() {
        Some((command, paths)) if command == "complexity" => complexity::run(paths),
        _ => {
            eprintln!("usage: xtask complexity <paths>");
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
