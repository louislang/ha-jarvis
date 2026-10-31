//! Softmax of model outputs.

use vstd::prelude::*;

/// The probabilities of `logits`: `e^(x - max) / sum` for each logit `x`.
#[verus_spec(values =>
    ensures
        softmaxed(logits@, values@),
)]
#[must_use]
pub fn softmax(logits: &[f32]) -> Vec<f32> {
    let max = maximum(logits);
    let (powers, sum) = exponentials(logits, max);
    let mut values = Vec::new();
    let mut index = 0;
    #[cfg_attr(verus_only, verus_spec(
        invariant
            index <= powers@.len(),
            powers@.len() == logits@.len(),
            divided(powers@.take(index as int), sum, values@),
        decreases powers@.len() - index,
    ))]
    while index < powers.len() {
        proof! {
            assert(powers@.take(index + 1).drop_last() =~= powers@.take(index as int));
        }
        values.push(powers[index] / sum);
        index += 1;
    }
    proof! {
        assert(powers@.take(index as int) =~= powers@);
        assert(softmax_of(logits@, max, powers@, sum, values@));
    }
    values
}

/// The largest of `logits`, or 0 for no logits.
#[verus_spec(max =>
    ensures
        peak(logits@, max),
)]
fn maximum(logits: &[f32]) -> f32 {
    let mut max = 0.0;
    let mut index = 0;
    #[cfg_attr(verus_only, verus_spec(
        invariant
            index <= logits@.len(),
            peak(logits@.take(index as int), max),
        decreases logits@.len() - index,
    ))]
    while index < logits.len() {
        let logit = logits[index];
        let greater = logit > max;
        proof! {
            assert(logits@.take(index + 1).drop_last() =~= logits@.take(index as int));
            assert(vstd::std_specs::cmp::gt_ensures::<f32>(logits@.take(index + 1).last(), max, greater));
        }
        if index == 0 || greater {
            max = logit;
        }
        index += 1;
    }
    proof! {
        assert(logits@.take(index as int) =~= logits@);
    }
    max
}

/// `e^(x - max)` for each logit `x`, and the sum of these powers.
#[verus_spec(result =>
    ensures
        exponents(logits@, max, result.0@),
        summed(result.0@, result.1),
)]
fn exponentials(logits: &[f32], max: f32) -> (Vec<f32>, f32) {
    let mut powers = Vec::new();
    let mut sum = 0.0;
    let mut index = 0;
    #[cfg_attr(verus_only, verus_spec(
        invariant
            index <= logits@.len(),
            exponents(logits@.take(index as int), max, powers@),
            summed(powers@, sum),
        decreases logits@.len() - index,
    ))]
    while index < logits.len() {
        let power = (logits[index] - max).exp();
        let next = sum + power;
        proof! {
            assert(powers@.push(power).drop_last() =~= powers@);
            assert(powers@.push(power).last() == power);
            assert(summed(powers@.push(power), next));
            assert(logits@.take(index + 1).drop_last() =~= logits@.take(index as int));
        }
        sum = next;
        powers.push(power);
        index += 1;
    }
    proof! {
        assert(logits@.take(index as int) =~= logits@);
    }
    (powers, sum)
}

verus! {

/// True when `max` is the result of the comparisons of `maximum` over `logits`.
pub open spec fn peak(logits: Seq<f32>, max: f32) -> bool
    decreases logits.len(),
{
    if logits.len() <= 1 {
        max == if logits.len() == 0 { 0.0f32 } else { logits[0] }
    } else {
        exists|before: f32, greater: bool| #[trigger] vstd::std_specs::cmp::gt_ensures::<f32>(logits.last(), before, greater)
            && peak(logits.drop_last(), before)
            && max == if greater { logits.last() } else { before }
    }
}

/// True when `power` is `e^(logit - max)`.
pub open spec fn exponent(logit: f32, max: f32, power: f32) -> bool {
    exists|difference: f32| #[trigger] vstd::std_specs::ops::sub_ensures::<f32>(logit, max, difference)
        && crate::domain::contracts::exp_ensures(difference, power)
}

/// True when each of `powers` is `e^(logit - max)` of its logit.
pub open spec fn exponents(logits: Seq<f32>, max: f32, powers: Seq<f32>) -> bool {
    &&& powers.len() == logits.len()
    &&& forall|index: int| 0 <= index < logits.len() ==> exponent(logits[index], max, #[trigger] powers[index])
}

/// True when `sum` is the sum of `powers` from the left.
pub open spec fn summed(powers: Seq<f32>, sum: f32) -> bool
    decreases powers.len(),
{
    if powers.len() == 0 {
        sum == 0.0f32
    } else {
        exists|before: f32| #[trigger] vstd::std_specs::ops::add_ensures::<f32>(before, powers.last(), sum)
            && summed(powers.drop_last(), before)
    }
}

/// True when each of `values` is its power divided by `sum`.
pub open spec fn divided(powers: Seq<f32>, sum: f32, values: Seq<f32>) -> bool {
    &&& values.len() == powers.len()
    &&& forall|index: int| 0 <= index < powers.len() ==> vstd::std_specs::ops::div_ensures::<f32>(powers[index], sum, #[trigger] values[index])
}

/// True when `values` is the softmax of `logits` with the maximum `max`, the powers `powers` and
/// their sum `sum`.
pub open spec fn softmax_of(logits: Seq<f32>, max: f32, powers: Seq<f32>, sum: f32, values: Seq<f32>) -> bool {
    &&& peak(logits, max)
    &&& exponents(logits, max, powers)
    &&& summed(powers, sum)
    &&& divided(powers, sum, values)
}

/// True when `values` is the softmax of `logits`.
pub open spec fn softmaxed(logits: Seq<f32>, values: Seq<f32>) -> bool {
    exists|max: f32, powers: Seq<f32>, sum: f32| #[trigger] softmax_of(logits, max, powers, sum, values)
}

} // verus!

/// Tests of softmax.
#[cfg(test)]
mod tests {
    use super::*;

    /// AC-3.11-1.
    #[test]
    fn ac_3_11_1_softmax_values_are_probabilities() {
        let values = softmax(&[2.0, -1.0, 0.5, 0.5]);
        let sum: f32 = values.iter().sum();
        assert!(
            values.iter().all(|value| (0.0..=1.0).contains(value)),
            "{values:?}"
        );
        assert!((sum - 1.0).abs() <= 0.0001, "sum {sum}");
    }

    /// AC-3.11-2.
    #[test]
    fn ac_3_11_2_softmax_of_large_logits_is_finite() {
        let values = softmax(&[1000.0, 0.0]);
        assert!(
            values.len() == 2 && values.iter().all(|value| value.is_finite()),
            "{values:?}"
        );
    }
}
