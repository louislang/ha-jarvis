//! Audio type, audio chunks and audio duration.

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

verus! {

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
}
