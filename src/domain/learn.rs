//! Learned intents: clusters of the requests that System Two answered.

use dbscan::Classification;
use jiff::Timestamp;
use vstd::prelude::*;

#[cfg(verus_only)]
use crate::domain::contracts::euclidean;

/// One request that System Two answered, with the embedding of its text.
#[verus_verify]
#[derive(Clone, Debug, PartialEq)]
pub struct Point {
    /// The transcript.
    pub text: String,
    /// The time of the request.
    pub time: Timestamp,
    /// True when the request came from a satellite.
    pub spoken: bool,
    /// The embedding of the transcript.
    pub embedding: Vec<f32>,
}

/// A group of close requests.
#[verus_verify]
#[derive(Clone, Debug, PartialEq)]
pub struct Cluster {
    /// The transcripts of the points.
    pub texts: Vec<String>,
    /// The times of the spoken points.
    pub times: Vec<Timestamp>,
    /// The mean of the L2-normalized embeddings of the points.
    pub centroid: Vec<f32>,
}

/// An entry of the learned intents file.
#[verus_verify]
#[derive(Clone, Debug, PartialEq)]
pub struct LearnedIntent {
    /// The intent name.
    pub name: String,
    /// The centroid of the cluster of the entry.
    pub centroid: Vec<f32>,
    /// The HA service, for example `scene.turn_on`.
    pub service: Option<String>,
}

/// The DBSCAN clusters of `points` on their L2-normalized embeddings. Noise points are in no
/// cluster.
#[verus_verify]
#[must_use]
pub fn clusters(points: &[Point], distance: f64, min_points: usize) -> Vec<Cluster> {
    let mut unit = Vec::new();
    let mut embeddings = Vec::new();
    for point in points {
        let mut normal = point.clone();
        normal.embedding = normalized(&point.embedding);
        embeddings.push(normal.embedding.clone());
        unit.push(normal);
    }
    let classes = dbscan::cluster(distance, min_points, &embeddings);
    let mut found = Vec::new();
    for id in 0..unit.len() {
        let members = members(&unit, &classes, id);
        if !members.is_empty() {
            found.push(cluster(&members));
        }
    }
    found
}

/// The points whose class is a core or edge point of the cluster `id`.
#[verus_verify]
fn members(points: &[Point], classes: &[Classification], id: usize) -> Vec<Point> {
    let mut found = Vec::new();
    let mut index = 0;
    #[cfg_attr(verus_only, verus_spec(decreases points@.len() - index))]
    while index < points.len() && index < classes.len() {
        match classes[index] {
            Classification::Core(cluster) | Classification::Edge(cluster) => {
                if cluster == id {
                    found.push(points[index].clone());
                }
            }
            Classification::Noise => {}
        }
        index += 1;
    }
    found
}

/// The cluster of `members`: their texts, the times of the spoken ones and their centroid.
#[verus_spec(found =>
    ensures
        found.times@ == spoken_times(members@),
)]
fn cluster(members: &[Point]) -> Cluster {
    let mut texts = Vec::new();
    let mut times = Vec::new();
    let mut index = 0;
    #[cfg_attr(verus_only, verus_spec(
        invariant
            index <= members@.len(),
            times@ == spoken_times(members@.take(index as int)),
        decreases members@.len() - index,
    ))]
    while index < members.len() {
        texts.push(members[index].text.clone());
        if members[index].spoken {
            times.push(members[index].time);
        }
        proof! {
            assert(members@.take(index + 1).drop_last() =~= members@.take(index as int));
        }
        index += 1;
    }
    proof! {
        assert(members@.take(index as int) =~= members@);
    }
    Cluster {
        texts,
        times,
        centroid: centroid(members),
    }
}

/// The mean of the embeddings of `members`, over the dimensions of the first member.
#[verus_verify]
fn centroid(members: &[Point]) -> Vec<f32> {
    let mut found = Vec::new();
    let Some(first) = members.first() else {
        return found;
    };
    #[expect(
        clippy::cast_precision_loss,
        reason = "a cluster has far fewer than 2^24 points"
    )]
    let count = members.len() as f32;
    for dimension in 0..first.embedding.len() {
        let mut sum = 0.0;
        for member in members {
            if dimension < member.embedding.len() {
                sum += member.embedding[dimension];
            }
        }
        found.push(sum / count);
    }
    found
}

/// `values` divided by their L2 norm.
#[verus_verify]
fn normalized(values: &[f32]) -> Vec<f32> {
    let mut sum = 0.0;
    for value in values {
        sum += *value * *value;
    }
    let length = f32::sqrt(sum);
    let mut found = Vec::new();
    for value in values {
        found.push(*value / length);
    }
    found
}

/// The name of the first entry of `learned`, at any status, whose centroid is within `distance`
/// of the centroid of `cluster`.
#[verus_spec(found =>
    ensures
        match found {
            Some(name) => exists|index: int| #[trigger] first_near(learned@, cluster.centroid@, distance, index)
                && name@ == learned@[index].name@,
            None => forall|index: int| 0 <= index < learned@.len()
                ==> near(cluster.centroid@, #[trigger] learned@[index].centroid@, distance, false),
        },
)]
#[must_use]
pub fn covering<'learned>(
    cluster: &Cluster,
    learned: &'learned [LearnedIntent],
    distance: f64,
) -> Option<&'learned str> {
    let mut index = 0;
    #[cfg_attr(verus_only, verus_spec(
        invariant
            index <= learned@.len(),
            forall|seen: int| 0 <= seen < index
                ==> near(cluster.centroid@, #[trigger] learned@[seen].centroid@, distance, false),
        decreases learned@.len() - index,
    ))]
    while index < learned.len() {
        let entry = &learned[index];
        let apart =
            dbscan::euclidean_distance(cluster.centroid.as_slice(), entry.centroid.as_slice());
        if apart <= distance {
            proof! {
                assert(first_near(learned@, cluster.centroid@, distance, index as int));
            }
            return Some(entry.name.as_str());
        }
        index += 1;
    }
    None
}

/// True when `intent` calls an HA service, so it waits for approval.
#[verus_spec(approval =>
    ensures
        approval == intent.service is Some,
)]
#[must_use]
pub fn needs_approval(intent: &LearnedIntent) -> bool {
    intent.service.is_some()
}

verus! {

/// The times of the spoken points of `points`, in order.
pub open spec fn spoken_times(points: Seq<Point>) -> Seq<Timestamp>
    decreases points.len(),
{
    if points.len() == 0 {
        Seq::empty()
    } else if points.last().spoken {
        spoken_times(points.drop_last()).push(points.last().time)
    } else {
        spoken_times(points.drop_last())
    }
}

/// True when `close` is the result of `<=` of the Euclidean distance of `left` and `right` and
/// `distance`.
pub open spec fn near(left: Seq<f32>, right: Seq<f32>, distance: f64, close: bool) -> bool {
    vstd::std_specs::cmp::le_ensures::<f64>(euclidean(left, right), distance, close)
}

/// True when the entry `index` of `learned` is the first one near `centroid`.
pub open spec fn first_near(learned: Seq<LearnedIntent>, centroid: Seq<f32>, distance: f64, index: int) -> bool {
    &&& 0 <= index < learned.len()
    &&& near(centroid, learned[index].centroid@, distance, true)
    &&& forall|seen: int| 0 <= seen < index ==> near(centroid, #[trigger] learned[seen].centroid@, distance, false)
}

} // verus!

/// Tests of clusters and covering entries.
#[cfg(test)]
mod tests {
    use super::*;

    /// The default `learn.cluster_distance`.
    const DISTANCE: f64 = 0.3;
    /// The default `learn.min_cluster`.
    const MIN_POINTS: usize = 5;

    /// A spoken point at second `second` with `embedding`.
    fn point(second: i64, embedding: [f32; 3]) -> Point {
        Point {
            text: format!("request {second}"),
            time: Timestamp::from_second(second).expect("a valid second"),
            spoken: true,
            embedding: embedding.to_vec(),
        }
    }

    /// `count` close points, at the seconds 0 to `count - 1`.
    fn close(count: u8) -> Vec<Point> {
        let points =
            (0..count).map(|second| point(i64::from(second), [1.0, 0.0, 0.01 * f32::from(second)]));
        points.collect()
    }

    /// A cluster with `centroid` and other texts than the learned entries.
    fn cluster_at(centroid: [f32; 3]) -> Cluster {
        Cluster {
            texts: vec!["tell me something funny".to_owned()],
            times: Vec::new(),
            centroid: centroid.to_vec(),
        }
    }

    /// AC-14.1-1.
    #[test]
    fn ac_14_1_1_six_close_points_make_one_cluster() {
        let mut points = close(6);
        points.push(point(6, [0.0, 1.0, 0.0]));
        let found = clusters(&points, DISTANCE, MIN_POINTS);
        let texts: Vec<String> = close(6).into_iter().map(|point| point.text).collect();
        assert_eq!(found.len(), 1, "one cluster: {found:?}");
        assert_eq!(found[0].texts, texts);
    }

    /// AC-14.1-2.
    #[test]
    fn ac_14_1_2_four_close_points_make_no_cluster() {
        assert_eq!(clusters(&close(4), DISTANCE, MIN_POINTS), Vec::new());
    }

    /// AC-14.1-3.
    #[test]
    fn ac_14_1_3_covering_names_near_entry() {
        let learned = [
            LearnedIntent {
                name: "weather".to_owned(),
                centroid: vec![0.0, 1.0, 0.0],
                service: None,
            },
            LearnedIntent {
                name: "tell_joke".to_owned(),
                centroid: vec![1.0, 0.0, 0.0],
                service: None,
            },
        ];
        let found = covering(&cluster_at([0.99, 0.1, 0.0]), &learned, DISTANCE);
        assert_eq!(found, Some("tell_joke"));
    }

    /// AC-14.1-4.
    #[test]
    fn ac_14_1_4_covering_without_near_entry_is_none() {
        let learned = [LearnedIntent {
            name: "weather".to_owned(),
            centroid: vec![0.0, 1.0, 0.0],
            service: None,
        }];
        let found = covering(&cluster_at([1.0, 0.0, 0.0]), &learned, DISTANCE);
        assert_eq!(found, None);
    }

    /// AC-14.1-5.
    #[test]
    fn ac_14_1_5_times_are_spoken_times() {
        let mut points = close(6);
        points[2].spoken = false;
        let found = clusters(&points, DISTANCE, MIN_POINTS);
        let times: Vec<Timestamp> = [0, 1, 3, 4, 5]
            .map(|second| Timestamp::from_second(second).expect("a valid second"))
            .to_vec();
        assert_eq!(found.len(), 1, "one cluster: {found:?}");
        assert_eq!(found[0].times, times);
    }

    /// AC-14.2-4.
    #[test]
    fn ac_14_2_4_service_needs_approval() {
        let mut intent = LearnedIntent {
            name: "good_night".to_owned(),
            centroid: Vec::new(),
            service: Some("scene.turn_on".to_owned()),
        };
        assert!(
            needs_approval(&intent),
            "an intent with a service needs approval"
        );
        intent.service = None;
        assert!(
            !needs_approval(&intent),
            "an intent without a service is active at once"
        );
    }
}
