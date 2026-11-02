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
    use std::fs;
    use std::thread;
    use std::time::Instant;

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

    /// The Nemotron int8 export with 160 ms chunks.
    const INT8_160: &str =
        "models/sherpa-onnx-nemotron-speech-streaming-en-0.6b-160ms-int8-2026-04-25";

    /// The Nemotron int8 export with 80 ms chunks.
    const INT8_80: &str =
        "models/sherpa-onnx-nemotron-speech-streaming-en-0.6b-80ms-int8-2026-04-25";

    /// The Nemotron float32 export with 160 ms chunks.
    const FP32_160: &str = "models/sherpa-onnx-nemotron-speech-streaming-en-0.6b-160ms-2026-04-25";

    /// The Nemotron float32 export with 80 ms chunks.
    const FP32_80: &str = "models/sherpa-onnx-nemotron-speech-streaming-en-0.6b-80ms-2026-04-25";

    /// One first-pass model, provider and thread count of the GPU host measurements.
    #[derive(Clone)]
    struct Setup {
        /// The model folder.
        dir: &'static str,
        /// The infix of the model file names: `.int8` or empty.
        infix: &'static str,
        /// The execution provider, with an optional `:config` file.
        provider: String,
        /// The number of threads.
        threads: i32,
    }

    impl Setup {
        /// The setup of `dir` with the file infix `infix` on `provider` with `threads` threads.
        fn new(dir: &'static str, infix: &'static str, provider: &str, threads: i32) -> Self {
            let provider = provider.to_owned();
            Self {
                dir,
                infix,
                provider,
                threads,
            }
        }

        /// The recognizer of the setup.
        fn recognizer(&self) -> anyhow::Result<OnlineRecognizer> {
            let file = |name: &str| Some(format!("{}/{name}{}.onnx", self.dir, self.infix));
            let transducer = OnlineTransducerModelConfig {
                encoder: file("encoder"),
                decoder: file("decoder"),
                joiner: file("joiner"),
            };
            let model_config = OnlineModelConfig {
                transducer,
                tokens: Some(format!("{}/tokens.txt", self.dir)),
                provider: Some(self.provider.clone()),
                num_threads: self.threads,
                ..OnlineModelConfig::default()
            };
            let config = OnlineRecognizerConfig {
                model_config,
                ..OnlineRecognizerConfig::default()
            };
            OnlineRecognizer::create(&config).with_context(|| format!("cannot load {}", self.dir))
        }

        /// The name of the setup in the printed lines.
        fn name(&self) -> String {
            let model = self
                .dir
                .trim_start_matches("models/sherpa-onnx-nemotron-speech-streaming-en-0.6b-");
            let provider = self.provider.split(':').next().unwrap_or_default();
            format!("{model}/{provider}/threads={}", self.threads)
        }
    }

    /// The measurements of one pass of the request audio in 20 ms chunks.
    #[derive(Default)]
    struct Pass {
        /// The time of each `decode` call.
        decodes: Vec<Duration>,
        /// The time of the whole pass.
        total: Duration,
        /// The audio end in ms and the pass time of the first chunk whose partial is `REQUEST`.
        complete: Option<(u64, Duration)>,
        /// Each new partial text with the audio end in ms of its chunk.
        changes: Vec<(u64, String)>,
        /// The final tokens with their times in s.
        tokens: String,
    }

    impl Pass {
        /// Records the partial `text` after the chunk that ends at `end_ms`, `elapsed` into the
        /// pass.
        fn note(&mut self, end_ms: u64, text: String, elapsed: Duration) {
            let last = self.changes.last().map_or("", |(_, last)| last.as_str());
            if last == text {
                return;
            }
            if self.complete.is_none() && intent::normalize(&text) == REQUEST {
                self.complete = Some((end_ms, elapsed));
            }
            self.changes.push((end_ms, text));
        }
    }

    /// The float samples of `samples`.
    fn wave(samples: &[i16]) -> Vec<f32> {
        samples
            .iter()
            .map(|&sample| f32::from(sample) / f32::from(i16::MAX))
            .collect()
    }

    /// Feeds `samples` to `stream`, decodes each ready chunk and gives the time of each decode.
    fn feed(
        recognizer: &OnlineRecognizer,
        stream: &OnlineStream,
        samples: &[f32],
    ) -> Vec<Duration> {
        if !samples.is_empty() {
            stream.accept_waveform(i32::from(RATE), samples);
        }
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

    /// The tokens of `stream` with their times in s.
    fn tokens_of(recognizer: &OnlineRecognizer, stream: &OnlineStream) -> String {
        let Some(result) = recognizer.get_result(stream) else {
            return String::new();
        };
        let times = result.timestamps.unwrap_or_default();
        let pairs: Vec<String> = result
            .tokens
            .iter()
            .zip(times)
            .map(|(token, time)| format!("{token}@{time:.2}"))
            .collect();
        pairs.join(" ")
    }

    /// One pass of `audio` in 20 ms chunks: in real time when `paced`, else as fast as possible.
    fn pass(recognizer: &OnlineRecognizer, audio: &Audio, paced: bool) -> anyhow::Result<Pass> {
        let stream = recognizer.create_stream();
        let mut pass = Pass::default();
        let start = Instant::now();
        let chunks = speech::chunks(audio, CHUNK);
        for (index, chunk) in chunks.into_iter().enumerate() {
            let end_ms = u64::try_from(index + 1)? * 20;
            if paced {
                let arrival = start + Duration::from_millis(end_ms);
                thread::sleep(arrival.saturating_duration_since(Instant::now()));
            }
            pass.decodes.extend(feed(recognizer, &stream, &wave(chunk)));
            pass.note(end_ms, text_of(recognizer, &stream), start.elapsed());
        }
        pass.total = start.elapsed();
        pass.tokens = tokens_of(recognizer, &stream);
        Ok(pass)
    }

    /// The milliseconds of `duration`.
    fn ms(duration: Duration) -> f64 {
        duration.as_secs_f64() * 1000.0
    }

    /// The mean and the largest of `decodes` in ms.
    fn stats(decodes: &[Duration]) -> (f64, f64) {
        let total: Duration = decodes.iter().sum();
        let count = u32::try_from(decodes.len()).unwrap_or(u32::MAX).max(1);
        let max = decodes.iter().max().copied().unwrap_or_default();
        (ms(total / count), ms(max))
    }

    /// Feeds `audio` up to `cut_ms` in 20 ms chunks, then `pad_ms` of zeros at once, then
    /// `input_finished` when `finished`. Gives the time after the cut and the partial text.
    fn flush(
        recognizer: &OnlineRecognizer,
        audio: &Audio,
        cut_ms: u64,
        pad_ms: u64,
        finished: bool,
    ) -> anyhow::Result<(Duration, String)> {
        let stream = recognizer.create_stream();
        let cut = usize::try_from(cut_ms * 16)?.min(audio.samples.len());
        for chunk in audio.samples[..cut].chunks(CHUNK) {
            feed(recognizer, &stream, &wave(chunk));
        }
        let start = Instant::now();
        let zeros = vec![0.0; usize::try_from(pad_ms * 16)?];
        if !zeros.is_empty() {
            stream.accept_waveform(i32::from(RATE), &zeros);
        }
        if finished {
            stream.input_finished();
        }
        feed(recognizer, &stream, &[]);
        Ok((start.elapsed(), text_of(recognizer, &stream)))
    }

    /// Prints the flush time and text of `recognizer` for each cut after the last voiced
    /// sample, each zero padding and each `input_finished` choice.
    fn print_flushes(
        name: &str,
        recognizer: &OnlineRecognizer,
        audio: &Audio,
    ) -> anyhow::Result<()> {
        let voiced = last_voiced_ms(audio);
        for after in [0, 132] {
            for pad_ms in [0, 160, 320, 480, 640] {
                for finished in [false, true] {
                    let (time, text) = flush(recognizer, audio, voiced + after, pad_ms, finished)?;
                    let complete = intent::normalize(&text) == REQUEST;
                    println!(
                        "MEASURE flush {name} cut=voiced+{after}ms pad={pad_ms}ms finished={finished} complete={complete} flush_ms={:.1} text={text:?}",
                        ms(time)
                    );
                }
            }
        }
        Ok(())
    }

    /// Prints the real-time factor and the decode times of `fast`, after the warm-up `warm`.
    fn print_speed(name: &str, warm: &Pass, fast: &Pass, audio: &Audio) {
        let seconds = speech::duration(audio).as_secs_f64();
        let factor = fast.total.as_secs_f64() / seconds;
        let warm_factor = warm.total.as_secs_f64() / seconds;
        let (mean, max) = stats(&fast.decodes);
        let count = fast.decodes.len();
        println!(
            "MEASURE speed {name} rtf={factor:.3} warmup_rtf={warm_factor:.3} decodes={count} decode_mean_ms={mean:.1} decode_max_ms={max:.1}"
        );
    }

    /// Prints when the full request is in the partial of the fast pass (audio time) and of the
    /// real-time pass (wall time after the last voiced sample).
    fn print_latency(name: &str, fast: &Pass, paced: &Pass, audio: &Audio) {
        let voiced = last_voiced_ms(audio);
        let audio_ms = speech::duration(audio).as_millis();
        let complete = fast.complete.map(|(end_ms, _)| end_ms);
        let paced_audio = paced.complete.map(|(end_ms, _)| end_ms);
        let voiced_time = Duration::from_millis(voiced);
        let wall = paced
            .complete
            .map(|(_, elapsed)| ms(elapsed) - ms(voiced_time));
        println!(
            "MEASURE latency {name} last_voiced_ms={voiced} audio_ms={audio_ms} complete_audio_ms={complete:?} paced_complete_audio_ms={paced_audio:?} paced_latency_ms={wall:?}"
        );
        println!("MEASURE partials {name} {:?}", fast.changes);
        println!("MEASURE tokens {name} {}", fast.tokens);
    }

    /// Prints the load time, the speed, the latency and the flush times of `setup`.
    fn measure(setup: &Setup, audio: &Audio) -> anyhow::Result<()> {
        let name = setup.name();
        let start = Instant::now();
        let recognizer = match setup.recognizer() {
            Ok(recognizer) => recognizer,
            Err(error) => {
                println!("MEASURE skip {name}: {error:#}");
                return Ok(());
            }
        };
        println!("MEASURE load {name} load_ms={:.0}", ms(start.elapsed()));
        let warm = pass(&recognizer, audio, false)?;
        let fast = pass(&recognizer, audio, false)?;
        let paced = pass(&recognizer, audio, true)?;
        print_speed(&name, &warm, &fast, audio);
        print_latency(&name, &fast, &paced, audio);
        print_flushes(&name, &recognizer, audio)
    }

    /// The kernel time in µs of each execution provider and operator in the profiles in
    /// `folder`.
    fn kernel_times(folder: &Path) -> anyhow::Result<BTreeMap<(String, String), u64>> {
        let mut times = BTreeMap::new();
        for entry in fs::read_dir(folder)? {
            let path = entry?.path();
            if path.extension().is_none_or(|extension| extension != "json") {
                continue;
            }
            let text = fs::read_to_string(&path)?;
            let Ok(events) = serde_json::from_str::<Vec<serde_json::Value>>(&text) else {
                println!("MEASURE placement unparsable {}", path.display());
                continue;
            };
            for event in &events {
                add_kernel_time(&mut times, event);
            }
        }
        Ok(times)
    }

    /// Adds the time of `event` to `times` when it is a kernel event.
    fn add_kernel_time(times: &mut BTreeMap<(String, String), u64>, event: &serde_json::Value) {
        let kernel = event["name"]
            .as_str()
            .is_some_and(|name| name.ends_with("_kernel_time"));
        let args = &event["args"];
        let (true, Some(provider), Some(op), Some(time)) = (
            kernel,
            args["provider"].as_str(),
            args["op_name"].as_str(),
            event["dur"].as_u64(),
        ) else {
            return;
        };
        *times
            .entry((provider.to_owned(), op.to_owned()))
            .or_default() += time;
    }

    /// Prints the kernel time of each execution provider in `times` and the slowest operators.
    fn print_times(name: &str, times: &BTreeMap<(String, String), u64>) {
        let mut providers: BTreeMap<&str, u64> = BTreeMap::new();
        for ((provider, _), time) in times {
            *providers.entry(provider.as_str()).or_default() += time;
        }
        let mut slowest: Vec<(&(String, String), &u64)> = times.iter().collect();
        slowest.sort_by_key(|(_, time)| std::cmp::Reverse(**time));
        slowest.truncate(15);
        println!("MEASURE placement {name} kernel_us={providers:?}");
        println!("MEASURE placement {name} slowest_us={slowest:?}");
    }

    /// Prints the kernel time of each execution provider and the slowest operators of `setup`
    /// on CUDA, from the ONNX Runtime profile of one pass.
    fn print_placement(setup: &Setup, audio: &Audio) -> anyhow::Result<()> {
        let folder = tempfile::tempdir()?;
        let config = folder.path().join("ort.config");
        let prefix = folder.path().join("profile");
        fs::write(
            &config,
            format!("ProfilingFilePrefix={}\n", prefix.display()),
        )?;
        let profiled = Setup {
            provider: format!("cuda:{}", config.display()),
            ..setup.clone()
        };
        let recognizer = profiled.recognizer()?;
        pass(&recognizer, audio, false)?;
        drop(recognizer);
        let name = setup.name();
        print_times(&name, &kernel_times(folder.path())?);
        Ok(())
    }

    /// The real-time factor of the Silero VAD of `jarvis.toml` alone on `device`, in 20 ms
    /// chunks, after a warm-up.
    fn vad_factor(device: Device, audio: &Audio) -> anyhow::Result<f64> {
        let detector = VoiceActivityDetector::create(&vad(&config()?.asr, device), 30.0)
            .context("cannot load the VAD")?;
        let samples = wave(&audio.samples);
        let mut time = Duration::ZERO;
        for _ in 0..2 {
            detector.reset();
            let start = Instant::now();
            for chunk in samples.chunks(CHUNK) {
                detector.accept_waveform(chunk);
                while detector.front().is_some() {
                    detector.pop();
                }
            }
            time = start.elapsed();
        }
        Ok(time.as_secs_f64() / speech::duration(audio).as_secs_f64())
    }

    /// Prints the real-time factor of the `SherpaStt` of `jarvis.toml` with its VAD, and of the
    /// VAD alone, on `device`.
    fn print_production(device: Device, audio: &Audio) -> anyhow::Result<()> {
        let stt = SherpaStt::try_new(&config()?.asr, device)?;
        let seconds = speech::duration(audio).as_secs_f64();
        let mut factors = Vec::new();
        for _ in 0..2 {
            let mut stream = stt.stream()?;
            let start = Instant::now();
            let mut texts = Vec::new();
            for chunk in speech::chunks(audio, CHUNK) {
                stream.accept(chunk);
                texts.push(stream.partial().text);
            }
            factors.push(start.elapsed().as_secs_f64() / seconds);
        }
        let vad = vad_factor(device, audio)?;
        println!("MEASURE production {device:?} rtf={factors:?} vad_rtf={vad:.3}");
        Ok(())
    }

    /// Measures the `SherpaStt` of `jarvis.toml` and its VAD on the GPU host.
    #[tokio::test]
    #[ignore = "GPU host"]
    async fn measure_a_production() -> anyhow::Result<()> {
        let audio = fakes::speech(REQUEST).await?;
        print_production(Device::Cpu, &audio)?;
        print_production(Device::Cuda, &audio)
    }

    /// The folder, file infix, provider and thread count of each measured setup.
    const SETUPS: [(&str, &str, &str, i32); 13] = [
        (INT8_160, ".int8", "cpu", 4),
        (INT8_160, ".int8", "cpu", 6),
        (INT8_160, ".int8", "cpu", 8),
        (INT8_80, ".int8", "cpu", 4),
        (INT8_80, ".int8", "cpu", 8),
        (INT8_160, ".int8", "cuda", 4),
        (INT8_160, ".int8", "cuda", 8),
        (INT8_80, ".int8", "cuda", 4),
        (FP32_160, "", "cuda", 4),
        (FP32_80, "", "cuda", 4),
        (FP32_160, "", "cpu", 4),
        (FP32_160, "", "cpu", 8),
        (FP32_80, "", "cpu", 8),
    ];

    /// Measures each Nemotron export, provider and thread count on the GPU host.
    #[tokio::test]
    #[ignore = "GPU host"]
    async fn measure_b_exports() -> anyhow::Result<()> {
        let audio = fakes::speech(REQUEST).await?;
        let setups =
            SETUPS.map(|(dir, infix, provider, threads)| Setup::new(dir, infix, provider, threads));
        for setup in &setups {
            measure(setup, &audio)?;
        }
        Ok(())
    }

    /// Prints where the CUDA kernels of each Nemotron export run on the GPU host.
    #[tokio::test]
    #[ignore = "GPU host"]
    async fn measure_c_placement() -> anyhow::Result<()> {
        let audio = fakes::speech(REQUEST).await?;
        for setup in [
            Setup::new(INT8_160, ".int8", "cuda", 4),
            Setup::new(FP32_160, "", "cuda", 4),
        ] {
            print_placement(&setup, &audio)?;
        }
        Ok(())
    }
}
