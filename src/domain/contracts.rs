//! Trusted Verus contracts and axioms for the `std` and domain crate functions that the domain calls.

use std::borrow::Borrow;
use std::cmp::Ordering;
use std::convert::Infallible;
use std::hash::Hash;
use std::slice::SliceIndex;
use std::sync::Arc;

use jiff::Timestamp;
use minijinja::Environment;
use minijinja::Error;
use minijinja::Template;
use rand::Rng;
use rand::TryRng;
use rand::rngs::StdRng;
use serde_json::Map;
use serde_json::Value;

use crate::domain::catalog::Catalog;
use crate::domain::catalog::Filter;
use crate::domain::catalog::Intent;
use crate::domain::catalog::Kind;
use crate::domain::catalog::Target;
use crate::domain::intent::Span;
use crate::domain::state::Area;
use crate::domain::state::AreaId;
use crate::domain::state::Context;
use crate::domain::state::Device;
use crate::domain::state::Entity;
use crate::domain::state::EntityId;
use crate::domain::state::Event;
use crate::domain::state::HomeConfig;
use crate::domain::state::Snapshot;
use crate::domain::state::State;
use crate::domain::state_cache::StateCache;
use crate::domain::training::Example;
use crate::domain::training::ExampleFile;
use crate::domain::training::Label;
use crate::domain::training::SpanFile;
use crate::domain::training::TrainingSet;
use vstd::prelude::*;

verus! {

/// An instant in time. <https://docs.rs/jiff/0.2/jiff/struct.Timestamp.html>
#[verifier::external_type_specification]
#[verifier::external_body]
pub struct ExTimestamp(Timestamp);

/// A JSON object. <https://docs.rs/serde_json/1/serde_json/struct.Map.html>
#[verifier::external_type_specification]
#[verifier::external_body]
#[verifier::reject_recursive_types(Key)]
#[verifier::reject_recursive_types(Item)]
pub struct ExMap<Key, Item>(Map<Key, Item>);

/// A JSON value. <https://docs.rs/serde_json/1/serde_json/enum.Value.html>
#[verifier::external_type_specification]
#[verifier::external_body]
pub struct ExValue(Value);

/// The states, registries and configuration of HA at one time.
#[verifier::external_type_specification]
pub struct ExSnapshot(Snapshot);

/// The state of one entity.
#[verifier::external_type_specification]
pub struct ExState(State);

/// A change in HA.
#[verifier::external_type_specification]
pub struct ExEvent(Event);

/// The hot cache.
#[verifier::external_type_specification]
pub struct ExStateCache(StateCache);

/// The cause of a state change.
#[verifier::external_type_specification]
#[verifier::external_body]
pub struct ExContext(Context);

/// An area registry entry.
#[verifier::external_type_specification]
pub struct ExArea(Area);

/// A device registry entry.
#[verifier::external_type_specification]
pub struct ExDevice(Device);

/// An entity registry entry.
#[verifier::external_type_specification]
pub struct ExEntity(Entity);

/// The ID of an area.
#[verifier::external_type_specification]
pub struct ExAreaId(AreaId);

/// The ID of an entity.
#[verifier::external_type_specification]
pub struct ExEntityId(EntityId);

/// The HA configuration that Jarvis uses.
#[verifier::external_type_specification]
#[verifier::external_body]
pub struct ExHomeConfig(HomeConfig);

/// The intent catalog of `intents.toml`.
#[verifier::external_type_specification]
pub struct ExCatalog(Catalog);

/// One action that Jarvis can take.
#[verifier::external_type_specification]
pub struct ExIntent(Intent);

/// How Jarvis replies to an intent and acts on it.
#[verifier::external_type_specification]
#[verifier::external_body]
pub struct ExKind(Kind);

/// The entities that the service of an intent gets.
#[verifier::external_type_specification]
#[verifier::external_body]
pub struct ExTarget(Target);

/// An entity filter of `select`.
#[verifier::external_type_specification]
#[verifier::external_body]
pub struct ExFilter(Filter);

/// One slot value in the text of an example.
#[verifier::external_type_specification]
pub struct ExSpan(Span);

/// The training set file.
#[verifier::external_type_specification]
pub struct ExTrainingSet(TrainingSet);

/// One labeled transcript with its model context.
#[verifier::external_type_specification]
pub struct ExExample(Example);

/// The label of an example.
#[verifier::external_type_specification]
pub struct ExLabel(Label);

/// An example in the training set file.
#[verifier::external_type_specification]
pub struct ExExampleFile(ExampleFile);

/// One slot value in the text of an example in the training set file.
#[verifier::external_type_specification]
pub struct ExSpanFile(SpanFile);

/// A template environment. <https://docs.rs/minijinja/3/minijinja/struct.Environment.html>
#[verifier::external_type_specification]
#[verifier::external_body]
pub struct ExEnvironment<'source>(Environment<'source>);

/// A compiled template. <https://docs.rs/minijinja/3/minijinja/struct.Template.html>
#[verifier::external_type_specification]
#[verifier::external_body]
pub struct ExTemplate<'env: 'source, 'source>(Template<'env, 'source>);

/// A template error. <https://docs.rs/minijinja/3/minijinja/struct.Error.html>
#[verifier::external_type_specification]
#[verifier::external_body]
pub struct ExError(Error);

/// Creates an environment with the default filters, tests and globals. <https://docs.rs/minijinja/3/minijinja/struct.Environment.html#method.new>
#[verifier::external_fn_specification]
pub fn ex_environment_new<'source>() -> Environment<'source> {
    Environment::new()
}

/// Compiles `source` into a template. <https://docs.rs/minijinja/3/minijinja/struct.Environment.html#method.template_from_str>
#[verifier::external_fn_specification]
pub fn ex_environment_template_from_str<'source, 'env>(
    environment: &'env Environment<'source>,
    source: &'source str,
) -> Result<Template<'env, 'source>, Error> {
    environment.template_from_str(source)
}

/// Gives the variables that the template reads and does not set. <https://docs.rs/minijinja/3/minijinja/struct.Template.html#method.undeclared_variables>
#[verifier::external_fn_specification]
pub fn ex_template_undeclared_variables<'env, 'source>(
    template: &Template<'env, 'source>,
    nested: bool,
) -> std::collections::hash_set::HashSet<String> {
    template.undeclared_variables(nested)
}

/// The number of nanoseconds from the Unix epoch to `timestamp`.
pub uninterp spec fn nanosecond(timestamp: Timestamp) -> i128;

/// Gives the number of nanoseconds since the Unix epoch. <https://docs.rs/jiff/0.2/jiff/struct.Timestamp.html#method.as_nanosecond>
#[verifier::external_fn_specification]
pub fn ex_timestamp_as_nanosecond(timestamp: Timestamp) -> (result: i128)
    ensures
        result == nanosecond(timestamp),
{
    Timestamp::as_nanosecond(timestamp)
}

/// The `Arc<str>` with the text `text`.
pub uninterp spec fn str_key(text: Seq<char>) -> Arc<str>;

/// Moves the text of `text` into a new `Arc<str>`. <https://doc.rust-lang.org/std/sync/struct.Arc.html#impl-From%3CString%3E-for-Arc%3Cstr%3E>
#[verifier::external_fn_specification]
pub fn ex_arc_str_from(text: String) -> (arc: Arc<str>)
    ensures
        arc == str_key(text@),
{
    <Arc<str> as From<String>>::from(text)
}

/// `Arc<str>` compares by its text, and `str` has a total order. <https://doc.rust-lang.org/std/sync/struct.Arc.html#impl-Ord-for-Arc%3CT,+A%3E>
pub broadcast axiom fn axiom_arc_str_obeys_cmp()
    ensures
        #[trigger] vstd::laws_cmp::obeys_cmp::<Arc<str>>(),
;

/// `Arc<str>` borrows as `str` with the same order. <https://doc.rust-lang.org/std/borrow/trait.Borrow.html>
pub broadcast axiom fn axiom_arc_str_borrow()
    ensures
        #[trigger] vstd::std_specs::btree::borrowed_key_ordering_matches::<Arc<str>, str>(),
;

/// `Arc<str>` borrows as its text, so a lookup by `&str` finds the key with that text. <https://doc.rust-lang.org/std/sync/struct.Arc.html#impl-Borrow%3CT%3E-for-Arc%3CT,+A%3E>
pub broadcast axiom fn axiom_arc_str_contains_borrowed_key<Item>(map: vstd::map::Map<Arc<str>, Item>, key: &str)
    ensures
        #[trigger] vstd::std_specs::btree::contains_borrowed_key::<Arc<str>, Item, str>(map, key)
            <==> map.contains_key(str_key(key@)),
;

/// `Arc<str>` borrows as its text, so a lookup by `&str` gives the value of the key with that text. <https://doc.rust-lang.org/std/sync/struct.Arc.html#impl-Borrow%3CT%3E-for-Arc%3CT,+A%3E>
pub broadcast axiom fn axiom_arc_str_maps_borrowed_key<Item>(map: vstd::map::Map<Arc<str>, Item>, key: &str, item: Item)
    ensures
        #[trigger] vstd::std_specs::btree::maps_borrowed_key_to_value::<Arc<str>, Item, str>(map, key, item)
            <==> map.contains_key(str_key(key@)) && map[str_key(key@)] == item,
;

/// `String` has a total order. <https://doc.rust-lang.org/std/string/struct.String.html#impl-Ord-for-String>
pub broadcast axiom fn axiom_string_obeys_cmp()
    ensures
        #[trigger] vstd::laws_cmp::obeys_cmp::<String>(),
;

/// The `String` key with the text `text`.
pub uninterp spec fn string_key(text: Seq<char>) -> String;

/// `String` borrows as `str` with the same order. <https://doc.rust-lang.org/std/string/struct.String.html#impl-Borrow%3Cstr%3E-for-String>
pub broadcast axiom fn axiom_string_borrow()
    ensures
        #[trigger] vstd::std_specs::btree::borrowed_key_ordering_matches::<String, str>(),
;

/// `String` borrows as its text, so a lookup by `&str` finds the key with that text. <https://doc.rust-lang.org/std/string/struct.String.html#impl-Borrow%3Cstr%3E-for-String>
pub broadcast axiom fn axiom_string_contains_borrowed_key<Item>(map: vstd::map::Map<String, Item>, key: &str)
    ensures
        #[trigger] vstd::std_specs::btree::contains_borrowed_key::<String, Item, str>(map, key)
            <==> map.contains_key(string_key(key@)),
;

/// `String` borrows as its text, so a lookup by `&str` gives the value of the key with that text. <https://doc.rust-lang.org/std/string/struct.String.html#impl-Borrow%3Cstr%3E-for-String>
pub broadcast axiom fn axiom_string_maps_borrowed_key<Item>(map: vstd::map::Map<String, Item>, key: &str, item: Item)
    ensures
        #[trigger] vstd::std_specs::btree::maps_borrowed_key_to_value::<String, Item, str>(map, key, item)
            <==> map.contains_key(string_key(key@)) && map[string_key(key@)] == item,
;

/// `String` borrows as its text, so a removal by `&str` removes the key with that text. <https://doc.rust-lang.org/std/string/struct.String.html#impl-Borrow%3Cstr%3E-for-String>
pub broadcast axiom fn axiom_string_removes_borrowed_key<Item>(old: vstd::map::Map<String, Item>, new: vstd::map::Map<String, Item>, key: &str)
    ensures
        #[trigger] vstd::std_specs::btree::borrowed_key_removed::<String, Item, str>(old, new, key)
            <==> new == old.remove(string_key(key@)),
;

/// The value of `key` in the JSON object `map`.
pub uninterp spec fn attribute<Key: ?Sized>(map: Map<String, Value>, key: &Key) -> Option<Value>;

/// Gives the value of `key`. <https://docs.rs/serde_json/1/serde_json/struct.Map.html#method.get>
#[verifier::external_fn_specification]
pub fn ex_map_get<'map, Key>(map: &'map Map<String, Value>, key: &Key) -> (value: Option<&'map Value>)
    where
        String: Borrow<Key>,
        Key: ?Sized + Ord + Eq + Hash,
    ensures
        match value {
            Some(value) => attribute(*map, key) == Some(*value),
            None => attribute(*map, key) is None,
        },
{
    map.get(key)
}

/// The text of the JSON string `value`, or `None` for another JSON type.
pub uninterp spec fn text(value: Value) -> Option<Seq<char>>;

/// Gives the text of a JSON string. <https://docs.rs/serde_json/1/serde_json/enum.Value.html#method.as_str>
#[verifier::external_fn_specification]
pub fn ex_value_as_str(value: &Value) -> (result: Option<&str>)
    ensures
        match result {
            Some(result) => text(*value) == Some(result@),
            None => text(*value) is None,
        },
{
    value.as_str()
}

/// Gives the UTF-8 bytes of `text`. <https://doc.rust-lang.org/std/string/struct.String.html#method.as_bytes>
#[verifier::external_fn_specification]
pub fn ex_string_as_bytes(text: &String) -> (bytes: &[u8])
    ensures
        bytes@ == vstd::utf8::encode_utf8(text@),
{
    text.as_bytes()
}

/// Gives the length of `text` in bytes. <https://doc.rust-lang.org/std/string/struct.String.html#method.len>
#[verifier::external_fn_specification]
pub fn ex_string_len(text: &String) -> (len: usize)
    ensures
        len == vstd::utf8::encode_utf8(text@).len(),
{
    text.len()
}

/// Gives the subslice of `text` at `index`, or `None` when indexing would panic: the range is out
/// of bounds or does not start and end on character boundaries. <https://doc.rust-lang.org/std/primitive.str.html#method.get>
#[verifier::external_fn_specification]
pub fn ex_str_get<Index: SliceIndex<str>>(text: &str, index: Index) -> (part: Option<&Index::Output>)
    ensures
        match part {
            Some(part) => vstd::slice::SliceIndexSpec::in_bounds(&index, text)
                && vstd::slice::SliceIndexSpec::index_postcondition(&index, text, part),
            None => !vstd::slice::SliceIndexSpec::in_bounds(&index, text),
        },
{
    text.get(index)
}

/// The lowercase form of `text`.
pub uninterp spec fn lowercase(text: Seq<char>) -> Seq<char>;

/// Gives the lowercase form of `text`. <https://doc.rust-lang.org/std/primitive.str.html#method.to_lowercase>
#[verifier::external_fn_specification]
pub fn ex_str_to_lowercase(text: &str) -> (lower: String)
    ensures
        lower@ == lowercase(text@),
{
    text.to_lowercase()
}

/// Checks if `c` is an ASCII decimal digit. <https://doc.rust-lang.org/std/primitive.char.html#method.is_ascii_digit>
#[verifier::external_fn_specification]
pub fn ex_char_is_ascii_digit(c: &char) -> (digit: bool)
    ensures
        digit == ('0' <= *c && *c <= '9'),
{
    c.is_ascii_digit()
}

/// Maps `A` to `Z` to `a` to `z` and gives other characters unchanged. <https://doc.rust-lang.org/std/primitive.char.html#method.to_ascii_lowercase>
#[verifier::external_fn_specification]
pub fn ex_char_to_ascii_lowercase(c: &char) -> (lower: char)
    ensures
        'A' <= *c && *c <= 'Z' ==> 'a' <= lower && lower <= 'z',
        !('A' <= *c && *c <= 'Z') ==> lower == *c,
{
    c.to_ascii_lowercase()
}

/// True when `c` has the Unicode `Alphabetic` property.
pub uninterp spec fn alphabetic(c: char) -> bool;

/// Checks if `c` has the Unicode `Alphabetic` property. <https://doc.rust-lang.org/std/primitive.char.html#method.is_alphabetic>
#[verifier::external_fn_specification]
pub fn ex_char_is_alphabetic(c: char) -> (result: bool)
    ensures
        result == alphabetic(c),
{
    c.is_alphabetic()
}

/// The ASCII characters with the `Alphabetic` property are the letters `A` to `Z` and `a` to `z`. <https://www.unicode.org/Public/UCD/latest/ucd/DerivedCoreProperties.txt>
pub broadcast axiom fn axiom_ascii_alphabetic(c: char)
    ensures
        c <= '\x7f' ==> (#[trigger] alphabetic(c) <==> ('A' <= c && c <= 'Z') || ('a' <= c && c <= 'z')),
;

/// The standard RNG. <https://docs.rs/rand/0.10/rand/rngs/struct.StdRng.html>
#[verifier::external_type_specification]
#[verifier::external_body]
pub struct ExStdRng(StdRng);

/// An error value. <https://doc.rust-lang.org/std/error/trait.Error.html>
#[verifier::external_trait_specification]
pub trait ExStdError: std::fmt::Debug + std::fmt::Display {
    /// The trait that this specification is for.
    type ExternalTraitSpecificationFor: std::error::Error;
}

/// A random number generator that can fail. <https://docs.rs/rand_core/0.10/rand_core/trait.TryRng.html>
#[verifier::external_trait_specification]
pub trait ExTryRng {
    /// The trait that this specification is for.
    type ExternalTraitSpecificationFor: TryRng;

    /// The type of a generator error.
    type Error: std::error::Error;
}

/// The value that `next_u32` gives in the state `rng`.
pub uninterp spec fn next<Random: ?Sized>(rng: &Random) -> u32;

/// Gives the next random `u32`. A seeded RNG gives the same sequence. <https://docs.rs/rand_core/0.10/rand_core/trait.SeedableRng.html>
#[verifier::external_fn_specification]
pub fn ex_rng_next_u32<Random: TryRng<Error = Infallible> + ?Sized>(rng: &mut Random) -> (value: u32)
    ensures
        value == next(&*old(rng)),
{
    rng.next_u32()
}

/// The XXH3 64-bit hash of `bytes` with the default seed.
pub uninterp spec fn xxh3(bytes: Seq<u8>) -> u64;

/// Gives the XXH3 64-bit hash of `input`. <https://docs.rs/xxhash-rust/0.8/xxhash_rust/xxh3/fn.xxh3_64.html>
#[verifier::external_fn_specification]
pub fn ex_xxh3_64(input: &[u8]) -> (hash: u64)
    ensures
        hash == xxh3(input@),
{
    xxhash_rust::xxh3::xxh3_64(input)
}

/// True when `power` is a value that `f32::exp` can give for `exponent`.
pub uninterp spec fn exp_ensures(exponent: f32, power: f32) -> bool;

/// Gives `e^exponent`, with a precision that can change between calls. <https://doc.rust-lang.org/std/primitive.f32.html#method.exp>
#[verifier::external_fn_specification]
pub fn ex_f32_exp(exponent: f32) -> (power: f32)
    ensures
        exp_ensures(exponent, power),
{
    exponent.exp()
}

/// True when `order` is a total order: `a < b` exactly when `b > a`, and `<`, `==` and `>` are
/// each transitive. <https://doc.rust-lang.org/std/cmp/trait.Ord.html#how-can-i-implement-ord>
pub open spec fn total<T>(order: spec_fn(T, T) -> Ordering) -> bool {
    &&& forall|a: T, b: T| (#[trigger] order(a, b) == Ordering::Less) == (order(b, a) == Ordering::Greater)
    &&& forall|a: T, b: T, c: T| #[trigger] order(a, b) == #[trigger] order(b, c) ==> order(a, c) == order(a, b)
}

/// True when every call of `compare` on `a` and `b` can run and gives `order(a, b)`.
pub open spec fn gives<T, Compare: FnMut(&T, &T) -> Ordering>(compare: Compare, order: spec_fn(T, T) -> Ordering) -> bool {
    &&& forall|a: &T, b: &T| #[trigger] call_requires(compare, (a, b))
    &&& forall|a: &T, b: &T, ordering: Ordering| #[trigger] call_ensures(compare, (a, b), ordering) ==> ordering == order(*a, *b)
}

/// True when `compare` gives a total order.
pub open spec fn total_comparator<T, Compare: FnMut(&T, &T) -> Ordering>(compare: Compare) -> bool {
    exists|order: spec_fn(T, T) -> Ordering| #[trigger] total(order) && gives(compare, order)
}

/// True when `new[index]` is `old[places[index]]`, each index of `old` is in `places` once, `new`
/// is in the order `order`, and equal elements of `new` keep the order of their places.
pub open spec fn placed<T>(old: Seq<T>, new: Seq<T>, order: spec_fn(T, T) -> Ordering, places: Seq<int>) -> bool {
    &&& new.len() == old.len()
    &&& places.len() == old.len()
    &&& forall|index: int| 0 <= index < new.len() ==> 0 <= #[trigger] places[index] < old.len() && new[index] == old[places[index]]
    &&& forall|first: int, second: int| #![trigger places[first], places[second]] 0 <= first < second < new.len() ==> {
        &&& places[first] != places[second]
        &&& order(new[first], new[second]) != Ordering::Greater
        &&& order(new[first], new[second]) == Ordering::Equal ==> places[first] < places[second]
    }
}

/// True when `new` is `old` sorted by `order` with a stable sort.
pub open spec fn sorted_stable<T>(old: Seq<T>, new: Seq<T>, order: spec_fn(T, T) -> Ordering) -> bool {
    exists|places: Seq<int>| #[trigger] placed(old, new, order, places)
}

/// Sorts `slice` with `compare` and keeps the order of equal elements. `compare` must give a total
/// order. <https://doc.rust-lang.org/std/primitive.slice.html#method.sort_by>
#[verifier::external_fn_specification]
pub fn ex_slice_sort_by<T, Compare: FnMut(&T, &T) -> Ordering>(slice: &mut [T], compare: Compare)
    requires
        total_comparator(compare),
    ensures
        forall|order: spec_fn(T, T) -> Ordering| #[trigger] gives(compare, order) ==> sorted_stable(old(slice)@, final(slice)@, order),
{
    slice.sort_by(compare)
}

/// The IEEE 754 totalOrder of two floats.
pub uninterp spec fn total_order() -> spec_fn(f32, f32) -> Ordering;

/// Gives the IEEE 754 totalOrder of `left` and `right`. <https://doc.rust-lang.org/std/primitive.f32.html#method.total_cmp>
#[verifier::external_fn_specification]
pub fn ex_f32_total_cmp(left: &f32, right: &f32) -> (ordering: Ordering)
    ensures
        ordering == total_order()(*left, *right),
{
    left.total_cmp(right)
}

/// IEEE 754 totalOrder is a total order. <https://doc.rust-lang.org/std/primitive.f32.html#method.total_cmp>
pub axiom fn axiom_total_order_is_total()
    ensures
        total(total_order()),
;

} // verus!
