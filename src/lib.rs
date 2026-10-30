//! Voice-controlled home automation for Home Assistant.

/// Pure rules.
pub mod domain {
    pub mod contracts;
}

/// Entry point of Jarvis.
pub fn main() {}

/// Tests of the entry point.
#[cfg(test)]
mod tests {
    /// AC-0.1-2.
    #[test]
    fn ac_0_1_2_main_returns() {
        super::main();
    }
}
