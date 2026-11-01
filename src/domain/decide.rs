//! `decide`: a prediction and a request to a decision.

use rand::rngs::StdRng;
use serde_json::Map;
use serde_json::Value;
use vstd::prelude::*;

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
    /// Speak the reply from the pool `fallback`: Jarvis knows the intent but cannot act.
    Fallback {
        /// The reply.
        reply: Chosen,
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

/// The decision for `prediction` of `request`: a System Two job for an `unknown`, uncertain,
/// removed or non-command label, else the reply and the service call of the command in its area.
#[verus_spec(decision =>
    requires
        responses::valid_pools(pools@, *catalog),
    ensures
        decision is Reply ==> prediction.ranked@.len() > 0
            && prediction.ranked@[0].0@ != "unknown"@
            && vstd::std_specs::cmp::ge_ensures::<f32>(prediction.ranked@[0].1, threshold, true),
        prediction.ranked@.len() > 0 && !cataloged(*catalog, prediction.ranked@[0].0@)
            ==> deferred(decision, request.transcript),
        !(decision is Defer) ==> exists|index: int| #[trigger] commanded(*prediction, *catalog, threshold, index)
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
                && matches!(catalog.intents[index].kind, Kind::Command) =>
        {
            let decision = command(
                &catalog.intents[index],
                prediction,
                request,
                cache,
                pools,
                rng,
            );
            proof! {
                assert(commanded(*prediction, *catalog, threshold, index as int));
            }
            decision
        }
        _ => defer(request, pools, rng),
    }
}

/// The reply and the service call of the command `intent` in the area of the area slot, else of
/// the satellite, or `Fallback` when the slot names no area or a service has no area.
#[verus_spec(decision =>
    requires
        pools@.contains_key(crate::domain::contracts::string_key(intent.name@)),
        pools@.contains_key(crate::domain::contracts::string_key("fallback"@)),
        responses::filled(pools@),
    ensures
        acted(*intent, *prediction, *request, *cache, decision),
)]
fn command(
    intent: &Intent,
    prediction: &Prediction,
    request: &Request,
    cache: &StateCache,
    pools: &Pools,
    rng: &mut StdRng,
) -> Decision {
    let area = match area_slot(&prediction.slots) {
        Some(slot) => match cache.area_by_name(prediction.slots[slot].value.as_str()) {
            Some(area) => Some(area),
            None => return fallback(pools, rng),
        },
        None => request.area.as_ref(),
    };
    match (&intent.service, area) {
        (None, _) => respond(intent, area, &[], None, cache, pools, rng),
        (Some(service), Some(area)) => {
            proof! {
                assert(located(*prediction, *request, *cache, *area));
            }
            call(intent, service.as_str(), area, cache, pools, rng)
        }
        (Some(_), None) => fallback(pools, rng),
    }
}

/// The reply and the call of `service` on the selection of `intent` in `area`, or `Fallback`
/// when `service` has no dot, the selection is empty or the reply does not render.
#[verus_spec(decision =>
    requires
        pools@.contains_key(crate::domain::contracts::string_key(intent.name@)),
        pools@.contains_key(crate::domain::contracts::string_key("fallback"@)),
        responses::filled(pools@),
    ensures
        decision is Fallback || (decision matches Decision::Reply { call: Some(made), .. }
            && made.target@.len() > 0 && served(service@, made)
            && targeted(*intent, *cache, *area, entity_ids(made.target@))),
)]
fn call(
    intent: &Intent,
    service: &str,
    area: &AreaId,
    cache: &StateCache,
    pools: &Pools,
    rng: &mut StdRng,
) -> Decision {
    let Some((domain, name)) = service.split_once('.') else {
        return fallback(pools, rng);
    };
    let selection = select::select(intent, area, cache);
    if selection.is_empty() {
        return fallback(pools, rng);
    }
    let call = ServiceCall {
        domain: domain.to_owned(),
        service: name.to_owned(),
        target: texts(&selection),
        data: Map::new(),
        return_response: false,
    };
    respond(
        intent,
        Some(area),
        &selection,
        Some(call),
        cache,
        pools,
        rng,
    )
}

/// `Reply` with `call` and the reply of `intent` rendered for `area` and `selection`, or
/// `Fallback` when the reply does not render.
#[verus_spec(decision =>
    requires
        pools@.contains_key(crate::domain::contracts::string_key(intent.name@)),
        pools@.contains_key(crate::domain::contracts::string_key("fallback"@)),
        responses::filled(pools@),
    ensures
        decision is Fallback || (decision matches Decision::Reply { call: made, .. } && made == call),
)]
fn respond(
    intent: &Intent,
    area: Option<&AreaId>,
    selection: &[EntityId],
    call: Option<ServiceCall>,
    cache: &StateCache,
    pools: &Pools,
    rng: &mut StdRng,
) -> Decision {
    let mut reply = choose(pools, intent.name.as_str(), rng);
    let Some(context) = context(intent, area, selection, cache) else {
        return fallback(pools, rng);
    };
    let Ok(text) = responses::render(reply.text.as_str(), &context) else {
        return fallback(pools, rng);
    };
    reply.text = text;
    Decision::Reply { reply, call }
}

/// The template context of a reply of `intent`: the name of `area` in lowercase, and the first
/// entity of `selection` for `target = "entity"`. `None` when the cache has no state for it.
#[verus_verify]
fn context(
    intent: &Intent,
    area: Option<&AreaId>,
    selection: &[EntityId],
    cache: &StateCache,
) -> Option<Map<String, Value>> {
    let mut context = Map::new();
    let found = match area {
        Some(area) => cache.areas.iter().find(|known| known.area_id.0 == area.0),
        None => None,
    };
    if let Some(found) = found {
        context.insert("area".to_owned(), Value::from(found.name.to_lowercase()));
    }
    if let (Target::Entity, Some(id)) = (intent.target, selection.first()) {
        context.insert("entity".to_owned(), entity(cache, id)?);
    }
    Some(context)
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

/// The reply from the pool `fallback`.
#[verus_spec(decision =>
    requires
        pools@.contains_key(crate::domain::contracts::string_key("fallback"@)),
        responses::filled(pools@),
    ensures
        decision is Fallback,
)]
fn fallback(pools: &Pools, rng: &mut StdRng) -> Decision {
    Decision::Fallback {
        reply: choose(pools, "fallback", rng),
    }
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
};

/// True when `catalog` has an intent with the name `name`.
pub open spec fn cataloged(catalog: Catalog, name: Seq<char>) -> bool {
    exists|index: int| 0 <= index < catalog.intents@.len() && #[trigger] catalog.intents@[index].name@ == name
}

/// True when `decision` asks System Two `question`.
pub open spec fn deferred(decision: Decision, question: String) -> bool {
    decision matches Decision::Defer { job: Job::Llm(asked), .. } && asked@ == question@
}

/// True when the top label of `prediction` is confident and names the command at `index` of
/// `catalog`.
pub open spec fn commanded(prediction: Prediction, catalog: Catalog, threshold: f32, index: int) -> bool {
    &&& prediction.ranked@.len() > 0
    &&& prediction.ranked@[0].0@ != "unknown"@
    &&& vstd::std_specs::cmp::ge_ensures::<f32>(prediction.ranked@[0].1, threshold, true)
    &&& 0 <= index < catalog.intents@.len()
    &&& catalog.intents@[index].name@ == prediction.ranked@[0].0@
    &&& catalog.intents@[index].kind == Kind::Command
}

/// True when `decision` for the command `intent` obeys the area rules: an area slot that names no
/// area, or a service without an area, gives `Fallback`, and no service gives no call. A call
/// makes the service of the intent on its selection in the area of the request.
pub open spec fn acted(intent: Intent, prediction: Prediction, request: Request, cache: StateCache, decision: Decision) -> bool {
    &&& forall|slot: int| #[trigger] first_area(prediction.slots@, slot)
        && !area_named(cache, prediction.slots@[slot].value@) ==> decision is Fallback
    &&& unslotted(prediction.slots@) && request.area is None && intent.service is Some
        ==> decision is Fallback
    &&& intent.service is None
        ==> (decision is Fallback || decision matches Decision::Reply { call: None, .. })
    &&& decision matches Decision::Reply { call: Some(made), .. } ==> made.target@.len() > 0
        && (intent.service matches Some(service) && served(service@, made))
        && aimed(intent, prediction, request, cache, made)
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
    &&& !select::fixed(intent) && !select::any_local(intent, cache, area) ==> selected.len() <= 1
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

    use rand::SeedableRng;

    use crate::domain::model::Slot;
    use crate::domain::responses::Pool;
    use crate::domain::responses::Response;
    use crate::testing::fakes::house;

    /// The value of `model.threshold` in `jarvis.toml`.
    const THRESHOLD: f32 = 0.6;

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
            .chain(["filler", "failure", "fallback"])
            .map(|name| {
                let text = match name {
                    "lights_on" => "The {{ area }} lights.".to_owned(),
                    "fan_on" => "The {{ entity.name }} is {{ entity.state }}.".to_owned(),
                    _ => format!("The {name} response."),
                };
                let pool = Pool {
                    responses: vec![Response { text, uses: 0 }],
                    last: None,
                };
                (name.to_owned(), pool)
            })
            .collect()
    }

    /// The decision of `catalog` in the reference house for the top label and probability `top`,
    /// the area slot `slot` and `request`.
    fn decided_for(
        catalog: &Catalog,
        top: (&str, f32),
        slot: Option<&str>,
        request: &Request,
    ) -> Decision {
        let slots = slot.map(|value| Slot {
            name: "area".to_owned(),
            value: value.to_owned(),
        });
        let prediction = Prediction {
            ranked: vec![(top.0.to_owned(), top.1), ("fan_on".to_owned(), 0.01)],
            slots: slots.into_iter().collect(),
        };
        let cache = StateCache::load(house());
        let mut rng = StdRng::seed_from_u64(1);
        let pools = pools(catalog);
        decide(
            &prediction,
            request,
            catalog,
            &cache,
            &pools,
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
        decided_for(&catalog(), (label, probability), None, &request)
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
        decided_for(catalog, (label, 0.89), slot, &request)
    }

    /// The decision that defers `transcript` to System Two with the filler.
    fn deferral(transcript: &str) -> Decision {
        Decision::Defer {
            filler: Chosen {
                text: "The filler response.".to_owned(),
                pool: "filler".to_owned(),
                index: 0,
            },
            job: Job::Llm(transcript.to_owned()),
        }
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

    /// The decision from the pool `fallback`.
    fn fallback() -> Decision {
        let reply = Chosen {
            text: "The fallback response.".to_owned(),
            pool: "fallback".to_owned(),
            index: 0,
        };
        Decision::Fallback { reply }
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
    fn ac_4_5b_3_unknown_slot_area_falls_back() {
        let decision = decided_in(&catalog(), "lights_on", Some("attic"), Some("kitchen"));
        assert_eq!(decision, fallback());
    }

    /// AC-4.5b-4.
    #[test]
    fn ac_4_5b_4_no_area_falls_back() {
        assert_eq!(decided_in(&catalog(), "lights_on", None, None), fallback());
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
    fn ac_4_5c_3_empty_selection_falls_back() {
        let decision = decided_in(&catalog(), "lights_on", None, Some("bathroom"));
        assert_eq!(decision, fallback());
    }

    /// A `service` without a dot, an unregistered area or an entity without a state gives
    /// `Fallback`, and a reply renders the selected entity.
    #[test]
    fn fallbacks_and_entity_reply() {
        let undotted = serviced(Some("turn_on"));
        assert_eq!(
            decided_in(&undotted, "lights_on", None, Some("kitchen")),
            fallback()
        );
        assert_eq!(
            decided_in(&serviced(None), "lights_on", None, Some("attic")),
            fallback()
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
            fallback()
        );
    }
}
