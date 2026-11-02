//! Audio type.

/// 16-bit mono PCM audio.
#[derive(Clone, Debug, PartialEq)]
pub struct Audio {
    /// The samples in time order.
    pub samples: Vec<i16>,
    /// The sample rate in Hz.
    pub rate: u32,
}
