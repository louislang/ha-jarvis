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
        window_size: i32::from(WINDOW),
        // An utterance commits at `max_utterance_ms`, so the VAD never splits a segment.
        max_speech_duration: seconds(config.max_utterance_ms),
        ..SileroVadModelConfig::default()
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
    /// Updates the VAD positions after `count` more samples.
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
        self.vad.accept_waveform(&wave);
        self.track(samples.len());
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
    use std::sync::Mutex;
    use std::thread;
    use std::time::Instant;

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

    /// The `SherpaStt` of `jarvis.toml` on `device`.
    fn stt(device: Device) -> anyhow::Result<SherpaStt> {
        SherpaStt::try_new(&config()?.asr, device)
    }

    /// The 20 ms chunks of 16 kHz audio.
    const CHUNK: usize = 320;

    /// Serializes the timing tests, so that they do not share the CPU.
    static TIMING: Mutex<()> = Mutex::new(());

    /// The partial after each 20 ms chunk of `audio` in a new first pass of `stt`.
    fn partials(stt: &SherpaStt, audio: &Audio) -> anyhow::Result<Vec<Partial>> {
        let mut stream = stt.stream()?;
        let chunks = speech::chunks(audio, CHUNK);
        let decode = |chunk: &[i16]| {
            stream.accept(chunk);
            stream.partial()
        };
        Ok(chunks.into_iter().map(decode).collect())
    }

    /// The normalized text of the last of `partials`.
    fn final_text(partials: &[Partial]) -> Option<String> {
        partials
            .last()
            .map(|partial| intent::normalize(&partial.text))
    }

    /// The partials of `speech(REQUEST)` on the CPU.
    async fn request_partials() -> anyhow::Result<Vec<Partial>> {
        partials(&stt(Device::Cpu)?, &fakes::speech(REQUEST).await?)
    }

    /// The partial of `speech("lights on")` on the CPU, then the partial after `reset`.
    async fn reset_partials() -> anyhow::Result<(Partial, Partial)> {
        let audio = fakes::speech("lights on").await?;
        let mut stream = stt(Device::Cpu)?.stream()?;
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

    /// The time of the first pass of `speech(REQUEST)` on `device` after a warm-up, divided by
    /// the audio time.
    async fn real_time_factor(device: Device) -> anyhow::Result<f64> {
        let (stt, audio) = (stt(device)?, fakes::speech(REQUEST).await?);
        let _only = TIMING.lock().expect("no timing test panicked");
        partials(&stt, &audio)?;
        let start = Instant::now();
        partials(&stt, &audio)?;
        Ok(start.elapsed().as_secs_f64() / speech::duration(&audio).as_secs_f64())
    }

    /// The time from the last voiced sample of `audio` to the first partial with `REQUEST` on
    /// CUDA, with the chunks in real time after a warm-up.
    fn last_word_latency(audio: &Audio) -> anyhow::Result<Duration> {
        let stt = stt(Device::Cuda)?;
        let _only = TIMING.lock().expect("no timing test panicked");
        partials(&stt, audio)?;
        let mut stream = stt.stream()?;
        let start = Instant::now();
        let voiced = start + Duration::from_millis(last_voiced_ms(audio));
        let chunks = speech::chunks(audio, CHUNK);
        for (index, chunk) in chunks.into_iter().enumerate() {
            let arrival = start + Duration::from_millis(20) * u32::try_from(index + 1)?;
            thread::sleep(arrival.saturating_duration_since(Instant::now()));
            stream.accept(chunk);
            if intent::normalize(&stream.partial().text) == REQUEST {
                return Ok(Instant::now().saturating_duration_since(voiced));
            }
        }
        anyhow::bail!("no partial has the last word")
    }

    /// AC-6.6-1.
    #[tokio::test]
    async fn ac_6_6_1_final_partial_is_the_request() -> anyhow::Result<()> {
        let partials = request_partials().await?;
        let text = final_text(&partials);
        assert_eq!(text.as_deref(), Some(REQUEST), "{partials:?}");
        Ok(())
    }

    /// AC-6.6-2.
    #[tokio::test]
    async fn ac_6_6_2_partial_has_text_before_the_last_chunk() -> anyhow::Result<()> {
        let partials = request_partials().await?;
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
        let partials = partials(&stt(Device::Cpu)?, &audio)?;
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
        let partials = partials(&stt(Device::Cpu)?, &audio)?;
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

    /// AC-6.6b-4.
    #[tokio::test]
    #[ignore = "GPU host"]
    async fn ac_6_6b_4_cuda_final_partial_is_the_request() -> anyhow::Result<()> {
        let partials = partials(&stt(Device::Cuda)?, &fakes::speech(REQUEST).await?)?;
        let text = final_text(&partials);
        assert_eq!(text.as_deref(), Some(REQUEST), "{partials:?}");
        Ok(())
    }

    /// AC-6.6b-5.
    #[tokio::test]
    #[ignore = "GPU host"]
    async fn ac_6_6b_5_cuda_last_word_latency() -> anyhow::Result<()> {
        let latency = last_word_latency(&fakes::speech(REQUEST).await?)?;
        assert!(latency < Duration::from_millis(200), "latency {latency:?}");
        Ok(())
    }

    /// AC-6.6b-6.
    #[tokio::test]
    #[ignore = "GPU host"]
    async fn ac_6_6b_6_cuda_real_time_factor() -> anyhow::Result<()> {
        let factor = real_time_factor(Device::Cuda).await?;
        assert!(factor < 0.1, "real-time factor {factor}");
        Ok(())
    }

    /// AC-6.6b-8.
    #[tokio::test]
    #[ignore = "GPU host"]
    async fn ac_6_6b_8_cpu_real_time_factor() -> anyhow::Result<()> {
        let factor = real_time_factor(Device::Cpu).await?;
        assert!(factor < 0.5, "real-time factor {factor}");
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
