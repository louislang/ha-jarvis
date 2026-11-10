//! Response pools: select, render, record use, backfill policy and validation.

use std::collections::BTreeMap;
use std::collections::BTreeSet;

use minijinja::Environment;
use minijinja::ErrorKind;
use minijinja::UndefinedBehavior;
use minijinja::value::Serde;
use rand::Rng;
use rand::rngs::StdRng;
use serde::Deserialize;
use serde::Serialize;
use serde_json::Value;
use text2num::Language;
use vstd::prelude::*;

use crate::domain::catalog;
use crate::domain::catalog::Catalog;
use crate::domain::catalog::Kind;
use crate::domain::catalog::Target;
use crate::domain::intent;

/// The response pools by pool name.
pub type Pools = BTreeMap<String, Pool>;

/// The responses of one intent or one filler type.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct Pool {
    /// The response templates.
    pub responses: Vec<Response>,
    /// The index of the response that Jarvis used last.
    pub last: Option<usize>,
}

/// One response template and its use count.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct Response {
    /// The template text.
    pub text: String,
    /// The number of times that Jarvis used the response.
    pub uses: u64,
    /// True when a backfill generated the response, so it is not in `responses.toml`.
    #[serde(default)]
    pub generated: bool,
}

/// A failed pool check.
#[verus_verify]
#[must_use]
#[derive(Clone, Debug, PartialEq)]
pub enum PoolError {
    /// The pool with this name does not exist.
    Missing(String),
    /// The pool with this name has no responses.
    Empty(String),
    /// A template of the pool with this name does not parse.
    Template(String),
    /// A template of a pool uses a variable that the pool does not have.
    Variable {
        /// The pool name.
        pool: String,
        /// The variable name.
        variable: String,
    },
}

/// A rejected backfill candidate.
#[verus_verify]
#[must_use]
#[derive(Clone, Debug, PartialEq)]
pub enum CandidateError {
    /// The candidate has no words.
    Empty,
    /// The candidate has more characters than the limit.
    Long,
    /// The candidate equals a response of the pool after `normalize`.
    Duplicate,
    /// The candidate does not parse, or does not render with `area = "kitchen"`.
    Template,
    /// The candidate uses this variable, which the intent does not have.
    Variable(String),
}

/// The index of a random response of `pool` with the lowest use count, other than the last
/// response when the pool has two or more.
///
/// # Panics
///
/// `pool` has no responses.
#[verus_spec(index =>
    requires
        pool.responses.len() > 0,
    ensures
        index < pool.responses.len(),
        pool.responses.len() >= 2 ==> pool.last != Some(index),
        forall|other: int| allowed(*pool, other)
            ==> pool.responses[index as int].uses <= #[trigger] pool.responses[other].uses,
        index == chosen(*pool, crate::domain::contracts::next(&*old(rng))),
)]
#[must_use]
pub fn select(pool: &Pool, rng: &mut StdRng) -> usize {
    let random = rng.next_u32() as usize;
    let found = candidates(pool);
    proof! {
        let count = pool.responses.len() as int;
        assert(allowed(*pool, 0) || allowed(*pool, 1));
        lemma_minimum_found(*pool, count);
        lemma_minimum_lowest(*pool, count);
        lemma_matching_members(*pool, count, minimum(*pool, count));
    }
    found[random % found.len()]
}

/// True when `select` can give the response at `index` of `pool`.
#[verus_spec(result =>
    ensures
        result == allowed(*pool, index as int),
)]
fn is_allowed(pool: &Pool, index: usize) -> bool {
    index < pool.responses.len() && (pool.responses.len() < 2 || pool.last != Some(index))
}

/// The indices of the allowed responses of `pool` with the lowest use count, in order.
#[verus_spec(found =>
    ensures
        found@ == matching(*pool, pool.responses.len() as int, minimum(*pool, pool.responses.len() as int)),
)]
fn candidates(pool: &Pool) -> Vec<usize> {
    let mut lowest = u64::MAX;
    let mut found = Vec::new();
    let mut index = 0;
    #[cfg_attr(verus_only, verus_spec(
        invariant
            index <= pool.responses.len(),
            lowest == minimum(*pool, index as int),
            found@ == matching(*pool, index as int, lowest),
        decreases pool.responses.len() - index,
    ))]
    while index < pool.responses.len() {
        let uses = pool.responses[index].uses;
        if is_allowed(pool, index) && uses < lowest {
            proof! {
                lemma_none_below(*pool, index as int, uses);
            }
            lowest = uses;
            found = Vec::new();
        }
        if is_allowed(pool, index) && uses == lowest {
            found.push(index);
        }
        index += 1;
    }
    found
}

/// `pools` with one more use of the response at `index` of the pool `name`, which becomes the
/// last response. A count of `u64::MAX` stays. `pools` does not change when it has no such
/// response.
#[verus_spec(updated =>
    ensures
        recorded(pools@, updated@, crate::domain::contracts::string_key(name@), index as int),
)]
#[must_use]
pub fn record_use(mut pools: Pools, name: &str, index: usize) -> Pools {
    proof_decl! {
        let ghost before = pools@;
    }
    let Some(pool) = pools.get_mut(name) else {
        return pools;
    };
    if index < pool.responses.len() {
        pool.responses[index].uses = pool.responses[index].uses.saturating_add(1);
        pool.last = Some(index);
    }
    proof! {
        let key = crate::domain::contracts::string_key(name@);
        if index >= before[key].responses.len() {
            assert(pools@ =~= pools@.remove(key).insert(key, pools@[key]));
            assert(before =~= before.remove(key).insert(key, before[key]));
        }
    }
    pools
}

/// `template` rendered with the variables of `context`. An undefined value is an error.
///
/// # Errors
///
/// `template` does not parse, or it uses a value that `context` does not have.
#[verus_verify]
pub fn render(
    template: &str,
    context: &serde_json::Map<String, Value>,
) -> Result<String, minijinja::Error> {
    let mut environment = environment();
    // Without debug mode, an undefined error does not name the variable.
    environment.set_debug(true);
    environment.render_str(template, Serde(context))
}

/// A template environment where an undefined value is an error, with the filter `number`.
#[must_use]
#[verus_verify]
pub fn environment<'source>() -> Environment<'source> {
    let mut environment = Environment::new();
    environment.set_undefined_behavior(UndefinedBehavior::Strict);
    environment.add_filter("number", number);
    environment
}

/// `value` with number words as digits: "two" gives "2". Digits stay unchanged.
///
/// # Errors
///
/// `value` is not a number.
#[verus_verify]
fn number(value: &str) -> Result<String, minijinja::Error> {
    // The second pass writes numbers as words, and HA refuses "two" as a number.
    if let Ok(digits) = text2num::text2digits(value, &Language::english()) {
        return Ok(digits);
    }
    if value.parse::<u64>().is_ok() {
        return Ok(value.to_owned());
    }
    Err(minijinja::Error::new(
        ErrorKind::InvalidOperation,
        "not a number",
    ))
}

/// Checks that each intent of `catalog` and each required pool has a pool, and that each pool has
/// responses whose templates parse and use only the variables of the pool.
///
/// # Errors
///
/// The first check that fails.
#[verus_spec(result =>
    ensures
        result is Ok ==> valid_pools(pools@, *catalog),
)]
pub fn validate_pools(pools: &Pools, catalog: &Catalog) -> Result<(), PoolError> {
    let mut index = 0;
    #[cfg_attr(verus_only, verus_spec(
        invariant
            index <= catalog.intents@.len(),
            pooled(pools@, *catalog, index as int),
        decreases catalog.intents@.len() - index,
    ))]
    while index < catalog.intents.len() {
        require(pools, catalog.intents[index].name.as_str())?;
        index += 1;
    }
    require(pools, "filler")?;
    require(pools, "failure")?;
    require(pools, "ask_area")?;
    require(pools, "cancelled")?;
    check_pools(pools, catalog)
}

/// Checks each pool of `pools` with `check_pool`.
#[verus_spec(result =>
    ensures
        result is Ok ==> filled(pools@),
)]
fn check_pools(pools: &Pools, catalog: &Catalog) -> Result<(), PoolError> {
    let entries = pools.iter();
    proof_decl! {
        let ghost all = vstd::std_specs::iter::IteratorSpec::remaining(&entries);
    }
    #[cfg_attr(verus_only, verus_spec(entry_iter =>
        invariant
            entry_iter.seq() == all,
            forall|name: String| #[trigger] pools@.contains_key(name) ==> all.contains((&name, &pools@[name])),
            forall|seen: int| 0 <= seen < entry_iter.index() ==> (#[trigger] all[seen]).1.responses.len() > 0,
        ensures
            forall|seen: int| 0 <= seen < all.len() ==> (#[trigger] all[seen]).1.responses.len() > 0,
    ))]
    for (name, pool) in entries {
        check_pool(name, pool, catalog)?;
    }
    Ok(())
}

/// Checks that `pools` has the pool `name`.
#[verus_spec(result =>
    ensures
        result is Ok ==> pools@.contains_key(crate::domain::contracts::string_key(name@)),
)]
fn require(pools: &Pools, name: &str) -> Result<(), PoolError> {
    if pools.contains_key(name) {
        Ok(())
    } else {
        Err(PoolError::Missing(name.to_owned()))
    }
}

/// Checks that the pool `name` has responses whose templates parse and use only its variables.
#[verus_spec(result =>
    ensures
        result is Ok ==> pool.responses.len() > 0,
)]
fn check_pool(name: &str, pool: &Pool, catalog: &Catalog) -> Result<(), PoolError> {
    if pool.responses.is_empty() {
        return Err(PoolError::Empty(name.to_owned()));
    }
    let allowed = variables(name, catalog);
    for response in &pool.responses {
        check_template(name, response.text.as_str(), &allowed)?;
    }
    Ok(())
}

/// The template variables of the pool `name`: none when no intent of `catalog` has the name.
#[verus_verify]
fn variables(name: &str, catalog: &Catalog) -> BTreeSet<String> {
    let mut variables = BTreeSet::new();
    let Some(intent) = catalog
        .intents
        .iter()
        .find(|intent| intent.name.as_str() == name)
    else {
        return variables;
    };
    catalog::insert_slots(intent, &mut variables);
    for key in intent.habit_defaults.keys() {
        variables.insert(key.clone());
    }
    variables.insert("area".to_owned());
    if intent.target == Target::Entity {
        variables.insert("entity".to_owned());
    } else if intent.kind == Kind::Query {
        variables.insert("entities".to_owned());
    }
    if intent.kind == Kind::Fetch {
        variables.insert("response".to_owned());
    }
    if intent.needs_speaker {
        variables.insert("person".to_owned());
    }
    variables
}

/// Checks that `template` of the pool `pool` parses and uses only `allowed` variables.
#[verus_verify]
fn check_template(pool: &str, template: &str, allowed: &BTreeSet<String>) -> Result<(), PoolError> {
    match catalog::unknown_variable(template, allowed) {
        Err(_) => Err(PoolError::Template(pool.to_owned())),
        Ok(Some(variable)) => Err(PoolError::Variable {
            pool: pool.to_owned(),
            variable,
        }),
        Ok(None) => Ok(()),
    }
}

/// True when `pool` gets new responses: fewer than `min_fresh` are unused, it has fewer than
/// `max_pool`, and its templates use no variable but `area`, so each one can be pre-rendered.
#[verus_spec(result =>
    ensures
        result ==> fresh(pool.responses@) < min_fresh && pool.responses.len() < max_pool,
)]
#[must_use]
pub fn needs_backfill(pool: &Pool, min_fresh: usize, max_pool: usize) -> bool {
    pool.responses.len() < max_pool && count_fresh(pool) < min_fresh && only_area(pool)
}

/// The number of responses of `pool` with no use.
#[verus_spec(count =>
    ensures
        count == fresh(pool.responses@),
)]
fn count_fresh(pool: &Pool) -> usize {
    let mut count = 0;
    let mut index = 0;
    #[cfg_attr(verus_only, verus_spec(
        invariant
            index <= pool.responses.len(),
            count <= index,
            count == fresh(pool.responses@.take(index as int)),
        decreases pool.responses.len() - index,
    ))]
    while index < pool.responses.len() {
        proof! {
            assert(pool.responses@.take(index + 1).drop_last() =~= pool.responses@.take(index as int));
        }
        if pool.responses[index].uses == 0 {
            count += 1;
        }
        index += 1;
    }
    proof! {
        assert(pool.responses@.take(index as int) =~= pool.responses@);
    }
    count
}

/// True when each template of `pool` parses and uses no variable but `area`.
#[verus_verify]
fn only_area(pool: &Pool) -> bool {
    let mut area = BTreeSet::new();
    area.insert("area".to_owned());
    for response in &pool.responses {
        if !matches!(
            catalog::unknown_variable(response.text.as_str(), &area),
            Ok(None)
        ) {
            return false;
        }
    }
    true
}

/// Checks that the backfill `candidate` for `pool` has words, has at most `max_chars`
/// characters, differs from each response after `normalize`, uses only `variables` and renders
/// with `area = "kitchen"`.
///
/// # Errors
///
/// The first check that fails.
#[verus_spec(result =>
    ensures
        result is Ok ==> fits(*pool, candidate@, max_chars as int),
)]
pub fn validate(
    candidate: &str,
    pool: &Pool,
    variables: &BTreeSet<String>,
    max_chars: usize,
) -> Result<(), CandidateError> {
    let normal = intent::normalize(candidate);
    if normal.is_empty() {
        return Err(CandidateError::Empty);
    }
    let chars: Vec<char> = candidate.chars().collect();
    if chars.len() > max_chars {
        return Err(CandidateError::Long);
    }
    if repeats(pool, normal.as_str()) {
        return Err(CandidateError::Duplicate);
    }
    match catalog::unknown_variable(candidate, variables) {
        Err(_) => return Err(CandidateError::Template),
        Ok(Some(variable)) => return Err(CandidateError::Variable(variable)),
        Ok(None) => {}
    }
    let mut context = serde_json::Map::new();
    context.insert("area".to_owned(), Value::from("kitchen".to_owned()));
    match render(candidate, &context) {
        Ok(_) => Ok(()),
        Err(_) => Err(CandidateError::Template),
    }
}

/// True when a response of `pool` equals `normal` after `normalize`.
#[verus_spec(result =>
    ensures
        result == !distinct(*pool, normal@),
)]
fn repeats(pool: &Pool, normal: &str) -> bool {
    let mut index = 0;
    #[cfg_attr(verus_only, verus_spec(
        invariant
            index <= pool.responses.len(),
            forall|seen: int| 0 <= seen < index
                ==> intent::normalized(#[trigger] pool.responses[seen].text@) != normal@,
        decreases pool.responses.len() - index,
    ))]
    while index < pool.responses.len() {
        if intent::normalize(pool.responses[index].text.as_str()).as_str() == normal {
            return true;
        }
        index += 1;
    }
    false
}

/// `pool` with `candidates` appended as unused responses, up to `max_pool` responses.
#[verus_spec(merged =>
    ensures
        merged.last == pool.last,
        merged.responses@ == appended(pool.responses@, candidates@, max_pool as int),
        pool.responses.len() <= max_pool ==> merged.responses.len() <= max_pool,
)]
#[must_use]
pub fn merge(mut pool: Pool, candidates: &[String], max_pool: usize) -> Pool {
    proof_decl! {
        let ghost before = pool;
    }
    let mut index = 0;
    #[cfg_attr(verus_only, verus_spec(
        invariant
            index <= candidates.len(),
            index == 0 || before.responses.len() + index <= max_pool,
            pool.last == before.last,
            pool.responses@ == before.responses@ + unused(candidates@.take(index as int)),
        decreases candidates.len() - index,
    ))]
    while index < candidates.len() && pool.responses.len() < max_pool {
        pool.responses.push(Response {
            text: candidates[index].clone(),
            uses: 0,
            generated: true,
        });
        proof! {
            assert(unused(candidates@.take(index + 1)) =~= unused(candidates@.take(index as int)).push(pool.responses@.last()));
        }
        index += 1;
    }
    pool
}

verus! {

broadcast use {
    crate::domain::contracts::axiom_string_obeys_cmp,
    crate::domain::contracts::axiom_string_borrow,
    crate::domain::contracts::axiom_string_contains_borrowed_key,
    crate::domain::contracts::axiom_string_maps_borrowed_key,
    crate::domain::contracts::axiom_string_removes_borrowed_key,
};

/// True when each intent of `catalog` before `count` has a pool in `pools`.
pub open spec fn pooled(pools: Map<String, Pool>, catalog: Catalog, count: int) -> bool {
    forall|index: int| 0 <= index < count
        ==> pools.contains_key(crate::domain::contracts::string_key(#[trigger] catalog.intents@[index].name@))
}

/// True when each intent of `catalog` has a pool, the `filler`, `failure`, `ask_area` and
/// `cancelled` pools exist, and each pool has one or more responses.
pub open spec fn valid_pools(pools: Map<String, Pool>, catalog: Catalog) -> bool {
    &&& pooled(pools, catalog, catalog.intents@.len() as int)
    &&& pools.contains_key(crate::domain::contracts::string_key("filler"@))
    &&& pools.contains_key(crate::domain::contracts::string_key("failure"@))
    &&& pools.contains_key(crate::domain::contracts::string_key("ask_area"@))
    &&& pools.contains_key(crate::domain::contracts::string_key("cancelled"@))
    &&& filled(pools)
}

/// True when each pool of `pools` has one or more responses.
pub open spec fn filled(pools: Map<String, Pool>) -> bool {
    forall|name: String| #[trigger] pools.contains_key(name) ==> pools[name].responses.len() > 0
}

/// True when `select` can give the response at `index` of `pool`.
pub open spec fn allowed(pool: Pool, index: int) -> bool {
    &&& 0 <= index < pool.responses.len()
    &&& pool.responses.len() < 2 || pool.last != Some(index as usize)
}

/// The lowest use count of the allowed responses before `count`, or `u64::MAX` when none is.
pub open spec fn minimum(pool: Pool, count: int) -> u64
    decreases count,
{
    if count <= 0 {
        u64::MAX
    } else {
        let before = minimum(pool, count - 1);
        let uses = pool.responses[count - 1].uses;
        if allowed(pool, count - 1) && uses < before { uses } else { before }
    }
}

/// The indices before `count` of the allowed responses with `lowest` uses, in order.
pub open spec fn matching(pool: Pool, count: int, lowest: u64) -> Seq<usize>
    decreases count,
{
    if count <= 0 {
        seq![]
    } else {
        let before = matching(pool, count - 1, lowest);
        if allowed(pool, count - 1) && pool.responses[count - 1].uses == lowest {
            before.push((count - 1) as usize)
        } else {
            before
        }
    }
}

/// The index that `select` gives for `pool` and the random value `random`.
pub open spec fn chosen(pool: Pool, random: u32) -> usize {
    let found = matching(pool, pool.responses.len() as int, minimum(pool, pool.responses.len() as int));
    found[random as int % found.len() as int]
}

/// True when `new` is `old` with one more use of the response at `index`, which is the last.
pub open spec fn used(old: Pool, new: Pool, index: int) -> bool {
    let response = old.responses[index];
    let uses = if response.uses < u64::MAX { (response.uses + 1) as u64 } else { response.uses };
    &&& new.last == Some(index as usize)
    &&& new.responses@ == old.responses@.update(index, Response { text: response.text, uses, generated: response.generated })
}

/// True when `new` is `old` with one more use of the response at `index` of the pool `key`, or
/// `old` when it has no such response.
pub open spec fn recorded(old: Map<String, Pool>, new: Map<String, Pool>, key: String, index: int) -> bool {
    if old.contains_key(key) && 0 <= index < old[key].responses.len() {
        &&& new.contains_key(key)
        &&& new.remove(key) == old.remove(key)
        &&& used(old[key], new[key], index)
    } else {
        new == old
    }
}

/// The number of `responses` with no use.
pub open spec fn fresh(responses: Seq<Response>) -> int
    decreases responses.len(),
{
    if responses.len() == 0 {
        0
    } else {
        fresh(responses.drop_last()) + if responses.last().uses == 0 { 1int } else { 0int }
    }
}

/// True when no response of `pool` equals `normal` after `normalize`.
pub open spec fn distinct(pool: Pool, normal: Seq<char>) -> bool {
    forall|index: int| 0 <= index < pool.responses.len()
        ==> crate::domain::intent::normalized(#[trigger] pool.responses[index].text@) != normal
}

/// True when `candidate` has words, has at most `max_chars` characters and differs from each
/// response of `pool` after `normalize`.
pub open spec fn fits(pool: Pool, candidate: Seq<char>, max_chars: int) -> bool {
    let normal = crate::domain::intent::normalized(candidate);
    &&& normal.len() > 0
    &&& candidate.len() <= max_chars
    &&& distinct(pool, normal)
}

/// Unused generated responses with the texts `texts`.
pub open spec fn unused(texts: Seq<String>) -> Seq<Response> {
    texts.map_values(|text: String| Response { text, uses: 0, generated: true })
}

/// `responses`, then unused responses with the first `candidates`, up to `max_pool` responses.
pub open spec fn appended(responses: Seq<Response>, candidates: Seq<String>, max_pool: int) -> Seq<Response> {
    let room = if max_pool > responses.len() { max_pool - responses.len() } else { 0 };
    let count = if candidates.len() < room { candidates.len() as int } else { room };
    responses + unused(candidates.take(count))
}

/// Every allowed response before `count` has `minimum(pool, count)` uses or more.
pub proof fn lemma_minimum_lowest(pool: Pool, count: int)
    ensures
        forall|other: int| 0 <= other < count && allowed(pool, other)
            ==> minimum(pool, count) <= #[trigger] pool.responses[other].uses,
    decreases count,
{
    if count > 0 {
        lemma_minimum_lowest(pool, count - 1);
    }
}

/// Each index of `matching(pool, count, lowest)` is an allowed response with `lowest` uses.
pub proof fn lemma_matching_members(pool: Pool, count: int, lowest: u64)
    ensures
        forall|position: int| 0 <= position < matching(pool, count, lowest).len() ==> {
            let index = #[trigger] matching(pool, count, lowest)[position] as int;
            index < count && allowed(pool, index) && pool.responses[index].uses == lowest
        },
    decreases count,
{
    if count > 0 {
        lemma_matching_members(pool, count - 1, lowest);
        let before = matching(pool, count - 1, lowest);
        assert forall|position: int| 0 <= position < before.len()
            implies #[trigger] matching(pool, count, lowest)[position] == before[position] by {}
    }
}

/// No allowed response before `count` has fewer uses than `minimum(pool, count)`.
pub proof fn lemma_none_below(pool: Pool, count: int, uses: u64)
    requires
        uses < minimum(pool, count),
    ensures
        matching(pool, count, uses) == Seq::<usize>::empty(),
{
    lemma_minimum_lowest(pool, count);
    lemma_matching_members(pool, count, uses);
    let below = matching(pool, count, uses);
    if below.len() > 0 {
        assert(pool.responses[below[0] as int].uses == uses);
    }
    assert(below =~= seq![]);
}

/// The candidates for the minimum before `count` are not empty, or no response before `count` is
/// allowed and the minimum is `u64::MAX`.
pub proof fn lemma_minimum_found(pool: Pool, count: int)
    ensures
        matching(pool, count, minimum(pool, count)).len() > 0 || minimum(pool, count) == u64::MAX
            && forall|other: int| 0 <= other < count ==> !#[trigger] allowed(pool, other),
    decreases count,
{
    if count > 0 {
        lemma_minimum_found(pool, count - 1);
    }
}

} // verus!

#[cfg(test)]
mod tests {
    use std::ops::Range;

    use rand::SeedableRng;

    use super::*;

    /// The pools of the repository.
    const RESPONSES: &str = include_str!("../../responses.toml");

    /// The intent catalog of the repository.
    const INTENTS: &str = include_str!("../../intents.toml");

    /// The result of `validate_pools` for the repository files after `change`.
    fn validate(change: impl FnOnce(&mut Pools, &mut Catalog)) -> Result<(), PoolError> {
        let file: toml::Table = toml::from_str(RESPONSES).expect("responses.toml is TOML");
        let mut pools = Pools::new();
        for (name, pool) in file["pools"].as_table().expect("pools is a table") {
            let texts = pool["responses"].as_array().expect("responses is an array");
            let responses = texts.iter().map(|text| Response {
                text: text.as_str().expect("a response is a string").to_owned(),
                uses: 0,
                generated: false,
            });
            let pool = Pool {
                responses: responses.collect(),
                last: None,
            };
            pools.insert(name.clone(), pool);
        }
        let mut catalog = toml::from_str(INTENTS).expect("intents.toml is a catalog");
        change(&mut pools, &mut catalog);
        validate_pools(&pools, &catalog)
    }

    /// Sets the responses of the pool `name` of `pools` to `texts`.
    fn set(pools: &mut Pools, name: &str, texts: &[&str]) {
        let pool = pools.get_mut(name).expect("the pool exists");
        pool.responses = self::texts(texts).responses;
    }

    /// The error of a template of the pool `pool` that uses `variable`.
    fn unknown(pool: &str, variable: &str) -> Result<(), PoolError> {
        Err(PoolError::Variable {
            pool: pool.to_owned(),
            variable: variable.to_owned(),
        })
    }

    /// A pool with one response for each of `uses` and the last response `last`.
    fn pool(uses: &[u64], last: Option<usize>) -> Pool {
        let responses: Vec<Response> = uses
            .iter()
            .map(|&uses| Response {
                text: String::new(),
                uses,
                generated: false,
            })
            .collect();
        Pool { responses, last }
    }

    /// The index that `select` gives for `pool` with an RNG from each seed of `seeds`.
    fn selected(pool: &Pool, seeds: Range<u64>) -> Vec<usize> {
        seeds
            .map(|seed| select(pool, &mut StdRng::seed_from_u64(seed)))
            .collect()
    }

    /// AC-4.1-1.
    #[test]
    fn ac_4_1_1_select_never_returns_last() {
        let indices = selected(&pool(&[0, 0, 0], Some(1)), 0..50);
        assert!(
            !indices.contains(&1) && indices.contains(&0) && indices.contains(&2),
            "{indices:?}"
        );
    }

    /// AC-4.1-2.
    #[test]
    fn ac_4_1_2_select_returns_a_lowest_count() {
        let indices = selected(&pool(&[2, 1, 3, 1, 0], Some(4)), 0..50);
        assert!(
            indices.iter().all(|&index| index == 1 || index == 3),
            "{indices:?}"
        );
    }

    /// AC-4.1-3.
    #[test]
    fn ac_4_1_3_same_rng_gives_same_index() {
        let pool = pool(&[0; 8], None);
        let first = selected(&pool, 0..20);
        assert!(
            first == selected(&pool, 0..20) && first.iter().any(|&index| index != 0),
            "{first:?}"
        );
    }

    /// AC-4.1-4.
    #[test]
    fn ac_4_1_4_record_use_counts_one_use() {
        let pools = Pools::from([
            ("a".to_owned(), pool(&[0, 4, 2], None)),
            ("b".to_owned(), pool(&[1], None)),
        ]);
        let mut expected = pools.clone();
        let used = expected.get_mut("a").expect("pool a is in the map");
        used.responses[1].uses = 5;
        used.last = Some(1);
        assert_eq!(record_use(pools.clone(), "a", 1), expected);
        assert_eq!(record_use(pools.clone(), "c", 0), pools);
        assert_eq!(record_use(pools.clone(), "a", 3), pools);
    }

    /// AC-4.1-5.
    #[test]
    fn ac_4_1_5_one_response_is_selected() {
        assert_eq!(selected(&pool(&[3], Some(0)), 0..20), [0; 20]);
    }

    /// AC-4.2-1.
    #[test]
    fn ac_4_2_1_render_substitutes_area() {
        let context = serde_json::Map::from_iter([("area".to_owned(), Value::from("kitchen"))]);
        let text = render("Powering up the {{ area }} lights.", &context);
        assert_eq!(
            text.ok().as_deref(),
            Some("Powering up the kitchen lights.")
        );
    }

    /// AC-4.2-2.
    #[test]
    fn ac_4_2_2_undefined_variable_names_it() {
        let context = serde_json::Map::from_iter([("area".to_owned(), Value::from("kitchen"))]);
        let result = render("Playing {{ show }} in the {{ area }}.", &context);
        let error = result.expect_err("show is undefined").to_string();
        assert!(error.contains("`show`"), "{error}");
    }

    /// AC-4.2-3.
    #[test]
    fn ac_4_2_3_intent_without_pool_names_intent() {
        assert_eq!(validate(|_, _| {}), Ok(()));
        let result = validate(|pools, _| _ = pools.remove("fan_on"));
        assert_eq!(result, Err(PoolError::Missing("fan_on".to_owned())));
    }

    /// AC-4.2-4.
    #[test]
    fn ac_4_2_4_template_syntax_error_names_pool() {
        let result = validate(|pools, _| set(pools, "tv_show", &["Playing {{ show"]));
        assert_eq!(result, Err(PoolError::Template("tv_show".to_owned())));
    }

    /// AC-4.2-5.
    #[test]
    fn ac_4_2_5_missing_required_pool_names_it() {
        for name in ["filler", "failure"] {
            let result = validate(|pools, _| _ = pools.remove(name));
            assert_eq!(result, Err(PoolError::Missing(name.to_owned())));
        }
    }

    /// AC-4.2-6.
    #[test]
    fn ac_4_2_6_unknown_variable_names_pool_and_variable() {
        let result = validate(|pools, _| set(pools, "tv_show", &["{{ show }} in {{ season }}."]));
        assert_eq!(result, unknown("tv_show", "season"));
        let result = validate(|pools, catalog| {
            let speed = ("speed".to_owned(), "fan.set_percentage".to_owned());
            catalog.intents[1].habit_defaults.extend([speed]);
            catalog.intents[1].needs_speaker = true;
            set(
                pools,
                "fan_on",
                &["{{ speed }} {{ area }} {{ entity }} {{ person }}"],
            );
        });
        assert_eq!(result, Ok(()));
    }

    /// AC-4.2-7.
    #[test]
    fn ac_4_2_7_empty_pool_names_pool() {
        let result = validate(|pools, _| set(pools, "failure", &[]));
        assert_eq!(result, Err(PoolError::Empty("failure".to_owned())));
    }

    /// AC-4.2-8.
    #[test]
    fn ac_4_2_8_unavailable_built_in_names_pool_and_variable() {
        let result = validate(|pools, _| set(pools, "lights_on", &["{{ entity.state }}"]));
        assert_eq!(result, unknown("lights_on", "entity"));
        let result = validate(|pools, _| set(pools, "fan_on", &["{{ response }}"]));
        assert_eq!(result, unknown("fan_on", "response"));
        let result = validate(|pools, _| set(pools, "filler", &["One moment, {{ area }}."]));
        assert_eq!(result, unknown("filler", "area"));
    }

    /// AC-10.4-3.
    #[test]
    fn ac_10_4_3_missing_ask_area_names_it() {
        let result = validate(|pools, _| _ = pools.remove("ask_area"));
        assert_eq!(result, Err(PoolError::Missing("ask_area".to_owned())));
    }

    /// AC-10.5c-3.
    #[test]
    fn ac_10_5c_3_missing_cancelled_names_it() {
        let result = validate(|pools, _| _ = pools.remove("cancelled"));
        assert_eq!(result, Err(PoolError::Missing("cancelled".to_owned())));
    }

    /// The result of `validate_pools` when the pool `pool` is `template` and `temperature_query`
    /// has `target`.
    fn targeted(target: Target, pool: &str, template: &str) -> Result<(), PoolError> {
        validate(|pools, catalog| {
            catalog.intents[4].target = target;
            set(pools, pool, &[template]);
        })
    }

    /// AC-18.1-7.
    #[test]
    fn ac_18_1_7_list_variables_belong_to_list_pools() {
        let query = "temperature_query";
        let result = targeted(Target::Entity, query, "{{ entities }}");
        assert_eq!(result, unknown(query, "entities"));
        let result = targeted(Target::Entity, "lights_on", "{{ entities }}");
        assert_eq!(result, unknown("lights_on", "entities"));
        let result = targeted(Target::House, query, "{{ entity.state }}");
        assert_eq!(result, unknown(query, "entity"));
        let result = targeted(Target::House, query, "{{ entities | length }}");
        assert_eq!(result, Ok(()));
    }

    /// A pool with one unused response for each of `texts`.
    fn texts(texts: &[&str]) -> Pool {
        let responses = texts.iter().map(|text| Response {
            text: (*text).to_owned(),
            uses: 0,
            generated: false,
        });
        Pool {
            responses: responses.collect(),
            last: None,
        }
    }

    /// The result of `validate` for `candidate` against the pool "Turning on the lights." with
    /// the variable `area` and at most 30 characters.
    fn check(candidate: &str) -> Result<(), CandidateError> {
        let area = BTreeSet::from(["area".to_owned()]);
        super::validate(candidate, &texts(&["Turning on the lights."]), &area, 30)
    }

    /// AC-8.1-1.
    #[test]
    fn ac_8_1_1_fewer_than_min_fresh_unused_needs_backfill() {
        assert!(needs_backfill(&pool(&[0, 0, 1], None), 3, 30));
        assert!(!needs_backfill(&pool(&[0, 0, 0, 1], None), 3, 30));
    }

    /// AC-8.1-2.
    #[test]
    fn ac_8_1_2_normalized_duplicate_is_rejected() {
        assert_eq!(
            check("turning on the  LIGHTS!"),
            Err(CandidateError::Duplicate)
        );
        assert_eq!(check("The {{ area }} lights are on."), Ok(()));
    }

    /// AC-8.1-3.
    #[test]
    fn ac_8_1_3_unknown_variable_is_rejected() {
        assert_eq!(
            check("Playing {{ show }}."),
            Err(CandidateError::Variable("show".to_owned()))
        );
    }

    /// AC-8.1-4.
    #[test]
    fn ac_8_1_4_empty_or_long_candidate_is_rejected() {
        assert_eq!(check(""), Err(CandidateError::Empty));
        assert_eq!(check(&"a".repeat(31)), Err(CandidateError::Long));
        assert_eq!(check(&"a".repeat(30)), Ok(()));
    }

    /// AC-8.1-5.
    #[test]
    fn ac_8_1_5_merge_appends_unused_candidates() {
        let mut old = texts(&["One.", "Two."]);
        old.responses[0].uses = 3;
        old.last = Some(0);
        let mut expected = old.clone();
        let mut added = texts(&["Three.", "Four."]).responses;
        for response in &mut added {
            response.generated = true;
        }
        expected.responses.extend(added);
        let candidates = ["Three.".to_owned(), "Four.".to_owned()];
        assert_eq!(merge(old, &candidates, 30), expected);
    }

    /// AC-8.1-6.
    #[test]
    fn ac_8_1_6_full_pool_gets_no_backfill() {
        let full = pool(&[0, 0, 0], None);
        assert!(!needs_backfill(&full, 5, 3));
        let candidates = ["New.".to_owned()];
        assert_eq!(merge(full.clone(), &candidates, 3), full);
        let merged = merge(pool(&[1, 1], None), &["A.".to_owned(), "B.".to_owned()], 3);
        assert_eq!(merged.responses.len(), 3);
    }

    /// AC-8.1-7.
    #[test]
    fn ac_8_1_7_variable_other_than_area_gets_no_backfill() {
        assert!(!needs_backfill(&texts(&["{{ entity }} is on."]), 3, 30));
        assert!(needs_backfill(&texts(&["In the {{ area }}."]), 3, 30));
    }
}
