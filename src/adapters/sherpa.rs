//! `SpeechToText` and `TextToSpeech` with sherpa-onnx.

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
use sherpa_onnx::OnlineModelConfig;
use sherpa_onnx::OnlineRecognizer;
use sherpa_onnx::OnlineRecognizerConfig;
use sherpa_onnx::OnlineStream;
use sherpa_onnx::OnlineTransducerModelConfig;

use crate::AsrConfig;
use crate::Device;
use crate::TtsConfig;
use crate::domain::speech::Audio;
use crate::ports::Partial;
use crate::ports::SpeechToText;
use crate::ports::SttStream;
use crate::ports::TextToSpeech;

/// A `SpeechToText` with a streaming transducer.
pub struct SherpaStt {
    /// The streaming recognizer.
    recognizer: Arc<OnlineRecognizer>,
}

impl SherpaStt {
    /// Loads the first-pass model of `config` on `device`.
    ///
    /// # Errors
    ///
    /// The model cannot load.
    pub fn try_new(config: &AsrConfig, device: Device) -> anyhow::Result<Self> {
        let dir = &config.model_dir;
        let file = |name: &str| Some(file(dir, name));
        let transducer = OnlineTransducerModelConfig {
            encoder: file("encoder.int8.onnx"),
            decoder: file("decoder.int8.onnx"),
            joiner: file("joiner.int8.onnx"),
        };
        let model_config = OnlineModelConfig {
            transducer,
            tokens: file("tokens.txt"),
            provider: Some(provider(device)),
            ..OnlineModelConfig::default()
        };
        let config = OnlineRecognizerConfig {
            model_config,
            ..OnlineRecognizerConfig::default()
        };
        let recognizer = OnlineRecognizer::create(&config)
            .with_context(|| format!("cannot load the STT model in {}", dir.display()))?;
        Ok(Self {
            recognizer: Arc::new(recognizer),
        })
    }
}

impl SpeechToText for SherpaStt {
    fn stream(&self) -> Box<dyn SttStream> {
        Box::new(SherpaStream {
            stream: self.recognizer.create_stream(),
            recognizer: Arc::clone(&self.recognizer),
        })
    }
}

/// The recognizer stream of one utterance.
struct SherpaStream {
    /// The recognizer of `stream`.
    recognizer: Arc<OnlineRecognizer>,
    /// The decoded audio.
    stream: OnlineStream,
}

impl SttStream for SherpaStream {
    fn accept(&mut self, samples: &[i16]) {
        let wave: Vec<f32> = samples
            .iter()
            .map(|&sample| f32::from(sample) / f32::from(i16::MAX))
            .collect();
        self.stream.accept_waveform(16_000, &wave);
        while self.recognizer.is_ready(&self.stream) {
            self.recognizer.decode(&self.stream);
        }
    }

    fn partial(&self) -> Partial {
        let result = self.recognizer.get_result(&self.stream);
        Partial {
            text: result.map(|result| result.text).unwrap_or_default(),
        }
    }

    /// Makes a new recognizer stream, which has no decoded tokens.
    fn reset(&mut self) {
        self.stream = self.recognizer.create_stream();
    }
}

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
    let file = |name: &str| Some(file(dir, name));
    let supertonic = OfflineTtsSupertonicModelConfig {
        duration_predictor: file("duration_predictor.int8.onnx"),
        text_encoder: file("text_encoder.int8.onnx"),
        vector_estimator: file("vector_estimator.int8.onnx"),
        vocoder: file("vocoder.int8.onnx"),
        tts_json: file("tts.json"),
        unicode_indexer: file("unicode_indexer.bin"),
        voice_style: file("voice.bin"),
    };
    let model = OfflineTtsModelConfig {
        supertonic,
        provider: Some(provider(device)),
        ..OfflineTtsModelConfig::default()
    };
    OfflineTtsConfig {
        model,
        ..OfflineTtsConfig::default()
    }
}

/// The path of the file `name` in `dir`.
fn file(dir: &Path, name: &str) -> String {
    dir.join(name).to_string_lossy().into_owned()
}

/// The sherpa-onnx execution provider of `device`.
fn provider(device: Device) -> String {
    match device {
        Device::Cuda => "cuda",
        Device::Cpu => "cpu",
    }
    .to_owned()
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

/// Tests of the STT adapter with the Nemotron model and of the TTS adapter with the Supertonic-3
/// model.
#[cfg(test)]
mod tests {
    use super::*;
    use crate::Config;
    use crate::domain::intent;
    use crate::domain::speech;
    use crate::testing::fakes;

    /// The request of the STT tests.
    const REQUEST: &str = "turn on the kitchen lights";

    /// The configuration of `jarvis.toml`.
    fn config() -> anyhow::Result<Config> {
        Config::parse(include_str!("../../jarvis.toml"))
    }

    /// The `SherpaTts` of `jarvis.toml` on the CPU.
    fn voice() -> anyhow::Result<SherpaTts> {
        SherpaTts::try_new(&config()?.tts, Device::Cpu)
    }

    /// A new first pass of the `SherpaStt` of `jarvis.toml` on the CPU.
    fn first_pass() -> anyhow::Result<Box<dyn SttStream>> {
        Ok(SherpaStt::try_new(&config()?.asr, Device::Cpu)?.stream())
    }

    /// The partial after each 20 ms chunk of `speech(REQUEST)`.
    async fn partials() -> anyhow::Result<Vec<Partial>> {
        let audio = fakes::speech(REQUEST).await?;
        let mut stream = first_pass()?;
        let chunks = speech::chunks(&audio, 320);
        let decode = |chunk: &[i16]| {
            stream.accept(chunk);
            stream.partial()
        };
        Ok(chunks.into_iter().map(decode).collect())
    }

    /// AC-6.6-1.
    #[tokio::test]
    async fn ac_6_6_1_final_partial_is_the_request() -> anyhow::Result<()> {
        let partials = partials().await?;
        let text = partials
            .last()
            .map(|partial| intent::normalize(&partial.text));
        assert_eq!(text.as_deref(), Some(REQUEST), "{partials:?}");
        Ok(())
    }

    /// AC-6.6-2.
    #[tokio::test]
    async fn ac_6_6_2_partial_has_text_before_the_last_chunk() -> anyhow::Result<()> {
        let partials = partials().await?;
        let early = &partials[..partials.len() - 1];
        assert!(
            early.iter().any(|partial| !partial.text.is_empty()),
            "no partial before the last chunk has text: {partials:?}"
        );
        Ok(())
    }

    /// AC-6.6-3.
    #[tokio::test]
    async fn ac_6_6_3_reset_drops_the_text() -> anyhow::Result<()> {
        let audio = fakes::speech("lights on").await?;
        let mut stream = first_pass()?;
        stream.accept(&audio.samples);
        let before = stream.partial();
        assert!(!before.text.is_empty(), "the audio gives text: {before:?}");
        stream.reset();
        assert_eq!(stream.partial(), Partial::default());
        Ok(())
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
