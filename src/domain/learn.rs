//! Learned intents: clusters of the requests that System Two answered, and proposal rules.

use std::collections::BTreeMap;
use std::collections::BTreeSet;

use dbscan::Classification;
use jiff::Timestamp;
use vstd::prelude::*;

use crate::domain::catalog;
use crate::domain::catalog::Catalog;
use crate::domain::catalog::Filter;
use crate::domain::catalog::Kind;
use crate::domain::catalog::Target;
use crate::domain::intent;
use crate::domain::intent::Rejection;
use crate::domain::responses;

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

/// An entry of the learned intents file, or a proposal of System Two.
#[verus_verify]
#[derive(Clone, Debug, PartialEq)]
pub struct LearnedIntent {
    /// The intent name.
    pub name: String,
    /// The centroid of the cluster of the entry.
    pub centroid: Vec<f32>,
    /// The HA service, for example `scene.turn_on`.
    pub service: Option<String>,
    /// How Jarvis replies and acts, or `none` for a cluster that is not suitable.
    pub kind: LearnedKind,
    /// The examples in `[value](slot)` markup.
    pub examples: Vec<String>,
    /// The response templates of the pool.
    pub responses: Vec<String>,
    /// The tool name of the plugin of a fetch.
    pub plugin: Option<String>,
    /// The entities that the service gets.
    pub target: Option<Target>,
    /// The entity template, used instead of `select`.
    pub entity: Option<String>,
    /// The entity filters.
    pub select: Vec<Filter>,
    /// The service data templates, or the plugin argument templates of a fetch.
    pub data: BTreeMap<String, String>,
    /// The question that a prefetch asks System Two.
    pub question: Option<String>,
    /// How long the answer of a fetch or prefetch stays fresh, in minutes.
    pub fresh_min: Option<u32>,
    /// The remote data that a new plugin must give, in one line.
    pub needs: Option<String>,
}

/// The kind of a learned intent.
#[verus_verify]
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum LearnedKind {
    /// Reply from the pool and call the optional service.
    Command,
    /// Reply with data from the hot cache.
    Query,
    /// Fetch from a plugin and announce the answer.
    Fetch,
    /// Reply with the answer that System Two gave before the usual time.
    Prefetch,
    /// The requests need remote data that no plugin gives.
    NewPlugin,
    /// The requests are not suitable for an intent.
    None,
}

/// The reason that `validate_proposal` refuses a proposal.
#[verus_verify]
#[must_use]
#[derive(Clone, Debug, PartialEq)]
pub enum ProposalError {
    /// An example fails `validate_example`.
    Example(Rejection),
    /// The field with this name fails its check.
    Field(&'static str),
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

/// `proposal` when its name, examples and responses pass the checks. `plugins` are the tool
/// names of the plugin registry.
///
/// # Errors
///
/// The first check that fails.
#[verus_spec(result =>
    ensures
        result is Ok ==> proposal.examples@.len() >= catalog.min_examples
            && new_name(proposal.name@, catalog.intents@, learned@),
)]
pub fn validate_proposal(
    proposal: LearnedIntent,
    catalog: &Catalog,
    learned: &[LearnedIntent],
    _plugins: &BTreeSet<String>,
) -> Result<LearnedIntent, ProposalError> {
    check_name(proposal.name.as_str(), catalog, learned)?;
    if proposal.examples.len() < catalog.min_examples {
        return Err(ProposalError::Field("examples"));
    }
    let slots = check_examples(&proposal.examples)?;
    if let Some(kind) = pool_kind(proposal.kind) {
        let target = proposal.target.unwrap_or_default();
        let allowed = responses::pool_variables(slots, kind, target);
        for response in &proposal.responses {
            check_template(response.as_str(), &allowed, "responses")?;
        }
    }
    Ok(proposal)
}

/// Checks that `name` is snake case, is not `unknown` and names no intent of `catalog` and no
/// entry of `learned`.
#[verus_spec(result =>
    ensures
        result is Ok ==> new_name(name@, catalog.intents@, learned@),
)]
fn check_name(
    name: &str,
    catalog: &Catalog,
    learned: &[LearnedIntent],
) -> Result<(), ProposalError> {
    let unused = !intent_named(name, &catalog.intents) && !entry_named(name, learned);
    if name == "unknown" || !snake(name) || !unused {
        return Err(ProposalError::Field("name"));
    }
    Ok(())
}

/// True when an intent of `intents` has the name `name`.
#[verus_spec(found =>
    ensures
        !found ==> forall|index: int| 0 <= index < intents@.len() ==> #[trigger] intents@[index].name@ != name@,
)]
fn intent_named(name: &str, intents: &[catalog::Intent]) -> bool {
    let mut index = 0;
    #[cfg_attr(verus_only, verus_spec(
        invariant
            index <= intents@.len(),
            forall|seen: int| 0 <= seen < index ==> #[trigger] intents@[seen].name@ != name@,
        decreases intents@.len() - index,
    ))]
    while index < intents.len() {
        if intents[index].name.as_str() == name {
            return true;
        }
        index += 1;
    }
    false
}

/// True when an entry of `learned` has the name `name`.
#[verus_spec(found =>
    ensures
        !found ==> forall|index: int| 0 <= index < learned@.len() ==> #[trigger] learned@[index].name@ != name@,
)]
fn entry_named(name: &str, learned: &[LearnedIntent]) -> bool {
    let mut index = 0;
    #[cfg_attr(verus_only, verus_spec(
        invariant
            index <= learned@.len(),
            forall|seen: int| 0 <= seen < index ==> #[trigger] learned@[seen].name@ != name@,
        decreases learned@.len() - index,
    ))]
    while index < learned.len() {
        if learned[index].name.as_str() == name {
            return true;
        }
        index += 1;
    }
    false
}

/// True when `name` has only ASCII lowercase letters, digits and `_`, with a letter first.
#[verus_verify]
fn snake(name: &str) -> bool {
    let chars: Vec<char> = name.chars().collect();
    let mut index = 0;
    #[cfg_attr(verus_only, verus_spec(decreases chars@.len() - index))]
    while index < chars.len() {
        let c = chars[index];
        if !(c.is_ascii_lowercase() || (index > 0 && (c.is_ascii_digit() || c == '_'))) {
            return false;
        }
        index += 1;
    }
    index > 0
}

/// The slots of `examples` when each one passes `validate_example` with them and no seeds.
#[verus_verify]
fn check_examples(examples: &[String]) -> Result<BTreeSet<String>, ProposalError> {
    let mut slots = BTreeSet::new();
    for markup in examples {
        if let Ok(example) = intent::parse_example(markup.as_str()) {
            for span in example.spans {
                slots.insert(span.slot);
            }
        }
    }
    let seeds = BTreeSet::new();
    for markup in examples {
        if let Err(rejection) = intent::validate_example(markup.as_str(), &slots, &seeds) {
            return Err(ProposalError::Example(rejection));
        }
    }
    Ok(slots)
}

/// The catalog kind whose pool rules the responses of a proposal of `kind` follow, or none when
/// the responses get no check.
#[verus_verify]
fn pool_kind(kind: LearnedKind) -> Option<Kind> {
    match kind {
        LearnedKind::Command => Some(Kind::Command),
        LearnedKind::Query => Some(Kind::Query),
        LearnedKind::Fetch => Some(Kind::Fetch),
        LearnedKind::Prefetch => Some(Kind::Prefetch),
        LearnedKind::NewPlugin | LearnedKind::None => None,
    }
}

/// Checks that `template` parses and uses only `allowed` variables. The error names `field`.
#[verus_verify]
fn check_template(
    template: &str,
    allowed: &BTreeSet<String>,
    field: &'static str,
) -> Result<(), ProposalError> {
    match catalog::unknown_variable(template, allowed) {
        Ok(None) => Ok(()),
        _ => Err(ProposalError::Field(field)),
    }
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

broadcast use crate::domain::contracts::axiom_string_obeys_cmp;

/// True when `name` is not `unknown` and no intent of `intents` or entry of `learned` has it.
pub open spec fn new_name(name: Seq<char>, intents: Seq<catalog::Intent>, learned: Seq<LearnedIntent>) -> bool {
    &&& name != "unknown"@
    &&& forall|index: int| 0 <= index < intents.len() ==> #[trigger] intents[index].name@ != name
    &&& forall|index: int| 0 <= index < learned.len() ==> #[trigger] learned[index].name@ != name
}

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

    /// A proposal of `kind` with valid names, examples and responses and no other fields.
    fn proposal(kind: LearnedKind) -> LearnedIntent {
        LearnedIntent {
            name: "good_night".to_owned(),
            centroid: Vec::new(),
            service: None,
            kind,
            examples: ["good night", "night night", "time for bed"]
                .map(str::to_owned)
                .to_vec(),
            responses: vec!["Good night.".to_owned()],
            plugin: None,
            target: None,
            entity: None,
            select: Vec::new(),
            data: BTreeMap::new(),
            question: None,
            fresh_min: None,
            needs: None,
        }
    }

    /// A learned entry `name` with `centroid`.
    fn entry(name: &str, centroid: [f32; 3]) -> LearnedIntent {
        let mut entry = proposal(LearnedKind::None);
        entry.name = name.to_owned();
        entry.centroid = centroid.to_vec();
        entry
    }

    /// `validate_proposal` of `intents.toml`, `learned` and the plugin `get_weather`.
    fn check(
        proposal: LearnedIntent,
        learned: &[LearnedIntent],
    ) -> Result<LearnedIntent, ProposalError> {
        let catalog: Catalog =
            toml::from_str(include_str!("../../intents.toml")).expect("intents.toml is a catalog");
        let plugins = BTreeSet::from(["get_weather".to_owned()]);
        validate_proposal(proposal, &catalog, learned, &plugins)
    }

    /// The error of `check` after `change` modifies a valid proposal of `kind`.
    fn refused(kind: LearnedKind, change: impl FnOnce(&mut LearnedIntent)) -> ProposalError {
        let mut changed = proposal(kind);
        change(&mut changed);
        check(changed, &[]).expect_err("the proposal is not valid")
    }

    /// The error that names `name`.
    fn field(name: &'static str) -> ProposalError {
        ProposalError::Field(name)
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
            entry("weather", [0.0, 1.0, 0.0]),
            entry("tell_joke", [1.0, 0.0, 0.0]),
        ];
        let found = covering(&cluster_at([0.99, 0.1, 0.0]), &learned, DISTANCE);
        assert_eq!(found, Some("tell_joke"));
    }

    /// AC-14.1-4.
    #[test]
    fn ac_14_1_4_covering_without_near_entry_is_none() {
        let learned = [entry("weather", [0.0, 1.0, 0.0])];
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
        let mut intent = proposal(LearnedKind::Command);
        intent.service = Some("scene.turn_on".to_owned());
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

    /// AC-14.2b-1.
    #[test]
    fn ac_14_2b_1_too_few_examples_is_an_error() {
        let error = refused(LearnedKind::None, |proposal| proposal.examples.truncate(2));
        assert_eq!(error, field("examples"));
    }

    /// AC-14.2b-2.
    #[test]
    fn ac_14_2b_2_markup_error_is_an_error() {
        let error = refused(LearnedKind::None, |proposal| {
            proposal.examples[0] = "good [night".to_owned();
        });
        assert!(
            matches!(error, ProposalError::Example(Rejection::Markup(_))),
            "expected a markup error: {error:?}"
        );
    }

    /// AC-14.2b-3.
    #[test]
    fn ac_14_2b_3_existing_or_unknown_name_is_an_error() {
        for name in ["lights_on", "unknown", "Good night"] {
            let error = refused(LearnedKind::None, |proposal| {
                proposal.name = name.to_owned();
            });
            assert_eq!(error, ProposalError::Field("name"), "{name}");
        }
        let learned = [entry("good_night", [1.0, 0.0, 0.0])];
        let result = check(proposal(LearnedKind::None), &learned);
        assert_eq!(result, Err(ProposalError::Field("name")));
    }

    /// AC-14.2b-4.
    #[test]
    fn ac_14_2b_4_responses_use_pool_variables() {
        let mut query = proposal(LearnedKind::Query);
        query.target = Some(Target::Entities);
        query.responses = vec!["{{ entities | length }} lights are on.".to_owned()];
        assert_eq!(check(query.clone(), &[]), Ok(query));
        let error = refused(LearnedKind::Command, |proposal| {
            proposal.responses = vec!["It is {{ response }}.".to_owned()];
        });
        assert_eq!(error, field("responses"));
    }
}
