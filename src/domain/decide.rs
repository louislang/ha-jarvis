//! `decide`: a prediction and a request to a decision.

use std::collections::BTreeSet;

use jiff::civil::DateTime;
use minijinja::Environment;
use minijinja::UndefinedBehavior;
use minijinja::value::Serde;
use rand::rngs::StdRng;
use serde_json::Map;
use serde_json::Value;
use vstd::prelude::*;

use crate::domain::catalog;
use crate::domain::catalog::Catalog;
use crate::domain::catalog::Intent;
use crate::domain::catalog::Kind;
use crate::domain::catalog::Target;
use crate::domain::model::Prediction;
use crate::domain::model::Slot;
use crate::domain::responses;
use crate::domain::responses::Pools;
use crate::domain::select;
use crate::domain::state::AreaId;
use crate::domain::state::EntityId;
use crate::domain::state::ServiceCall;
use crate::domain::state_cache::StateCache;

/// What Jarvis does for a request.
#[verus_verify]
#[derive(Clone, Debug, PartialEq)]
pub enum Decision {
    /// Speak the reply and make the call.
    Reply {
        /// The reply.
        reply: Chosen,
        /// The service call, if any.
        call: Option<ServiceCall>,
    },
    /// Speak the filler and run the job.
    Defer {
        /// The filler.
        filler: Chosen,
        /// The deferred work.
        job: Job,
    },
}

/// A selected response.
#[verus_verify]
#[derive(Clone, Debug, PartialEq)]
pub struct Chosen {
    /// The text to speak.
    pub text: String,
    /// The pool name.
    pub pool: String,
    /// The index of the response in the pool.
    pub index: usize,
}

/// Work that System One defers.
#[verus_verify]
#[derive(Clone, Debug, PartialEq)]
pub enum Job {
    /// Ask System Two this question.
    Llm(String),
    /// Fetch from the plugin and announce its result with a reply from the pool.
    Fetch {
        /// The plugin call.
        call: PluginCall,
        /// The pool name of the announced reply.
        pool: String,
    },
}

/// A call of a plugin.
#[verus_verify]
#[derive(Clone, Debug, PartialEq)]
pub struct PluginCall {
    /// The tool name of the plugin.
    pub plugin: String,
    /// The arguments.
    pub arguments: Map<String, Value>,
}

/// The transcript and the satellite area of a request.
#[verus_verify]
#[derive(Clone, Debug, PartialEq)]
pub struct Request {
    /// The transcript.
    pub transcript: String,
    /// The area of the satellite.
    pub area: Option<AreaId>,
}

/// A System Two question with the context of its area.
#[verus_verify]
#[derive(Clone, Debug, PartialEq)]
pub struct Question {
    /// The request text.
    pub text: String,
    /// The area of the satellite.
    pub area: Option<AreaId>,
    /// The HA location name.
    pub location: String,
    /// The entities of `area` that `select` does not skip and that have a state.
    pub entities: Vec<EntityState>,
    /// The names of all areas of the hot cache.
    pub areas: Vec<String>,
    /// The entities with no area and a domain of the read services that `select` does not skip
    /// and that have a state.
    pub unassigned: Vec<EntityState>,
    /// The local time in the HA time zone.
    pub now: DateTime,
}

/// The name, the entity ID and the state of one entity.
#[verus_verify]
#[derive(Clone, Debug, PartialEq)]
pub struct EntityState {
    /// The name that HA shows.
    pub name: String,
    /// For example `light.overhead`.
    pub entity_id: String,
    /// For example `on` or `64.0`.
    pub state: String,
}

/// The question `text` from `area` at `location` at the local time `now`, with the entities of
/// `area` in `cache` and the entities with no area in `domains`.
#[verus_spec(asked =>
    ensures
        area is None ==> asked.entities@.len() == 0,
        area matches Some(found) ==> forall|position: int| 0 <= position < asked.entities@.len()
            ==> #[trigger] described(*cache, *found, asked.entities@[position]),
        forall|position: int| 0 <= position < asked.unassigned@.len()
            ==> #[trigger] described_unassigned(*cache, domains@, asked.unassigned@[position]),
)]
#[must_use]
pub fn question(
    text: &str,
    area: Option<&AreaId>,
    location: &str,
    now: DateTime,
    cache: &StateCache,
    domains: &[&str],
) -> Question {
    let (area, entities) = match area {
        Some(area) => (Some(area.clone()), area_entities(area, cache)),
        None => (None, Vec::new()),
    };
    Question {
        text: text.to_owned(),
        area,
        location: location.to_owned(),
        entities,
        areas: area_names(cache),
        unassigned: unassigned_entities(domains, cache),
        now,
    }
}

/// The name of each area of `cache`.
#[verus_verify]
fn area_names(cache: &StateCache) -> Vec<String> {
    let mut names = Vec::new();
    let mut index = 0;
    #[cfg_attr(verus_only, verus_spec(
        invariant
            index <= cache.areas@.len(),
        decreases cache.areas@.len() - index,
    ))]
    while index < cache.areas.len() {
        names.push(cache.areas[index].name.clone());
        index += 1;
    }
    names
}

/// The entities of the area with the name or alias `name` in `cache`, as `question` gives them,
/// or `None` when `cache` has no such area.
#[verus_spec(found =>
    ensures
        found matches Some(entities) ==> forall|position: int| 0 <= position < entities@.len()
            ==> #[trigger] in_named_area(*cache, name@, entities@[position]),
)]
#[must_use]
pub fn area_states(name: &str, cache: &StateCache) -> Option<Vec<EntityState>> {
    let area = cache.area_by_name(name)?;
    Some(area_entities(area, cache))
}

/// The name and the state of each entity of `area` that `select` does not skip and that has a
/// state.
#[verus_spec(found =>
    ensures
        forall|position: int| 0 <= position < found@.len()
            ==> #[trigger] described(*cache, *area, found@[position]),
)]
fn area_entities(area: &AreaId, cache: &StateCache) -> Vec<EntityState> {
    let mut found = Vec::new();
    let mut index = 0;
    #[cfg_attr(verus_only, verus_spec(
        invariant
            index <= cache.entities@.len(),
            forall|position: int| 0 <= position < found@.len()
                ==> #[trigger] described(*cache, *area, found@[position]),
        decreases cache.entities@.len() - index,
    ))]
    while index < cache.entities.len() {
        let entity = &cache.entities[index];
        let present = !select::is_skipped(cache, entity)
            && matches!(cache.entity_area(entity), Some(found) if found.0.as_str() == area.0.as_str());
        if let (true, Some(state)) = (present, cache.state(entity.entity_id.0.as_str())) {
            found.push(EntityState {
                name: entity.name.clone(),
                entity_id: entity.entity_id.0.clone(),
                state: state.state.clone(),
            });
        }
        index += 1;
    }
    found
}

/// The name, the entity ID and the state of each entity of `cache` with no area and a domain of
/// `domains` that `select` does not skip and that has a state.
#[verus_spec(found =>
    ensures
        forall|position: int| 0 <= position < found@.len()
            ==> #[trigger] described_unassigned(*cache, domains@, found@[position]),
)]
fn unassigned_entities(domains: &[&str], cache: &StateCache) -> Vec<EntityState> {
    let mut found = Vec::new();
    let mut index = 0;
    #[cfg_attr(verus_only, verus_spec(
        invariant
            index <= cache.entities@.len(),
            forall|position: int| 0 <= position < found@.len()
                ==> #[trigger] described_unassigned(*cache, domains@, found@[position]),
        decreases cache.entities@.len() - index,
    ))]
    while index < cache.entities.len() {
        let entity = &cache.entities[index];
        let id = entity.entity_id.0.as_str();
        let kept = !select::is_skipped(cache, entity)
            && cache.entity_area(entity).is_none()
            && has_domain(id, domains);
        if let (true, Some(state)) = (kept, cache.state(id)) {
            found.push(EntityState {
                name: entity.name.clone(),
                entity_id: entity.entity_id.0.clone(),
                state: state.state.clone(),
            });
        }
        index += 1;
    }
    found
}

/// True when the entity ID `entity_id` is in one of `domains`.
#[verus_spec(found =>
    ensures
        found == in_domains(entity_id@, domains@),
)]
fn has_domain(entity_id: &str, domains: &[&str]) -> bool {
    let mut index = 0;
    #[cfg_attr(verus_only, verus_spec(
        invariant
            index <= domains@.len(),
            forall|earlier: int| 0 <= earlier < index
                ==> !crate::domain::state_cache::in_domain(entity_id@, #[trigger] domains@[earlier]@),
        decreases domains@.len() - index,
    ))]
    while index < domains.len() {
        if StateCache::in_domain(entity_id, domains[index]) {
            return true;
        }
        index += 1;
    }
    false
}

/// The decision for `prediction` of `request`: a System Two job for an `unknown`, uncertain or
/// removed label, a `train` or a `prefetch`, else the decision of the command, the query or the
/// fetch in its area.
#[verus_spec(decision =>
    requires
        responses::valid_pools(pools@, *catalog),
    ensures
        decision is Reply ==> prediction.ranked@.len() > 0
            && prediction.ranked@[0].0@ != "unknown"@
            && vstd::std_specs::cmp::ge_ensures::<f32>(prediction.ranked@[0].1, threshold, true),
        prediction.ranked@.len() > 0 && !cataloged(*catalog, prediction.ranked@[0].0@)
            ==> deferred(decision, request.transcript),
        !(decision matches Decision::Defer { job: Job::Llm(_), .. }) ==> exists|index: int|
            #[trigger] handled(*prediction, *catalog, threshold, index)
            && acted(catalog.intents@[index], *prediction, *request, *cache, decision),
)]
#[must_use]
pub fn decide(
    prediction: &Prediction,
    request: &Request,
    catalog: &Catalog,
    cache: &StateCache,
    pools: &Pools,
    threshold: f32,
    rng: &mut StdRng,
) -> Decision {
    let Some((label, probability)) = prediction.ranked.as_slice().first() else {
        return defer(request, pools, rng);
    };
    let confident = *probability >= threshold;
    match position(catalog, label.as_str()) {
        Some(index)
            if confident
                && label.as_str() != "unknown"
                && matches!(
                    catalog.intents[index].kind,
                    Kind::Command | Kind::Query | Kind::Fetch
                ) =>
        {
            let decision = act(
                &catalog.intents[index],
                prediction,
                request,
                cache,
                pools,
                rng,
            );
            proof! {
                assert(handled(*prediction, *catalog, threshold, index as int));
            }
            decision
        }
        _ => defer(request, pools, rng),
    }
}

/// The decision of `intent` in the area of the area slot, else of the satellite, or a System Two
/// job when System One cannot act: the slot names no area, a query or a service has no area, or a
/// later step fails. A fetch and a `house` query need no area.
#[verus_spec(decision =>
    requires
        pools@.contains_key(crate::domain::contracts::string_key(intent.name@)),
        pools@.contains_key(crate::domain::contracts::string_key("filler"@)),
        responses::filled(pools@),
    ensures
        acted(*intent, *prediction, *request, *cache, decision),
)]
fn act(
    intent: &Intent,
    prediction: &Prediction,
    request: &Request,
    cache: &StateCache,
    pools: &Pools,
    rng: &mut StdRng,
) -> Decision {
    let Some((area, context)) = situation(intent, prediction, request, cache) else {
        return defer(request, pools, rng);
    };
    let decision = match (intent.kind, &intent.service, area) {
        (Kind::Fetch, _, _) => fetch(intent, &context, pools, rng),
        (Kind::Query, _, _) => query(intent, area, context, cache, pools, rng),
        (Kind::Command, None, _) => respond(intent, &context, None, pools, rng),
        (_, Some(service), Some(area)) => {
            call(intent, service.as_str(), area, context, cache, pools, rng)
        }
        _ => None,
    };
    match decision {
        Some(decision) => decision,
        None => defer(request, pools, rng),
    }
}

/// The area of the first area slot of `prediction`, else of `request`, and the template context of
/// `intent` in it, or `None` when the slot names no area or a slot is missing.
#[verus_spec(found =>
    ensures
        found matches Some((area, _)) ==> match area {
            Some(area) => located(*prediction, *request, *cache, *area),
            None => unslotted(prediction.slots@) && request.area is None,
        },
)]
fn situation<'a>(
    intent: &Intent,
    prediction: &Prediction,
    request: &'a Request,
    cache: &'a StateCache,
) -> Option<(Option<&'a AreaId>, Map<String, Value>)> {
    let area = match area_slot(&prediction.slots) {
        Some(slot) => Some(cache.area_by_name(prediction.slots[slot].value.as_str())?),
        None => request.area.as_ref(),
    };
    Some((area, context(intent, prediction, area, cache)?))
}

/// The reply of the query `intent` rendered with its selection in `area`, or `None` when the
/// selection is empty, the reply does not render, or `area` is `None` for a target that is not
/// `house`. A list query also gets the kept entities of its selection as `entities`.
#[verus_spec(decision =>
    requires
        pools@.contains_key(crate::domain::contracts::string_key(intent.name@)),
        responses::filled(pools@),
    ensures
        area is None && !(intent.target is House) ==> decision is None,
        decision is None || (decision matches Some(Decision::Reply { call: None, .. })
            && exists|known: AreaId, selected: Seq<EntityId>| selected.len() > 0
                && #[trigger] targeted(*intent, *cache, known, selected)
                && (area matches Some(area) ==> known == *area)),
)]
fn query(
    intent: &Intent,
    area: Option<&AreaId>,
    mut context: Map<String, Value>,
    cache: &StateCache,
    pools: &Pools,
    rng: &mut StdRng,
) -> Option<Decision> {
    let anywhere = AreaId(String::new());
    let area = match (area, intent.target) {
        (Some(area), _) => area,
        // `select` does not read the area of a `house` target.
        (None, Target::House) => &anywhere,
        (None, _) => return None,
    };
    let selected = selection(intent, area, &mut context, cache)?;
    if !matches!(intent.target, Target::Entity) {
        let ids = kept(&selected, intent.state.as_ref(), cache);
        context.insert("entities".to_owned(), entities(&ids, cache));
    }
    respond(intent, &context, None, pools, rng)
}

/// The reply and the call of `service` with the data of `intent` on its selection in `area`, or
/// `None` when `service` has no dot, the selection is empty or a template does not render.
#[verus_spec(decision =>
    requires
        pools@.contains_key(crate::domain::contracts::string_key(intent.name@)),
        responses::filled(pools@),
    ensures
        decision is None
            || (decision matches Some(Decision::Reply { call: Some(made), .. })
            && made.target@.len() > 0 && served(service@, made)
            && targeted(*intent, *cache, *area, entity_ids(made.target@))
            && stringed(*intent, made.data)),
)]
fn call(
    intent: &Intent,
    service: &str,
    area: &AreaId,
    mut context: Map<String, Value>,
    cache: &StateCache,
    pools: &Pools,
    rng: &mut StdRng,
) -> Option<Decision> {
    let (domain, name) = service.split_once('.')?;
    let selection = selection(intent, area, &mut context, cache)?;
    let data = data(intent, &context)?;
    let call = ServiceCall {
        domain: domain.to_owned(),
        service: name.to_owned(),
        target: texts(&selection),
        data,
        return_response: false,
    };
    respond(intent, &context, Some(call), pools, rng)
}

/// The filler and the fetch of the plugin of `intent` with its `data` rendered with `context` as
/// the arguments, or `None` when `intent` has no plugin or a data value does not render.
#[verus_spec(decision =>
    requires
        pools@.contains_key(crate::domain::contracts::string_key("filler"@)),
        responses::filled(pools@),
    ensures
        intent.plugin is None ==> decision is None,
        decision matches Some(decision) ==> fetched(*intent, decision),
)]
fn fetch(
    intent: &Intent,
    context: &Map<String, Value>,
    pools: &Pools,
    rng: &mut StdRng,
) -> Option<Decision> {
    let plugin = intent.plugin.as_ref()?;
    let arguments = data(intent, context)?;
    let call = PluginCall {
        plugin: plugin.clone(),
        arguments,
    };
    let pool = intent.name.clone();
    Some(Decision::Defer {
        filler: choose(pools, "filler", rng),
        job: Job::Fetch { call, pool },
    })
}

/// The selection of `intent` in `area`, with the `entity` text rendered with `context`, and with
/// the first selected entity as `entity` in `context` for `target = "entity"`. `None` when the
/// selection is empty, the `entity` text does not render or the cache has no state for `entity`.
#[verus_spec(selected =>
    ensures
        selected matches Some(selected)
            ==> selected@.len() > 0 && targeted(*intent, *cache, *area, selected@),
)]
fn selection(
    intent: &Intent,
    area: &AreaId,
    context: &mut Map<String, Value>,
    cache: &StateCache,
) -> Option<Vec<EntityId>> {
    let selected = match (intent.target, &intent.entity) {
        (Target::Entity, Some(entity)) => {
            vec![EntityId(responses::render(entity.as_str(), context).ok()?)]
        }
        _ => select::select(intent, area, cache),
    };
    let first = selected.first()?;
    if let Target::Entity = intent.target {
        context.insert("entity".to_owned(), entity(cache, first)?);
    }
    Some(selected)
}

/// The `data` of `intent` rendered with `context`, or `None` when a value does not render.
#[verus_spec(data =>
    ensures
        data matches Some(data) ==> stringed(*intent, data),
)]
fn data(intent: &Intent, context: &Map<String, Value>) -> Option<Map<String, Value>> {
    let mut data = Map::new();
    let entries = intent.data.iter();
    proof_decl! {
        let ghost all = vstd::std_specs::iter::IteratorSpec::remaining(&entries);
    }
    #[cfg_attr(verus_only, verus_spec(entry_iter =>
        invariant
            entry_iter.seq() == all,
            listed(*intent, all),
            forall|seen: int| 0 <= seen < entry_iter.index() ==> strung(data, #[trigger] all[seen]),
    ))]
    for (key, template) in entries {
        data.insert(key.clone(), rendered(template.as_str(), context)?);
    }
    proof! {
        lemma_stringed(*intent, all, data);
    }
    Some(data)
}

/// `template` rendered with `context`: the result of one `{{ expression }}` with its type, else
/// the rendered text. `None` when `template` does not render.
#[verus_spec(result =>
    ensures
        result matches Some(value)
            ==> (!braced(template@) ==> crate::domain::contracts::text(value) is Some),
)]
fn rendered(template: &str, context: &Map<String, Value>) -> Option<Value> {
    if let Some(source) = expression(template) {
        return evaluate(source, context);
    }
    match responses::render(template, context) {
        Ok(text) => Some(Value::from(text)),
        Err(_) => None,
    }
}

/// The text between `{{` at the start and `}}` at the end of `template`, when it has no other `}}`.
#[verus_spec(source =>
    ensures
        source is Some ==> braced(template@),
)]
fn expression(template: &str) -> Option<&str> {
    let (before, rest) = template.split_once("{{")?;
    let (source, after) = rest.split_once("}}")?;
    if before.is_empty() && after.is_empty() {
        proof! {
            assert(template@ =~= ("{{"@ + source@) + "}}"@);
        }
        Some(source)
    } else {
        None
    }
}

/// The result of the expression `source` with `context`, or `None` when it does not compile or
/// evaluate, or its result is undefined.
#[verus_verify]
fn evaluate(source: &str, context: &Map<String, Value>) -> Option<Value> {
    let mut environment = Environment::new();
    environment.set_undefined_behavior(UndefinedBehavior::Strict);
    let expression = environment.compile_expression(source).ok()?;
    let value = expression.eval(Serde(context)).ok()?;
    if value.is_undefined() {
        return None;
    }
    serde_json::to_value(value).ok()
}

/// `Reply` with `call` and the reply of `intent` rendered with `context`, or `None` when the
/// reply does not render.
#[verus_spec(decision =>
    requires
        pools@.contains_key(crate::domain::contracts::string_key(intent.name@)),
        responses::filled(pools@),
    ensures
        decision is None || (decision matches Some(Decision::Reply { call: made, .. }) && made == call),
)]
fn respond(
    intent: &Intent,
    context: &Map<String, Value>,
    call: Option<ServiceCall>,
    pools: &Pools,
    rng: &mut StdRng,
) -> Option<Decision> {
    let mut reply = choose(pools, intent.name.as_str(), rng);
    reply.text = responses::render(reply.text.as_str(), context).ok()?;
    Some(Decision::Reply { reply, call })
}

/// The template context of `intent`: the name of `area` in lowercase and the slots of
/// `prediction`, or `None` when a slot is missing.
#[verus_verify]
fn context(
    intent: &Intent,
    prediction: &Prediction,
    area: Option<&AreaId>,
    cache: &StateCache,
) -> Option<Map<String, Value>> {
    let mut context = slots(intent, prediction)?;
    let found = match area {
        Some(area) => cache.areas.iter().find(|known| known.area_id.0 == area.0),
        None => None,
    };
    if let Some(found) = found {
        context.insert("area".to_owned(), Value::from(found.name.to_lowercase()));
    }
    Some(context)
}

/// The value in `prediction` of each slot of `intent` except `area`, with an empty string for a
/// missing `optional` slot, or `None` when another slot is missing.
#[verus_verify]
fn slots(intent: &Intent, prediction: &Prediction) -> Option<Map<String, Value>> {
    let mut names = BTreeSet::new();
    catalog::insert_slots(intent, &mut names);
    names.remove("area");
    let mut slots = Map::new();
    let names = names.iter();
    for name in names {
        let value = match prediction.slots.iter().find(|slot| slot.name == *name) {
            Some(slot) => slot.value.clone(),
            None if intent.optional.contains(name) => String::new(),
            None => return None,
        };
        slots.insert(name.clone(), Value::from(value));
    }
    Some(slots)
}

/// The state, the attributes and the name of the entity `id`, or `None` when the cache has no
/// state for it.
#[verus_verify]
fn entity(cache: &StateCache, id: &EntityId) -> Option<Value> {
    let state = cache.state(id.0.as_str())?;
    let mut entity = Map::new();
    entity.insert("state".to_owned(), Value::from(state.state.clone()));
    entity.insert(
        "attributes".to_owned(),
        Value::from(state.attributes.clone()),
    );
    let registered = cache
        .entities
        .iter()
        .find(|known| known.entity_id.0 == id.0);
    if let Some(registered) = registered {
        entity.insert("name".to_owned(), Value::from(registered.name.clone()));
    }
    Some(Value::from(entity))
}

/// The IDs of `ids` that have a state in `cache`, and the state `state` when it is `Some`, in
/// order.
#[verus_spec(found =>
    ensures
        forall|position: int| 0 <= position < found@.len() ==> ids@.contains(#[trigger] found@[position])
            && stated(*cache, crate::domain::state_cache::optional(state), found@[position]),
        forall|index: int| 0 <= index < ids@.len()
            && stated(*cache, crate::domain::state_cache::optional(state), #[trigger] ids@[index])
            ==> found@.contains(ids@[index]),
)]
fn kept(ids: &[EntityId], state: Option<&String>, cache: &StateCache) -> Vec<EntityId> {
    let mut found = Vec::new();
    let mut index = 0;
    #[cfg_attr(verus_only, verus_spec(
        invariant
            index <= ids@.len(),
            forall|position: int| 0 <= position < found@.len() ==> ids@.contains(#[trigger] found@[position])
                && stated(*cache, crate::domain::state_cache::optional(state), found@[position]),
            forall|earlier: int| 0 <= earlier < index
                && stated(*cache, crate::domain::state_cache::optional(state), #[trigger] ids@[earlier])
                ==> found@.contains(ids@[earlier]),
        decreases ids@.len() - index,
    ))]
    while index < ids.len() {
        if is_stated(&ids[index], state, cache) {
            found.push(EntityId(ids[index].0.clone()));
        }
        index += 1;
    }
    found
}

/// True when `cache` has a state for `id`, and it is `state` when `state` is `Some`.
#[verus_spec(found =>
    ensures
        found == stated(*cache, crate::domain::state_cache::optional(state), *id),
)]
fn is_stated(id: &EntityId, state: Option<&String>, cache: &StateCache) -> bool {
    match (cache.states.get(id.0.as_str()), state) {
        (Some(known), Some(state)) => known.state.as_str() == state.as_str(),
        (Some(_), None) => true,
        (None, _) => false,
    }
}

/// The `entity` object of each ID of `ids` that has a state in `cache`, in order.
#[verus_verify]
fn entities(ids: &[EntityId], cache: &StateCache) -> Value {
    let mut entities = Vec::new();
    for id in ids {
        if let Some(found) = entity(cache, id) {
            entities.push(found);
        }
    }
    Value::from(entities)
}

/// The text of each ID of `ids`.
#[verus_spec(texts =>
    ensures
        entity_ids(texts@) == ids@,
)]
fn texts(ids: &[EntityId]) -> Vec<String> {
    let mut texts = Vec::new();
    let mut index = 0;
    #[cfg_attr(verus_only, verus_spec(
        invariant
            index <= ids@.len(),
            texts@.len() == index,
            entity_ids(texts@) == ids@.take(index as int),
        decreases ids@.len() - index,
    ))]
    while index < ids.len() {
        proof_decl! {
            let ghost before = texts@;
        }
        texts.push(ids[index].0.clone());
        proof! {
            assert forall|position: int| 0 <= position <= index
                implies #[trigger] entity_ids(texts@)[position] == ids@[position] by {
                if position < index {
                    assert(entity_ids(before)[position] == ids@.take(index as int)[position]);
                }
            }
            assert(entity_ids(texts@) =~= ids@.take(index + 1));
        }
        index += 1;
    }
    proof! {
        assert(ids@.take(index as int) =~= ids@);
    }
    texts
}

/// The index of the first `area` slot of `slots`.
#[verus_spec(found =>
    ensures
        match found {
            Some(index) => first_area(slots@, index as int),
            None => unslotted(slots@),
        },
)]
fn area_slot(slots: &[Slot]) -> Option<usize> {
    let mut index = 0;
    #[cfg_attr(verus_only, verus_spec(
        invariant
            index <= slots@.len(),
            forall|earlier: int| 0 <= earlier < index ==> #[trigger] slots@[earlier].name@ != "area"@,
        decreases slots@.len() - index,
    ))]
    while index < slots.len() {
        if slots[index].name.as_str() == "area" {
            return Some(index);
        }
        index += 1;
    }
    None
}

/// A filler and a System Two job with the transcript of `request`.
#[verus_spec(decision =>
    requires
        pools@.contains_key(crate::domain::contracts::string_key("filler"@)),
        responses::filled(pools@),
    ensures
        deferred(decision, request.transcript),
)]
fn defer(request: &Request, pools: &Pools, rng: &mut StdRng) -> Decision {
    Decision::Defer {
        filler: choose(pools, "filler", rng),
        job: Job::Llm(request.transcript.clone()),
    }
}

/// A random response of the pool `name`, with its template as the text.
#[verus_spec(
    requires
        pools@.contains_key(crate::domain::contracts::string_key(name@)),
        responses::filled(pools@),
)]
fn choose(pools: &Pools, name: &str, rng: &mut StdRng) -> Chosen {
    let pool = pools.get(name).expect("the pool exists");
    let index = responses::select(pool, rng);
    Chosen {
        text: pool.responses[index].text.clone(),
        pool: name.to_owned(),
        index,
    }
}

/// The index of the intent of `catalog` with the name `name`.
#[verus_spec(found =>
    ensures
        match found {
            Some(index) => index < catalog.intents@.len() && catalog.intents@[index as int].name@ == name@,
            None => !cataloged(*catalog, name@),
        },
)]
fn position(catalog: &Catalog, name: &str) -> Option<usize> {
    let mut index = 0;
    #[cfg_attr(verus_only, verus_spec(
        invariant
            index <= catalog.intents@.len(),
            forall|earlier: int| 0 <= earlier < index
                ==> #[trigger] catalog.intents@[earlier].name@ != name@,
        decreases catalog.intents@.len() - index,
    ))]
    while index < catalog.intents.len() {
        if catalog.intents[index].name.as_str() == name {
            return Some(index);
        }
        index += 1;
    }
    None
}

verus! {

broadcast use {
    crate::domain::contracts::axiom_string_obeys_cmp,
    crate::domain::contracts::axiom_string_borrow,
    crate::domain::contracts::axiom_string_contains_borrowed_key,
    crate::domain::contracts::axiom_string_maps_borrowed_key,
    crate::domain::contracts::axiom_char_pattern,
    crate::domain::contracts::axiom_str_pattern,
    crate::domain::contracts::axiom_arc_str_obeys_cmp,
    crate::domain::contracts::axiom_arc_str_borrow,
    crate::domain::contracts::axiom_arc_str_contains_borrowed_key,
    crate::domain::contracts::axiom_arc_str_maps_borrowed_key,
    vstd::seq_lib::lemma_seq_contains_after_push,
};

/// True when `catalog` has an intent with the name `name`.
pub open spec fn cataloged(catalog: Catalog, name: Seq<char>) -> bool {
    exists|index: int| 0 <= index < catalog.intents@.len() && #[trigger] catalog.intents@[index].name@ == name
}

/// True when `entity` is in `area` and `select` does not skip it.
pub open spec fn present(cache: StateCache, area: AreaId, entity: crate::domain::state::Entity) -> bool {
    &&& !select::skipped(cache, entity)
    &&& crate::domain::state_cache::entity_area(cache, entity) matches Some(found) && found.0@ == area.0@
}

/// True when `item` has the name of an entity of `cache` that is present in `area`.
pub open spec fn described(cache: StateCache, area: AreaId, item: EntityState) -> bool {
    exists|index: int| 0 <= index < cache.entities@.len()
        && #[trigger] cache.entities@[index].name == item.name
        && present(cache, area, cache.entities@[index])
}

/// True when `entity` has no area and a domain of `domains`, and `select` does not skip it.
pub open spec fn unassigned(cache: StateCache, domains: Seq<&str>, entity: crate::domain::state::Entity) -> bool {
    &&& !select::skipped(cache, entity)
    &&& crate::domain::state_cache::entity_area(cache, entity) is None
    &&& in_domains(entity.entity_id.0@, domains)
}

/// True when the entity ID `entity_id` is in one of `domains`.
pub open spec fn in_domains(entity_id: Seq<char>, domains: Seq<&str>) -> bool {
    exists|index: int| 0 <= index < domains.len()
        && crate::domain::state_cache::in_domain(entity_id, #[trigger] domains[index]@)
}

/// True when `item` has the name of an entity of `cache` that is unassigned in `domains`.
pub open spec fn described_unassigned(cache: StateCache, domains: Seq<&str>, item: EntityState) -> bool {
    exists|index: int| 0 <= index < cache.entities@.len()
        && #[trigger] cache.entities@[index].name == item.name
        && unassigned(cache, domains, cache.entities@[index])
}

/// True when `item` is described in an area of `cache` with the name or alias `name`.
pub open spec fn in_named_area(cache: StateCache, name: Seq<char>, item: EntityState) -> bool {
    exists|index: int| 0 <= index < cache.areas@.len()
        && crate::domain::state_cache::named(
            #[trigger] cache.areas@[index],
            crate::domain::contracts::lowercase(name),
        )
        && described(cache, cache.areas@[index].area_id, item)
}

/// True when `decision` asks System Two `question`.
pub open spec fn deferred(decision: Decision, question: String) -> bool {
    decision matches Decision::Defer { job: Job::Llm(asked), .. } && asked@ == question@
}

/// True when the top label of `prediction` is confident and names the command, the query or the
/// fetch at `index` of `catalog`.
pub open spec fn handled(prediction: Prediction, catalog: Catalog, threshold: f32, index: int) -> bool {
    &&& prediction.ranked@.len() > 0
    &&& prediction.ranked@[0].0@ != "unknown"@
    &&& vstd::std_specs::cmp::ge_ensures::<f32>(prediction.ranked@[0].1, threshold, true)
    &&& 0 <= index < catalog.intents@.len()
    &&& catalog.intents@[index].name@ == prediction.ranked@[0].0@
    &&& (catalog.intents@[index].kind is Command || catalog.intents@[index].kind is Query
        || catalog.intents@[index].kind is Fetch)
}

/// True when `decision` for `intent` obeys the area rules: an area slot that names no area, or a
/// service without an area, defers the request to System Two, and no service gives no call. A
/// call makes the service of the intent on its selection in the area of the request. A query gives
/// no call and reads a selection in the area of the request. A fetch needs no area and defers to
/// System Two or gives a fetch job, and a fetch with no plugin defers to System Two. A `house`
/// query needs no area and reads the selection of the house.
pub open spec fn acted(intent: Intent, prediction: Prediction, request: Request, cache: StateCache, decision: Decision) -> bool {
    &&& forall|slot: int| #[trigger] first_area(prediction.slots@, slot)
        && !area_named(cache, prediction.slots@[slot].value@) ==> deferred(decision, request.transcript)
    &&& unslotted(prediction.slots@) && request.area is None && intent.service is Some
        && !(intent.kind is Fetch) && !housing(intent) ==> deferred(decision, request.transcript)
    &&& intent.service is None && !(intent.kind is Fetch) ==> (deferred(decision, request.transcript)
        || decision matches Decision::Reply { call: None, .. })
    &&& decision matches Decision::Reply { call: Some(made), .. } ==> made.target@.len() > 0
        && (intent.service matches Some(service) && served(service@, made))
        && aimed(intent, prediction, request, cache, made) && stringed(intent, made.data)
    &&& housing(intent) ==> deferred(decision, request.transcript)
        || (decision matches Decision::Reply { call: None, .. }
        && exists|selected: Seq<EntityId>| selected.len() > 0 && #[trigger] select::housed(intent, cache, selected))
    &&& intent.kind is Query && !housing(intent) ==> deferred(decision, request.transcript)
        || (decision matches Decision::Reply { call: None, .. }
        && exists|area: AreaId, selected: Seq<EntityId>|
            #![trigger located(prediction, request, cache, area), targeted(intent, cache, area, selected)]
            located(prediction, request, cache, area) && selected.len() > 0
            && targeted(intent, cache, area, selected))
    &&& intent.kind is Fetch && intent.plugin is None ==> deferred(decision, request.transcript)
    &&& intent.kind is Fetch ==> deferred(decision, request.transcript) || fetched(intent, decision)
}

/// True when `intent` is a query with `target = "house"`.
pub open spec fn housing(intent: Intent) -> bool {
    intent.kind is Query && intent.target is House
}

/// True when the cache has a state for `id`, and it is `state` when `state` is `Some`.
pub open spec fn stated(cache: StateCache, state: Option<Seq<char>>, id: EntityId) -> bool {
    let key = crate::domain::contracts::str_key(id.0@);
    &&& cache.states@.contains_key(key)
    &&& state matches Some(wanted) ==> cache.states@[key].state@ == wanted
}

/// True when `decision` defers a fetch of the plugin of `intent` with the pool of `intent`.
pub open spec fn fetched(intent: Intent, decision: Decision) -> bool {
    decision matches Decision::Defer { job: Job::Fetch { call, pool }, .. }
        && intent.plugin matches Some(plugin) && call.plugin@ == plugin@ && pool@ == intent.name@
}

/// True when each `data` value of `intent` that is not braced is a JSON string in `data`.
pub open spec fn stringed(intent: Intent, data: Map<String, Value>) -> bool {
    forall|key: String| #[trigger] intent.data@.contains_key(key)
        ==> strung(data, (&key, &intent.data@[key]))
}

/// True when each key of `intent.data` is in `all` with its value, and each entry of `all` is an
/// entry of `intent.data`.
pub open spec fn listed(intent: Intent, all: Seq<(&String, &String)>) -> bool {
    &&& forall|key: String| #[trigger] intent.data@.contains_key(key)
        ==> all.contains((&key, &intent.data@[key]))
    &&& forall|seen: int| 0 <= seen < all.len()
        ==> intent.data@[*(#[trigger] all[seen]).0] == *all[seen].1
}

/// True when the value of `entry` is braced, or `data` has a JSON string at the key of `entry`.
pub open spec fn strung(data: Map<String, Value>, entry: (&String, &String)) -> bool {
    !braced(entry.1@) ==> (crate::domain::contracts::attribute(data, entry.0) matches Some(value)
        && crate::domain::contracts::text(value) is Some)
}

/// `stringed` holds when each entry of `all` is strung and `all` lists `intent.data`.
pub proof fn lemma_stringed(intent: Intent, all: Seq<(&String, &String)>, data: Map<String, Value>)
    requires
        listed(intent, all),
        forall|seen: int| 0 <= seen < all.len() ==> strung(data, #[trigger] all[seen]),
    ensures
        stringed(intent, data),
{
    assert forall|key: String| #[trigger] intent.data@.contains_key(key)
        implies strung(data, (&key, &intent.data@[key])) by {
        let seen = choose|seen: int| 0 <= seen < all.len() && all[seen] == (&key, &intent.data@[key]);
    }
}

/// True when `template` is `{{`, a text and `}}`.
pub open spec fn braced(template: Seq<char>) -> bool {
    exists|inner: Seq<char>| template == #[trigger] ("{{"@ + inner) + "}}"@
}

/// True when the target of `call` is the selection of `intent` in the area of the request.
pub open spec fn aimed(intent: Intent, prediction: Prediction, request: Request, cache: StateCache, call: ServiceCall) -> bool {
    exists|area: AreaId| #[trigger] located(prediction, request, cache, area)
        && targeted(intent, cache, area, entity_ids(call.target@))
}

/// True when `area` is the area of the first area slot of `prediction`, else the area of `request`.
pub open spec fn located(prediction: Prediction, request: Request, cache: StateCache, area: AreaId) -> bool {
    &&& forall|slot: int| #[trigger] first_area(prediction.slots@, slot)
        ==> area_called(cache, prediction.slots@[slot].value@, area)
    &&& unslotted(prediction.slots@) ==> request.area == Some(area)
}

/// True when `area` is the ID of an area of `cache` whose name or alias is `text`, without case.
pub open spec fn area_called(cache: StateCache, text: Seq<char>, area: AreaId) -> bool {
    exists|index: int| 0 <= index < cache.areas@.len() && area == #[trigger] cache.areas@[index].area_id
        && crate::domain::state_cache::named(cache.areas@[index], crate::domain::contracts::lowercase(text))
}

/// True when an area of `cache` has the name or alias `text`, without case.
pub open spec fn area_named(cache: StateCache, text: Seq<char>) -> bool {
    exists|index: int| 0 <= index < cache.areas@.len()
        && crate::domain::state_cache::named(#[trigger] cache.areas@[index], crate::domain::contracts::lowercase(text))
}

/// True when `index` is the first `area` slot of `slots`.
pub open spec fn first_area(slots: Seq<Slot>, index: int) -> bool {
    &&& 0 <= index < slots.len()
    &&& slots[index].name@ == "area"@
    &&& forall|earlier: int| 0 <= earlier < index ==> #[trigger] slots[earlier].name@ != "area"@
}

/// True when `slots` has no `area` slot.
pub open spec fn unslotted(slots: Seq<Slot>) -> bool {
    forall|index: int| 0 <= index < slots.len() ==> #[trigger] slots[index].name@ != "area"@
}

/// True when `service` is the domain of `call`, a dot and the service of `call`, and the domain
/// has no dot.
pub open spec fn served(service: Seq<char>, call: ServiceCall) -> bool {
    service == call.domain@ + seq!['.'] + call.service@ && !call.domain@.contains('.')
}

/// The entity IDs with the texts `texts`.
pub open spec fn entity_ids(texts: Seq<String>) -> Seq<EntityId> {
    texts.map_values(|text: String| EntityId(text))
}

/// True when `selected` has each property that `select` ensures for `intent` in `area`.
pub open spec fn targeted(intent: Intent, cache: StateCache, area: AreaId, selected: Seq<EntityId>) -> bool {
    &&& !select::fixed(intent) ==> forall|position: int| 0 <= position < selected.len()
        ==> #[trigger] select::picked(intent, cache, selected[position])
    &&& intent.target is Entities && select::any_local(intent, cache, area) ==> (forall|position: int|
        0 <= position < selected.len() ==> #[trigger] select::picked_local(intent, cache, area, selected[position]))
        && forall|index: int| 0 <= index < cache.entities@.len()
            && select::local_match(intent, cache, area, #[trigger] cache.entities@[index])
            ==> selected.contains(cache.entities@[index].entity_id)
    &&& intent.target is House ==> select::housed(intent, cache, selected)
    &&& !select::fixed(intent) && !(intent.target is House) && !select::any_local(intent, cache, area)
        ==> selected.len() <= 1
        && (selected.len() == 1 <==> select::count(intent, cache, cache.entities@.len() as int) == 1)
    &&& intent.target is Entity && intent.entity is None && select::any_local(intent, cache, area)
        ==> selected.len() == 1 && select::picked_local(intent, cache, area, selected[0])
            && forall|index: int| 0 <= index < cache.entities@.len()
                && select::local_match(intent, cache, area, #[trigger] cache.entities@[index])
                ==> !select::lower(cache.entities@[index].entity_id.0, selected[0].0)
}

} // verus!

/// Tests of `decide`.
#[cfg(test)]
mod tests {
    use super::*;

    use std::collections::BTreeMap;

    use rand::SeedableRng;

    use crate::domain::catalog::Filter;
    use crate::domain::model::Slot;
    use crate::domain::responses::Pool;
    use crate::domain::responses::Response;
    use crate::domain::state::Snapshot;
    use crate::testing::fakes::house;

    /// The value of `model.threshold` in `jarvis.toml`.
    const THRESHOLD: f32 = 0.6;

    /// The temperature sensor of the kitchen thermostat.
    const THERMOSTAT: &str = "sensor.thermostat_current_temperature";

    /// The pool `details`: the name, the state and the friendly name of each entity of a list.
    const DETAILS: &str = "{% for entity in entities %}[{{ entity.name }} {{ entity.state }} \
        {{ entity.attributes.friendly_name }}]{% else %}No entities.{% endfor %}";

    /// The kitchen lights of the reference house.
    const KITCHEN: [&str; 2] = ["light.kitchen", "light.strawberry_lamp"];

    /// The catalog of `intents.toml`.
    fn catalog() -> Catalog {
        toml::from_str(include_str!("../../intents.toml")).expect("intents.toml is a catalog")
    }

    /// One response for each intent of `catalog` and each required pool. The `lights_on` reply
    /// renders the area, and the `fan_on` reply renders the entity.
    fn pools(catalog: &Catalog) -> Pools {
        let names = catalog.intents.iter().map(|intent| intent.name.as_str());
        names
            .chain(["filler", "failure"])
            .map(|name| {
                let text = match name {
                    "lights_on" => "The {{ area }} lights.".to_owned(),
                    "temperature_query" => {
                        "It is {{ entity.state }} degrees in the {{ area }}.".to_owned()
                    }
                    "fan_on" => "The {{ entity.name }} is {{ entity.state }}.".to_owned(),
                    "names" => r#"{{ entities | map(attribute="name") | join(", ") }}"#.to_owned(),
                    "details" => DETAILS.to_owned(),
                    _ => format!("The {name} response."),
                };
                (name.to_owned(), pool(&[text]))
            })
            .collect()
    }

    /// A pool of `texts` with no uses.
    fn pool(texts: &[String]) -> Pool {
        let responses = texts.iter().map(|text| Response {
            text: text.clone(),
            uses: 0,
            generated: false,
        });
        Pool {
            responses: responses.collect(),
            last: None,
        }
    }

    /// The pools of `responses.toml`.
    fn filed() -> Pools {
        let file: BTreeMap<String, BTreeMap<String, BTreeMap<String, Vec<String>>>> =
            toml::from_str(include_str!("../../responses.toml")).expect("responses.toml is TOML");
        let pools = file["pools"].iter();
        pools
            .map(|(name, texts)| (name.clone(), pool(&texts["responses"])))
            .collect()
    }

    /// The decision of `catalog` in `cache` for the top label and probability `top`, the slot
    /// names and values `slots` and `request`.
    fn decided_for(
        catalog: &Catalog,
        top: (&str, f32),
        slots: &[(&str, &str)],
        request: &Request,
        cache: &StateCache,
    ) -> Decision {
        decided_with(catalog, &pools(catalog), top, slots, request, cache)
    }

    /// The decision of `catalog` with `pools` in `cache` for the top label and probability `top`,
    /// the slot names and values `slots` and `request`.
    fn decided_with(
        catalog: &Catalog,
        pools: &Pools,
        top: (&str, f32),
        slots: &[(&str, &str)],
        request: &Request,
        cache: &StateCache,
    ) -> Decision {
        let slots = slots.iter().map(|(name, value)| Slot {
            name: (*name).to_owned(),
            value: (*value).to_owned(),
        });
        let prediction = Prediction {
            ranked: vec![(top.0.to_owned(), top.1), ("fan_on".to_owned(), 0.01)],
            slots: slots.collect(),
        };
        let mut rng = StdRng::seed_from_u64(1);
        decide(
            &prediction,
            request,
            catalog,
            cache,
            pools,
            THRESHOLD,
            &mut rng,
        )
    }

    /// The decision for a top label `label` with `probability` and the transcript `transcript`
    /// on the kitchen satellite.
    fn decided(label: &str, probability: f32, transcript: &str) -> Decision {
        let request = Request {
            transcript: transcript.to_owned(),
            area: Some(AreaId("kitchen".to_owned())),
        };
        let cache = StateCache::load(house());
        decided_for(&catalog(), (label, probability), &[], &request, &cache)
    }

    /// The decision of `catalog` for `label` with the area slot `slot` and the satellite in `area`.
    fn decided_in(
        catalog: &Catalog,
        label: &str,
        slot: Option<&str>,
        area: Option<&str>,
    ) -> Decision {
        let request = Request {
            transcript: String::new(),
            area: area.map(|area| AreaId(area.to_owned())),
        };
        let slots: Vec<(&str, &str)> = slot.map(|value| ("area", value)).into_iter().collect();
        let cache = StateCache::load(house());
        decided_for(catalog, (label, 0.89), &slots, &request, &cache)
    }

    /// The filler response.
    fn filler() -> Chosen {
        Chosen {
            text: "The filler response.".to_owned(),
            pool: "filler".to_owned(),
            index: 0,
        }
    }

    /// The decision that defers `transcript` to System Two with the filler.
    fn deferral(transcript: &str) -> Decision {
        Decision::Defer {
            filler: filler(),
            job: Job::Llm(transcript.to_owned()),
        }
    }

    /// The decision of `catalog` for `label` in `cache` on the satellite in `area`.
    fn decided_at(catalog: &Catalog, label: &str, area: &str, cache: &StateCache) -> Decision {
        let request = Request {
            transcript: String::new(),
            area: Some(AreaId(area.to_owned())),
        };
        decided_for(catalog, (label, 0.89), &[], &request, cache)
    }

    /// The reference house with the state `state` for the entity `id`.
    fn changed(id: &str, state: &str) -> StateCache {
        let mut snapshot = house();
        let mut states = snapshot.states.iter();
        let position = states.position(|known| known.entity_id == id);
        let position = position.expect("the house has the entity");
        snapshot.states[position].state = state.to_owned();
        StateCache::load(snapshot)
    }

    /// The `temperature_query` reply `text` with no call.
    fn queried(text: &str) -> Decision {
        answered("temperature_query", text)
    }

    /// The reply `text` of the pool `pool` with no call.
    fn answered(pool: &str, text: &str) -> Decision {
        let reply = Chosen {
            text: text.to_owned(),
            pool: pool.to_owned(),
            index: 0,
        };
        Decision::Reply { reply, call: None }
    }

    /// The catalog with the list query `name`: the filters of `lights_on`, `target` and `state`.
    fn listing(name: &str, target: Target, state: Option<&str>) -> Catalog {
        let mut catalog = catalog();
        let mut list = catalog.intents[0].clone();
        list.name = name.to_owned();
        list.kind = Kind::Query;
        list.service = None;
        list.target = target;
        list.state = state.map(str::to_owned);
        catalog.intents.push(list);
        catalog
    }

    /// The decision of the list query `name` of `catalog` on the satellite in `area` of the
    /// reference house.
    fn listed(catalog: &Catalog, name: &str, area: &str) -> Decision {
        decided_at(catalog, name, area, &StateCache::load(house()))
    }

    /// The `lights_on` reply `text` with a `homeassistant.turn_on` call for `target`, if any.
    fn lights(text: &str, target: Option<&[&str]>) -> Decision {
        let call = target.map(|target| ServiceCall {
            domain: "homeassistant".to_owned(),
            service: "turn_on".to_owned(),
            target: target.iter().map(|id| (*id).to_owned()).collect(),
            data: Map::new(),
            return_response: false,
        });
        let reply = Chosen {
            text: text.to_owned(),
            pool: "lights_on".to_owned(),
            index: 0,
        };
        Decision::Reply { reply, call }
    }

    /// The catalog with the `service` of `lights_on`.
    fn serviced(service: Option<&str>) -> Catalog {
        let mut catalog = catalog();
        catalog.intents[0].service = service.map(str::to_owned);
        catalog
    }

    /// The call of `decision`.
    fn called(decision: Decision) -> ServiceCall {
        match decision {
            Decision::Reply {
                call: Some(call), ..
            } => call,
            other => panic!("the decision makes a call: {other:?}"),
        }
    }

    /// The data of the call of `decision`.
    fn data(decision: Decision) -> Map<String, Value> {
        called(decision).data
    }

    /// The `media_content_id` of the `tv_show` call of `catalog` with `slots` in the office.
    fn content_id(catalog: &Catalog, slots: &[(&str, &str)]) -> String {
        let request = Request {
            transcript: String::new(),
            area: Some(AreaId("office".to_owned())),
        };
        let cache = StateCache::load(house());
        let decision = decided_for(catalog, ("tv_show", 0.89), slots, &request, &cache);
        let id = &data(decision)["media_content_id"];
        id.as_str()
            .expect("media_content_id is a string")
            .to_owned()
    }

    /// AC-4.5-2.
    #[test]
    fn ac_4_5_2_unknown_defers_with_transcript() {
        let transcript = "what is the weather like";
        assert_eq!(decided("unknown", 0.76, transcript), deferral(transcript));
    }

    /// AC-4.5-3.
    #[test]
    fn ac_4_5_3_low_probability_defers() {
        assert_eq!(
            decided("lights_on", 0.59, "turn on the lights"),
            deferral("turn on the lights")
        );
        assert_eq!(
            decided("lights_on", THRESHOLD, "turn on the lights"),
            lights("The kitchen lights.", Some(KITCHEN.as_slice()))
        );
    }

    /// AC-4.5-12.
    #[test]
    fn ac_4_5_12_label_not_in_catalog_defers() {
        assert_eq!(
            decided("open_garage", 0.95, "open the garage"),
            deferral("open the garage")
        );
    }

    /// AC-4.5b-3.
    #[test]
    fn ac_4_5b_3_unknown_slot_area_defers() {
        let decision = decided_in(&catalog(), "lights_on", Some("attic"), Some("kitchen"));
        assert_eq!(decision, deferral(""));
    }

    /// AC-4.5b-4.
    #[test]
    fn ac_4_5b_4_no_area_defers() {
        assert_eq!(
            decided_in(&catalog(), "lights_on", None, None),
            deferral("")
        );
    }

    /// AC-4.5b-5.
    #[test]
    fn ac_4_5b_5_no_service_replies_without_call() {
        let decision = decided_in(&serviced(None), "lights_on", None, Some("garage"));
        assert_eq!(decision, lights("The garage lights.", None));
    }

    /// AC-4.5c-1.
    #[test]
    fn ac_4_5c_1_kitchen_lights_call() {
        let decision = decided_in(&catalog(), "lights_on", None, Some("kitchen"));
        assert_eq!(
            decision,
            lights("The kitchen lights.", Some(KITCHEN.as_slice()))
        );
    }

    /// AC-4.5c-2.
    #[test]
    fn ac_4_5c_2_slot_area_replaces_satellite_area() {
        let office = [
            "light.office_fan_light",
            "light.desk_led",
            "light.office_lamp",
            "light.workbench_led",
        ];
        let decision = decided_in(&catalog(), "lights_on", Some("Office"), Some("kitchen"));
        assert_eq!(
            decision,
            lights("The office lights.", Some(office.as_slice()))
        );
    }

    /// AC-4.5c-3.
    #[test]
    fn ac_4_5c_3_empty_selection_defers() {
        let decision = decided_in(&catalog(), "lights_on", None, Some("bathroom"));
        assert_eq!(decision, deferral(""));
    }

    /// AC-4.5d-1.
    #[test]
    fn ac_4_5d_1_climate_down_sends_number() {
        let mut snapshot = house();
        let thermostat = snapshot
            .states
            .iter_mut()
            .find(|state| state.entity_id == "climate.thermostat");
        let thermostat = thermostat.expect("the house has a thermostat");
        thermostat
            .attributes
            .insert("temperature".to_owned(), Value::from(72));
        let request = Request {
            transcript: String::new(),
            area: Some(AreaId("office".to_owned())),
        };
        let cache = StateCache::load(snapshot);
        let decision = decided_for(&catalog(), ("climate_down", 0.89), &[], &request, &cache);
        assert_eq!(data(decision)["temperature"], Value::from(70));
    }

    /// AC-4.5d-2.
    #[test]
    fn ac_4_5d_2_show_slot_renders_string() {
        let id = content_id(&catalog(), &[("show", "24")]);
        assert!(
            id.contains(r#""show_name": "24""#),
            "media_content_id is {id}"
        );
    }

    /// AC-4.5d-3.
    #[test]
    fn ac_4_5d_3_missing_slots() {
        let mut catalog = catalog();
        let decision = decided_in(&catalog, "tv_show", None, Some("office"));
        assert_eq!(decision, deferral(""));
        catalog.intents[3].optional = vec!["show".to_owned()];
        let id = content_id(&catalog, &[]);
        assert!(
            id.contains(r#""show_name": """#),
            "media_content_id is {id}"
        );
    }

    /// A `service` without a dot, an unregistered area or an entity without a state gives `Defer`
    /// with `Job::Llm`, and a reply renders the selected entity.
    #[test]
    fn deferrals_and_entity_reply() {
        let undotted = serviced(Some("turn_on"));
        assert_eq!(
            decided_in(&undotted, "lights_on", None, Some("kitchen")),
            deferral("")
        );
        assert_eq!(
            decided_in(&serviced(None), "lights_on", None, Some("attic")),
            deferral("")
        );
        let mut catalog = catalog();
        let Decision::Reply { reply, .. } = decided_in(&catalog, "fan_on", None, Some("office"))
        else {
            panic!("fan_on in the office gives a reply");
        };
        assert_eq!(reply.text, "The Office Fan is on.");
        catalog.intents[1].entity = Some("fan.missing".to_owned());
        assert_eq!(
            decided_in(&catalog, "fan_on", None, Some("office")),
            deferral("")
        );
    }

    /// AC-4.6-1.
    #[test]
    fn ac_4_6_1_temperature_query_reads_sensor() {
        let cache = changed(THERMOSTAT, "21");
        let decision = decided_at(&catalog(), "temperature_query", "kitchen", &cache);
        assert_eq!(decision, queried("It is 21 degrees in the kitchen."));
    }

    /// AC-4.6-2.
    #[test]
    fn ac_4_6_2_missing_query_entity_defers() {
        let mut catalog = catalog();
        let cache = StateCache::load(house());
        let decision = decided_at(&catalog, "temperature_query", "bathroom", &cache);
        assert_eq!(decision, deferral(""));
        catalog.intents[4].entity = Some("sensor.missing".to_owned());
        let decision = decided_at(&catalog, "temperature_query", "kitchen", &cache);
        assert_eq!(decision, deferral(""));
    }

    /// The filler and the `get_weather` fetch with `when = today` of the pool `weather`.
    fn weather() -> Decision {
        let arguments = Map::from_iter([("when".to_owned(), Value::from("today"))]);
        let call = PluginCall {
            plugin: "get_weather".to_owned(),
            arguments,
        };
        let pool = "weather".to_owned();
        let job = Job::Fetch { call, pool };
        Decision::Defer {
            filler: filler(),
            job,
        }
    }

    /// AC-17.4-1.
    #[test]
    fn ac_17_4_1_weather_fetches_from_the_plugin() {
        let cache = StateCache::load(house());
        let decision = decided_at(&catalog(), "weather", "kitchen", &cache);
        assert_eq!(decision, weather());
    }

    /// AC-17.4-2.
    #[test]
    fn ac_17_4_2_weather_needs_no_area() {
        assert_eq!(decided_in(&catalog(), "weather", None, None), weather());
    }

    /// AC-17.4-3.
    #[test]
    fn ac_17_4_3_fetch_without_plugin_defers() {
        let mut catalog = catalog();
        for intent in &mut catalog.intents {
            intent.plugin = None;
        }
        let decision = decided_in(&catalog, "weather", None, Some("kitchen"));
        assert_eq!(decision, deferral(""));
    }

    /// AC-4.6-4.
    #[test]
    fn ac_4_6_4_query_has_no_call() {
        let mut catalog = catalog();
        catalog.intents[4].service = Some("homeassistant.turn_on".to_owned());
        let cache = changed(THERMOSTAT, "21");
        let decision = decided_at(&catalog, "temperature_query", "kitchen", &cache);
        assert_eq!(decision, queried("It is 21 degrees in the kitchen."));
    }

    /// AC-18.1-2.
    #[test]
    fn ac_18_1_2_list_query_names_the_kept_entities() {
        let catalog = listing("names", Target::Entities, Some("on"));
        let names = "Ali's Living Room Lamp, TV Lamp";
        assert_eq!(
            listed(&catalog, "names", "living_room"),
            answered("names", names)
        );
    }

    /// AC-18.1-3.
    #[test]
    fn ac_18_1_3_no_kept_entity_gives_empty_entities() {
        let catalog = listing("details", Target::Entities, Some("dimmed"));
        assert_eq!(
            listed(&catalog, "details", "living_room"),
            answered("details", "No entities.")
        );
    }

    /// AC-18.1-4.
    #[test]
    fn ac_18_1_4_empty_list_selection_defers() {
        let catalog = listing("names", Target::Entities, Some("on"));
        assert_eq!(listed(&catalog, "names", "bathroom"), deferral(""));
    }

    /// AC-18.1-5.
    #[test]
    fn ac_18_1_5_house_query_needs_no_area() {
        let mut catalog = listing("names", Target::House, Some("home"));
        let people = catalog
            .intents
            .last_mut()
            .expect("the catalog has the list query");
        people.select = vec![Filter {
            domain: "person".to_owned(),
            name: None,
            device_class: None,
        }];
        let decision = decided_in(&catalog, "names", None, None);
        assert_eq!(decision, answered("names", "Ali, Louis"));
    }

    /// AC-18.1-6.
    #[test]
    fn ac_18_1_6_list_without_state_keeps_each_entity_with_a_state() {
        let catalog = listing("details", Target::Entities, None);
        let mut snapshot = house();
        let corner = "light.living_room_corner_lamp";
        snapshot.states.retain(|state| state.entity_id != corner);
        let cache = StateCache::load(snapshot);
        let decision = decided_at(&catalog, "details", "living_room", &cache);
        let text = "[Ali's Living Room Lamp on Ali's Living Room Lamp]\
            [Louis's Living Room Lamp off Louis's Living Room Lamp]\
            [Living Room Overhead Light unavailable Living Room Overhead Light][TV Lamp on TV Lamp]";
        assert_eq!(decision, answered("details", text));
    }

    /// The decision of the intent `label` of `intents.toml` with the pools of `responses.toml`,
    /// the area slot `slot` and the satellite in `area` of `cache`.
    fn replied(label: &str, slot: Option<&str>, area: &str, cache: &StateCache) -> Decision {
        let request = Request {
            transcript: String::new(),
            area: Some(AreaId(area.to_owned())),
        };
        let slots: Vec<(&str, &str)> = slot.map(|value| ("area", value)).into_iter().collect();
        decided_with(&catalog(), &filed(), (label, 0.89), &slots, &request, cache)
    }

    /// AC-18.2-1.
    #[test]
    fn ac_18_2_1_lights_query_with_no_light_on_names_the_area() {
        let cache = StateCache::load(house());
        let decision = replied("lights_query", Some("garage"), "kitchen", &cache);
        let text = "No lights are on in the garage.";
        assert_eq!(decision, answered("lights_query", text));
    }

    /// AC-18.2-2.
    #[test]
    fn ac_18_2_2_lights_query_names_the_light_that_is_on() {
        let cache = StateCache::load(house());
        let decision = replied("lights_query", None, "kitchen", &cache);
        assert_eq!(decision, answered("lights_query", "Strawberry Lamp is on."));
    }

    /// AC-18.2-3.
    #[test]
    fn ac_18_2_3_brightness_query_gives_the_percent_of_each_light() {
        let mut snapshot = house();
        for state in &mut snapshot.states {
            match state.entity_id.as_str() {
                "light.office_fan_light" | "light.workbench_led" => state.state = "off".to_owned(),
                "light.office_lamp" => {
                    let brightness = Value::from(128);
                    state.attributes.insert("brightness".to_owned(), brightness);
                }
                _ => {}
            }
        }
        let office = StateCache::load(snapshot);
        let decision = replied("brightness_query", None, "office", &office);
        let text = "Desk LED is at 100 percent. Office Lamp is at 50 percent.";
        assert_eq!(decision, answered("brightness_query", text));
        let cache = StateCache::load(house());
        let decision = replied("brightness_query", None, "bedroom", &cache);
        let text = "Bedroom Fan Light is at 100 percent. Bedroom Lamp Left has no brightness \
            value. Bedroom Lamp Right has no brightness value. Bedroom Corner Lamp has no \
            brightness value.";
        assert_eq!(decision, answered("brightness_query", text));
    }

    /// AC-18.2-4.
    #[test]
    fn ac_18_2_4_repository_catalog_and_pools_are_valid() {
        let catalog = catalog();
        assert_eq!(catalog::validate_catalog(&catalog), Ok(()));
        let pools = filed();
        assert_eq!(responses::validate_pools(&pools, &catalog), Ok(()));
        let names = ["lights_query", "brightness_query"];
        let pooled = names.iter().all(|name| pools.contains_key(*name));
        assert!(pooled, "the pools {:?} have {names:?}", pools.keys());
    }

    /// The reference house with the lock, the garage door and the windows of sub-phase 18.3.
    /// The Kitchen Window has the state `kitchen`, and the Bedroom Window is `off`.
    fn secured(kitchen: &str) -> StateCache {
        let mut snapshot = house();
        let added = [
            ("lock.front_door", "Front Door", "outside", None, "locked"),
            (
                "cover.garage_door",
                "Garage Door",
                "garage",
                Some("garage"),
                "open",
            ),
            (
                "binary_sensor.kitchen_window",
                "Kitchen Window",
                "kitchen",
                Some("window"),
                kitchen,
            ),
            (
                "binary_sensor.bedroom_window",
                "Bedroom Window",
                "bedroom",
                Some("window"),
                "off",
            ),
        ];
        for entity in added {
            add(&mut snapshot, entity);
        }
        StateCache::load(snapshot)
    }

    /// Adds the entity with the ID, name, area, device class and state of `added` to
    /// `snapshot`, with no device.
    fn add(snapshot: &mut Snapshot, added: (&str, &str, &str, Option<&str>, &str)) {
        let (id, name, area, class, value) = added;
        let mut entity = snapshot.entities[0].clone();
        (entity.entity_id, entity.name) = (EntityId(id.to_owned()), name.to_owned());
        (entity.area_id, entity.device_id) = (Some(AreaId(area.to_owned())), None);
        let mut state = snapshot.states[0].clone();
        (state.entity_id, state.state) = (id.to_owned(), value.to_owned());
        let class = class.map(|class| ("device_class".to_owned(), Value::from(class)));
        state.attributes = class.into_iter().collect();
        snapshot.entities.push(entity);
        snapshot.states.push(state);
    }

    /// AC-18.3-1.
    #[test]
    fn ac_18_3_1_lock_query_names_the_lock_state() {
        let decision = replied("lock_query", None, "kitchen", &secured("on"));
        assert_eq!(
            decision,
            answered("lock_query", "The Front Door is locked.")
        );
    }

    /// AC-18.3-2.
    #[test]
    fn ac_18_3_2_garage_door_query_names_the_door_state() {
        let decision = replied("garage_door_query", None, "kitchen", &secured("on"));
        let text = "The Garage Door is open.";
        assert_eq!(decision, answered("garage_door_query", text));
    }

    /// AC-18.3-3.
    #[test]
    fn ac_18_3_3_windows_query_names_each_window_that_is_not_closed() {
        let cases = [
            ("on", "Kitchen Window is open."),
            ("off", "All windows are closed."),
            ("unavailable", "Kitchen Window is unavailable."),
        ];
        for (kitchen, text) in cases {
            let decision = replied("windows_query", None, "kitchen", &secured(kitchen));
            assert_eq!(
                decision,
                answered("windows_query", text),
                "Kitchen Window {kitchen}"
            );
        }
    }

    /// AC-18.4-1.
    #[test]
    fn ac_18_4_1_climate_query_gives_the_target_and_the_mode() {
        let cache = StateCache::load(house());
        let decision = replied("climate_query", None, "bedroom", &cache);
        let text = "The thermostat is set to 66 degrees in heat mode.";
        assert_eq!(decision, answered("climate_query", text));
    }

    /// AC-18.4-2.
    #[test]
    fn ac_18_4_2_climate_query_with_the_thermostat_off() {
        let cache = changed("climate.thermostat", "off");
        let decision = replied("climate_query", None, "bedroom", &cache);
        let text = "The thermostat is off.";
        assert_eq!(decision, answered("climate_query", text));
    }

    /// AC-18.4-3.
    #[test]
    fn ac_18_4_3_fan_query_names_the_fan_state() {
        let cache = StateCache::load(house());
        let decision = replied("fan_query", None, "living_room", &cache);
        let text = "The Living Room Fan is off.";
        assert_eq!(decision, answered("fan_query", text));
    }

    /// The reference house with each person of `people` `not_home`.
    fn away(people: &[&str]) -> StateCache {
        let mut snapshot = house();
        for state in &mut snapshot.states {
            if people.contains(&state.entity_id.as_str()) {
                state.state = "not_home".to_owned();
            }
        }
        StateCache::load(snapshot)
    }

    /// AC-18.5-1.
    #[test]
    fn ac_18_5_1_who_is_home_names_each_person_at_home() {
        let decision = replied("who_is_home", None, "kitchen", &away(&[]));
        let text = "Ali and Louis are home.";
        assert_eq!(decision, answered("who_is_home", text));
    }

    /// AC-18.5-2.
    #[test]
    fn ac_18_5_2_who_is_home_leaves_out_each_person_away() {
        let cases = [
            (&["person.louis"][..], "Ali is home."),
            (&["person.ali", "person.louis"][..], "Nobody is home."),
        ];
        for (people, text) in cases {
            let decision = replied("who_is_home", None, "kitchen", &away(people));
            assert_eq!(decision, answered("who_is_home", text), "away {people:?}");
        }
    }

    /// AC-18.5-3.
    #[test]
    fn ac_18_5_3_alarm_query_names_the_alarm_state() {
        let cases = [
            ("armed_home", "The alarm is armed home."),
            ("disarmed", "The alarm is off."),
        ];
        for (state, text) in cases {
            let cache = changed("alarm_control_panel.spring_alarm", state);
            let decision = replied("alarm_query", None, "kitchen", &cache);
            assert_eq!(decision, answered("alarm_query", text), "alarm {state}");
        }
    }

    /// The speaker of the Living Room that is not on the satellite device.
    const SPEAKER: &str = "media_player.carson_living_room_media_player";

    /// The TV of the Living Room.
    const TV: &str = "media_player.apple_tv";

    /// The reference house with each player of `players` `playing` with its media attributes.
    fn playing(players: &[(&str, &[(&str, Value)])]) -> StateCache {
        let mut snapshot = house();
        for state in &mut snapshot.states {
            let found = players.iter().find(|(id, _)| state.entity_id == *id);
            if let Some((_, media)) = found {
                state.state = "playing".to_owned();
                let media = media
                    .iter()
                    .map(|(name, value)| ((*name).to_owned(), value.clone()));
                state.attributes.extend(media);
            }
        }
        StateCache::load(snapshot)
    }

    /// AC-18.6-1.
    #[test]
    fn ac_18_6_1_now_playing_names_each_player_that_plays_in_the_area() {
        let title = ("media_title", Value::from("Karma Police"));
        let song = [title, ("media_artist", Value::from("Radiohead"))];
        let cache = playing(&[(SPEAKER, &song)]);
        let decision = replied("now_playing", None, "living_room", &cache);
        let text = "Karma Police by Radiohead is playing on the Carson Living Room Media Player.";
        assert_eq!(decision, answered("now_playing", text));
        let satellite = "media_player.carson_living_room";
        let cache = playing(&[(SPEAKER, &song), (TV, &[]), (satellite, &[])]);
        let decision = replied("now_playing", None, "living_room", &cache);
        let text = format!("{text} Something with no title is playing on the Apple TV.");
        assert_eq!(decision, answered("now_playing", &text));
    }

    /// AC-18.6-2.
    #[test]
    fn ac_18_6_2_now_playing_reads_the_area_of_the_slot() {
        let show = [
            ("media_title", Value::from("Bluey")),
            ("media_artist", Value::Null),
        ];
        let cache = playing(&[(TV, &show)]);
        let decision = replied("now_playing", Some("living room"), "office", &cache);
        let text = "Bluey is playing on the Apple TV.";
        assert_eq!(decision, answered("now_playing", text));
    }

    /// AC-18.6-3.
    #[test]
    fn ac_18_6_3_now_playing_with_nothing_playing_names_the_area() {
        let decision = replied("now_playing", None, "living_room", &playing(&[]));
        let text = "Nothing is playing in the living room.";
        assert_eq!(decision, answered("now_playing", text));
    }

    /// The Kitchen speaker that is not on the satellite device.
    const KITCHEN_SPEAKER: &str = "media_player.carson_kitchen_media_player";

    /// The decision for `label` with the slots `slots` on the satellite in `area`.
    fn played(label: &str, slots: &[(&str, &str)], area: &str) -> Decision {
        let request = Request {
            transcript: String::new(),
            area: Some(AreaId(area.to_owned())),
        };
        let cache = StateCache::load(house());
        decided_for(&catalog(), (label, 0.89), slots, &request, &cache)
    }

    /// AC-18.7-1.
    #[test]
    fn ac_18_7_1_play_music_calls_music_assistant_on_the_speaker_of_the_area() {
        let call = called(played("play_music", &[("media", "radiohead")], "kitchen"));
        let mut data = Map::new();
        data.insert("media_id".to_owned(), Value::from("radiohead"));
        let expected = ServiceCall {
            domain: "music_assistant".to_owned(),
            service: "play_media".to_owned(),
            target: vec![KITCHEN_SPEAKER.to_owned()],
            data,
            return_response: false,
        };
        assert_eq!(call, expected);
    }

    /// AC-18.7-2.
    #[test]
    fn ac_18_7_2_play_music_plays_in_the_area_of_the_slot() {
        let slots = [("media", "jazz"), ("area", "kitchen")];
        let call = called(played("play_music", &slots, "office"));
        assert_eq!(call.target, [KITCHEN_SPEAKER]);
    }

    /// AC-18.7-3.
    #[test]
    fn ac_18_7_3_play_radio_sends_radio_mode_as_a_boolean() {
        let data = data(played("play_radio", &[("media", "radiohead")], "kitchen"));
        assert_eq!(data["radio_mode"], Value::Bool(true));
    }

    /// AC-18.7-4.
    #[test]
    fn ac_18_7_4_queue_music_adds_to_the_queue() {
        let data = data(played("queue_music", &[("media", "yesterday")], "kitchen"));
        assert_eq!(data["enqueue"], Value::from("add"));
    }

    /// AC-18.7-5.
    #[test]
    fn ac_18_7_5_play_music_with_only_a_satellite_speaker_defers() {
        let decision = played("play_music", &[("media", "radiohead")], "bedroom");
        assert_eq!(decision, deferral(""));
    }

    /// AC-7.6-3.
    #[test]
    fn ac_7_6_3_area_states_give_the_unskipped_entities_of_the_area() {
        let cache = StateCache::load(house());
        let light = EntityState {
            name: "Garage Light".to_owned(),
            entity_id: "switch.garage_light".to_owned(),
            state: "off".to_owned(),
        };
        assert_eq!(area_states("garage", &cache), Some(vec![light]));
        assert_eq!(area_states("attic", &cache), None);
    }

    /// The domains of `openai.read_services` in `jarvis.toml`.
    const READ_DOMAINS: [&str; 2] = ["calendar", "todo"];

    /// The reference house with `calendar.family` and `light.porch`, both with no area and hidden
    /// by `hidden_by`.
    fn unassigned_house(hidden_by: Option<&str>) -> StateCache {
        let mut snapshot = house();
        for (id, name) in [("calendar.family", "Family"), ("light.porch", "Porch")] {
            let mut state = snapshot.states[0].clone();
            state.entity_id = id.to_owned();
            state.state = "off".to_owned();
            let mut entity = snapshot.entities[0].clone();
            entity.entity_id = EntityId(id.to_owned());
            entity.name = name.to_owned();
            (entity.area_id, entity.device_id) = (None, None);
            entity.hidden_by = hidden_by.map(str::to_owned);
            snapshot.states.push(state);
            snapshot.entities.push(entity);
        }
        StateCache::load(snapshot)
    }

    /// The unassigned entities of the question from the kitchen in `cache`.
    fn unassigned_of(cache: &StateCache) -> Vec<EntityState> {
        let kitchen = AreaId("kitchen".to_owned());
        let now = jiff::civil::date(2026, 10, 26).at(8, 0, 0, 0);
        question(
            "what is on",
            Some(&kitchen),
            "Home",
            now,
            cache,
            &READ_DOMAINS,
        )
        .unassigned
    }

    /// AC-7.7-1.
    #[test]
    fn ac_7_7_1_question_has_the_calendar_with_no_area() {
        let family = EntityState {
            name: "Family".to_owned(),
            entity_id: "calendar.family".to_owned(),
            state: "off".to_owned(),
        };
        assert_eq!(unassigned_of(&unassigned_house(None)), [family]);
    }

    /// AC-7.7-2.
    #[test]
    fn ac_7_7_2_question_leaves_out_a_light_with_no_area() {
        let unassigned = unassigned_of(&unassigned_house(None));
        let lights = unassigned
            .iter()
            .filter(|item| item.entity_id.starts_with("light."));
        assert_eq!(lights.count(), 0, "{unassigned:?}");
    }

    /// AC-7.7-4.
    #[test]
    fn ac_7_7_4_question_leaves_out_a_hidden_calendar() {
        assert_eq!(unassigned_of(&unassigned_house(Some("user"))), []);
    }

    /// AC-4.6-5.
    #[test]
    fn ac_4_6_5_query_reads_lowest_unskipped_sensor() {
        let mut catalog = catalog();
        let cache = StateCache::load(house());
        let decision = decided_at(&catalog, "temperature_query", "office", &cache);
        assert_eq!(decision, queried("It is 64.0 degrees in the office."));
        catalog.intents[4].exclude = vec!["sensor.climate_sensor_air_temperature".to_owned()];
        let decision = decided_at(&catalog, "temperature_query", "office", &cache);
        assert_eq!(decision, queried("It is 63.5 degrees in the office."));
    }
}
