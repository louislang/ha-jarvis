//! `TextToSpeech` with sherpa-onnx.

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

use anyhow::Context;
use async_trait::async_trait;
use serde_json::json;
use sherpa_onnx::GenerationConfig;
use sherpa_onnx::OfflineTts;
use sherpa_onnx::OfflineTtsConfig;
use sherpa_onnx::OfflineTtsModelConfig;
use sherpa_onnx::OfflineTtsSupertonicModelConfig;

use crate::Device;
use crate::TtsConfig;
use crate::domain::speech::Audio;
use crate::ports::TextToSpeech;

/// A `TextToSpeech` with the Supertonic-3 model. It runs each synthesis on a blocking thread.
pub struct SherpaTts {
    /// The engine.
    tts: Arc<OfflineTts>,
    /// The voice, the steps, the language and the seed of each synthesis.
    generation: GenerationConfig,
    /// The model rate in Hz.
    rate: u32,
}

impl SherpaTts {
    /// Loads the Supertonic-3 model of `config` on `device`.
    ///
    /// # Errors
    ///
    /// The model cannot load.
    pub fn try_new(config: &TtsConfig, device: Device) -> anyhow::Result<Self> {
        let dir = &config.model_dir;
        let tts = OfflineTts::create(&engine(dir, device))
            .with_context(|| format!("cannot load the TTS model in {}", dir.display()))?;
        // A fixed seed gives the same samples for the same text.
        let extra = HashMap::from([
            ("lang".to_owned(), json!("en")),
            ("seed".to_owned(), json!(1)),
        ]);
        let generation = GenerationConfig {
            sid: config.voice,
            num_steps: config.num_steps,
            extra: Some(extra),
            ..GenerationConfig::default()
        };
        let rate = u32::try_from(tts.sample_rate())?;
        Ok(Self {
            tts: Arc::new(tts),
            generation,
            rate,
        })
    }
}

/// The engine configuration of the Supertonic-3 model in `dir` on `device`.
fn engine(dir: &Path, device: Device) -> OfflineTtsConfig {
    let file = |name: &str| Some(dir.join(name).to_string_lossy().into_owned());
    let supertonic = OfflineTtsSupertonicModelConfig {
        duration_predictor: file("duration_predictor.int8.onnx"),
        text_encoder: file("text_encoder.int8.onnx"),
        vector_estimator: file("vector_estimator.int8.onnx"),
        vocoder: file("vocoder.int8.onnx"),
        tts_json: file("tts.json"),
        unicode_indexer: file("unicode_indexer.bin"),
        voice_style: file("voice.bin"),
    };
    let provider = match device {
        Device::Cuda => "cuda",
        Device::Cpu => "cpu",
    };
    let model = OfflineTtsModelConfig {
        supertonic,
        provider: Some(provider.to_owned()),
        ..OfflineTtsModelConfig::default()
    };
    OfflineTtsConfig {
        model,
        ..OfflineTtsConfig::default()
    }
}

#[async_trait]
impl TextToSpeech for SherpaTts {
    async fn synth(&self, text: &str) -> anyhow::Result<Audio> {
        let rate = self.rate;
        if text.is_empty() {
            let samples = Vec::new();
            return Ok(Audio { samples, rate });
        }
        let (tts, generation) = (Arc::clone(&self.tts), self.generation.clone());
        let text = text.to_owned();
        tokio::task::spawn_blocking(move || {
            let audio =
                tts.generate_with_config(&text, &generation, None::<fn(&[f32], f32) -> bool>);
            let audio = audio.context("the TTS engine gives no audio")?;
            let samples = audio.samples().iter().copied().map(pcm).collect();
            Ok(Audio { samples, rate })
        })
        .await?
    }
}

/// The 16-bit sample of the sample `value` in [-1, 1].
#[expect(
    clippy::cast_possible_truncation,
    reason = "a float cast saturates at the `i16` range"
)]
fn pcm(value: f32) -> i16 {
    (value * f32::from(i16::MAX)) as i16
}

/// Tests of the TTS adapter with the Supertonic-3 model.
#[cfg(test)]
mod tests {
    use super::*;

    /// The `SherpaTts` of `jarvis.toml` on the CPU.
    fn voice() -> anyhow::Result<SherpaTts> {
        let config = crate::Config::parse(include_str!("../../jarvis.toml"))?;
        SherpaTts::try_new(&config.tts, Device::Cpu)
    }

    /// AC-6.2-2.
    #[tokio::test]
    async fn ac_6_2_2_hello_gives_audio_at_the_model_rate() -> anyhow::Result<()> {
        let audio = voice()?.synth("Hello").await?;
        assert!(!audio.samples.is_empty(), "the audio has no samples");
        assert_eq!(audio.rate, 44_100, "the model rate");
        Ok(())
    }

    /// AC-6.2-3.
    #[tokio::test]
    async fn ac_6_2_3_empty_text_gives_no_samples() -> anyhow::Result<()> {
        let audio = voice()?.synth("").await?;
        assert_eq!(audio.samples, Vec::<i16>::new());
        Ok(())
    }
}
