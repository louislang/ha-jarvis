//! Audio type, audio chunks, audio duration, commit policy, sentence split, audio correlation and
//! hotword list.

use std::time::Duration;

use jiff::Timestamp;
use vstd::prelude::*;

use crate::domain::catalog::Catalog;
use crate::domain::intent;
use crate::domain::select;
use crate::domain::state_cache::StateCache;

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

/// A committed utterance, kept to find its audio by its transcript.
#[verus_verify]
#[derive(Clone, Debug, PartialEq)]
pub struct Utterance {
    /// The transcript.
    pub text: String,
    /// The audio.
    pub audio: Audio,
    /// The time at which it was kept.
    pub time: Timestamp,
    /// The time of the last voiced sample, or `None` without speech.
    pub voice_end: Option<Timestamp>,
}

/// The utterances of `utterances` that are not older than `window_s` at `now`, in order.
#[verus_spec(kept =>
    ensures
        kept@ == utterances@.filter(fresh_at(now, window_s)),
)]
#[must_use]
pub fn expire(utterances: Vec<Utterance>, now: Timestamp, window_s: u64) -> Vec<Utterance> {
    let mut kept = Vec::new();
    let all = utterances.into_iter();
    proof_decl! {
        let ghost seen = vstd::std_specs::iter::IteratorSpec::remaining(&all);
    }
    #[cfg_attr(verus_only, verus_spec(utterance_iter =>
        invariant
            utterance_iter.seq() == seen,
            seen == utterances@,
            kept@ == seen.take(utterance_iter.index() as int).filter(fresh_at(now, window_s)),
    ))]
    for utterance in all {
        proof! {
            lemma_filter_step(seen, utterance_iter.index() as int, fresh_at(now, window_s));
        }
        if is_fresh(&utterance, now, window_s) {
            kept.push(utterance);
        }
    }
    proof! {
        assert(seen.take(seen.len() as int) =~= seen);
    }
    kept
}

/// True when `utterance` is not older than `window_s` at `now`.
#[verus_spec(kept =>
    ensures
        kept == fresh(*utterance, now, window_s),
)]
fn is_fresh(utterance: &Utterance, now: Timestamp, window_s: u64) -> bool {
    let window = i128::from(window_s) * 1_000_000_000;
    match utterance.time.as_nanosecond().checked_add(window) {
        Some(deadline) => now.as_nanosecond() <= deadline,
        None => true,
    }
}

/// The utterances of `utterances` that are not older than `window_s` at `now`, without the one
/// with the normalized text of `text`, and that one. Two or more matches give `None` and remove
/// nothing.
#[verus_spec(correlated =>
    ensures
        correlated.1 == unique(matching(utterances@, text@, now, window_s)),
        forall|index: int| 0 <= index < correlated.0@.len() ==> fresh(#[trigger] correlated.0@[index], now, window_s),
        correlated.1 matches Some(found) ==> !correlated.0@.contains(found),
)]
#[must_use]
pub fn correlate(
    utterances: Vec<Utterance>,
    text: &str,
    now: Timestamp,
    window_s: u64,
) -> (Vec<Utterance>, Option<Utterance>) {
    let mut kept = expire(utterances, now, window_s);
    let (found, count) = search(&kept, text);
    proof! {
        assert(kept@.take(kept@.len() as int) =~= kept@);
        lemma_all_fresh(utterances@, now, window_s);
    }
    match found {
        Some(position) if count == 1 => {
            proof! {
                lemma_removed(kept@, position as int, same_at(text@));
            }
            let utterance = kept.remove(position);
            (kept, Some(utterance))
        }
        _ => (kept, None),
    }
}

/// The position of the last utterance of `utterances` with the normalized text of `text`, and
/// the number of such utterances.
#[verus_spec(searched =>
    ensures
        counted(utterances@, text@, utterances@.len() as int, searched.0, searched.1),
)]
fn search(utterances: &[Utterance], text: &str) -> (Option<usize>, usize) {
    let wanted = intent::normalize(text);
    let (mut found, mut count, mut index): (Option<usize>, usize, usize) = (None, 0, 0);
    #[cfg_attr(verus_only, verus_spec(
        invariant
            wanted@ == intent::normalized(text@),
            index <= utterances@.len(),
            counted(utterances@, text@, index as int, found, count),
        decreases utterances@.len() - index,
    ))]
    while index < utterances.len() {
        proof! {
            lemma_filter_step(utterances@, index as int, same_at(text@));
        }
        if intent::normalize(utterances[index].text.as_str()).as_str() == wanted.as_str() {
            found = Some(index);
            count += 1;
        }
        index += 1;
    }
    (found, count)
}

/// The domains of the entities whose names and aliases are hotwords.
#[verus_verify]
#[expect(
    clippy::redundant_static_lifetimes,
    reason = "Verus makes a function of the constant, which needs the lifetime"
)]
pub const HOTWORD_DOMAINS: [&'static str; 10] = [
    "automation",
    "climate",
    "cover",
    "fan",
    "light",
    "lock",
    "media_player",
    "scene",
    "script",
    "switch",
];

/// The second-pass hotwords: the names and aliases of the areas, the names and aliases of the
/// entities of `HOTWORD_DOMAINS` that are not skipped, the verbs of `catalog`, then `music`.
#[verus_spec(words =>
    ensures
        views(words@) == area_words(cache.areas@, cache.areas@.len() as int) + entity_words(*cache, cache.entities@, cache.entities@.len() as int)
            + verb_words(catalog.intents@, catalog.intents@.len() as int) + views(music@),
)]
#[must_use]
pub fn hotwords(cache: &StateCache, catalog: &Catalog, music: &[String]) -> Vec<String> {
    let mut words = Vec::new();
    push_area_words(&mut words, cache);
    push_entity_words(&mut words, cache);
    push_verb_words(&mut words, catalog);
    push_words(&mut words, music);
    proof! {
        assert(views(Seq::empty()) =~= Seq::empty());
    }
    words
}

/// Appends the name and then the aliases of each area of `cache` to `words`.
#[verus_spec(
    ensures
        views(final(words)@) == views(old(words)@) + area_words(cache.areas@, cache.areas@.len() as int),
)]
fn push_area_words(words: &mut Vec<String>, cache: &StateCache) {
    let mut index = 0;
    #[cfg_attr(verus_only, verus_spec(
        invariant
            index <= cache.areas@.len(),
            views(words@) == views(old(words)@) + area_words(cache.areas@, index as int),
        decreases cache.areas@.len() - index,
    ))]
    while index < cache.areas.len() {
        let area = &cache.areas[index];
        push_named(words, area.name.as_str(), &area.aliases);
        index += 1;
    }
}

/// Appends the name and then the aliases of each entity of `cache` that is in a domain of
/// `HOTWORD_DOMAINS` and is not skipped to `words`.
#[verus_spec(
    ensures
        views(final(words)@) == views(old(words)@) + entity_words(*cache, cache.entities@, cache.entities@.len() as int),
)]
fn push_entity_words(words: &mut Vec<String>, cache: &StateCache) {
    let mut index = 0;
    #[cfg_attr(verus_only, verus_spec(
        invariant
            index <= cache.entities@.len(),
            views(words@) == views(old(words)@) + entity_words(*cache, cache.entities@, index as int),
        decreases cache.entities@.len() - index,
    ))]
    while index < cache.entities.len() {
        let entity = &cache.entities[index];
        if is_hotword_domain(entity.entity_id.0.as_str()) && !select::is_skipped(cache, entity) {
            push_named(words, entity.name.as_str(), &entity.aliases);
        }
        index += 1;
    }
}

/// Appends the verbs of each intent of `catalog` to `words`.
#[verus_spec(
    ensures
        views(final(words)@) == views(old(words)@) + verb_words(catalog.intents@, catalog.intents@.len() as int),
)]
fn push_verb_words(words: &mut Vec<String>, catalog: &Catalog) {
    let mut index = 0;
    #[cfg_attr(verus_only, verus_spec(
        invariant
            index <= catalog.intents@.len(),
            views(words@) == views(old(words)@) + verb_words(catalog.intents@, index as int),
        decreases catalog.intents@.len() - index,
    ))]
    while index < catalog.intents.len() {
        push_words(words, &catalog.intents[index].verbs);
        index += 1;
    }
}

/// Appends `name` and then `aliases` to `words`.
#[verus_spec(
    ensures
        views(final(words)@) == views(old(words)@) + seq![name@] + views(aliases@),
)]
fn push_named(words: &mut Vec<String>, name: &str, aliases: &[String]) {
    words.push(name.to_owned());
    proof! {
        assert(views(words@) =~= views(old(words)@) + seq![name@]);
    }
    push_words(words, aliases);
}

/// Appends a copy of each phrase of `phrases` to `words`.
#[verus_spec(
    ensures
        views(final(words)@) == views(old(words)@) + views(phrases@),
)]
fn push_words(words: &mut Vec<String>, phrases: &[String]) {
    words.extend_from_slice(phrases);
    proof! {
        assert(views(words@) =~= views(old(words)@) + views(phrases@));
    }
}

/// True when the entity ID `entity_id` is in a domain of `HOTWORD_DOMAINS`.
#[verus_spec(found =>
    ensures
        found == hotword_domain(entity_id@),
)]
fn is_hotword_domain(entity_id: &str) -> bool {
    let mut index = 0;
    #[cfg_attr(verus_only, verus_spec(
        invariant
            index <= HOTWORD_DOMAINS@.len(),
            forall|earlier: int| 0 <= earlier < index
                ==> !crate::domain::state_cache::in_domain(entity_id@, #[trigger] HOTWORD_DOMAINS@[earlier]@),
        decreases HOTWORD_DOMAINS@.len() - index,
    ))]
    while index < HOTWORD_DOMAINS.len() {
        if StateCache::in_domain(entity_id, HOTWORD_DOMAINS[index]) {
            return true;
        }
        index += 1;
    }
    false
}

/// The config entry of the first entity of `cache` with a config entry and a state with the
/// attribute `mass_player_type`: the config entry of Music Assistant.
#[verus_spec(entry =>
    ensures
        match entry {
            Some(found) => exists|index: int| #[trigger] first_music(*cache, index)
                && (cache.entities@[index].config_entry_id matches Some(id) && id@ == found@),
            None => forall|index: int| 0 <= index < cache.entities@.len()
                ==> !music_player(*cache, #[trigger] cache.entities@[index]),
        },
)]
#[must_use]
pub fn music_entry(cache: &StateCache) -> Option<&str> {
    let mut index = 0;
    #[cfg_attr(verus_only, verus_spec(
        invariant
            index <= cache.entities@.len(),
            forall|earlier: int| 0 <= earlier < index
                ==> !music_player(*cache, #[trigger] cache.entities@[earlier]),
        decreases cache.entities@.len() - index,
    ))]
    while index < cache.entities.len() {
        let entity = &cache.entities[index];
        let typed = match cache.states.get(entity.entity_id.0.as_str()) {
            Some(state) => state.attributes.get("mass_player_type").is_some(),
            None => false,
        };
        match &entity.config_entry_id {
            Some(entry) if typed => {
                proof! {
                    assert(first_music(*cache, index as int));
                }
                return Some(entry.as_str());
            }
            _ => index += 1,
        }
    }
    None
}

verus! {

broadcast use {
    crate::domain::contracts::axiom_arc_str_obeys_cmp,
    crate::domain::contracts::axiom_arc_str_borrow,
    crate::domain::contracts::axiom_arc_str_contains_borrowed_key,
    crate::domain::contracts::axiom_arc_str_maps_borrowed_key,
};

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
            assert forall|position: int| 0 <= position < done.len() + 1
                implies #[trigger] complete(done.push(ended)[position]@) by {
                if position < done.len() {
                    assert(complete(done[position]@));
                }
            }
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
        assert(-1 * size + size == 0) by (nonlinear_arith);
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

/// True when `utterance` is not older than `window_s` at `now`.
pub open spec fn fresh(utterance: Utterance, now: Timestamp, window_s: u64) -> bool {
    crate::domain::contracts::nanosecond(now) - crate::domain::contracts::nanosecond(utterance.time) <= window_s * 1_000_000_000
}

/// The predicate of the utterances that are not older than `window_s` at `now`.
pub open spec fn fresh_at(now: Timestamp, window_s: u64) -> spec_fn(Utterance) -> bool {
    |utterance: Utterance| fresh(utterance, now, window_s)
}

/// The predicate of the utterances with the normalized text of `text`.
pub open spec fn same_at(text: Seq<char>) -> spec_fn(Utterance) -> bool {
    |utterance: Utterance| intent::normalized(utterance.text@) == intent::normalized(text)
}

/// The utterances of `utterances` that are not older than `window_s` at `now` and have the
/// normalized text of `text`.
pub open spec fn matching(utterances: Seq<Utterance>, text: Seq<char>, now: Timestamp, window_s: u64) -> Seq<Utterance> {
    utterances.filter(fresh_at(now, window_s)).filter(same_at(text))
}

/// The only utterance of `matches`, or `None` when it has none or more than one.
pub open spec fn unique(matches: Seq<Utterance>) -> Option<Utterance> {
    if matches.len() == 1 { Some(matches[0]) } else { None }
}

/// True when `count` of the first `index` utterances of `utterances` have the normalized text of
/// `text`, and `found` is the position of the last of them.
pub open spec fn counted(utterances: Seq<Utterance>, text: Seq<char>, index: int, found: Option<usize>, count: usize) -> bool {
    let matches = utterances.take(index).filter(same_at(text));
    &&& count <= index
    &&& count == matches.len()
    &&& match found {
        Some(position) => position < index && count > 0 && matches.last() == utterances[position as int],
        None => count == 0,
    }
}

/// The filter of the first `index + 1` utterances is the filter of the first `index`, with the
/// utterance at `index` when `keep` holds for it.
proof fn lemma_filter_step(utterances: Seq<Utterance>, index: int, keep: spec_fn(Utterance) -> bool)
    requires
        0 <= index < utterances.len(),
    ensures
        utterances.take(index + 1).filter(keep) == if keep(utterances[index]) {
            utterances.take(index).filter(keep).push(utterances[index])
        } else {
            utterances.take(index).filter(keep)
        },
{
    reveal_with_fuel(Seq::filter, 1);
    assert(utterances.take(index + 1).drop_last() =~= utterances.take(index));
}

/// Each utterance of the filter by `fresh_at`, also without one of them, is fresh.
proof fn lemma_all_fresh(utterances: Seq<Utterance>, now: Timestamp, window_s: u64)
    ensures
        forall|index: int| 0 <= index < utterances.filter(fresh_at(now, window_s)).len()
            ==> fresh(#[trigger] utterances.filter(fresh_at(now, window_s))[index], now, window_s),
        forall|position: int, index: int| 0 <= position < utterances.filter(fresh_at(now, window_s)).len()
            && 0 <= index < utterances.filter(fresh_at(now, window_s)).len() - 1
            ==> fresh(#[trigger] utterances.filter(fresh_at(now, window_s)).remove(position)[index], now, window_s),
{
    broadcast use Seq::lemma_filter_pred;
    let kept = utterances.filter(fresh_at(now, window_s));
    assert forall|position: int, index: int| 0 <= position < kept.len() && 0 <= index < kept.len() - 1
        implies fresh(#[trigger] kept.remove(position)[index], now, window_s) by {
        if index < position {
            assert(kept.remove(position)[index] == kept[index]);
        } else {
            assert(kept.remove(position)[index] == kept[index + 1]);
        }
    }
}

/// Without the only utterance at `position` for which `keep` holds, no utterance is equal to it.
proof fn lemma_removed(utterances: Seq<Utterance>, position: int, keep: spec_fn(Utterance) -> bool)
    requires
        0 <= position < utterances.len(),
        utterances.filter(keep).len() == 1,
        keep(utterances[position]),
    ensures
        !utterances.remove(position).contains(utterances[position]),
{
    broadcast use Seq::filter_distributes_over_add, Seq::lemma_filter_contains;
    let found = utterances[position];
    let (before, after) = (utterances.take(position), utterances.skip(position + 1));
    assert(utterances =~= before + seq![found] + after);
    reveal_with_fuel(Seq::filter, 2);
    assert(seq![found].drop_last() =~= Seq::<Utterance>::empty());
    assert(seq![found].filter(keep) =~= seq![found]);
    assert((before + seq![found] + after).filter(keep) == before.filter(keep) + seq![found].filter(keep) + after.filter(keep));
    let rest = utterances.remove(position);
    assert(rest =~= before + after);
    assert(rest.filter(keep) == before.filter(keep) + after.filter(keep));
    assert(rest.filter(keep).len() == 0);
    if rest.contains(found) {
        let index = choose|index: int| 0 <= index < rest.len() && rest[index] == found;
        assert(rest.filter(keep).contains(rest[index]));
    }
}

/// The texts of `strings`.
pub open spec fn views(strings: Seq<String>) -> Seq<Seq<char>> {
    crate::domain::catalog::views(strings)
}

/// The name and then the aliases of each of the first `count` areas of `areas`, in order.
pub open spec fn area_words(areas: Seq<crate::domain::state::Area>, count: int) -> Seq<Seq<char>>
    decreases count,
{
    if count <= 0 {
        Seq::empty()
    } else {
        let area = areas[count - 1];
        area_words(areas, count - 1) + seq![area.name@] + views(area.aliases@)
    }
}

/// The name and then the aliases of each of the first `count` entities of `entities` that is in
/// a domain of `HOTWORD_DOMAINS` and is not skipped in `cache`, in order.
pub open spec fn entity_words(cache: StateCache, entities: Seq<crate::domain::state::Entity>, count: int) -> Seq<Seq<char>>
    decreases count,
{
    if count <= 0 {
        Seq::empty()
    } else {
        let entity = entities[count - 1];
        let earlier = entity_words(cache, entities, count - 1);
        if hotword_domain(entity.entity_id.0@) && !select::skipped(cache, entity) {
            earlier + seq![entity.name@] + views(entity.aliases@)
        } else {
            earlier
        }
    }
}

/// The verbs of each of the first `count` intents of `intents`, in order.
pub open spec fn verb_words(intents: Seq<crate::domain::catalog::Intent>, count: int) -> Seq<Seq<char>>
    decreases count,
{
    if count <= 0 {
        Seq::empty()
    } else {
        verb_words(intents, count - 1) + views(intents[count - 1].verbs@)
    }
}

/// True when the entity ID `entity_id` is in a domain of `HOTWORD_DOMAINS`.
pub open spec fn hotword_domain(entity_id: Seq<char>) -> bool {
    exists|index: int| 0 <= index < HOTWORD_DOMAINS@.len()
        && crate::domain::state_cache::in_domain(entity_id, #[trigger] HOTWORD_DOMAINS@[index]@)
}

/// True when `entity` has a config entry and its state in `cache` has the attribute
/// `mass_player_type`.
pub open spec fn music_player(cache: StateCache, entity: crate::domain::state::Entity) -> bool {
    let key = crate::domain::contracts::str_key(entity.entity_id.0@);
    &&& entity.config_entry_id is Some
    &&& cache.states@.contains_key(key)
    &&& crate::domain::contracts::attribute(cache.states@[key].attributes, "mass_player_type") is Some
}

/// True when the entity at `index` of `cache` is the first music player of `cache`.
pub open spec fn first_music(cache: StateCache, index: int) -> bool {
    &&& 0 <= index < cache.entities@.len()
    &&& music_player(cache, cache.entities@[index])
    &&& forall|earlier: int| 0 <= earlier < index ==> !music_player(cache, #[trigger] cache.entities@[earlier])
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
    use serde_json::Value;

    use super::*;
    use crate::domain::state::Entity;
    use crate::domain::state::Snapshot;
    use crate::testing::fakes::house;

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

    /// `log.correlate_s` of `jarvis.toml`.
    const WINDOW_S: u64 = 10;

    /// The time `second` seconds after the Unix epoch.
    fn at(second: i64) -> Timestamp {
        Timestamp::from_second(second).expect("a valid time")
    }

    /// An utterance of `text` kept at `second`, with one sample of `sample`.
    fn utterance(text: &str, second: i64, sample: i16) -> Utterance {
        Utterance {
            text: text.to_owned(),
            audio: Audio {
                samples: vec![sample],
                rate: 16_000,
            },
            time: at(second),
            voice_end: None,
        }
    }

    /// AC-12.1-1.
    #[test]
    fn ac_12_1_1_same_normalized_text_in_the_window_correlates() {
        let kept = utterance("Turn on the lights.", 0, 1);
        let other = utterance("turn off the fan", 0, 2);
        let utterances = vec![other.clone(), kept.clone()];
        let (rest, found) = correlate(utterances, "turn on the lights", at(10), WINDOW_S);
        assert_eq!((rest, found), (vec![other], Some(kept)));
    }

    /// AC-12.1-2.
    #[test]
    fn ac_12_1_2_only_match_older_than_the_window_gives_none() {
        let utterances = vec![utterance("turn on the lights", 0, 1)];
        let (_, found) = correlate(utterances, "turn on the lights", at(11), WINDOW_S);
        assert_eq!(found, None);
    }

    /// AC-12.1-3.
    #[test]
    fn ac_12_1_3_two_matches_in_the_window_give_none() {
        let utterances = vec![
            utterance("turn on the lights", 0, 1),
            utterance("Turn on the lights", 5, 2),
        ];
        let (rest, found) = correlate(utterances.clone(), "turn on the lights", at(5), WINDOW_S);
        assert_eq!((rest, found), (utterances, None));
    }

    /// AC-12.1-4.
    #[test]
    fn ac_12_1_4_correlate_removes_utterances_older_than_the_window() {
        let old = utterance("turn off the fan", 0, 1);
        let new = utterance("pause the music", 5, 2);
        let (rest, _) = correlate(
            vec![old, new.clone()],
            "turn on the lights",
            at(11),
            WINDOW_S,
        );
        assert_eq!(rest, vec![new]);
    }

    /// AC-12.1-5.
    #[test]
    fn ac_12_1_5_second_correlate_for_the_same_text_gives_none() {
        let utterances = vec![utterance("turn on the lights", 0, 1)];
        let (rest, first) = correlate(utterances, "turn on the lights", at(1), WINDOW_S);
        let (_, second) = correlate(rest, "turn on the lights", at(1), WINDOW_S);
        assert_eq!(
            (first.is_some(), second),
            (true, None),
            "the first did not match, or the second matched"
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

    /// The catalog with only `music_resume`, with the verbs "Resume" and "Unpause".
    fn resume_catalog() -> Catalog {
        let mut catalog: Catalog =
            toml::from_str(include_str!("../../intents.toml")).expect("intents.toml is a catalog");
        catalog
            .intents
            .retain(|intent| intent.name == "music_resume");
        catalog.intents[0].verbs = vec!["Resume".to_owned(), "Unpause".to_owned()];
        catalog
    }

    /// The hotwords of `snapshot`, the `music_resume` catalog and "Jazzy" and "Radiohead".
    fn words(snapshot: Snapshot) -> Vec<String> {
        let music = ["Jazzy".to_owned(), "Radiohead".to_owned()];
        hotwords(&StateCache::load(snapshot), &resume_catalog(), &music)
    }

    /// The house with `change` on the entity `entity_id`.
    fn changed(entity_id: &str, change: impl FnOnce(&mut Entity)) -> Snapshot {
        let mut snapshot = house();
        let entity = snapshot
            .entities
            .iter_mut()
            .find(|entity| entity.entity_id.0 == entity_id);
        change(entity.unwrap_or_else(|| panic!("the house has no {entity_id}")));
        snapshot
    }

    /// AC-18.18-1.
    #[test]
    fn ac_18_18_1_areas_entities_verbs_then_music() {
        let expected = "Bedroom/Kitchen/Living Room/Office/Bathroom/Garage/Outside/\
            Carson Bedroom/Carson Kitchen/Carson Kitchen Media Player/Carson Living Room/\
            Carson Living Room Media Player/Carson Office/Carson Office Media Player/Thermostat/\
            Office Fan/Office Fan Light/Bedroom Fan Light/Bedroom Lamp Left/Bedroom Lamp Right/\
            Bedroom Corner Lamp/Ali's Living Room Lamp/Louis's Living Room Lamp/\
            Living Room Corner Lamp/Living Room Overhead Light/TV Lamp/Overhead Light/\
            Strawberry Lamp/Desk LED/Office Lamp/Workbench LED/Front Porch Light/Porch Light/\
            Garage Light/Living Room Fan/Apple TV/the TV/plex/Office/Downstairs Speakers/\
            Downstairs Speakers/Office Speakers/Office Speakers/Bathtub Blind/\
            Water Closet Blinds/Bedtime/Resume/Unpause/Jazzy/Radiohead";
        let words = words(house());
        assert_eq!(words.join("/"), expected);
        assert!(!words.contains(&"Kitchen Presence".to_owned()), "{words:?}");
    }

    /// AC-18.18-2.
    #[test]
    fn ac_18_18_2_skipped_entity_is_not_a_hotword() {
        let changes: [fn(&mut Entity); 3] = [
            |entity| entity.entity_category = Some("config".to_owned()),
            |entity| entity.hidden_by = Some("user".to_owned()),
            |entity| entity.disabled_by = Some("user".to_owned()),
        ];
        for change in changes {
            let words = words(changed("fan.office_fan", change));
            assert!(!words.contains(&"Office Fan".to_owned()), "{words:?}");
        }
    }

    /// AC-18.18-3.
    #[test]
    fn ac_18_18_3_music_entry_is_the_entry_of_a_music_assistant_player() {
        let id = "media_player.carson_kitchen";
        let entry =
            |entity: &mut Entity| entity.config_entry_id = Some("music_assistant".to_owned());
        let mut snapshot = changed(id, entry);
        assert_eq!(
            music_entry(&StateCache::load(snapshot.clone())),
            None,
            "no attribute"
        );
        let state = snapshot
            .states
            .iter_mut()
            .find(|state| state.entity_id == id);
        let state = state.unwrap_or_else(|| panic!("the house has no state of {id}"));
        let kind = Value::String("player".to_owned());
        state.attributes.insert("mass_player_type".to_owned(), kind);
        let cache = StateCache::load(snapshot);
        assert_eq!(music_entry(&cache), Some("music_assistant"));
        assert_eq!(
            music_entry(&StateCache::load(house())),
            None,
            "no config entry"
        );
    }
}
