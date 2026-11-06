//! `TextToSpeech` with the Supertonic-3 graphs in ONNX Runtime.

use std::fs;
use std::path::Path;
use std::sync::Arc;
use std::sync::Mutex;

use anyhow::Context;
use async_trait::async_trait;
use ort::execution_providers::CPUExecutionProvider;
use ort::execution_providers::CUDAExecutionProvider;
use ort::session::Session;
use ort::session::builder::SessionBuilder;
use ort::value::DynValue;
use ort::value::Tensor;
use rand::SeedableRng;
use rand::rngs::StdRng;
use rand_distr::Distribution;
use rand_distr::StandardNormal;
use serde::Deserialize;
use unicode_normalization::UnicodeNormalization;

use crate::Device;
use crate::TtsConfig;
use crate::domain::speech::Audio;
use crate::ports::TextToSpeech;

/// A `TextToSpeech` that runs the four Supertonic-3 graphs on a blocking thread, one text at a time.
pub struct SupertonicTts(Arc<Mutex<Engine>>);

/// The four graphs of `[tts].model_dir`, the indexer and the voice.
struct Engine {
    /// The duration predictor, the text encoder, the fp32 vector estimator and the fp32 vocoder.
    graphs: [Session; 4],
    /// The text id of each Unicode code point.
    indexer: Vec<i32>,
    /// The `style_ttl` of the voice.
    style_ttl: Tensor<f32>,
    /// The `style_dp` of the voice.
    style_dp: Tensor<f32>,
    /// The sizes of `tts.json`.
    settings: Settings,
    /// The denoising steps.
    steps: u16,
}

/// The sizes of `tts.json`.
#[derive(Deserialize)]
struct Settings {
    /// The autoencoder sizes.
    ae: Autoencoder,
    /// The text-to-latent sizes.
    ttl: TextToLatent,
}

/// The autoencoder sizes of `tts.json`.
#[derive(Deserialize)]
struct Autoencoder {
    /// The sample rate in Hz.
    sample_rate: u32,
    /// The samples of one latent step.
    base_chunk_size: usize,
}

/// The text-to-latent sizes of `tts.json`.
#[derive(Deserialize)]
struct TextToLatent {
    /// The latent steps in one latent frame.
    chunk_compress_factor: usize,
    /// The channels of one latent step.
    latent_dim: usize,
}

impl SupertonicTts {
    /// Loads the Supertonic-3 files of `config` on `device`.
    ///
    /// # Errors
    ///
    /// A file cannot be read, a graph cannot load, or the provider of `device` cannot register.
    pub fn try_new(config: &TtsConfig, device: Device) -> anyhow::Result<Self> {
        let engine = Engine::open(config, &builder(device)?)?;
        Ok(Self(Arc::new(Mutex::new(engine))))
    }
}

impl Engine {
    /// Opens the files of `config` with the session options of `builder`.
    fn open(config: &TtsConfig, builder: &SessionBuilder) -> anyhow::Result<Self> {
        let dir = &config.model_dir;
        let [style_ttl, style_dp] = styles(&read(dir, "voice.bin")?, config.voice)?;
        Ok(Self {
            graphs: graphs(dir, builder)?,
            indexer: numbers(&read(dir, "unicode_indexer.bin")?, i32::from_le_bytes),
            style_ttl,
            style_dp,
            settings: serde_json::from_slice(&read(dir, "tts.json")?)?,
            steps: u16::try_from(config.num_steps)?,
        })
    }

    /// The 16-bit samples of `text`. Empty text gives no samples and runs no graph.
    fn synth(&mut self, text: &str) -> anyhow::Result<Vec<i16>> {
        if text.is_empty() {
            return Ok(Vec::new());
        }
        let ids = text_ids(text, &self.indexer);
        let length = ids.len();
        let mask = Tensor::from_array(([1, 1, length], vec![1.0_f32; length]))?;
        let ids = Tensor::from_array(([1, length], ids))?;
        let samples = self.samples(&ids, &mask)?;
        let text_emb = self.text_emb(&ids, &mask)?;
        let latent = self.denoise(samples, &text_emb, &mask)?;
        let [.., vocoder] = &mut self.graphs;
        let outputs = vocoder.run(ort::inputs!["latent" => latent])?;
        let wave = outputs["wav_tts"].try_extract_tensor::<f32>()?.1;
        Ok(wave.iter().take(samples).copied().map(pcm).collect())
    }

    /// The sample count of the predicted duration, rounded down, and 1 or more.
    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "saturating cast"
    )]
    fn samples(&mut self, ids: &Tensor<i64>, mask: &Tensor<f32>) -> anyhow::Result<usize> {
        let [duration, ..] = &mut self.graphs;
        let outputs = duration.run(ort::inputs![
            "text_ids" => ids,
            "style_dp" => &self.style_dp,
            "text_mask" => mask,
        ])?;
        let seconds = outputs["duration"].try_extract_tensor::<f32>()?.1;
        let seconds = f64::from(*seconds.first().context("no duration")?);
        Ok(((seconds * f64::from(self.settings.ae.sample_rate)) as usize).max(1))
    }

    /// The `text_emb` that the text encoder gives for the text.
    fn text_emb(&mut self, ids: &Tensor<i64>, mask: &Tensor<f32>) -> anyhow::Result<DynValue> {
        let [_, encoder, ..] = &mut self.graphs;
        let mut outputs = encoder.run(ort::inputs![
            "text_ids" => ids,
            "style_ttl" => &self.style_ttl,
            "text_mask" => mask,
        ])?;
        outputs.remove("text_emb").context("no text_emb")
    }

    /// The latent of `samples` samples after the denoising steps from the seeded noise.
    fn denoise(
        &mut self,
        samples: usize,
        text_emb: &DynValue,
        text_mask: &Tensor<f32>,
    ) -> anyhow::Result<DynValue> {
        let ttl = &self.settings.ttl;
        let frames = samples.div_ceil(self.settings.ae.base_chunk_size * ttl.chunk_compress_factor);
        let mut latent = noise(ttl.latent_dim * ttl.chunk_compress_factor, frames)?;
        let latent_mask = Tensor::from_array(([1, 1, frames], vec![1.0_f32; frames]))?;
        for step in 0..self.steps {
            let [.., estimator, _] = &mut self.graphs;
            let mut outputs = estimator.run(ort::inputs![
                "noisy_latent" => &latent,
                "text_emb" => text_emb,
                "style_ttl" => &self.style_ttl,
                "latent_mask" => &latent_mask,
                "text_mask" => text_mask,
                "current_step" => Tensor::from_array(([1], vec![f32::from(step)]))?,
                "total_step" => Tensor::from_array(([1], vec![f32::from(self.steps)]))?,
            ])?;
            latent = outputs.remove("denoised_latent").context("no latent")?;
        }
        Ok(latent)
    }
}

/// A session builder that registers the execution provider of `device`.
fn builder(device: Device) -> ort::Result<SessionBuilder> {
    let provider = match device {
        Device::Cuda => CUDAExecutionProvider::default().build(),
        Device::Cpu => CPUExecutionProvider::default().build(),
    };
    // The CUDA provider of ort 2.0.0-rc.10 needs the environment but does not create it.
    ort::environment::get_environment()?;
    Session::builder()?.with_execution_providers([provider.error_on_failure()])
}

/// The sessions of the four graphs in `dir` in run order, with the options of `builder`.
fn graphs(dir: &Path, builder: &SessionBuilder) -> anyhow::Result<[Session; 4]> {
    let names = [
        "duration_predictor.int8.onnx",
        "text_encoder.int8.onnx",
        "vector_estimator.onnx",
        "vocoder.onnx",
    ];
    let [duration, encoder, estimator, vocoder] = names.map(|name| {
        let session = builder.clone().commit_from_file(dir.join(name));
        session.with_context(|| format!("cannot load {name} in {}", dir.display()))
    });
    Ok([duration?, encoder?, estimator?, vocoder?])
}

/// The bytes of the file `name` in `dir`.
fn read(dir: &Path, name: &str) -> anyhow::Result<Vec<u8>> {
    fs::read(dir.join(name)).with_context(|| format!("cannot read {name} in {}", dir.display()))
}

/// The `style_ttl` and `style_dp` tensors of the voice `voice` in the bytes of `voice.bin`.
fn styles(bytes: &[u8], voice: i32) -> anyhow::Result<[Tensor<f32>; 2]> {
    let (header, values) = bytes.split_at_checked(48).context("no dimensions")?;
    let dimensions = numbers(header, |bytes| usize::try_from(i64::from_le_bytes(bytes)));
    let dimensions: Vec<usize> = dimensions.into_iter().collect::<Result<_, _>>()?;
    let &[ttl, dp] = dimensions.as_chunks().0 else {
        anyhow::bail!("voice.bin has no two styles");
    };
    let values = numbers(values, f32::from_le_bytes);
    let split = values.split_at_checked(ttl.iter().product());
    let (ttl_values, dp_values) = split.context("voice.bin is too short")?;
    let voice = usize::try_from(voice)?;
    Ok([style(ttl_values, ttl, voice)?, style(dp_values, dp, voice)?])
}

/// The tensor of the voice `voice` in `values`, the styles of all voices with `dimensions`.
fn style(values: &[f32], dimensions: [usize; 3], voice: usize) -> anyhow::Result<Tensor<f32>> {
    let [voices, rows, columns] = dimensions;
    anyhow::ensure!(voice < voices, "voice.bin has no voice {voice}");
    let size = rows * columns;
    let values = values.get(voice * size..(voice + 1) * size);
    let values = values.context("voice.bin is too short")?.to_vec();
    Ok(Tensor::from_array(([1, rows, columns], values))?)
}

/// The little-endian numbers of `bytes`, `N` bytes each.
fn numbers<const N: usize, T>(bytes: &[u8], decode: fn([u8; N]) -> T) -> Vec<T> {
    bytes.as_chunks().0.iter().copied().map(decode).collect()
}

/// Seeded latent noise of shape `[1, channels, frames]`, so a text always gives the same samples.
fn noise(channels: usize, frames: usize) -> ort::Result<DynValue> {
    let mut random = StdRng::seed_from_u64(1);
    let noise = (0..channels * frames).map(|_| StandardNormal.sample(&mut random));
    let noise: Vec<f32> = noise.collect();
    Ok(Tensor::from_array(([1, channels, frames], noise))?.into_dyn())
}

/// The text of `text` after the replacements of the first text rule of sherpa-onnx 2.0.1.
fn replaced(text: &str) -> String {
    text.replace(['–', '‑', '—'], "-")
        .replace(['“', '”'], "\"")
        .replace(['‘', '’', '´', '`'], "'")
        .replace(['_', '[', ']', '|', '/', '#', '→', '←'], " ")
        .replace(['♥', '☆', '♡', '©', '\\'], "")
        .replace('@', " at ")
        .replace("e.g.,", "for example, ")
        .replace("i.e.,", "that is, ")
}

/// The text of `text` after the text rules of sherpa-onnx 2.0.1, between the language tags.
fn tagged(text: &str) -> String {
    let mut text = replaced(text);
    text.retain(|character| !('\u{1F000}'..='\u{1FFFF}').contains(&character));
    for mark in [",", ".", "!", "?", ";", ":", "'"] {
        text = text.replace(&format!(" {mark}"), mark);
    }
    let text = text.replace("\"\"", "\"").replace("''", "'");
    let mut text = text.split_whitespace().collect::<Vec<&str>>().join(" ");
    let endings = ".!?;:,'\")]}>…。」』】〉》›»“”‘’";
    if text.ends_with(|last| !endings.contains(last)) {
        text.push('.');
    }
    format!("<en>{text}</en>")
}

/// The text ids of `text` with the values of `indexer`. A code point after its end gives 0.
fn text_ids(text: &str, indexer: &[i32]) -> Vec<i64> {
    let text = tagged(text);
    let codes = text.nfkd().map(u32::from).filter(|&code| code <= 0xFFFF);
    let ids = codes.map(|code| indexer.get(code as usize).copied().unwrap_or_default());
    ids.map(i64::from).collect()
}

/// The 16-bit sample of the sample `value` in [-1, 1].
#[expect(
    clippy::cast_possible_truncation,
    reason = "a float cast saturates at the `i16` range"
)]
fn pcm(value: f32) -> i16 {
    (value * f32::from(i16::MAX)) as i16
}

#[async_trait]
impl TextToSpeech for SupertonicTts {
    async fn synth(&self, text: &str) -> anyhow::Result<Audio> {
        let (engine, text) = (Arc::clone(&self.0), text.to_owned());
        tokio::task::spawn_blocking(move || {
            let mut engine = engine.lock().map_err(|_| anyhow::anyhow!("poisoned"))?;
            let samples = engine.synth(&text)?;
            let rate = engine.settings.ae.sample_rate;
            Ok(Audio { samples, rate })
        })
        .await?
    }
}

/// Tests of the TTS adapter with the Supertonic-3 files of `jarvis.toml`.
#[cfg(test)]
mod tests {
    use super::*;
    use crate::Config;

    /// The `[tts]` section of `jarvis.toml`.
    fn config() -> anyhow::Result<TtsConfig> {
        Ok(Config::parse(include_str!("../../jarvis.toml"))?.tts)
    }

    /// AC-9.3-2.
    #[tokio::test]
    async fn ac_9_3_2_hello_gives_audio_at_44100_hz() -> anyhow::Result<()> {
        let tts = SupertonicTts::try_new(&config()?, Device::Cpu)?;
        let audio = tts.synth("Hello").await?;
        assert!(!audio.samples.is_empty(), "the audio has no samples");
        assert_eq!(audio.rate, 44_100, "the model rate");
        Ok(())
    }

    /// AC-9.3-3.
    #[tokio::test]
    async fn ac_9_3_3_empty_text_gives_no_samples() -> anyhow::Result<()> {
        let tts = SupertonicTts::try_new(&config()?, Device::Cpu)?;
        let audio = tts.synth("").await?;
        assert!(audio.samples.is_empty(), "{} samples", audio.samples.len());
        Ok(())
    }

    /// AC-9.3-4.
    #[tokio::test]
    async fn ac_9_3_4_same_text_gives_identical_samples() -> anyhow::Result<()> {
        let tts = SupertonicTts::try_new(&config()?, Device::Cpu)?;
        let text = "I am turning the lights on.";
        let (first, second) = (tts.synth(text).await?, tts.synth(text).await?);
        assert!(first == second, "the two audios differ");
        Ok(())
    }

    /// AC-9.3-5.
    #[test]
    fn ac_9_3_5_text_ids_follow_the_text_rules() -> anyhow::Result<()> {
        let dir = config()?.model_dir;
        let indexer = numbers(&read(&dir, "unicode_indexer.bin")?, i32::from_le_bytes);
        let ids = |text: &str| -> Vec<i64> {
            let codes = text.chars().map(|character| character as usize);
            codes.map(|code| i64::from(indexer[code])).collect()
        };
        assert_eq!(
            text_ids("It’s  “on” — now", &indexer),
            ids("<en>It's \"on\" - now.</en>")
        );
        assert_eq!(text_ids("café…", &indexer), ids("<en>cafe\u{301}...</en>"));
        Ok(())
    }

    /// AC-9.3-6. The profile of each session names the provider of each node that it ran.
    #[test]
    #[ignore = "GPU host"]
    fn ac_9_3_6_cuda_runs_the_four_graphs() -> anyhow::Result<()> {
        let folder = tempfile::tempdir()?;
        let builder = builder(Device::Cuda)?.with_profiling(folder.path().join("profile"))?;
        let mut engine = Engine::open(&config()?, &builder)?;
        assert!(!engine.synth("Hello")?.is_empty(), "no samples");
        for session in &mut engine.graphs {
            let profile = fs::read_to_string(session.end_profiling()?)?;
            assert!(profile.contains("CUDAExecutionProvider"), "{profile}");
        }
        Ok(())
    }
}
