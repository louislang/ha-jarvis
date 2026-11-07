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

/// The text ids of the short input lengths.
const SHORT_TEXT: usize = 64;
/// The latent frames of the short input lengths.
const SHORT_FRAMES: usize = 64;
/// The text ids of the long input lengths.
const LONG_TEXT: usize = 160;
/// The latent frames of the long input lengths.
const LONG_FRAMES: usize = 192;

/// A `TextToSpeech` that runs the four Supertonic-3 graphs on a blocking thread, one text at a time.
pub struct SupertonicTts(Arc<Mutex<Engine>>);

/// The input lengths of the graphs and the set of sessions that runs them.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Lengths {
    /// The set of sessions: the long set, else the short set.
    long: bool,
    /// The text ids after the added zeros.
    text: usize,
    /// The latent frames after the added zeros.
    frames: usize,
}

/// The four graphs of `[tts].model_dir` for each length, the indexer and the voice.
struct Engine {
    /// The duration predictor, the text encoder, the fp32 vector estimator and the fp32 vocoder,
    /// for the short lengths, then for the long lengths.
    graphs: [[Session; 4]; 2],
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

impl Settings {
    /// The samples of one latent frame.
    fn frame_samples(&self) -> usize {
        self.ae.base_chunk_size * self.ttl.chunk_compress_factor
    }

    /// The channels of one latent frame.
    fn channels(&self) -> usize {
        self.ttl.latent_dim * self.ttl.chunk_compress_factor
    }
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
            graphs: [graphs(dir, builder)?, graphs(dir, builder)?],
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
        self.speak(&ids, lengths)
    }

    /// The 16-bit samples of the text ids `ids` at the input lengths that `rule` gives.
    fn speak(
        &mut self,
        ids: &[i64],
        rule: fn(usize, usize) -> Lengths,
    ) -> anyhow::Result<Vec<i16>> {
        let texts = ids.len();
        let first = rule(texts, 0);
        let text_mask = mask(texts, first.text)?;
        let ids = Tensor::from_array(([1, first.text], padded(ids, texts, first.text)))?;
        let samples = self.samples(first.long, &ids, &text_mask)?;
        let frames = samples.div_ceil(self.settings.frame_samples());
        let last = rule(texts, frames);
        let text_emb = self.text_emb(first, &ids, &text_mask, last.text)?;
        let latent = self.denoise(last, [texts, frames], &text_emb)?;
        self.vocode(last.long, latent, samples)
    }

    /// The first `samples` 16-bit samples that the vocoder of the long or the short set gives for
    /// `latent`.
    fn vocode(&mut self, long: bool, latent: DynValue, samples: usize) -> anyhow::Result<Vec<i16>> {
        let [.., vocoder] = &mut self.graphs[usize::from(long)];
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
    fn samples(
        &mut self,
        long: bool,
        ids: &Tensor<i64>,
        mask: &Tensor<f32>,
    ) -> anyhow::Result<usize> {
        let [duration, ..] = &mut self.graphs[usize::from(long)];
        let outputs = duration.run(ort::inputs![
            "text_ids" => ids,
            "style_dp" => &self.style_dp,
            "text_mask" => mask,
        ])?;
        let seconds = outputs["duration"].try_extract_tensor::<f32>()?.1;
        let seconds = f64::from(*seconds.first().context("no duration")?);
        Ok(((seconds * f64::from(self.settings.ae.sample_rate)) as usize).max(1))
    }

    /// The `text_emb` that the text encoder gives at the lengths `first`, with zeros up to
    /// `length` text ids.
    fn text_emb(
        &mut self,
        first: Lengths,
        ids: &Tensor<i64>,
        mask: &Tensor<f32>,
        length: usize,
    ) -> anyhow::Result<Tensor<f32>> {
        let [_, encoder, ..] = &mut self.graphs[usize::from(first.long)];
        let outputs = encoder.run(ort::inputs![
            "text_ids" => ids,
            "style_ttl" => &self.style_ttl,
            "text_mask" => mask,
        ])?;
        let values = outputs["text_emb"].try_extract_tensor::<f32>()?.1;
        let values = padded(values, first.text, length);
        let shape = [1, values.len() / length, length];
        Ok(Tensor::from_array((shape, values))?)
    }

    /// The latent at the lengths `last` after the denoising steps from the seeded noise, for the
    /// real lengths `[texts, frames]`.
    fn denoise(
        &mut self,
        last: Lengths,
        [texts, frames]: [usize; 2],
        text_emb: &Tensor<f32>,
    ) -> anyhow::Result<DynValue> {
        let mut latent = noise(self.settings.channels(), frames, last.frames)?;
        let latent_mask = mask(frames, last.frames)?;
        let text_mask = mask(texts, last.text)?;
        let [.., estimator, _] = &mut self.graphs[usize::from(last.long)];
        for step in 0..self.steps {
            let mut outputs = estimator.run(ort::inputs![
                "noisy_latent" => &latent,
                "text_emb" => text_emb,
                "style_ttl" => &self.style_ttl,
                "latent_mask" => &latent_mask,
                "text_mask" => &text_mask,
                "current_step" => Tensor::from_array(([1], vec![f32::from(step)]))?,
                "total_step" => Tensor::from_array(([1], vec![f32::from(self.steps)]))?,
            ])?;
            latent = outputs.remove("denoised_latent").context("no latent")?;
        }
        Ok(latent)
    }
}

/// The input lengths for a text with `texts` ids and a speech with `frames` frames: the short
/// lengths when both fit, else the long lengths.
fn lengths(texts: usize, frames: usize) -> Lengths {
    if texts <= SHORT_TEXT && frames <= SHORT_FRAMES {
        Lengths {
            long: false,
            text: SHORT_TEXT,
            frames: SHORT_FRAMES,
        }
    } else {
        long_lengths(texts, frames)
    }
}

/// The long input lengths, or the real lengths `texts` and `frames` when they are longer.
fn long_lengths(texts: usize, frames: usize) -> Lengths {
    Lengths {
        long: true,
        text: texts.max(LONG_TEXT),
        frames: frames.max(LONG_FRAMES),
    }
}

/// The rows of `width` values in `values`, each with zeros added up to `length` values.
fn padded<T: Clone + Default>(values: &[T], width: usize, length: usize) -> Vec<T> {
    let rows = values.chunks(width.max(1)).map(|row| {
        let mut row = row.to_vec();
        row.resize(length, T::default());
        row
    });
    rows.flatten().collect()
}

/// A mask of shape `[1, 1, length]`: 1 for each of the first `real` positions, else 0.
fn mask(real: usize, length: usize) -> ort::Result<Tensor<f32>> {
    Tensor::from_array(([1, 1, length], padded(&vec![1.0; real], real, length)))
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

/// Seeded latent noise of shape `[1, channels, length]` with zeros after the first `frames`
/// frames, so a text always gives the same samples.
fn noise(channels: usize, frames: usize, length: usize) -> ort::Result<DynValue> {
    let mut random = StdRng::seed_from_u64(1);
    let noise = (0..channels * frames).map(|_| StandardNormal.sample(&mut random));
    let noise: Vec<f32> = noise.collect();
    let noise = padded(&noise, frames, length);
    Ok(Tensor::from_array(([1, channels, length], noise))?.into_dyn())
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
    use std::time::Duration;
    use std::time::Instant;

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
        let [short, _] = &mut engine.graphs;
        for session in short {
            let profile = fs::read_to_string(session.end_profiling()?)?;
            assert!(profile.contains("CUDAExecutionProvider"), "{profile}");
        }
        Ok(())
    }

    /// AC-9.3b-1. Each case gives the lengths of the first two graphs, then of the last two.
    #[test]
    fn ac_9_3b_1_lengths_follow_the_rule() {
        let short = Lengths {
            long: false,
            text: 64,
            frames: 64,
        };
        let long = |text, frames| Lengths {
            long: true,
            text,
            frames,
        };
        let cases = [
            ((20, 20), short, short),
            ((64, 64), short, short),
            ((64, 65), short, long(160, 192)),
            ((65, 64), long(160, 192), long(160, 192)),
            ((160, 192), long(160, 192), long(160, 192)),
            ((200, 150), long(200, 192), long(200, 192)),
            ((100, 250), long(160, 192), long(160, 250)),
        ];
        for ((texts, frames), first, last) in cases {
            let actual = [lengths(texts, 0), lengths(texts, frames)];
            assert_eq!(actual, [first, last], "n = {texts}, f = {frames}");
        }
    }

    /// AC-9.3b-2.
    #[test]
    fn ac_9_3b_2_short_and_long_lengths_give_the_same_audio() -> anyhow::Result<()> {
        let mut engine = Engine::open(&config()?, &builder(Device::Cpu)?)?;
        let ids = text_ids("I am turning the lights on.", &engine.indexer);
        let short = engine.speak(&ids, lengths)?;
        let long = engine.speak(&ids, long_lengths)?;
        assert_eq!(short.len(), long.len(), "the sample counts");
        let pairs = short.iter().zip(&long);
        let difference = pairs.map(|(short, long)| short.abs_diff(*long)).max();
        assert!(difference <= Some(64), "maximum difference {difference:?}");
        Ok(())
    }

    /// The sorted `synth` times on CUDA of the short replies and of the sentences of 40 runs that
    /// alternate `replies` with `sentence`, after a warm-up.
    fn alternate_times(replies: &[&str], sentence: &str) -> anyhow::Result<[Vec<Duration>; 2]> {
        let mut engine = Engine::open(&config()?, &builder(Device::Cuda)?)?;
        let runs = replies.iter().flat_map(|&reply| [reply, sentence]).cycle();
        let (warm_up, runs) = (runs.clone().take(2), runs.take(40).enumerate());
        for text in warm_up {
            engine.synth(text)?;
        }
        let mut times = [Vec::new(), Vec::new()];
        for (run, text) in runs {
            let start = Instant::now();
            engine.synth(text)?;
            times[run % 2].push(start.elapsed());
        }
        Ok(times.map(|mut times| {
            times.sort();
            times
        }))
    }

    /// AC-9.3b-3. Writes the two p95 times to stderr.
    #[test]
    #[ignore = "GPU host"]
    fn ac_9_3b_3_alternate_lengths_p95_on_cuda() -> anyhow::Result<()> {
        use std::io::Write;

        let replies = [
            "Playing jeopardy.",
            "Putting jeopardy on the TV.",
            "One moment.",
            "I am turning the lights on.",
        ];
        let sentence = "Tomorrow will be mostly sunny with a light breeze from the west, and the evening should stay clear and mild, so it is a good night for a walk.";
        let [short, long] = alternate_times(&replies, sentence)?;
        // The 95th percentile of 20 times is the 19th.
        let p95 = [short[18], long[18]];
        writeln!(
            std::io::stderr(),
            "p95 short {:?}, sentence {:?}",
            p95[0],
            p95[1]
        )?;
        assert!(p95[0] <= Duration::from_millis(80), "short {short:?}");
        assert!(p95[1] <= Duration::from_millis(150), "sentence {long:?}");
        Ok(())
    }
}
