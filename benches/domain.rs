//! Instruction budgets of the domain (`Callgrind::hard_limits`).

#![expect(
    missing_docs,
    clippy::must_use_candidate,
    reason = "Gungraun macros generate undocumented items and refuse doc comments."
)]

use std::hint;
use std::sync::Arc;

use async_trait::async_trait;
use futures::executor;

use gungraun::Callgrind;
use gungraun::EventKind;
use gungraun::LibraryBenchmarkConfig;
use gungraun::library_benchmark;
use gungraun::library_benchmark_group;
use gungraun::main;
use jarvis::app::speech::Speech;
use jarvis::domain::responses::Pool;
use jarvis::domain::responses::Pools;
use jarvis::domain::responses::Response;
use jarvis::domain::speech::Audio;
use jarvis::domain::state::Area;
use jarvis::domain::state::AreaId;
use jarvis::domain::state::Context;
use jarvis::domain::state::Device;
use jarvis::domain::state::Entity;
use jarvis::domain::state::EntityId;
use jarvis::domain::state::Event;
use jarvis::domain::state::HomeConfig;
use jarvis::domain::state::Snapshot;
use jarvis::domain::state::State;
use jarvis::domain::state_cache::StateCache;
use jarvis::ports::TextToSpeech;
use jiff::Timestamp;

/// Entities in the benchmark cache.
const ENTITIES: usize = 2000;

/// Areas in the benchmark cache.
const AREAS: usize = 20;

/// Entity domains, in turn.
const DOMAINS: [&str; 4] = ["light", "switch", "sensor", "binary_sensor"];

/// The ID of entity `index`.
fn entity_id(index: usize) -> String {
    format!("{}.entity_{index}", DOMAINS[index % DOMAINS.len()])
}

/// The ID of the area of entity or device `index`.
fn area_id(index: usize) -> AreaId {
    AreaId(format!("area_{}", index % AREAS))
}

/// The state of entity `index`, with a `device_class` attribute.
fn state(index: usize, second: i64) -> State {
    let mut attributes = serde_json::Map::new();
    attributes.insert("device_class".to_owned(), "motion".into());
    State {
        entity_id: entity_id(index),
        state: "off".to_owned(),
        attributes,
        last_updated: Timestamp::from_second(second).expect("the second is in range"),
        context: Context {
            id: format!("context_{index}_{second}"),
            user_id: None,
            parent_id: None,
        },
    }
}

/// A cache with `ENTITIES` entities and one device for each. Every third entity takes the area of its device.
fn cache() -> StateCache {
    let indices = 0..ENTITIES;
    let areas: Vec<Area> = (0..AREAS)
        .map(|index| Area {
            area_id: area_id(index),
            name: format!("Area {index}"),
            aliases: Vec::new(),
        })
        .collect();
    let devices: Vec<Device> = indices
        .clone()
        .map(|index| Device {
            id: format!("device_{index}"),
            area_id: Some(area_id(index)),
        })
        .collect();
    let entities: Vec<Entity> = indices
        .clone()
        .map(|index| Entity {
            entity_id: EntityId(entity_id(index)),
            name: format!("Entity {index}"),
            aliases: Vec::new(),
            area_id: (index % 3 != 0).then(|| area_id(index)),
            device_id: Some(format!("device_{index}")),
            entity_category: None,
            hidden_by: None,
            disabled_by: None,
        })
        .collect();
    StateCache::load(Snapshot {
        states: indices.map(|index| state(index, 0)).collect(),
        areas,
        devices,
        entities,
        config: HomeConfig {
            time_zone: "UTC".to_owned(),
            location_name: "Bench".to_owned(),
        },
        user_id: String::new(),
    })
}

/// A cache that is never dropped, so that the benchmark measures no drop.
fn shared_cache() -> &'static StateCache {
    Box::leak(Box::new(cache()))
}

/// A cache and a newer state of one of its entities.
fn cache_and_event() -> (StateCache, Event) {
    let new = state(ENTITIES / 2, 1);
    let event = Event::StateChanged {
        entity_id: new.entity_id.clone(),
        old: None,
        context: new.context.clone(),
        new: Some(Box::new(new)),
    };
    (cache(), event)
}

/// A `TextToSpeech` that gives 2 s of silence at 44.1 kHz.
struct Silence;

#[async_trait]
impl TextToSpeech for Silence {
    async fn synth(&self, _text: &str) -> anyhow::Result<Audio> {
        Ok(Audio {
            samples: vec![0; 88_200],
            rate: 44_100,
        })
    }
}

/// A speech use case that has the 2 s audio of "One moment." in its cache and is never dropped.
fn cached_speech() -> &'static Speech {
    let speech = Speech::new(Arc::new(Silence));
    let response = Response {
        text: "One moment.".to_owned(),
        uses: 0,
    };
    let pool = Pool {
        responses: vec![response],
        last: None,
    };
    let pools = Pools::from([("filler".to_owned(), pool)]);
    executor::block_on(speech.prerender(&pools, &["Office".to_owned()]))
        .expect("the silence has no error");
    Box::leak(Box::new(speech))
}

/// A Callgrind configuration that fails above `instructions`.
fn budget(instructions: u64) -> LibraryBenchmarkConfig {
    let mut callgrind = Callgrind::default();
    callgrind.hard_limits([(EventKind::Ir, instructions)]);
    let mut config = LibraryBenchmarkConfig::default();
    config.tool(callgrind);
    config
}

// `state` of one entity.
#[library_benchmark(config = budget(5_000))]
#[bench::entities_2000(shared_cache())]
fn state_lookup(cache: &'static StateCache) -> Option<&'static State> {
    cache.state(hint::black_box("sensor.entity_1002"))
}

// `entities` of one area, domain and device class.
#[library_benchmark(config = budget(1_000_000))]
#[bench::entities_2000(shared_cache())]
fn entities(cache: &'static StateCache) -> Vec<&'static EntityId> {
    let area = hint::black_box(AreaId("area_3".to_owned()));
    cache.entities(&area, hint::black_box("binary_sensor"), Some("motion"))
}

// `apply` of a newer state.
#[library_benchmark(config = budget(1_000_000))]
#[bench::entities_2000(cache_and_event())]
fn apply(input: (StateCache, Event)) -> StateCache {
    let (cache, event) = input;
    cache.apply(event)
}

library_benchmark_group!(
    name = ac_2_8c_1_cache_budgets,
    benchmarks = [state_lookup, entities, apply]
);

// `speak` of a cached response of 2 s.
#[library_benchmark(config = budget(100_000))]
#[bench::seconds_2(cached_speech())]
fn cache_hit(speech: &'static Speech) -> anyhow::Result<Arc<Audio>> {
    executor::block_on(speech.speak(hint::black_box("One moment.")))
}

library_benchmark_group!(name = ac_6_3b_4_cache_hit_budget, benchmarks = [cache_hit]);

main!(
    library_benchmark_groups = ac_2_8c_1_cache_budgets,
    ac_6_3b_4_cache_hit_budget
);
