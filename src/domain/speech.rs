//! Audio type, audio chunks, audio duration, commit policy and sentence split.

use std::time::Duration;

use vstd::prelude::*;

/// 16-bit mono PCM audio.
#[verus_verify]
#[derive(Clone, Debug, PartialEq)]
pub struct Audio {
    /// The samples in time order.
    pub samples: Vec<i16>,
    /// The sample rate in Hz.
    pub rate: u32,
}

/// The samples of `audio` in order, in pieces of `size` samples and a shorter last piece.
///
/// # Panics
///
/// Panics when `size` is 0.
#[verus_spec(pieces =>
    requires
        size > 0,
    ensures
        joined(pieces@) == audio.samples@,
        forall|index: int| 0 <= index < pieces@.len() - 1 ==> #[trigger] pieces@[index]@.len() == size,
)]
#[must_use]
pub fn chunks(audio: &Audio, size: usize) -> Vec<&[i16]> {
    let pieces: Vec<&[i16]> = audio.samples.chunks(size).collect();
    proof! {
        lemma_joined_prefix(pieces@, audio.samples@, size as int, pieces@.len() as int);
        assert(pieces@.take(pieces@.len() as int) =~= pieces@);
    }
    pieces
}

/// The play time of `audio`: the number of samples divided by the sample rate.
///
/// # Panics
///
/// Panics when the sample rate is 0.
#[verus_spec(time =>
    requires
        audio.rate > 0,
    ensures
        crate::domain::contracts::nanoseconds(time) == audio.samples@.len() * 1_000_000_000 / audio.rate as nat,
)]
#[must_use]
pub fn duration(audio: &Audio) -> Duration {
    let seconds = Duration::from_secs(audio.samples.len() as u64);
    seconds
        .checked_div(audio.rate)
        .expect("the rate is more than 0")
}

/// The silence and length limits that end an utterance, in milliseconds.
#[verus_verify]
#[derive(Clone, Debug, PartialEq)]
pub struct CommitPolicy {
    /// The silence after a confident partial that commits.
    pub early_commit_ms: u64,
    /// The silence after any partial that commits.
    pub endpoint_ms: u64,
    /// The utterance length that always commits.
    pub max_utterance_ms: u64,
}

/// The utterance ends: after `silence_ms` of silence that follows text, or at `audio_ms` of audio.
/// `silence_ms` is `None` before any speech.
#[verus_spec(commit =>
    ensures
        commit == ((silence_ms matches Some(silence) && has_text
            && ((confident && silence >= policy.early_commit_ms) || silence >= policy.endpoint_ms))
            || audio_ms >= policy.max_utterance_ms),
)]
#[must_use]
pub fn should_commit(
    silence_ms: Option<u64>,
    has_text: bool,
    audio_ms: u64,
    confident: bool,
    policy: &CommitPolicy,
) -> bool {
    let spoken = match silence_ms {
        Some(silence) => {
            has_text
                && ((confident && silence >= policy.early_commit_ms)
                    || silence >= policy.endpoint_ms)
        }
        None => false,
    };
    spoken || audio_ms >= policy.max_utterance_ms
}

/// The complete sentences of `buffer` followed by `chunk`, and the rest. A sentence is the text up
/// to and including a `.`, `?` or `!` that a space follows. The space is in no sentence.
#[verus_spec(split =>
    ensures
        spaced(split.0@) + split.1@ == buffer@ + chunk@,
        forall|index: int| 0 <= index < split.0@.len() ==> #[trigger] complete(split.0@[index]@),
        unbroken(split.1@),
)]
#[must_use]
pub fn sentences(buffer: &str, chunk: &str) -> (Vec<String>, String) {
    let mut text = buffer.to_owned();
    text.push_str(chunk);
    let chars: Vec<char> = text.chars().collect();
    let mut complete: Vec<String> = Vec::new();
    let mut rest = String::new();
    let mut index = 0;
    proof! {
        assert(spaced(complete@) + rest@ =~= chars@.take(0));
    }
    #[cfg_attr(verus_only, verus_spec(
        invariant
            chars@ == buffer@ + chunk@,
            splitting(chars@, complete@, rest@, index as int),
        decreases chars@.len() - index,
    ))]
    while index < chars.len() {
        proof! {
            lemma_split(chars@, complete@, rest@, index as int);
        }
        if is_boundary(&chars, index) {
            complete.push(rest);
            rest = String::new();
        } else {
            rest.push(chars[index]);
        }
        index += 1;
    }
    proof! {
        assert(chars@.take(chars@.len() as int) =~= chars@);
    }
    (complete, rest)
}

/// True when the character at `index` of `chars` is a space after `.`, `?` or `!`.
#[verus_spec(result =>
    requires
        index < chars@.len(),
    ensures
        result == boundary(chars@, index as int),
)]
fn is_boundary(chars: &[char], index: usize) -> bool {
    if index == 0 || chars[index] != ' ' {
        return false;
    }
    let before = chars[index - 1];
    before == '.' || before == '?' || before == '!'
}

verus! {

/// True when `c` ends a sentence before a space.
pub open spec fn mark(c: char) -> bool {
    c == '.' || c == '?' || c == '!'
}

/// True when the character at `index` of `text` is a space after a mark.
pub open spec fn boundary(text: Seq<char>, index: int) -> bool {
    0 < index < text.len() && text[index] == ' ' && mark(text[index - 1])
}

/// True when `text` has no space after a mark.
pub open spec fn unbroken(text: Seq<char>) -> bool {
    forall|index: int| !#[trigger] boundary(text, index)
}

/// True when `text` is one complete sentence: it ends with a mark and has no space after a mark.
pub open spec fn complete(text: Seq<char>) -> bool {
    text.len() > 0 && mark(text.last()) && unbroken(text)
}

/// The text of `sentences` in order, each with the space after it.
pub open spec fn spaced(sentences: Seq<String>) -> Seq<char> {
    sentences.map_values(|sentence: String| sentence@.push(' ')).flatten()
}

/// The loop invariant of `sentences`: `done` and `rest` split the first `index` characters
/// of `chars`, and `rest` ends with the character before `index` when it is not empty.
spec fn splitting(chars: Seq<char>, done: Seq<String>, rest: Seq<char>, index: int) -> bool {
    &&& 0 <= index <= chars.len()
    &&& spaced(done) + rest == chars.take(index)
    &&& forall|position: int| 0 <= position < done.len() ==> #[trigger] complete(done[position]@)
    &&& unbroken(rest)
    &&& rest.len() > 0 ==> index > 0 && rest.last() == chars[index - 1]
    &&& index > 0 && chars[index - 1] != ' ' ==> rest.len() > 0
}

/// The loop invariant of `sentences` holds after the character at `index` ends a sentence or
/// joins `rest`.
proof fn lemma_split(chars: Seq<char>, done: Seq<String>, rest: Seq<char>, index: int)
    requires
        index < chars.len(),
        splitting(chars, done, rest, index),
    ensures
        boundary(chars, index) ==> forall|ended: String| #[trigger] ended@ == rest
            ==> splitting(chars, done.push(ended), Seq::empty(), index + 1),
        !boundary(chars, index) ==> splitting(chars, done, rest.push(chars[index]), index + 1),
{
    assert(chars.take(index).push(chars[index]) =~= chars.take(index + 1));
    if boundary(chars, index) {
        assert forall|ended: String| #[trigger] ended@ == rest
            implies splitting(chars, done.push(ended), Seq::empty(), index + 1) by {
            let mapped = done.map_values(|sentence: String| sentence@.push(' '));
            assert(done.push(ended).map_values(|sentence: String| sentence@.push(' '))
                =~= mapped.push(rest.push(' ')));
            mapped.lemma_flatten_push(rest.push(' '));
            assert(spaced(done.push(ended)) + Seq::empty() =~= spaced(done) + rest + seq![' ']);
            assert(complete(ended@));
            assert forall|position: int| 0 <= position < done.len() + 1
                implies #[trigger] complete(done.push(ended)[position]@) by {
                if position < done.len() {
                    assert(complete(done[position]@));
                }
            }
            assert(unbroken(Seq::<char>::empty()));
        }
    } else {
        let pushed = rest.push(chars[index]);
        assert(spaced(done) + pushed =~= (spaced(done) + rest).push(chars[index]));
        assert forall|position: int| !#[trigger] boundary(pushed, position) by {
            if position < rest.len() {
                assert(!boundary(rest, position));
            }
        }
    }
}

/// The samples of `pieces` in order.
pub open spec fn joined(pieces: Seq<&[i16]>) -> Seq<i16> {
    pieces.map_values(|piece: &[i16]| piece@).flatten()
}

/// The end of piece `index` when `samples` is cut in pieces of `size`.
pub open spec fn end(samples: Seq<i16>, size: int, index: int) -> int {
    if index * size + size <= samples.len() { index * size + size } else { samples.len() as int }
}

/// The first `count` pieces of `samples`, cut in order in pieces of `size`, join to the samples
/// up to the end of piece `count - 1`. All pieces join to `samples`, and each piece except the
/// last has `size` samples.
proof fn lemma_joined_prefix(pieces: Seq<&[i16]>, samples: Seq<i16>, size: int, count: int)
    requires
        size > 0,
        pieces.len() == (samples.len() + size - 1) / size,
        forall|index: int| 0 <= index < pieces.len() ==> #[trigger] pieces[index]@ == samples.subrange(index * size, end(samples, size, index)),
        0 <= count <= pieces.len(),
    ensures
        joined(pieces.take(count)) == samples.subrange(0, end(samples, size, count - 1)),
        count == pieces.len() ==> joined(pieces.take(count)) == samples,
        forall|index: int| 0 <= index < pieces.len() - 1 ==> #[trigger] pieces[index]@.len() == size,
    decreases count,
{
    let total = samples.len() as int;
    let last = pieces.len() as int;
    assert(total <= last * size && last * size < total + size) by (nonlinear_arith)
        requires
            size > 0,
            last == (total + size - 1) / size,
            total >= 0,
    ;
    assert(end(samples, size, last - 1) == total) by (nonlinear_arith)
        requires
            size > 0,
            total <= last * size,
            end(samples, size, last - 1) == if (last - 1) * size + size <= total { (last - 1) * size + size } else { total },
    ;
    assert forall|index: int| 0 <= index < last - 1 implies #[trigger] pieces[index]@.len() == size by {
        assert(index * size + size <= total) by (nonlinear_arith)
            requires
                size > 0,
                0 <= index < last - 1,
                last * size < total + size,
        ;
    }
    assert(samples.subrange(0, total) =~= samples);
    if count == 0 {
        assert(joined(pieces.take(0)) =~= Seq::<i16>::empty());
        assert(samples.subrange(0, end(samples, size, -1)) =~= Seq::<i16>::empty());
    } else {
        lemma_joined_prefix(pieces, samples, size, count - 1);
        let start = (count - 1) * size;
        assert(0 <= start < total && start == (count - 2) * size + size) by (nonlinear_arith)
            requires
                size > 0,
                0 < count <= last,
                last * size < total + size,
                start == (count - 1) * size,
        ;
        assert(end(samples, size, count - 2) == start);
        let prior = pieces.take(count - 1).map_values(|piece: &[i16]| piece@);
        assert(pieces.take(count).map_values(|piece: &[i16]| piece@) =~= prior.push(pieces[count - 1]@));
        prior.lemma_flatten_push(pieces[count - 1]@);
        assert(samples.subrange(0, end(samples, size, count - 1)) =~= samples.subrange(0, start)
            + samples.subrange(start, end(samples, size, count - 1)));
    }
}

} // verus!

#[cfg(test)]
mod tests {
    use quint_connect::Driver;
    use quint_connect::State as ModelState;
    use quint_connect::Step;
    use quint_connect::quint_run;
    use quint_connect::switch;
    use serde::Deserialize;

    use super::*;

    #[test]
    fn ac_6_3_4_chunks_join_to_the_audio_in_pieces_of_1024() {
        let audio = Audio {
            samples: (0..2500).collect(),
            rate: 16_000,
        };
        let pieces: Vec<&[i16]> = chunks(&audio, 1024);
        let sizes: Vec<usize> = pieces.iter().map(|piece| piece.len()).collect();
        assert_eq!(pieces.concat(), audio.samples, "joined chunks");
        assert_eq!(sizes, [1024, 1024, 452], "chunk sizes");
    }

    #[test]
    fn ac_6_3_5_22050_samples_at_22050_hz_last_1_s() {
        let audio = Audio {
            samples: vec![0; 22_050],
            rate: 22_050,
        };
        assert_eq!(duration(&audio), Duration::from_secs(1));
    }

    /// The commit policy of `jarvis.toml`.
    const POLICY: CommitPolicy = CommitPolicy {
        early_commit_ms: 200,
        endpoint_ms: 600,
        max_utterance_ms: 8000,
    };

    /// Each combination of silence, text and confidence.
    fn inputs() -> Vec<(Option<u64>, bool, bool)> {
        let silences = [None, Some(0), Some(200), Some(600), Some(u64::MAX)];
        let flags = [(false, false), (false, true), (true, false), (true, true)];
        let pairs = silences
            .into_iter()
            .flat_map(|silence| flags.map(|flag| (silence, flag)));
        pairs
            .map(|(silence, (text, confident))| (silence, text, confident))
            .collect()
    }

    /// AC-6.5-1.
    #[test]
    fn ac_6_5_1_confident_partial_commits_at_early_commit_ms() {
        assert!(should_commit(Some(200), true, 1000, true, &POLICY));
    }

    /// AC-6.5-2.
    #[test]
    fn ac_6_5_2_uncertain_partial_commits_at_endpoint_ms() {
        assert!(
            !should_commit(Some(599), true, 1000, false, &POLICY),
            "commit at 599 ms"
        );
        assert!(
            should_commit(Some(600), true, 1000, false, &POLICY),
            "no commit at 600 ms"
        );
    }

    /// AC-6.5-3.
    #[test]
    fn ac_6_5_3_max_utterance_ms_commits_for_any_input() {
        for (silence, text, confident) in inputs() {
            assert!(
                should_commit(silence, text, 8000, confident, &POLICY),
                "no commit for {silence:?}, text {text}, confident {confident}"
            );
        }
    }

    /// AC-6.5-5.
    #[test]
    fn ac_6_5_5_no_speech_or_no_text_does_not_commit() {
        let quiet = inputs()
            .into_iter()
            .filter(|&(silence, text, _)| silence.is_none() || !text);
        for (silence, text, confident) in quiet {
            assert!(
                !should_commit(silence, text, 7999, confident, &POLICY),
                "commit for {silence:?}, text {text}, confident {confident}"
            );
        }
    }

    /// AC-7.3-1.
    #[test]
    fn ac_7_3_1_complete_sentence_is_split_from_the_rest() {
        let (complete, rest) = sentences("", "Hello there. It is");
        assert_eq!(
            (complete, rest.as_str()),
            (vec!["Hello there.".to_owned()], "It is")
        );
    }

    /// AC-7.3-3.
    #[test]
    fn ac_7_3_3_sentences_and_rest_join_to_the_chunks() {
        let chunks = [
            "Hel",
            "lo there.",
            " It is",
            " sunny! Is it",
            "?  Yes.\tNo. ",
            "Maybe",
        ];
        let mut buffer = String::new();
        let mut joined = String::new();
        for chunk in chunks {
            let (complete, rest) = sentences(&buffer, chunk);
            joined = format!("{joined} {}", complete.join(" "));
            buffer = rest;
        }
        let normal = |text: &str| text.split_whitespace().collect::<Vec<&str>>().join(" ");
        assert_eq!(
            normal(&format!("{joined} {buffer}")),
            normal(&chunks.concat())
        );
    }

    /// The commit model.
    const MODEL: &str = include_str!("../../quint/commit.qnt");

    /// The length of one model chunk.
    const CHUNK_MS: u64 = 100;

    /// The constant `name` of `commit.qnt`, in milliseconds.
    fn model_ms(name: &str) -> u64 {
        let prefix = format!("pure val {name} = ");
        let value = MODEL
            .lines()
            .find_map(|line| line.trim().strip_prefix(prefix.as_str()));
        let value: u64 = value
            .expect("the model has the constant")
            .parse()
            .expect("a number");
        value * CHUNK_MS
    }

    /// The model state that the conformance test compares.
    #[derive(Debug, Deserialize, PartialEq)]
    struct Commit {
        /// The utterance ended.
        committed: bool,
    }

    impl ModelState<CommitDriver> for Commit {
        /// `should_commit` of the inputs. `CommitOnlyWhenAllowed` and `NoLateCommit` make the
        /// model `committed` equal to `allowed` after each step.
        fn from_driver(driver: &CommitDriver) -> anyhow::Result<Self> {
            let silence = driver
                .voice_end
                .map(|end| (driver.decided - end) * CHUNK_MS);
            let audio = u64::try_from(driver.chunks.len())? * CHUNK_MS;
            let (text, confident) = (driver.text, driver.confident);
            Ok(Self {
                committed: should_commit(silence, text, audio, confident, &driver.policy),
            })
        }
    }

    /// Replays `commit.qnt` against `should_commit`, with the model constants as the policy.
    struct CommitDriver {
        /// The policy from the model constants.
        policy: CommitPolicy,
        /// The chunks in arrival order. True is a voiced chunk.
        chunks: Vec<bool>,
        /// The number of chunks that the VAD decided.
        decided: u64,
        /// The number of decided chunks up to the last voiced chunk, or `None` before any.
        voice_end: Option<u64>,
        /// The partial is not empty.
        text: bool,
        /// The partial is confident.
        confident: bool,
    }

    impl CommitDriver {
        /// A driver with no audio and an empty partial.
        fn new() -> Self {
            Self {
                policy: CommitPolicy {
                    early_commit_ms: model_ms("EARLY"),
                    endpoint_ms: model_ms("ENDPOINT"),
                    max_utterance_ms: model_ms("MAX"),
                },
                chunks: Vec::new(),
                decided: 0,
                voice_end: None,
                text: false,
                confident: false,
            }
        }
    }

    impl Driver for CommitDriver {
        type State = Commit;

        #[expect(
            non_snake_case,
            reason = "`switch!` binds the model pick `nonEmpty` by its name"
        )]
        fn step(&mut self, step: &Step) -> anyhow::Result<()> {
            switch!(step {
                init => *self = Self::new(),
                tick(voiced: bool) => self.chunks.push(voiced),
                decide => {
                    self.decided += 1;
                    if self.chunks[usize::try_from(self.decided)? - 1] {
                        self.voice_end = Some(self.decided);
                    }
                },
                partial(nonEmpty: bool, conf: bool) => (self.text, self.confident) = (nonEmpty, conf),
            })
        }
    }

    /// AC-6.5-4. `quint_run` replays 100 traces by default.
    #[quint_run(spec = "quint/commit.qnt", max_steps = 30, seed = "1")]
    fn ac_6_5_4_conformance() -> impl Driver {
        CommitDriver::new()
    }
}
