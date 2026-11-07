//! `SpeechToText` with sherpa-onnx.

use std::ops::RangeInclusive;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Context;
use sherpa_onnx::OfflineModelConfig;
use sherpa_onnx::OfflineRecognizer;
use sherpa_onnx::OfflineRecognizerConfig;
use sherpa_onnx::OfflineTransducerModelConfig;
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
use crate::ports::Partial;
use crate::ports::SpeechToText;
use crate::ports::SttStream;

/// The sample rate of the first pass in Hz.
const RATE: u16 = 16_000;

/// The samples of one Silero VAD frame.
const WINDOW: u16 = 512;

/// The zeros that flush the last word at the end of a VAD speech segment: 640 ms. The model emits
/// the last token about 410 ms of audio after the last voiced sample, and 480 ms is not enough.
const FLUSH: usize = 10_240;

/// The length of a short second-pass input. A shorter input decodes faster, and most commands
/// are shorter than 4 s.
const SHORT_UTTERANCE_MS: u64 = 4000;

/// A `SpeechToText` with a streaming transducer, the Silero VAD and an optional offline
/// transducer.
pub struct SherpaStt {
    /// The streaming recognizer.
    recognizer: Arc<OnlineRecognizer>,
    /// The offline recognizer of the second pass.
    second_pass: Option<OfflineRecognizer>,
    /// The VAD of each stream.
    vad: VadModelConfig,
    /// The silence in samples that ends a VAD speech segment.
    min_silence: u64,
    /// The length of a second-pass input longer than `SHORT_UTTERANCE_MS`.
    max_utterance_ms: u64,
}

impl SherpaStt {
    /// Loads the models of `config` on `device`.
    ///
    /// # Errors
    ///
    /// The model cannot load.
    pub fn try_new(config: &AsrConfig, device: Device) -> anyhow::Result<Self> {
        let vad = vad(config);
        let min_silence = config.vad_min_silence_ms * u64::from(RATE) / 1000;
        let max_utterance_ms = config.max_utterance_ms;
        let second_pass = second_pass(config, device)?;
        let dir = &config.model_dir;
        let file = |name: &str| Some(file(dir, name));
        let transducer = OnlineTransducerModelConfig {
            encoder: file(&config.encoder),
            decoder: file(&config.decoder),
            joiner: file(&config.joiner),
        };
        let model_config = OnlineModelConfig {
            transducer,
            tokens: file("tokens.txt"),
            provider: Some(stt_provider(device)),
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
            second_pass,
            vad,
            min_silence,
            max_utterance_ms,
        })
    }
}

/// The offline recognizer of the Parakeet model in `second_pass_dir` of `config` on `device`, or
/// `None` without `second_pass_dir`.
fn second_pass(config: &AsrConfig, device: Device) -> anyhow::Result<Option<OfflineRecognizer>> {
    let Some(dir) = &config.second_pass_dir else {
        return Ok(None);
    };
    let file = |name: &str| Some(file(dir, name));
    let transducer = OfflineTransducerModelConfig {
        encoder: file(&config.encoder),
        decoder: file(&config.decoder),
        joiner: file(&config.joiner),
    };
    let model_config = OfflineModelConfig {
        transducer,
        tokens: file("tokens.txt"),
        provider: Some(stt_provider(device)),
        num_threads: 4,
        model_type: Some("nemo_transducer".to_owned()),
        ..OfflineModelConfig::default()
    };
    let config = OfflineRecognizerConfig {
        model_config,
        ..OfflineRecognizerConfig::default()
    };
    let recognizer = OfflineRecognizer::create(&config)
        .with_context(|| format!("cannot load the second-pass model in {}", dir.display()))?;
    Ok(Some(recognizer))
}

/// The Silero VAD configuration of `config`. The VAD runs on the CPU, where it is faster than on
/// CUDA.
fn vad(config: &AsrConfig) -> VadModelConfig {
    let seconds = |ms: u64| Duration::from_millis(ms).as_secs_f32();
    let silero_vad = SileroVadModelConfig {
        model: Some(config.vad_model.to_string_lossy().into_owned()),
        threshold: config.vad_threshold,
        min_silence_duration: seconds(config.vad_min_silence_ms),
        // sherpa-onnx replaces 0 with 0.25 s.
        min_speech_duration: seconds(u64::from(WINDOW) * 1000 / u64::from(RATE)),
        window_size: i32::from(WINDOW),
        // An utterance commits at `max_utterance_ms`, so the VAD never splits a segment.
        max_speech_duration: seconds(config.max_utterance_ms),
    };
    VadModelConfig {
        silero_vad,
        sample_rate: i32::from(RATE),
        num_threads: 1,
        provider: Some(provider(Device::Cpu)),
        ..VadModelConfig::default()
    }
}

/// A new VAD of `config`.
///
/// # Errors
///
/// The VAD cannot load.
fn detector(config: &VadModelConfig) -> anyhow::Result<VoiceActivityDetector> {
    VoiceActivityDetector::create(config, 30.0).context("cannot load the VAD")
}

/// Removes the first finished speech segment of `vad` and gives its end in samples.
fn pop_segment_end(vad: &VoiceActivityDetector) -> Option<u64> {
    let segment = vad.front()?;
    vad.pop();
    Some(segment.start().unsigned_abs() + u64::from(segment.n().unsigned_abs()))
}

/// The voice end in samples that a stream of `config` gives for the 16 kHz `samples`, or `None`
/// when the VAD finds no speech. The latency tests measure from it (sub-phase 9.2).
///
/// # Errors
///
/// The VAD cannot load.
#[cfg(test)]
pub(crate) fn voice_end(config: &AsrConfig, samples: &[i16]) -> anyhow::Result<Option<u64>> {
    let vad = detector(&vad(config))?;
    let mut end = None;
    // A stream gives the VAD at most one frame in each call.
    for window in wave(samples).chunks(usize::from(WINDOW)) {
        vad.accept_waveform(window);
        while let Some(segment_end) = pop_segment_end(&vad) {
            end = Some(segment_end);
        }
    }
    Ok(end)
}

impl SpeechToText for SherpaStt {
    fn stream(&self) -> anyhow::Result<Box<dyn SttStream>> {
        let vad = detector(&self.vad)?;
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

    fn transcribe(&self, samples: &[i16]) -> anyhow::Result<Option<String>> {
        let Some(recognizer) = &self.second_pass else {
            return Ok(None);
        };
        let stream = recognizer.create_stream();
        let wave = padded(samples, self.max_utterance_ms)?;
        stream.accept_waveform(i32::from(RATE), &wave);
        recognizer.decode(&stream);
        let result = stream
            .get_result()
            .context("the second pass gives no result")?;
        Ok(Some(number_words(&result.text)))
    }
}

/// `text` with its numbers as words.
fn number_words(text: &str) -> String {
    let words: Vec<String> = text.split_whitespace().map(colon_time).collect();
    meridiems(&text_processing_rs::tn_normalize_sentence(&words.join(" ")))
}

/// `word` with a time "H.MM" written as "H:MM", because the text normalization reads "H.MM" as a
/// decimal.
fn colon_time(word: &str) -> String {
    let (time, rest) = word.split_at(
        word.trim_end_matches(|c: char| c.is_ascii_punctuation())
            .len(),
    );
    match time.split_once('.') {
        Some((hour, minute)) if in_range(hour, 1..=2, 23) && in_range(minute, 2..=2, 59) => {
            format!("{hour}:{minute}{rest}")
        }
        _ => word.to_owned(),
    }
}

/// True when `text` has a count of decimal digits in `digits` and a value of `max` or less.
fn in_range(text: &str, digits: RangeInclusive<usize>, max: u8) -> bool {
    digits.contains(&text.len())
        && text.bytes().all(|byte| byte.is_ascii_digit())
        && text.parse().is_ok_and(|value: u8| value <= max)
}

/// `text` with "a.m.", "p.m.", "a m" and "p m" written as "am" and "pm".
fn meridiems(text: &str) -> String {
    let mut words: Vec<String> = Vec::new();
    for word in text.split_whitespace() {
        match (words.last_mut(), word) {
            (_, "a.m." | "p.m.") => words.push(word.replace('.', "")),
            (Some(letter), _) if split_meridiem(letter, word) => letter.push_str(word),
            _ => words.push(word.to_owned()),
        }
    }
    words.join(" ")
}

/// True when `letter` and `word` are a meridiem in two words, as "p" and "m" or "m.".
fn split_meridiem(letter: &str, word: &str) -> bool {
    matches!(letter, "a" | "p")
        && word
            .strip_prefix('m')
            .is_some_and(|rest| rest.chars().all(|c| c.is_ascii_punctuation()))
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
    /// The end of the audio that the VAD has decided: all accepted audio out of a speech segment.
    decided: u64,
    /// The end of the last voiced frame, or `None` before any speech.
    voice_end: Option<u64>,
}

impl SherpaStream {
    /// Feeds `wave` to the recognizer and decodes it.
    fn decode(&self, wave: &[f32]) {
        self.stream.accept_waveform(i32::from(RATE), wave);
        while self.recognizer.is_ready(&self.stream) {
            self.recognizer.decode(&self.stream);
        }
    }

    /// Updates the VAD positions after `count` more samples of at most one frame. Gives true when
    /// the VAD ends a speech segment.
    fn track(&mut self, count: usize) -> bool {
        self.fed += count as u64;
        let mut ended = false;
        while let Some(end) = pop_segment_end(&self.vad) {
            self.voice_end = Some(end);
            ended = true;
        }
        let detected = self.vad.detected();
        // In a segment, the VAD holds the frames that can end it. Out of one, speech that the VAD
        // has not detected yet counts as silence.
        let decided = if detected {
            let run = self.fed - self.fed % u64::from(WINDOW);
            run.saturating_sub(self.min_silence)
        } else {
            self.fed
        };
        self.decided = self.decided.max(decided);
        if detected {
            self.voice_end = Some(self.decided);
        }
        ended
    }
}

impl SttStream for SherpaStream {
    fn accept(&mut self, samples: &[i16]) {
        let wave = wave(samples);
        self.decode(&wave);
        // The VAD gives one decision for all frames of one call.
        let mut ended = false;
        for window in wave.chunks(usize::from(WINDOW)) {
            self.vad.accept_waveform(window);
            ended |= self.track(window.len());
        }
        // sherpa-onnx cannot clone a stream, so the zeros go into the live stream, where a commit
        // and `reset` can follow. They do not move the audio positions.
        if ended {
            self.decode(&vec![0.0; FLUSH]);
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

/// The samples in [-1, 1] of the 16-bit `samples`.
fn wave(samples: &[i16]) -> Vec<f32> {
    samples
        .iter()
        .map(|&sample| f32::from(sample) / f32::from(i16::MAX))
        .collect()
}

/// The wave of `samples` with zeros up to `SHORT_UTTERANCE_MS`, or up to `max_utterance_ms` for
/// longer samples. ONNX Runtime builds new cuDNN plans for each new input length of the fp16
/// encoder, which adds about 190 ms to a decode.
///
/// # Errors
///
/// A length does not fit in `usize`.
fn padded(samples: &[i16], max_utterance_ms: u64) -> anyhow::Result<Vec<f32>> {
    let length = |ms: u64| usize::try_from(ms * u64::from(RATE) / 1000);
    let short = length(SHORT_UTTERANCE_MS)?;
    let target = if samples.len() <= short {
        short
    } else {
        length(max_utterance_ms)?
    };
    let mut wave = wave(samples);
    wave.resize(wave.len().max(target), 0.0);
    Ok(wave)
}

/// The path of the file `name` in `dir`.
fn file(dir: &Path, name: &str) -> String {
    dir.join(name).to_string_lossy().into_owned()
}

/// The execution provider of an STT session on `device`. sherpa-onnx reads the CUDA options from
/// the file after the colon.
fn stt_provider(device: Device) -> String {
    match device {
        Device::Cuda => "cuda:sherpa-cuda.config".to_owned(),
        Device::Cpu => provider(device),
    }
}

/// The sherpa-onnx execution provider of `device`.
fn provider(device: Device) -> String {
    match device {
        Device::Cuda => "cuda",
        Device::Cpu => "cpu",
    }
    .to_owned()
}

/// Tests of the STT adapter with the Nemotron model and the Silero VAD.
#[cfg(test)]
mod tests {
    use std::thread;
    use std::time::Instant;

    use super::*;
    use crate::Config;
    use crate::domain::intent;
    use crate::domain::speech;
    use crate::domain::speech::Audio;
    use crate::testing::fakes;

    /// The request of the STT tests.
    const REQUEST: &str = "turn on the kitchen lights";

    /// The 20 ms chunks of 16 kHz audio.
    const CHUNK: usize = 320;

    /// The configuration of `jarvis.toml`.
    fn config() -> anyhow::Result<Config> {
        Config::parse(include_str!("../../jarvis.toml"))
    }

    /// The `SherpaStt` of `jarvis.toml` on `device`. The CPU loads the int8 files without the second
    /// pass, as on a CPU machine.
    fn stt(device: Device) -> anyhow::Result<SherpaStt> {
        let mut asr = config()?.asr;
        if device == Device::Cpu {
            let int8 = |part: &str| format!("{part}.int8.onnx");
            (asr.encoder, asr.decoder, asr.joiner) =
                (int8("encoder"), int8("decoder"), int8("joiner"));
            asr.second_pass_dir = None;
        }
        SherpaStt::try_new(&asr, device)
    }

    /// The normalized second-pass text of `audio`.
    fn second_pass_text(stt: &SherpaStt, audio: &Audio) -> anyhow::Result<String> {
        let text = stt.transcribe(&audio.samples)?;
        Ok(intent::normalize(&text.context("no second pass")?))
    }

    /// The 50 phrases of the word error test: 40 commands and 10 phrases with numbers.
    fn phrases() -> Vec<String> {
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
            "living room",
        ];
        let mut phrases = Vec::new();
        for (action, device) in actions {
            for area in areas {
                phrases.push(format!("{action} the {area} {device}"));
            }
        }
        let numbers = [
            "set a timer for ten minutes",
            "set a timer for twenty five minutes",
            "set the thermostat to seventy two",
            "dim the kitchen lights to thirty percent",
            "set the bedroom lights to eighty percent",
            "set the volume to fifty percent",
            "wake me up at seven thirty",
            "remind me to call mom at three pm",
            "turn off the porch lights at ten fifteen pm",
            "start the dishwasher at nine forty five pm",
        ];
        phrases.extend(numbers.map(str::to_owned));
        phrases
    }

    /// The word error rate of `text` against `phrase`, both normalized.
    fn word_error_rate(phrase: &str, text: &str) -> anyhow::Result<f64> {
        let (phrase, text) = (intent::normalize(phrase), intent::normalize(text));
        let phrase: Vec<&str> = phrase.split_whitespace().collect();
        let text: Vec<&str> = text.split_whitespace().collect();
        let errors = u32::try_from(strsim::generic_levenshtein(&phrase, &text))?;
        Ok(f64::from(errors) / f64::from(u32::try_from(phrase.len())?))
    }

    /// The partials of `speech(REQUEST)` on the CPU.
    async fn partials() -> anyhow::Result<Vec<Partial>> {
        partials_of(&stt(Device::Cpu)?, &fakes::speech(REQUEST).await?)
    }

    /// The partial after each 20 ms chunk of `audio` in a new first pass of `stt`.
    fn partials_of(stt: &SherpaStt, audio: &Audio) -> anyhow::Result<Vec<Partial>> {
        let mut stream = stt.stream()?;
        let chunks = speech::chunks(audio, CHUNK);
        let decode = |chunk: &[i16]| {
            stream.accept(chunk);
            stream.partial()
        };
        Ok(chunks.into_iter().map(decode).collect())
    }

    /// Each partial of `speech(REQUEST)` on the CPU with `vad_ms > voice_end_ms`, with the length
    /// in ms of the audio that the stream accepted before it.
    async fn silent_partials() -> anyhow::Result<Vec<(u64, Partial)>> {
        let audio = fakes::speech(REQUEST).await?;
        let partials = partials_of(&stt(Device::Cpu)?, &audio)?;
        let accepted = |index: usize| {
            let samples = ((index + 1) * CHUNK).min(audio.samples.len());
            samples as u64 * 1000 / u64::from(RATE)
        };
        let silent =
            |partial: &Partial| partial.voice_end_ms.is_some_and(|end| partial.vad_ms > end);
        let pairs = partials.into_iter().enumerate();
        Ok(pairs
            .filter(|(_, partial)| silent(partial))
            .map(|(index, partial)| (accepted(index), partial))
            .collect())
    }

    /// The partial of `speech("lights on")`, then the partial after `reset`.
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

    /// The normalized text of the last of `partials`.
    fn final_text(partials: &[Partial]) -> Option<String> {
        partials
            .last()
            .map(|partial| intent::normalize(&partial.text))
    }

    /// The time of the first pass on `device` of `speech(REQUEST)` up to its last voiced sample,
    /// after a warm-up, divided by the audio time. The VAD does not end the speech segment, so
    /// no flush runs.
    async fn real_time_factor(device: Device) -> anyhow::Result<f64> {
        let (stt, mut audio) = (stt(device)?, fakes::speech(REQUEST).await?);
        let end = usize::try_from(last_voiced_ms(&audio))? * usize::from(RATE) / 1000;
        audio.samples.truncate(end);
        partials_of(&stt, &audio)?;
        let start = Instant::now();
        partials_of(&stt, &audio)?;
        Ok(start.elapsed().as_secs_f64() / speech::duration(&audio).as_secs_f64())
    }

    /// The time from the last voiced sample of `audio` to the first partial with `REQUEST` in a
    /// new first pass of `stt`, with each chunk at its real time.
    fn last_word_latency(stt: &SherpaStt, audio: &Audio) -> anyhow::Result<Duration> {
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
        let partials = partials().await?;
        let text = final_text(&partials);
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
        let partials = partials_of(&stt(Device::Cpu)?, &audio)?;
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
        let partials = partials_of(&stt(Device::Cpu)?, &audio)?;
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

    /// AC-9.1b-1.
    #[tokio::test]
    async fn ac_9_1b_1_silence_ends_at_the_accepted_audio() -> anyhow::Result<()> {
        let silent = silent_partials().await?;
        assert!(!silent.is_empty(), "no partial has silence");
        for (accepted_ms, partial) in &silent {
            assert_eq!(partial.vad_ms, *accepted_ms, "{partial:?}");
        }
        Ok(())
    }

    /// AC-9.1b-2.
    #[tokio::test]
    async fn ac_9_1b_2_first_silence_is_min_silence_and_less_than_one_frame() -> anyhow::Result<()>
    {
        let min_silence = config()?.asr.vad_min_silence_ms;
        let silent = silent_partials().await?;
        let (_, first) = silent.first().context("no partial has silence")?;
        let silence = first.vad_ms - first.voice_end_ms.unwrap_or_default();
        assert!(
            (min_silence..min_silence + 32).contains(&silence),
            "silence {silence} ms, vad_min_silence_ms {min_silence}: {first:?}"
        );
        Ok(())
    }

    /// AC-6.6c-1.
    #[tokio::test]
    #[ignore = "GPU host"]
    async fn ac_6_6c_1_cuda_final_partial_is_the_request() -> anyhow::Result<()> {
        let partials = partials_of(&stt(Device::Cuda)?, &fakes::speech(REQUEST).await?)?;
        let text = final_text(&partials);
        assert_eq!(text.as_deref(), Some(REQUEST), "{partials:?}");
        Ok(())
    }

    /// AC-6.6c-2.
    #[tokio::test]
    #[ignore = "GPU host"]
    async fn ac_6_6c_2_cuda_last_word_latency() -> anyhow::Result<()> {
        let (stt, audio) = (stt(Device::Cuda)?, fakes::speech(REQUEST).await?);
        // The first run warms up the model.
        last_word_latency(&stt, &audio)?;
        let latencies: anyhow::Result<Vec<Duration>> =
            (0..10).map(|_| last_word_latency(&stt, &audio)).collect();
        let mut latencies = latencies?;
        latencies.sort();
        let p90 = latencies[latencies.len() * 9 / 10];
        assert!(
            p90 < Duration::from_millis(300),
            "p90 {p90:?} of {latencies:?}"
        );
        Ok(())
    }

    /// AC-6.7-6.
    #[test]
    fn ac_6_7_6_numbers_become_words() {
        let text = "Set the lights to 30% at 7.30 p.m. and the heat to 72.5 at 3 p.m.";
        assert_eq!(
            intent::normalize(&number_words(text)),
            "set the lights to thirty percent at seven thirty pm and the heat to seventy two point five at three pm"
        );
    }

    /// AC-6.7-1.
    #[tokio::test]
    async fn ac_6_7_1_no_second_pass_gives_none() -> anyhow::Result<()> {
        let audio = fakes::speech(REQUEST).await?;
        assert_eq!(stt(Device::Cpu)?.transcribe(&audio.samples)?, None);
        Ok(())
    }

    /// AC-6.7-2.
    #[tokio::test]
    #[ignore = "GPU host"]
    async fn ac_6_7_2_second_pass_gives_the_request() -> anyhow::Result<()> {
        let request = "add dentist to my calendar for monday";
        let audio = fakes::speech(request).await?;
        assert_eq!(second_pass_text(&stt(Device::Cuda)?, &audio)?, request);
        Ok(())
    }

    /// AC-6.7-3.
    #[tokio::test]
    #[ignore = "GPU host"]
    async fn ac_6_7_3_second_pass_has_no_higher_word_error_rate() -> anyhow::Result<()> {
        let stt = stt(Device::Cuda)?;
        let (mut first, mut second, mut worse) = (0.0, 0.0, Vec::new());
        for phrase in phrases() {
            let audio = fakes::speech(&phrase).await?;
            let mut stream = stt.stream()?;
            stream.accept(&audio.samples);
            let first_rate = word_error_rate(&phrase, &stream.partial().text)?;
            let text = second_pass_text(&stt, &audio)?;
            let second_rate = word_error_rate(&phrase, &text)?;
            if second_rate > first_rate {
                worse.push(text);
            }
            first += first_rate;
            second += second_rate;
        }
        assert!(
            second <= first,
            "word error rates: {first} in the first pass, {second} in the second pass, worse: {worse:?}"
        );
        Ok(())
    }

    /// AC-6.6c-3.
    #[tokio::test]
    #[ignore = "GPU host"]
    async fn ac_6_6c_3_cuda_real_time_factor() -> anyhow::Result<()> {
        let factor = real_time_factor(Device::Cuda).await?;
        assert!(factor < 0.15, "real-time factor {factor}");
        Ok(())
    }

    /// AC-6.6c-5.
    #[tokio::test]
    #[ignore = "GPU host"]
    async fn ac_6_6c_5_cpu_real_time_factor() -> anyhow::Result<()> {
        let factor = real_time_factor(Device::Cpu).await?;
        assert!(factor < 0.6, "real-time factor {factor}");
        Ok(())
    }

    /// The samples of `speech("add dentist to my calendar for monday")` with zeros or a cut to
    /// `tenths` tenths of a second.
    async fn dentist(tenths: usize) -> anyhow::Result<Vec<i16>> {
        let mut samples = fakes::speech("add dentist to my calendar for monday")
            .await?
            .samples;
        samples.resize(tenths * usize::from(RATE) / 10, 0);
        Ok(samples)
    }

    /// The p95 of the `transcribe` times of each of `inputs` on CUDA, after a warm-up with
    /// `warm_up`, and the sorted times.
    fn cuda_p95(warm_up: &[i16], inputs: &[&[i16]]) -> anyhow::Result<(Duration, Vec<Duration>)> {
        let stt = stt(Device::Cuda)?;
        stt.transcribe(warm_up)?;
        let mut times = Vec::new();
        for input in inputs {
            let start = Instant::now();
            stt.transcribe(input)?;
            times.push(start.elapsed());
        }
        times.sort();
        // The 95th percentile of 20 times is the 19th.
        Ok((times[18], times))
    }

    /// AC-6.7-4.
    #[tokio::test]
    #[ignore = "GPU host"]
    async fn ac_6_7_4_three_second_utterance_p95() -> anyhow::Result<()> {
        let samples = dentist(30).await?;
        let (p95, times) = cuda_p95(&samples, &[samples.as_slice(); 20])?;
        assert!(p95 < Duration::from_millis(40), "p95 {p95:?} of {times:?}");
        Ok(())
    }

    /// AC-6.7-7.
    #[tokio::test]
    #[ignore = "GPU host"]
    async fn ac_6_7_7_new_lengths_p95() -> anyhow::Result<()> {
        use std::io::Write;

        let samples = dentist(34).await?;
        let prefix = |tenths: usize| &samples[..tenths * usize::from(RATE) / 10];
        let inputs: Vec<&[i16]> = (15..35).map(prefix).collect();
        // The warm-up has a length that no run has.
        let (p95, times) = cuda_p95(prefix(10), &inputs)?;
        // The test harness captures `eprintln!` of a test that passes, but not a direct write.
        writeln!(std::io::stderr(), "AC-6.7-7: p95 {p95:?} of {times:?}")?;
        assert!(p95 <= Duration::from_millis(50), "p95 {p95:?} of {times:?}");
        Ok(())
    }

    /// AC-6.7-8.
    #[tokio::test]
    async fn ac_6_7_8_input_has_zeros_to_4_s_or_max_utterance() -> anyhow::Result<()> {
        let max_utterance_ms = config()?.asr.max_utterance_ms;
        let short = fakes::speech(REQUEST).await?.samples;
        let long: Vec<i16> = short
            .iter()
            .copied()
            .cycle()
            .take(5 * usize::from(RATE))
            .collect();
        for (samples, ms) in [(short, 4000), (long, max_utterance_ms)] {
            let length = usize::try_from(ms)? * usize::from(RATE) / 1000;
            assert!(samples.len() < length, "{} samples", samples.len());
            let input = padded(&samples, max_utterance_ms)?;
            assert_eq!(input.len(), length, "{} samples", samples.len());
            let (speech, zeros) = input.split_at(samples.len());
            assert_eq!(speech, wave(&samples));
            assert!(zeros.iter().all(|&zero| zero == 0.0), "{zeros:?}");
        }
        Ok(())
    }
}
