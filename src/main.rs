//! The `jarvis` binary.

/// Entry point of the binary.
fn main() {
    jarvis::main();
}

/// Tests of the entry point.
#[cfg(test)]
mod tests {
    /// AC-0.1-2.
    #[test]
    fn ac_0_1_2_main_returns() {
        super::main();
    }
}
