//! Developer tools of Jarvis.

#[cfg(test)]
mod gates;

/// Entry point of the `xtask` command line.
fn main() {}

/// Tests of the entry point.
#[cfg(test)]
mod tests {
    /// AC-0.1-2.
    #[test]
    fn ac_0_1_2_main_returns() {
        super::main();
    }
}
