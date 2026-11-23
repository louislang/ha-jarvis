//! Learned intents: clusters of the requests that System Two answered, and proposal rules.

use std::collections::BTreeMap;
use std::collections::BTreeSet;

use dbscan::Classification;
use jiff::Timestamp;
use jiff::tz::TimeZone;
use serde::Deserialize;
use serde::Serialize;
use vstd::prelude::*;

use crate::domain::catalog;
use crate::domain::catalog::Catalog;
use crate::domain::catalog::Filter;
use crate::domain::catalog::Intent;
use crate::domain::catalog::Kind;
use crate::domain::catalog::Target;
use crate::domain::habits;
use crate::domain::habits::Bucket;
use crate::domain::habits::DayType;
use crate::domain::intent;
use crate::domain::intent::Rejection;
use crate::domain::responses;

#[cfg(verus_only)]
use crate::domain::contracts::euclidean;
#[cfg(verus_only)]
use crate::domain::contracts::nanosecond;
#[cfg(verus_only)]
use crate::domain::contracts::timestamp_at;
#[cfg(verus_only)]
use crate::domain::habits::bucket_at;

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
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
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
    /// Where the entry is in its lifecycle.
    pub status: LearnedStatus,
    /// The texts of the cluster of the entry.
    pub texts: Vec<String>,
    /// The path of the plugin request file.
    pub request_file: Option<String>,
    /// The time of the activation.
    pub activated: Option<Timestamp>,
}

/// Where a learned entry is in its lifecycle.
#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum LearnedStatus {
    /// Waits for your approval.
    Pending,
    /// An intent.
    Active,
    /// You rejected it.
    Rejected,
    /// Not an intent: the cluster is not suitable or the proposal is not valid.
    #[default]
    None,
    /// Waits for a new plugin.
    Requested,
}

/// A request to Claude for a new plugin.
#[verus_verify]
#[derive(Clone, Debug, PartialEq)]
pub struct PluginRequest {
    /// The name of the learned entry, which is the file name.
    pub name: String,
    /// The Markdown text.
    pub text: String,
}

/// The learned intents file.
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize)]
pub struct Learned {
    /// The entries, in file order.
    pub intents: Vec<LearnedIntent>,
}

/// The kind of a learned intent.
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
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
            && new_name(proposal.name@, catalog.intents@, learned@)
            && kind_valid(proposal, plugins@),
)]
pub fn validate_proposal(
    proposal: LearnedIntent,
    catalog: &Catalog,
    learned: &[LearnedIntent],
    plugins: &BTreeSet<String>,
) -> Result<LearnedIntent, ProposalError> {
    check_name(proposal.name.as_str(), catalog, learned)?;
    if proposal.examples.len() < catalog.min_examples {
        return Err(ProposalError::Field("examples"));
    }
    let slots = check_examples(&proposal.examples)?;
    if let Some(kind) = pool_kind(proposal.kind) {
        let target = proposal.target.unwrap_or_default();
        let allowed = responses::pool_variables(slots.clone(), kind, target);
        for response in &proposal.responses {
            check_template(response.as_str(), &allowed, "responses")?;
        }
    }
    check_kind(&proposal, plugins, !slots.is_empty())?;
    check_kind_templates(&proposal, &slots)?;
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

/// Checks the fields of the kind of `proposal`. `slotted` is true when its examples have a slot.
#[verus_spec(result =>
    ensures
        result is Ok <==> kind_valid(*proposal, plugins@) && !(proposal.kind is Prefetch && slotted),
)]
fn check_kind(
    proposal: &LearnedIntent,
    plugins: &BTreeSet<String>,
    slotted: bool,
) -> Result<(), ProposalError> {
    match proposal.kind {
        LearnedKind::Command => {
            require(proposal.target.is_some(), "target")?;
            require(
                !proposal.select.is_empty() || proposal.entity.is_some(),
                "select",
            )
        }
        LearnedKind::Query => {
            require(!proposal.select.is_empty(), "select")?;
            require(proposal.service.is_none(), "service")
        }
        LearnedKind::Fetch | LearnedKind::Prefetch => check_timed(proposal, plugins, slotted),
        LearnedKind::NewPlugin => check_new_plugin(proposal),
        LearnedKind::None => Ok(()),
    }
}

/// Checks the fields of a `fetch` or `prefetch` proposal. `slotted` is true when its examples have
/// a slot.
#[verus_spec(result =>
    requires
        proposal.kind is Fetch || proposal.kind is Prefetch,
    ensures
        result is Ok <==> kind_valid(*proposal, plugins@) && !(proposal.kind is Prefetch && slotted),
)]
fn check_timed(
    proposal: &LearnedIntent,
    plugins: &BTreeSet<String>,
    slotted: bool,
) -> Result<(), ProposalError> {
    let prefetch = matches!(proposal.kind, LearnedKind::Prefetch);
    if prefetch {
        require(proposal.question.is_some(), "question")?;
    } else {
        let listed = matches!(&proposal.plugin, Some(plugin) if plugins.contains(plugin));
        require(listed, "plugin")?;
    }
    require(proposal.service.is_none(), "service")?;
    let fresh = matches!(proposal.fresh_min, Some(minutes) if minutes > 0);
    require(fresh, "fresh_min")?;
    require(!(prefetch && slotted), "examples")
}

/// Checks that a `new_plugin` proposal has `needs` and no service, plugin or question.
#[verus_spec(result =>
    ensures
        result is Ok <==> (proposal.needs matches Some(needs) && needs@.len() > 0)
            && proposal.service is None && proposal.plugin is None && proposal.question is None,
)]
fn check_new_plugin(proposal: &LearnedIntent) -> Result<(), ProposalError> {
    let needs = matches!(&proposal.needs, Some(needs) if !needs.is_empty());
    require(needs, "needs")?;
    require(proposal.service.is_none(), "service")?;
    require(proposal.plugin.is_none(), "plugin")?;
    require(proposal.question.is_none(), "question")
}

/// `Ok` when `valid`, else the error that names `field`.
#[verus_spec(result =>
    ensures
        result is Ok <==> valid,
)]
fn require(valid: bool, field: &'static str) -> Result<(), ProposalError> {
    if valid {
        Ok(())
    } else {
        Err(ProposalError::Field(field))
    }
}

/// Checks the data templates of a command, and that each response of a fetch uses `response`.
#[verus_verify]
fn check_kind_templates(
    proposal: &LearnedIntent,
    slots: &BTreeSet<String>,
) -> Result<(), ProposalError> {
    let target = proposal.target.unwrap_or_default();
    match proposal.kind {
        LearnedKind::Command => {
            let allowed = responses::pool_variables(slots.clone(), Kind::Command, target);
            for template in proposal.data.values() {
                check_template(template.as_str(), &allowed, "data")?;
            }
        }
        LearnedKind::Fetch => check_uses_response(proposal, slots, target)?,
        _ => {}
    }
    Ok(())
}

/// Checks that each response of a fetch `proposal` with `slots` and `target` uses `response`.
#[verus_verify]
fn check_uses_response(
    proposal: &LearnedIntent,
    slots: &BTreeSet<String>,
    target: Target,
) -> Result<(), ProposalError> {
    let mut others = BTreeSet::new();
    let named = slots.iter();
    for slot in named {
        if slot.as_str() != "response" {
            others.insert(slot.clone());
        }
    }
    // The pool variables of a command are those of a fetch without `response`. The responses use
    // only the pool variables, so the first variable outside `others` is `response` exactly when
    // a response uses it.
    let others = responses::pool_variables(others, Kind::Command, target);
    for response in &proposal.responses {
        match catalog::unknown_variable(response.as_str(), &others) {
            Ok(Some(variable)) if variable.as_str() == "response" => {}
            _ => return Err(ProposalError::Field("responses")),
        }
    }
    Ok(())
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

/// The status of the entry of the valid `proposal`.
#[verus_spec(status =>
    ensures
        status == initial(proposal.kind, proposal.service is Some),
)]
#[must_use]
pub fn initial_status(proposal: &LearnedIntent) -> LearnedStatus {
    match proposal.kind {
        LearnedKind::NewPlugin => LearnedStatus::Requested,
        LearnedKind::None => LearnedStatus::None,
        _ => {
            if needs_approval(proposal) {
                LearnedStatus::Pending
            } else {
                LearnedStatus::Active
            }
        }
    }
}

/// The task of a plugin request after the intent name.
#[verus_verify]
#[expect(
    clippy::redundant_static_lifetimes,
    reason = "Verus makes a function of the constant, which needs the lifetime"
)]
pub const PLUGIN_TASK: &'static str = "` with `plugin` and `fresh_min` to `intents.toml` and its \
pool to `responses.toml`. Write a plugin for this data in the pattern of phase 17: one `Plugin` \
adapter, the change map row \"Add a plugin\" of `docs/spec/01-architecture.md` section 12, and tests \
with a local mock server. Use a free data source without an API key when one exists. Use a source \
with a key only after the owner agrees, so ask first. Then the PR names the environment variable \
and the sign-up page. Open a normal PR.\n";

/// The plugin request of `entry`: its texts, `usual`, its `needs` and the task for Claude.
#[verus_spec(request =>
    requires
        forall|index: int| 0 <= index < usual@.len() ==> 0 <= #[trigger] usual@[index].hour <= 23,
    ensures
        request.name@ == entry.name@,
        request.text@ == request_text(entry.name@, entry.texts@, usual@, entry.needs),
)]
#[must_use]
pub fn plugin_request(entry: &LearnedIntent, usual: &[Bucket]) -> PluginRequest {
    let mut text = "# Plugin request: ".to_owned();
    text.push_str(entry.name.as_str());
    text.push_str("\n\n## Requests\n\n");
    push_lines(&mut text, &entry.texts);
    text.push_str("\n## Usual times\n\n");
    if usual.is_empty() {
        text.push_str("none\n");
    } else {
        push_buckets(&mut text, usual);
    }
    text.push_str("\n## Data needed\n\n");
    if let Some(needs) = &entry.needs {
        text.push_str(needs.as_str());
    }
    text.push_str("\n\n## Task\n\nAdd the fetch intent `");
    text.push_str(entry.name.as_str());
    text.push_str(PLUGIN_TASK);
    PluginRequest {
        name: entry.name.clone(),
        text,
    }
}

/// Appends each text of `texts` to `text` on its own line.
#[verus_spec(
    ensures
        final(text)@ == old(text)@ + lines(texts@),
)]
fn push_lines(text: &mut String, texts: &[String]) {
    let mut index = 0;
    #[cfg_attr(verus_only, verus_spec(
        invariant
            index <= texts@.len(),
            text@ == old(text)@ + lines(texts@.take(index as int)),
        decreases texts@.len() - index,
    ))]
    while index < texts.len() {
        text.push_str(texts[index].as_str());
        text.push('\n');
        proof! {
            assert(texts@.take(index + 1).drop_last() =~= texts@.take(index as int));
            assert(text@ =~= old(text)@ + lines(texts@.take(index + 1)));
        }
        index += 1;
    }
    proof! {
        assert(texts@.take(index as int) =~= texts@);
    }
}

/// Appends the line of each bucket of `usual` to `text`.
#[verus_spec(
    requires
        forall|index: int| 0 <= index < usual@.len() ==> 0 <= #[trigger] usual@[index].hour <= 23,
    ensures
        final(text)@ == old(text)@ + bucket_lines(usual@),
)]
fn push_buckets(text: &mut String, usual: &[Bucket]) {
    let mut index = 0;
    #[cfg_attr(verus_only, verus_spec(
        invariant
            index <= usual@.len(),
            forall|seen: int| 0 <= seen < usual@.len() ==> 0 <= #[trigger] usual@[seen].hour <= 23,
            text@ == old(text)@ + bucket_lines(usual@.take(index as int)),
        decreases usual@.len() - index,
    ))]
    while index < usual.len() {
        push_bucket(text, usual[index]);
        proof! {
            assert(usual@.take(index + 1).drop_last() =~= usual@.take(index as int));
            assert(text@ =~= old(text)@ + bucket_lines(usual@.take(index + 1)));
        }
        index += 1;
    }
    proof! {
        assert(usual@.take(index as int) =~= usual@);
    }
}

/// Appends the line `<weekdays|weekends> <HH>:00` of `bucket` to `text`.
#[verus_spec(
    requires
        0 <= bucket.hour <= 23,
    ensures
        final(text)@ == old(text)@ + bucket_line(bucket),
)]
fn push_bucket(text: &mut String, bucket: Bucket) {
    text.push_str(match bucket.day {
        DayType::Weekdays => "weekdays ",
        DayType::Weekends => "weekends ",
    });
    #[expect(clippy::cast_sign_loss, reason = "a bucket hour is 0 to 23")]
    let hour = bucket.hour as u8;
    text.push((b'0' + hour / 10) as char);
    text.push((b'0' + hour % 10) as char);
    text.push_str(":00\n");
    proof! {
        assert(text@ =~= old(text)@ + bucket_line(bucket));
    }
}

/// The catalog intent of an `active` entry with a response, except a `new_plugin` or `none` entry.
#[verus_verify]
#[must_use]
pub fn intent(entry: LearnedIntent) -> Option<Intent> {
    let kind = pool_kind(entry.kind)?;
    if !matches!(entry.status, LearnedStatus::Active) || entry.responses.is_empty() {
        return None;
    }
    Some(Intent {
        name: entry.name,
        kind,
        examples: entry.examples,
        verbs: Vec::new(),
        needs_speaker: false,
        optional: Vec::new(),
        service: entry.service,
        plugin: entry.plugin,
        target: entry.target.unwrap_or_default(),
        entity: entry.entity,
        select: entry.select,
        exclude: Vec::new(),
        state: None,
        data: entry.data,
        search: false,
        title: None,
        fresh_min: entry.fresh_min,
        timer: None,
    })
}

/// `catalog` with the intent of each entry of `learned` that `catalog` does not name, in order.
#[verus_verify]
#[must_use]
pub fn live(catalog: Catalog, learned: Vec<LearnedIntent>) -> Catalog {
    let mut live = catalog;
    for entry in learned {
        match intent(entry) {
            Some(intent) if !intent_named(intent.name.as_str(), &live.intents) => {
                live.intents.push(intent);
            }
            _ => {}
        }
    }
    live
}

/// The nanoseconds of one minute.
#[verus_verify]
pub const MINUTE_NANOS: i128 = 60_000_000_000;

/// True when `now` is in the window of a time of `usual`: from `lead_min` before its hour to the
/// end of the hour.
#[verus_spec(open =>
    ensures
        open == windowed(usual, now, *zone, lead_min),
)]
#[must_use]
pub fn in_window(usual: &[Bucket], now: Timestamp, zone: &TimeZone, lead_min: u64) -> bool {
    if usual.contains(&habits::bucket(now, zone)) {
        return true;
    }
    let Some(sum) = after(now, lead_min) else {
        return false;
    };
    match Timestamp::from_nanosecond(sum) {
        Ok(later) => usual.contains(&habits::bucket(later, zone)),
        Err(_) => false,
    }
}

/// True when `now` is in the window of a time of `usual`, and no prefetch was made or the last
/// one, at `attempt`, is `fresh_min` or more ago.
#[verus_spec(due =>
    ensures
        due == (windowed(usual, now, *zone, lead_min) && waited(attempt, now, fresh_min)),
)]
#[must_use]
pub fn refresh_due(
    usual: &[Bucket],
    attempt: Option<Timestamp>,
    now: Timestamp,
    zone: &TimeZone,
    lead_min: u64,
    fresh_min: u32,
) -> bool {
    in_window(usual, now, zone, lead_min)
        && match attempt {
            Some(last) => {
                matches!(after(last, u64::from(fresh_min)), Some(due) if now.as_nanosecond() >= due)
            }
            None => true,
        }
}

/// True when the answer of `answer_time` is `fresh_min` or less old at `now`.
#[verus_spec(found =>
    ensures
        found == (nanosecond(now) - nanosecond(answer_time) <= fresh_min * MINUTE_NANOS),
)]
#[must_use]
pub fn fresh(answer_time: Timestamp, now: Timestamp, fresh_min: u32) -> bool {
    match after(answer_time, u64::from(fresh_min)) {
        Some(end) => now.as_nanosecond() <= end,
        None => true,
    }
}

/// The nanosecond of `time` plus `minutes`, or `None` when it overflows.
#[verus_spec(found =>
    ensures
        found == shifted(time, minutes),
)]
fn after(time: Timestamp, minutes: u64) -> Option<i128> {
    time.as_nanosecond()
        .checked_add(i128::from(minutes) * MINUTE_NANOS)
}

verus! {

broadcast use {
    crate::domain::contracts::axiom_string_obeys_cmp,
    crate::domain::contracts::axiom_bucket_obeys_cmp,
    vstd::laws_cmp::group_laws_cmp,
};

/// The status of a valid proposal of `kind`, with a service when `service`.
pub open spec fn initial(kind: LearnedKind, service: bool) -> LearnedStatus {
    match kind {
        LearnedKind::NewPlugin => LearnedStatus::Requested,
        LearnedKind::None => LearnedStatus::None,
        _ => if service { LearnedStatus::Pending } else { LearnedStatus::Active },
    }
}

/// True when `name` is not `unknown` and no intent of `intents` or entry of `learned` has it.
pub open spec fn new_name(name: Seq<char>, intents: Seq<catalog::Intent>, learned: Seq<LearnedIntent>) -> bool {
    &&& name != "unknown"@
    &&& forall|index: int| 0 <= index < intents.len() ==> #[trigger] intents[index].name@ != name
    &&& forall|index: int| 0 <= index < learned.len() ==> #[trigger] learned[index].name@ != name
}

/// The nanosecond of `time` plus `minutes`, or `None` above `i128::MAX`.
pub open spec fn shifted(time: Timestamp, minutes: u64) -> Option<i128> {
    let sum = nanosecond(time) + minutes * MINUTE_NANOS;
    if sum <= i128::MAX { Some(sum as i128) } else { None }
}

/// `time` plus `minutes`, or `None` after the last instant.
pub open spec fn plus(time: Timestamp, minutes: u64) -> Option<Timestamp> {
    match shifted(time, minutes) {
        Some(sum) => timestamp_at(sum),
        None => None,
    }
}

/// True when `usual` has the bucket of `now` or of `now` plus `lead_min` in `zone`.
pub open spec fn windowed(usual: &[Bucket], now: Timestamp, zone: TimeZone, lead_min: u64) -> bool {
    usual.contains(&bucket_at(now, zone))
        || (plus(now, lead_min) matches Some(later) && usual.contains(&bucket_at(later, zone)))
}

/// True when `attempt` is none or `fresh_min` or more before `now`.
pub open spec fn waited(attempt: Option<Timestamp>, now: Timestamp, fresh_min: u32) -> bool {
    match attempt {
        Some(last) => nanosecond(now) - nanosecond(last) >= fresh_min * MINUTE_NANOS,
        None => true,
    }
}

/// True when `minutes` is 1 or more.
pub open spec fn has_fresh_min(minutes: Option<u32>) -> bool {
    minutes matches Some(value) && value > 0
}

/// True when the fields of the kind of `proposal` pass, except the slot check of a prefetch.
pub open spec fn kind_valid(proposal: LearnedIntent, plugins: Set<String>) -> bool {
    match proposal.kind {
        LearnedKind::Command => proposal.target is Some
            && (proposal.select@.len() > 0 || proposal.entity is Some),
        LearnedKind::Query => proposal.select@.len() > 0 && proposal.service is None,
        LearnedKind::Fetch => (proposal.plugin matches Some(plugin) && plugins.contains(plugin))
            && proposal.service is None && has_fresh_min(proposal.fresh_min),
        LearnedKind::Prefetch => proposal.question is Some && proposal.service is None
            && has_fresh_min(proposal.fresh_min),
        LearnedKind::NewPlugin => (proposal.needs matches Some(needs) && needs@.len() > 0)
            && proposal.service is None && proposal.plugin is None && proposal.question is None,
        LearnedKind::None => true,
    }
}

/// The text of a plugin request for `name` with `texts`, `usual` and `needs`: the title line, then
/// the parts with the texts, one line for each bucket of `usual` or `none`, `needs` and the task.
pub open spec fn request_text(name: Seq<char>, texts: Seq<String>, usual: Seq<Bucket>, needs: Option<String>) -> Seq<char> {
    "# Plugin request: "@ + name + "\n\n## Requests\n\n"@ + lines(texts)
        + "\n## Usual times\n\n"@ + (if usual.len() == 0 { "none\n"@ } else { bucket_lines(usual) })
        + "\n## Data needed\n\n"@ + (match needs { Some(needs) => needs@, None => Seq::empty() })
        + "\n\n## Task\n\nAdd the fetch intent `"@ + name + PLUGIN_TASK@
}

/// Each text of `texts` on its own line.
pub open spec fn lines(texts: Seq<String>) -> Seq<char>
    decreases texts.len(),
{
    if texts.len() == 0 { Seq::empty() } else { lines(texts.drop_last()) + texts.last()@.push('\n') }
}

/// One line for each bucket of `usual`.
pub open spec fn bucket_lines(usual: Seq<Bucket>) -> Seq<char>
    decreases usual.len(),
{
    if usual.len() == 0 { Seq::empty() } else { bucket_lines(usual.drop_last()) + bucket_line(usual.last()) }
}

/// The line `<weekdays|weekends> <HH>:00` of `bucket`.
pub open spec fn bucket_line(bucket: Bucket) -> Seq<char> {
    let day = match bucket.day {
        DayType::Weekdays => "weekdays "@,
        DayType::Weekends => "weekends "@,
    };
    day + seq![digit_char(bucket.hour as int / 10), digit_char(bucket.hour as int % 10)] + ":00\n"@
}

/// The ASCII digit of `value`, from 0 to 9.
pub open spec fn digit_char(value: int) -> char {
    ((48 + value) as u8) as char
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
    use crate::domain::habits::DayType;

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

    /// A valid proposal of `kind` without a service.
    fn proposal(kind: LearnedKind) -> LearnedIntent {
        let fetch = kind == LearnedKind::Fetch;
        let timed = fetch || kind == LearnedKind::Prefetch;
        let command = kind == LearnedKind::Command;
        let selected = command || kind == LearnedKind::Query;
        let response = if fetch {
            "It is {{ response.text }}."
        } else {
            "Good night."
        };
        LearnedIntent {
            name: "good_night".to_owned(),
            centroid: Vec::new(),
            service: None,
            kind,
            examples: ["good night", "night night", "time for bed"]
                .map(str::to_owned)
                .to_vec(),
            responses: vec![response.to_owned()],
            plugin: fetch.then(|| "get_weather".to_owned()),
            target: command.then_some(Target::House),
            entity: None,
            select: selected.then(scenes).into_iter().collect(),
            data: BTreeMap::new(),
            question: (kind == LearnedKind::Prefetch).then(|| "Is it bedtime?".to_owned()),
            fresh_min: timed.then_some(30),
            needs: (kind == LearnedKind::NewPlugin).then(|| "bedtime data".to_owned()),
            status: LearnedStatus::None,
            texts: Vec::new(),
            request_file: None,
            activated: None,
        }
    }

    /// The entity filter of the scenes.
    fn scenes() -> Filter {
        Filter {
            domain: "scene".to_owned(),
            name: None,
            device_class: None,
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

    /// A change of a proposal.
    type Change = fn(&mut LearnedIntent);

    /// The error of `check` after `change` modifies a valid proposal of `kind`.
    fn refused(kind: LearnedKind, change: impl FnOnce(&mut LearnedIntent)) -> ProposalError {
        let mut changed = proposal(kind);
        change(&mut changed);
        check(changed, &[]).expect_err("the proposal is not valid")
    }

    /// Asserts that `check` accepts a valid proposal of `kind` after `change`.
    fn passes(kind: LearnedKind, change: Change) {
        let mut changed = proposal(kind);
        change(&mut changed);
        assert_eq!(check(changed.clone(), &[]), Ok(changed));
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

    /// AC-14.2c-1.
    #[test]
    fn ac_14_2c_1_fetch_and_prefetch_fields_are_named() {
        passes(LearnedKind::Fetch, |_| {});
        passes(LearnedKind::Prefetch, |_| {});
        passes(LearnedKind::Fetch, |proposal| {
            proposal.examples[0] = "good night [today](response)".to_owned();
        });
        let fetch = |change: Change| refused(LearnedKind::Fetch, change);
        let prefetch = |change: Change| refused(LearnedKind::Prefetch, change);
        let service: Change = |proposal| proposal.service = Some("light.turn_on".to_owned());
        let traffic: Change = |proposal| proposal.plugin = Some("get_traffic".to_owned());
        let slot: Change = |proposal| proposal.examples[0] = "night [kitchen](area)".to_owned();
        assert_eq!(fetch(|proposal| proposal.plugin = None), field("plugin"));
        assert_eq!(fetch(traffic), field("plugin"));
        assert_eq!(fetch(service), field("service"));
        assert_eq!(prefetch(service), field("service"));
        assert_eq!(
            prefetch(|proposal| proposal.question = None),
            field("question")
        );
        assert_eq!(prefetch(slot), field("examples"));
    }

    /// AC-14.2c-2.
    #[test]
    fn ac_14_2c_2_command_without_target_is_an_error() {
        let command = proposal(LearnedKind::Command);
        assert_eq!(check(command.clone(), &[]), Ok(command));
        let error = refused(LearnedKind::Command, |proposal| proposal.target = None);
        assert_eq!(error, field("target"));
    }

    /// AC-14.2c-3.
    #[test]
    fn ac_14_2c_3_missing_fresh_min_is_named() {
        for kind in [LearnedKind::Fetch, LearnedKind::Prefetch] {
            for fresh_min in [None, Some(0)] {
                let error = refused(kind, |proposal| proposal.fresh_min = fresh_min);
                assert_eq!(error, field("fresh_min"), "{kind:?} {fresh_min:?}");
            }
        }
    }

    /// AC-14.2c-4.
    #[test]
    fn ac_14_2c_4_new_plugin_fields_are_named() {
        let new_plugin = |change: Change| refused(LearnedKind::NewPlugin, change);
        assert_eq!(new_plugin(|proposal| proposal.needs = None), field("needs"));
        assert_eq!(
            new_plugin(|proposal| proposal.needs = Some(String::new())),
            field("needs")
        );
        assert_eq!(
            new_plugin(|proposal| proposal.service = Some("x".to_owned())),
            field("service")
        );
        assert_eq!(
            new_plugin(|proposal| proposal.plugin = Some("x".to_owned())),
            field("plugin")
        );
        assert_eq!(
            new_plugin(|proposal| proposal.question = Some("x".to_owned())),
            field("question")
        );
        let mut valid = proposal(LearnedKind::NewPlugin);
        valid.responses = vec!["Bedtime is in {{ response.minutes }} minutes.".to_owned()];
        assert_eq!(check(valid.clone(), &[]), Ok(valid));
    }

    /// The status of a proposal of `kind` with `service`.
    fn status(kind: LearnedKind, service: Option<&str>) -> LearnedStatus {
        let mut intent = proposal(kind);
        intent.service = service.map(str::to_owned);
        initial_status(&intent)
    }

    /// AC-14.5-10.
    #[test]
    fn ac_14_5_10_status_of_each_kind() {
        let service = Some("scene.turn_on");
        assert_eq!(
            status(LearnedKind::NewPlugin, None),
            LearnedStatus::Requested
        );
        assert_eq!(status(LearnedKind::None, service), LearnedStatus::None);
        assert_eq!(
            status(LearnedKind::Command, service),
            LearnedStatus::Pending
        );
        assert_eq!(status(LearnedKind::Query, None), LearnedStatus::Active);
    }

    /// AC-14.5-11.
    #[test]
    fn ac_14_5_11_file_round_trips_as_toml() {
        let mut first = proposal(LearnedKind::Command);
        first.data.insert("transition".to_owned(), "2".to_owned());
        first.centroid = vec![0.5, 0.25];
        first.status = LearnedStatus::Active;
        first.texts = vec!["good night jarvis".to_owned()];
        let mut second = entry("get_traffic", [0.0, 1.0, 0.0]);
        second.request_file = Some("plugin-requests/get_traffic.md".to_owned());
        let learned = Learned {
            intents: vec![first, second],
        };
        let text = toml::to_string_pretty(&learned).expect("the file serializes");
        assert_eq!(text.matches("[[intents]]").count(), 2, "the file is {text}");
        let read: Learned = toml::from_str(&text).expect("the file parses");
        assert_eq!(read, learned);
    }

    /// The lead of the prefetch window in the tests, in minutes.
    const LEAD_MIN: u64 = 5;

    /// The usual time weekdays 08:00.
    const WEEKDAY_8: Bucket = Bucket {
        day: DayType::Weekdays,
        hour: 8,
    };

    /// The time zone of the reference house.
    fn chicago() -> TimeZone {
        TimeZone::get("America/Chicago").expect("the time zone database has Chicago")
    }

    /// The instant of the local date and time `local` in Chicago.
    fn chicago_at(local: &str) -> Timestamp {
        let zoned: jiff::Zoned = format!("{local}[America/Chicago]")
            .parse()
            .expect("a zoned time");
        zoned.timestamp()
    }

    /// AC-14.9-1.
    #[test]
    fn ac_14_9_1_window_is_lead_before_to_end_of_hour() {
        // 2026-11-16 is a Monday, 2026-11-14 a Saturday.
        let cases = [
            ("2026-11-16T07:55:00", true),
            ("2026-11-16T08:59:59", true),
            ("2026-11-16T07:54:59", false),
            ("2026-11-16T09:00:00", false),
            ("2026-11-14T07:55:00", false),
        ];
        for (local, open) in cases {
            let found = in_window(&[WEEKDAY_8], chicago_at(local), &chicago(), LEAD_MIN);
            assert_eq!(found, open, "in_window of weekdays 08:00 at {local}");
        }
    }

    /// AC-14.9-2.
    #[test]
    fn ac_14_9_2_window_of_midnight_starts_on_sunday() {
        let midnight = Bucket {
            day: DayType::Weekdays,
            hour: 0,
        };
        // 2026-11-15 is a Sunday.
        let now = chicago_at("2026-11-15T23:55:00");
        assert!(
            in_window(&[midnight], now, &chicago(), LEAD_MIN),
            "the window of weekdays 00:00 is closed at Sunday 23:55"
        );
    }

    /// AC-14.9-3.
    #[test]
    fn ac_14_9_3_refresh_waits_fresh_min_after_attempt() {
        let due = |attempt: Option<&str>, local: &str| {
            let attempt = attempt.map(chicago_at);
            refresh_due(
                &[WEEKDAY_8],
                attempt,
                chicago_at(local),
                &chicago(),
                LEAD_MIN,
                5,
            )
        };
        assert!(
            due(None, "2026-11-16T07:55:00"),
            "no refresh at the window start without a prefetch"
        );
        let attempt = Some("2026-11-16T07:55:23");
        assert!(
            !due(attempt, "2026-11-16T07:59:23"),
            "a refresh 4 min after the prefetch"
        );
        assert!(
            due(attempt, "2026-11-16T08:00:23"),
            "no refresh 5 min after the prefetch"
        );
    }

    /// AC-14.9-4.
    #[test]
    fn ac_14_9_4_answer_is_fresh_to_fresh_min() {
        let answer = chicago_at("2026-11-16T07:55:23");
        let limit = answer + jiff::SignedDuration::from_mins(5);
        assert!(
            fresh(answer, limit, 5),
            "an answer of exactly 5 min is stale"
        );
        let later = limit + jiff::SignedDuration::from_nanos(1);
        assert!(
            !fresh(answer, later, 5),
            "an answer older than 5 min is fresh"
        );
    }

    /// AC-14.13-2.
    #[test]
    fn ac_14_13_2_request_has_texts_usual_times_needs_and_task() {
        let mut entry = proposal(LearnedKind::NewPlugin);
        entry.name = "get_traffic".to_owned();
        entry.texts = ["what's the traffic like", "how is the traffic to work"]
            .map(str::to_owned)
            .to_vec();
        entry.needs = Some("current road traffic from home to work".to_owned());
        let request = plugin_request(&entry, &[WEEKDAY_8]);
        let start = "# Plugin request: get_traffic\n\n## Requests\n\nwhat's the traffic like\nhow \
            is the traffic to work\n\n## Usual times\n\nweekdays 08:00\n\n## Data needed\n\ncurrent \
            road traffic from home to work\n\n## Task\n\nAdd the fetch intent `get_traffic` with";
        let key = "Use a source with a key only after the owner agrees, so ask first.";
        assert_eq!(request.name, "get_traffic");
        assert!(request.text.starts_with(start), "{}", request.text);
        assert!(request.text.contains(key), "{}", request.text);
    }

    /// AC-14.13-6.
    #[test]
    fn ac_14_13_6_no_usual_time_gives_none() {
        let text = plugin_request(&proposal(LearnedKind::NewPlugin), &[]).text;
        assert!(text.contains("## Usual times\n\nnone\n"), "{text}");
    }
}
