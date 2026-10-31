//! The `jarvis` binary.

use std::process::ExitCode;

/// Entry point of the binary.
fn main() -> ExitCode {
    jarvis::main()
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
