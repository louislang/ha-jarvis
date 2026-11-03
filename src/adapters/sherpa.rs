//! `SpeechToText` and `TextToSpeech` with sherpa-onnx.

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

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
use sherpa_onnx::SileroVadModelConfig;
use sherpa_onnx::VadModelConfig;
use sherpa_onnx::VoiceActivityDetector;

use crate::AsrConfig;
use crate::Device;
use crate::TtsConfig;
use crate::domain::speech::Audio;
use crate::ports::Partial;
use crate::ports::SpeechToText;
use crate::ports::SttStream;
use crate::ports::TextToSpeech;

/// The sample rate of the first pass in Hz.
const RATE: u16 = 16_000;

/// The samples of one Silero VAD frame.
const WINDOW: u16 = 512;

/// A `SpeechToText` with a streaming transducer and the Silero VAD.
pub struct SherpaStt {
    /// The streaming recognizer.
    recognizer: Arc<OnlineRecognizer>,
    /// The VAD of each stream.
    vad: VadModelConfig,
    /// The silence in samples that ends a VAD speech segment.
    min_silence: u64,
}

impl SherpaStt {
    /// Loads the first-pass model of `config` on `device`.
    ///
    /// # Errors
    ///
    /// The model cannot load.
    pub fn try_new(config: &AsrConfig, device: Device) -> anyhow::Result<Self> {
        let vad = vad(config, device);
        let min_silence = config.vad_min_silence_ms * u64::from(RATE) / 1000;
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
            num_threads: 4,
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
            vad,
            min_silence,
        })
    }
}

/// The Silero VAD configuration of `config` on `device`.
fn vad(config: &AsrConfig, device: Device) -> VadModelConfig {
    let seconds = |ms: u64| Duration::from_millis(ms).as_secs_f32();
    let silero_vad = SileroVadModelConfig {
        model: Some(config.vad_model.to_string_lossy().into_owned()),
        threshold: config.vad_threshold,
        min_silence_duration: seconds(config.vad_min_silence_ms),
        // sherpa-onnx replaces 0 with 0.25 s. One frame confirms the speech start, and
        // `SherpaStream::track` holds that frame.
        min_speech_duration: seconds(u64::from(WINDOW) * 1000 / u64::from(RATE)),
        window_size: i32::from(WINDOW),
        // An utterance commits at `max_utterance_ms`, so the VAD never splits a segment.
        max_speech_duration: seconds(config.max_utterance_ms),
    };
    VadModelConfig {
        silero_vad,
        sample_rate: i32::from(RATE),
        num_threads: 1,
        provider: Some(provider(device)),
        ..VadModelConfig::default()
    }
}

impl SpeechToText for SherpaStt {
    fn stream(&self) -> anyhow::Result<Box<dyn SttStream>> {
        let vad = VoiceActivityDetector::create(&self.vad, 30.0).context("cannot load the VAD")?;
        Ok(Box::new(SherpaStream {
            stream: self.recognizer.create_stream(),
            recognizer: Arc::clone(&self.recognizer),
            vad,
            min_silence: self.min_silence,
            fed: 0,
            decided: 0,
            voice_end: None,
        }))
    }
}

/// The recognizer stream and the VAD of one utterance, with audio positions in samples.
struct SherpaStream {
    /// The recognizer of `stream`.
    recognizer: Arc<OnlineRecognizer>,
    /// The decoded audio.
    stream: OnlineStream,
    /// The VAD of the same audio.
    vad: VoiceActivityDetector,
    /// The silence that ends a VAD speech segment.
    min_silence: u64,
    /// The number of samples so far.
    fed: u64,
    /// The end of the audio that the VAD has decided.
    decided: u64,
    /// The end of the last voiced frame, or `None` before any speech.
    voice_end: Option<u64>,
}

impl SherpaStream {
    /// Updates the VAD positions after `count` more samples of at most one frame.
    fn track(&mut self, count: usize) {
        self.fed += count as u64;
        while let Some(segment) = self.vad.front() {
            let end = segment.start().unsigned_abs() + u64::from(segment.n().unsigned_abs());
            self.voice_end = Some(end);
            self.vad.pop();
        }
        let detected = self.vad.detected();
        // In a segment, the VAD holds the frames that can end it. Out of one, it holds the frame
        // that can start one.
        let held = if detected {
            self.min_silence
        } else {
            u64::from(WINDOW)
        };
        let run = self.fed - self.fed % u64::from(WINDOW);
        self.decided = self
            .decided
            .max(run.saturating_sub(held))
            .max(self.voice_end.unwrap_or(0));
        if detected {
            self.voice_end = Some(self.decided);
        }
    }
}

impl SttStream for SherpaStream {
    fn accept(&mut self, samples: &[i16]) {
        let wave: Vec<f32> = samples
            .iter()
            .map(|&sample| f32::from(sample) / f32::from(i16::MAX))
            .collect();
        self.stream.accept_waveform(i32::from(RATE), &wave);
        while self.recognizer.is_ready(&self.stream) {
            self.recognizer.decode(&self.stream);
        }
        // The VAD gives one decision for all frames of one call.
        for window in wave.chunks(usize::from(WINDOW)) {
            self.vad.accept_waveform(window);
            self.track(window.len());
        }
    }

    fn partial(&self) -> Partial {
        let result = self.recognizer.get_result(&self.stream);
        let ms = |samples: u64| samples * 1000 / u64::from(RATE);
        Partial {
            text: result.map(|result| result.text).unwrap_or_default(),
            voice_end_ms: self.voice_end.map(ms),
            vad_ms: ms(self.decided),
        }
    }

    /// Makes a new recognizer stream, which has no decoded tokens, and resets the VAD.
    fn reset(&mut self) {
        self.stream = self.recognizer.create_stream();
        self.vad.reset();
        (self.fed, self.decided, self.voice_end) = (0, 0, None);
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

/// Tests of the STT adapter with the Nemotron model and the Silero VAD, and of the TTS adapter
/// with the Supertonic-3 model.
#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::time::Instant;

    use sherpa_onnx::OfflineModelConfig;
    use sherpa_onnx::OfflineRecognizer;
    use sherpa_onnx::OfflineRecognizerConfig;
    use sherpa_onnx::OfflineTransducerModelConfig;

    use super::*;
    use crate::Config;
    use crate::domain::intent;
    use crate::domain::speech;
    use crate::testing::fakes;

    /// The request of the STT tests.
    const REQUEST: &str = "turn on the kitchen lights";

    /// The 20 ms chunks of 16 kHz audio.
    const CHUNK: usize = 320;

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
        SherpaStt::try_new(&config()?.asr, Device::Cpu)?.stream()
    }

    /// The partial after each 20 ms chunk of `speech(REQUEST)`.
    async fn partials() -> anyhow::Result<Vec<Partial>> {
        partials_of(&fakes::speech(REQUEST).await?)
    }

    /// The partial after each 20 ms chunk of `audio`.
    fn partials_of(audio: &Audio) -> anyhow::Result<Vec<Partial>> {
        let mut stream = first_pass()?;
        let chunks = speech::chunks(audio, CHUNK);
        let decode = |chunk: &[i16]| {
            stream.accept(chunk);
            stream.partial()
        };
        Ok(chunks.into_iter().map(decode).collect())
    }

    /// The partial of `speech("lights on")`, then the partial after `reset`.
    async fn reset_partials() -> anyhow::Result<(Partial, Partial)> {
        let audio = fakes::speech("lights on").await?;
        let mut stream = first_pass()?;
        stream.accept(&audio.samples);
        let before = stream.partial();
        stream.reset();
        Ok((before, stream.partial()))
    }

    /// The position in ms of the last voiced sample of `audio`: the last sample above 1% of full
    /// scale. Supertonic adds quiet audio after the speech.
    fn last_voiced_ms(audio: &Audio) -> u64 {
        let voiced = |sample: &i16| sample.unsigned_abs() > u16::MAX / 200;
        let last = audio.samples.iter().rposition(voiced).unwrap_or_default();
        last as u64 * 1000 / u64::from(audio.rate)
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
        let (before, after) = reset_partials().await?;
        assert!(!before.text.is_empty(), "the audio gives text: {before:?}");
        assert_eq!(after, Partial::default());
        Ok(())
    }

    /// AC-6.6b-1.
    #[tokio::test]
    async fn ac_6_6b_1_voice_end_after_silence_and_speech() -> anyhow::Result<()> {
        let lights = fakes::speech("lights on").await?;
        let mut samples = vec![0; 8000];
        samples.extend(&lights.samples);
        let audio = Audio { samples, ..lights };
        let partials = partials_of(&audio)?;
        let silence = &partials[..8000 / CHUNK];
        assert!(
            silence.iter().all(|partial| partial.voice_end_ms.is_none()),
            "a partial of the silence has a voice end: {silence:?}"
        );
        let end = partials.last().and_then(|partial| partial.voice_end_ms);
        let voiced = last_voiced_ms(&audio);
        assert!(
            end.is_some_and(|end| end.abs_diff(voiced) < 100),
            "voice end {end:?}, last voiced sample {voiced} ms"
        );
        Ok(())
    }

    /// AC-6.6b-2.
    #[tokio::test]
    async fn ac_6_6b_2_silence_stays_short_until_the_last_voiced_sample() -> anyhow::Result<()> {
        let audio = fakes::speech(REQUEST).await?;
        let early_commit_ms = config()?.asr.early_commit_ms;
        let partials = partials_of(&audio)?;
        let count = usize::try_from(last_voiced_ms(&audio))? / 20;
        for partial in &partials[..count] {
            if let Some(voice_end) = partial.voice_end_ms {
                assert!(partial.vad_ms - voice_end < early_commit_ms, "{partial:?}");
            }
        }
        Ok(())
    }

    /// AC-6.6b-3.
    #[tokio::test]
    async fn ac_6_6b_3_reset_drops_the_voice_end() -> anyhow::Result<()> {
        let (before, after) = reset_partials().await?;
        assert!(
            before.voice_end_ms.is_some(),
            "the audio is voiced: {before:?}"
        );
        assert_eq!(after.voice_end_ms, None);
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

    /// The fp32 Nemotron 160 ms export and its fp16 conversion.
    const NEMOTRON: &str = "models/sherpa-onnx-nemotron-speech-streaming-en-0.6b-160ms-2026-04-25";

    /// The int8 Nemotron 160 ms export.
    const NEMOTRON_INT8: &str =
        "models/sherpa-onnx-nemotron-speech-streaming-en-0.6b-160ms-int8-2026-04-25";

    /// The fp32 Parakeet Unified export and its fp16 conversion.
    const PARAKEET: &str = "models/sherpa-onnx-nemo-parakeet-unified-en-0.6b-non-streaming";

    /// The int8 Parakeet Unified export.
    const PARAKEET_INT8: &str =
        "models/sherpa-onnx-nemo-parakeet-unified-en-0.6b-int8-non-streaming";

    /// The second-pass request.
    const CALENDAR: &str = "add dentist to my calendar for monday";

    /// One model folder, the infix of its file names and its execution provider.
    #[derive(Clone, Copy)]
    struct Export {
        /// The model folder.
        dir: &'static str,
        /// `.fp16`, `.int8` or empty.
        infix: &'static str,
        /// `cuda` or `cpu`.
        provider: &'static str,
    }

    /// Builds an `Export`.
    const fn export(dir: &'static str, infix: &'static str, provider: &'static str) -> Export {
        Export {
            dir,
            infix,
            provider,
        }
    }

    /// The int8 Nemotron on the CPU.
    const NEMOTRON_INT8_CPU: Export = export(NEMOTRON_INT8, ".int8", "cpu");

    /// The fp16 Nemotron on CUDA.
    const NEMOTRON_FP16: Export = export(NEMOTRON, ".fp16", "cuda");

    /// The fp32 Nemotron on CUDA.
    const NEMOTRON_FP32: Export = export(NEMOTRON, "", "cuda");

    /// The int8 Parakeet on the CPU.
    const PARAKEET_INT8_CPU: Export = export(PARAKEET_INT8, ".int8", "cpu");

    /// The fp16 Parakeet on CUDA.
    const PARAKEET_FP16: Export = export(PARAKEET, ".fp16", "cuda");

    /// The fp32 Parakeet on CUDA.
    const PARAKEET_FP32: Export = export(PARAKEET, "", "cuda");

    impl Export {
        /// The path of the model file `name` of the export.
        fn file(self, name: &str) -> String {
            format!("{}/{name}{}.onnx", self.dir, self.infix)
        }

        /// The path of `tokens.txt`.
        fn tokens(self) -> String {
            format!("{}/tokens.txt", self.dir)
        }

        /// The name of the export in the printed lines.
        fn name(self) -> String {
            let dir = self.dir.trim_start_matches("models/sherpa-onnx-");
            format!("{dir}{}/{}", self.infix, self.provider)
        }
    }

    /// The streaming recognizer of `export` with 4 threads on `provider`.
    fn online(export: Export, provider: &str) -> anyhow::Result<OnlineRecognizer> {
        let transducer = OnlineTransducerModelConfig {
            encoder: Some(export.file("encoder")),
            decoder: Some(export.file("decoder")),
            joiner: Some(export.file("joiner")),
        };
        let model_config = OnlineModelConfig {
            transducer,
            tokens: Some(export.tokens()),
            provider: Some(provider.to_owned()),
            num_threads: 4,
            ..OnlineModelConfig::default()
        };
        let config = OnlineRecognizerConfig {
            model_config,
            ..OnlineRecognizerConfig::default()
        };
        OnlineRecognizer::create(&config).with_context(|| format!("cannot load {}", export.name()))
    }

    /// The offline recognizer of `export` with 4 threads on `provider`.
    fn offline(export: Export, provider: &str) -> anyhow::Result<OfflineRecognizer> {
        let transducer = OfflineTransducerModelConfig {
            encoder: Some(export.file("encoder")),
            decoder: Some(export.file("decoder")),
            joiner: Some(export.file("joiner")),
        };
        let model_config = OfflineModelConfig {
            transducer,
            tokens: Some(export.tokens()),
            provider: Some(provider.to_owned()),
            num_threads: 4,
            model_type: Some("nemo_transducer".to_owned()),
            ..OfflineModelConfig::default()
        };
        let config = OfflineRecognizerConfig {
            model_config,
            ..OfflineRecognizerConfig::default()
        };
        OfflineRecognizer::create(&config).with_context(|| format!("cannot load {}", export.name()))
    }

    /// The samples in [-1, 1] of the 16-bit `samples`.
    fn wave(samples: &[i16]) -> Vec<f32> {
        samples
            .iter()
            .map(|&sample| f32::from(sample) / f32::from(i16::MAX))
            .collect()
    }

    /// Decodes each ready chunk of `stream` and gives the time of each decode.
    fn decode_ready(recognizer: &OnlineRecognizer, stream: &OnlineStream) -> Vec<Duration> {
        let mut decodes = Vec::new();
        while recognizer.is_ready(stream) {
            let start = Instant::now();
            recognizer.decode(stream);
            decodes.push(start.elapsed());
        }
        decodes
    }

    /// The partial text of `stream`.
    fn text_of(recognizer: &OnlineRecognizer, stream: &OnlineStream) -> String {
        recognizer
            .get_result(stream)
            .map(|result| result.text)
            .unwrap_or_default()
    }

    /// One first pass of `samples` in 20 ms chunks, then 640 ms of zeros.
    struct Pass {
        /// The time of each decode of the chunks.
        decodes: Vec<Duration>,
        /// The time of all chunks.
        total: Duration,
        /// The time to decode the zeros.
        flush: Duration,
        /// The text before the zeros.
        text: String,
        /// The text after the zeros.
        flushed: String,
    }

    /// The first pass of `samples` with `recognizer`.
    fn run_first(recognizer: &OnlineRecognizer, samples: &[i16]) -> Pass {
        let stream = recognizer.create_stream();
        let mut decodes = Vec::new();
        let start = Instant::now();
        for chunk in samples.chunks(CHUNK) {
            stream.accept_waveform(i32::from(RATE), &wave(chunk));
            decodes.extend(decode_ready(recognizer, &stream));
        }
        let total = start.elapsed();
        let text = text_of(recognizer, &stream);
        let start = Instant::now();
        stream.accept_waveform(i32::from(RATE), &vec![0.0; 640 * 16]);
        decode_ready(recognizer, &stream);
        let flush = start.elapsed();
        let flushed = text_of(recognizer, &stream);
        Pass {
            decodes,
            total,
            flush,
            text,
            flushed,
        }
    }

    /// The text of `samples` with `recognizer`.
    fn transcribe(recognizer: &OfflineRecognizer, samples: &[i16]) -> anyhow::Result<String> {
        let stream = recognizer.create_stream();
        stream.accept_waveform(i32::from(RATE), &wave(samples));
        recognizer.decode(&stream);
        Ok(stream.get_result().context("no result")?.text)
    }

    /// The milliseconds of `duration`.
    fn ms(duration: Duration) -> f64 {
        duration.as_secs_f64() * 1000.0
    }

    /// The GPU memory in MiB that `nvidia-smi` reports for this process.
    fn gpu_memory() -> anyhow::Result<u64> {
        let output = std::process::Command::new("nvidia-smi")
            .args([
                "--query-compute-apps=pid,used_memory",
                "--format=csv,noheader,nounits",
            ])
            .output()?;
        let pid = std::process::id().to_string();
        let mut total = 0;
        for line in String::from_utf8(output.stdout)?.lines() {
            if let Some((process, memory)) = line.split_once(',')
                && process.trim() == pid
            {
                total += memory.trim().parse::<u64>()?;
            }
        }
        Ok(total)
    }

    /// Creates the CUDA context with the Silero VAD and gives it with the GPU memory before and
    /// after.
    fn cuda_context() -> anyhow::Result<(VoiceActivityDetector, u64, u64)> {
        let before = gpu_memory()?;
        let detector = VoiceActivityDetector::create(&vad(&config()?.asr, Device::Cuda), 30.0)
            .context("cannot load the VAD")?;
        detector.accept_waveform(&[0.0; 512]);
        Ok((detector, before, gpu_memory()?))
    }

    /// True in the child process that runs the ignored test `name` alone. Else runs that child.
    fn alone(name: &str) -> anyhow::Result<bool> {
        let alone = "JARVIS_ALONE";
        if std::env::var_os(alone).is_some() {
            return Ok(true);
        }
        let name = format!("adapters::sherpa::tests::{name}");
        let status = std::process::Command::new(std::env::current_exe()?)
            .args(["--ignored", "--exact", &name, "--nocapture"])
            .env(alone, "1")
            .status()?;
        anyhow::ensure!(status.success(), "{name} fails alone: {status}");
        Ok(false)
    }

    /// Adds the time of `event` to `times` when it is a kernel event.
    fn add_kernel_time(times: &mut BTreeMap<(String, String), u64>, event: &serde_json::Value) {
        let args = &event["args"];
        let kernel = event["name"]
            .as_str()
            .is_some_and(|name| name.ends_with("_kernel_time"));
        if let (true, Some(provider), Some(op), Some(time)) = (
            kernel,
            args["provider"].as_str(),
            args["op_name"].as_str(),
            event["dur"].as_u64(),
        ) {
            *times
                .entry((provider.to_owned(), op.to_owned()))
                .or_default() += time;
        }
    }

    /// The kernel time in µs of each execution provider and operator in the profiles in
    /// `folder`.
    fn kernel_times(folder: &Path) -> anyhow::Result<BTreeMap<(String, String), u64>> {
        let mut times = BTreeMap::new();
        for entry in std::fs::read_dir(folder)? {
            let path = entry?.path();
            if path
                .extension()
                .is_some_and(|extension| extension == "json")
            {
                let events: Vec<serde_json::Value> =
                    serde_json::from_str(&std::fs::read_to_string(&path)?)?;
                for event in &events {
                    add_kernel_time(&mut times, event);
                }
            }
        }
        Ok(times)
    }

    /// Prints the kernel time of each provider, the slowest operators and the slowest CPU
    /// operators of `times`.
    fn print_times(name: &str, times: &BTreeMap<(String, String), u64>) {
        let mut providers: BTreeMap<&str, u64> = BTreeMap::new();
        for ((provider, _), time) in times {
            *providers.entry(provider.as_str()).or_default() += time;
        }
        let mut slowest: Vec<(&(String, String), &u64)> = times.iter().collect();
        slowest.sort_by_key(|(_, time)| std::cmp::Reverse(**time));
        let cpu: Vec<&(&(String, String), &u64)> = slowest
            .iter()
            .filter(|((provider, _), _)| provider.starts_with("CPU"))
            .take(10)
            .collect();
        println!("DIAGNOSTIC placement {name} kernel_us={providers:?}");
        println!("DIAGNOSTIC placement {name} cpu_us={cpu:?}");
        println!(
            "DIAGNOSTIC placement {name} slowest_us={:?}",
            &slowest[..slowest.len().min(12)]
        );
    }

    /// Runs `run` with a profiling CUDA provider and prints where the kernels run.
    fn print_placement(
        name: &str,
        run: impl FnOnce(&str) -> anyhow::Result<()>,
    ) -> anyhow::Result<()> {
        let folder = tempfile::tempdir()?;
        let config = folder.path().join("ort.config");
        let prefix = folder.path().join("profile");
        std::fs::write(
            &config,
            format!("ProfilingFilePrefix={}\n", prefix.display()),
        )?;
        run(&format!("cuda:{}", config.display()))?;
        print_times(name, &kernel_times(folder.path())?);
        Ok(())
    }

    /// Prints the GPU memory in MiB at the start, after the CUDA context, after the load and
    /// after the runs.
    fn print_model_memory(name: &str, [start, context, loaded, used]: [u64; 4]) {
        println!(
            "DIAGNOSTIC memory {name} start={start} context={context} loaded={loaded} used={used} model_mib={} run_mib={}",
            loaded.saturating_sub(context),
            used.saturating_sub(context)
        );
    }

    /// Prints the texts, the speed and the flush times of `pass` over `audio`, and of a pass of
    /// `audio` cut 132 ms after the last voiced sample, as at the VAD end.
    fn print_first(
        name: &str,
        recognizer: &OnlineRecognizer,
        pass: &Pass,
        audio: &Audio,
    ) -> anyhow::Result<()> {
        let voiced = usize::try_from(last_voiced_ms(audio) + 132)? * 16;
        let cut = run_first(recognizer, &audio.samples[..voiced]);
        let rtf = pass.total.as_secs_f64() / speech::duration(audio).as_secs_f64();
        let total: Duration = pass.decodes.iter().sum();
        let count = u32::try_from(pass.decodes.len()).unwrap_or(u32::MAX).max(1);
        let max = pass.decodes.iter().max().copied().unwrap_or_default();
        println!(
            "DIAGNOSTIC first {name} text={:?} flushed={:?} rtf={rtf:.3} decodes={count} decode_mean_ms={:.1} decode_max_ms={:.1} flush_640_ms={:.1} cut_flush_ms={:.1} cut_flushed={:?}",
            pass.text,
            pass.flushed,
            ms(total / count),
            ms(max),
            ms(pass.flush),
            ms(cut.flush),
            cut.flushed
        );
        Ok(())
    }

    /// Prints the text, the speed, the flush time, the GPU memory and the placement of the
    /// first pass `export`.
    async fn measure_first(export: Export) -> anyhow::Result<()> {
        let audio = fakes::speech(REQUEST).await?;
        let (_context, start, context) = cuda_context()?;
        let recognizer = online(export, export.provider)?;
        let loaded = gpu_memory()?;
        run_first(&recognizer, &audio.samples);
        let pass = run_first(&recognizer, &audio.samples);
        let used = gpu_memory()?;
        let name = export.name();
        print_first(&name, &recognizer, &pass, &audio)?;
        print_model_memory(&name, [start, context, loaded, used]);
        drop(recognizer);
        print_placement(&name, |provider| {
            run_first(&online(export, provider)?, &audio.samples);
            Ok(())
        })
    }

    /// The sorted times in ms of 20 second passes of `samples` after a warm-up.
    fn second_times(recognizer: &OfflineRecognizer, samples: &[i16]) -> anyhow::Result<Vec<f64>> {
        transcribe(recognizer, samples)?;
        let mut times = Vec::new();
        for _ in 0..20 {
            let start = Instant::now();
            transcribe(recognizer, samples)?;
            times.push(ms(start.elapsed()));
        }
        times.sort_by(f64::total_cmp);
        Ok(times)
    }

    /// Loads the second pass `export` after the CUDA context, and gives the context, the
    /// recognizer and the GPU memory at the start, after the context and after the load.
    fn load_second(
        export: Export,
    ) -> anyhow::Result<(VoiceActivityDetector, OfflineRecognizer, [u64; 3])> {
        let (detector, start, context) = cuda_context()?;
        let recognizer = offline(export, export.provider)?;
        Ok((detector, recognizer, [start, context, gpu_memory()?]))
    }

    /// Prints the second-pass `text` and the sorted `times` of 20 runs.
    fn print_second(name: &str, text: &str, times: &[f64]) {
        println!(
            "DIAGNOSTIC second {name} text={text:?} normalized_ok={} p95_ms={:.1} median_ms={:.1} min_ms={:.1} times={times:.0?}",
            intent::normalize(text) == CALENDAR,
            times[18],
            times[10],
            times[0]
        );
    }

    /// Prints the text, the p95 of a 3 s utterance, the GPU memory and the placement of the
    /// second pass `export`.
    async fn measure_second(export: Export) -> anyhow::Result<()> {
        let audio = fakes::speech(CALENDAR).await?;
        let (_context, recognizer, [start, context, loaded]) = load_second(export)?;
        let text = transcribe(&recognizer, &audio.samples)?;
        let mut samples = audio.samples.clone();
        samples.resize(3 * usize::from(RATE), 0);
        let times = second_times(&recognizer, &samples)?;
        let used = gpu_memory()?;
        let name = export.name();
        print_second(&name, &text, &times);
        print_model_memory(&name, [start, context, loaded, used]);
        drop(recognizer);
        let second = |provider: &str| offline(export, provider);
        print_placement(&name, |provider| {
            transcribe(&second(provider)?, &samples).map(drop)
        })
    }

    /// Diagnostic: the fp16 Nemotron first pass on CUDA.
    #[tokio::test]
    #[ignore = "GPU host"]
    async fn diag_a_nemotron_fp16() -> anyhow::Result<()> {
        if alone("diag_a_nemotron_fp16")? {
            measure_first(NEMOTRON_FP16).await?;
        }
        Ok(())
    }

    /// Diagnostic: the fp32 Nemotron first pass on CUDA.
    #[tokio::test]
    #[ignore = "GPU host"]
    async fn diag_b_nemotron_fp32() -> anyhow::Result<()> {
        if alone("diag_b_nemotron_fp32")? {
            measure_first(NEMOTRON_FP32).await?;
        }
        Ok(())
    }

    /// Diagnostic: the fp16 Parakeet second pass on CUDA.
    #[tokio::test]
    #[ignore = "GPU host"]
    async fn diag_c_parakeet_fp16() -> anyhow::Result<()> {
        if alone("diag_c_parakeet_fp16")? {
            measure_second(PARAKEET_FP16).await?;
        }
        Ok(())
    }

    /// Diagnostic: the fp32 Parakeet second pass on CUDA.
    #[tokio::test]
    #[ignore = "GPU host"]
    async fn diag_d_parakeet_fp32() -> anyhow::Result<()> {
        if alone("diag_d_parakeet_fp32")? {
            measure_second(PARAKEET_FP32).await?;
        }
        Ok(())
    }

    /// Prints the GPU memory of this process and of the whole GPU after `step`, and adds the
    /// process memory to `peak`.
    fn print_memory(step: &str, peak: &mut Vec<u64>) -> anyhow::Result<()> {
        let output = std::process::Command::new("nvidia-smi")
            .args([
                "--query-gpu=memory.used,memory.total",
                "--format=csv,noheader",
            ])
            .output()?;
        let process = gpu_memory()?;
        let gpu = String::from_utf8(output.stdout)?;
        println!(
            "DIAGNOSTIC peak {step} process_mib={process} gpu={}",
            gpu.trim()
        );
        peak.push(process);
        Ok(())
    }

    /// The models of the live process on CUDA.
    struct Live {
        /// The fp16 first pass.
        first: OnlineRecognizer,
        /// The fp16 second pass.
        second: OfflineRecognizer,
        /// The System One classifier.
        classifier: Arc<dyn crate::ports::Classifier>,
        /// The TTS.
        tts: SherpaTts,
    }

    /// Loads the models of the live process on CUDA and records the GPU memory after each.
    async fn load_live(peak: &mut Vec<u64>) -> anyhow::Result<Live> {
        use crate::ports::ModelLoader as _;
        let first = online(NEMOTRON_FP16, "cuda")?;
        print_memory("nemotron_fp16", peak)?;
        let second = offline(PARAKEET_FP16, "cuda")?;
        print_memory("parakeet_fp16", peak)?;
        let fixture = Path::new("models/system-one/fixture");
        let classifier = crate::adapters::onnx::OnnxLoader
            .load(fixture, Device::Cuda)
            .await?;
        print_memory("classifier", peak)?;
        let tts = SherpaTts::try_new(&config()?.tts, Device::Cuda)?;
        print_memory("tts", peak)?;
        Ok(Live {
            first,
            second,
            classifier,
            tts,
        })
    }

    /// Runs each model of `live` once and records the GPU memory after each.
    async fn run_live(
        live: &Live,
        request: &[i16],
        samples: &[i16],
        peak: &mut Vec<u64>,
    ) -> anyhow::Result<()> {
        run_first(&live.first, request);
        print_memory("first_pass_run", peak)?;
        transcribe(&live.second, samples)?;
        print_memory("second_pass_run", peak)?;
        live.classifier
            .predict("area: kitchen | habits: none", REQUEST)
            .await?;
        print_memory("classifier_run", peak)?;
        let reply = "The kitchen lights are on, and the calendar has a dentist visit on Monday.";
        live.tts.synth(reply).await?;
        print_memory("tts_run", peak)
    }

    /// Diagnostic: the GPU memory with the fp16 passes, the classifier and the TTS on CUDA.
    #[tokio::test]
    #[ignore = "GPU host"]
    async fn diag_e_peak() -> anyhow::Result<()> {
        if !alone("diag_e_peak")? {
            return Ok(());
        }
        let request = fakes::speech(REQUEST).await?;
        let mut samples = fakes::speech(CALENDAR).await?.samples;
        samples.resize(3 * usize::from(RATE), 0);
        let mut peak = Vec::new();
        print_memory("start", &mut peak)?;
        let live = load_live(&mut peak).await?;
        for _ in 0..3 {
            run_live(&live, &request.samples, &samples, &mut peak).await?;
        }
        println!("DIAGNOSTIC peak max_process_mib={:?}", peak.iter().max());
        Ok(())
    }

    /// The 50 requests of the word error comparison of AC-6.7-3.
    fn requests() -> Vec<String> {
        let actions = [
            ("turn on", "lights"),
            ("turn off", "fan"),
            ("open", "blinds"),
            ("close", "door"),
            ("pause", "speaker"),
        ];
        let areas = [
            "kitchen",
            "office",
            "bedroom",
            "garage",
            "hallway",
            "basement",
            "bathroom",
            "porch",
            "living room",
            "dining room",
        ];
        let mut requests = Vec::new();
        for (action, device) in actions {
            for area in areas {
                requests.push(format!("{action} the {area} {device}"));
            }
        }
        requests
    }

    /// The word-level edit distance from `reference` to `hypothesis`.
    fn word_errors(reference: &str, hypothesis: &str) -> usize {
        let hypothesis: Vec<&str> = hypothesis.split_whitespace().collect();
        let mut row: Vec<usize> = (0..=hypothesis.len()).collect();
        for (index, word) in reference.split_whitespace().enumerate() {
            let mut diagonal = row[0];
            row[0] = index + 1;
            for (column, other) in hypothesis.iter().enumerate() {
                let substitution = diagonal + usize::from(word != *other);
                diagonal = row[column + 1];
                row[column + 1] = substitution.min(row[column] + 1).min(diagonal + 1);
            }
        }
        row[hypothesis.len()]
    }

    /// Prints the word errors of `export` as the first pass over `audios` of `requests`, without
    /// and with the zeros.
    fn first_errors(export: Export, requests: &[String], audios: &[Audio]) -> anyhow::Result<()> {
        let recognizer = online(export, export.provider)?;
        let (mut plain, mut flushed) = (0, 0);
        for (request, audio) in requests.iter().zip(audios) {
            let pass = run_first(&recognizer, &audio.samples);
            let (text, after) = (
                intent::normalize(&pass.text),
                intent::normalize(&pass.flushed),
            );
            plain += word_errors(request, &text);
            flushed += word_errors(request, &after);
            if &after != request || &text != request {
                println!(
                    "DIAGNOSTIC miss {} {request:?} text={text:?} flushed={after:?}",
                    export.name()
                );
            }
        }
        println!(
            "DIAGNOSTIC errors first {} plain={plain} flushed={flushed}",
            export.name()
        );
        Ok(())
    }

    /// Prints the word errors of `export` as the second pass over `audios` of `requests`.
    fn second_errors(export: Export, requests: &[String], audios: &[Audio]) -> anyhow::Result<()> {
        let recognizer = offline(export, export.provider)?;
        let mut errors = 0;
        for (request, audio) in requests.iter().zip(audios) {
            let text = intent::normalize(&transcribe(&recognizer, &audio.samples)?);
            errors += word_errors(request, &text);
            if &text != request {
                println!(
                    "DIAGNOSTIC miss {} {request:?} text={text:?}",
                    export.name()
                );
            }
        }
        println!("DIAGNOSTIC errors second {} errors={errors}", export.name());
        Ok(())
    }

    /// The speech of each of `requests`.
    async fn speeches(requests: &[String]) -> anyhow::Result<Vec<Audio>> {
        let mut audios = Vec::new();
        for request in requests {
            audios.push(fakes::speech(request).await?);
        }
        Ok(audios)
    }

    /// Diagnostic: the word errors of each export over the 50 requests of AC-6.7-3.
    #[tokio::test]
    #[ignore = "GPU host"]
    async fn diag_f_word_errors() -> anyhow::Result<()> {
        if !alone("diag_f_word_errors")? {
            return Ok(());
        }
        let requests = requests();
        let audios = speeches(&requests).await?;
        let words: usize = requests
            .iter()
            .map(|request| request.split_whitespace().count())
            .sum();
        println!("DIAGNOSTIC errors words={words}");
        for export in [NEMOTRON_INT8_CPU, NEMOTRON_FP16, NEMOTRON_FP32] {
            first_errors(export, &requests, &audios)?;
        }
        for export in [PARAKEET_INT8_CPU, PARAKEET_FP16, PARAKEET_FP32] {
            second_errors(export, &requests, &audios)?;
        }
        Ok(())
    }
}
