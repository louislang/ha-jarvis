//! Trusted Verus contracts and axioms for the `std` and domain crate functions that the domain calls.

use std::borrow::Borrow;
use std::hash::Hash;
use std::sync::Arc;

use jiff::Timestamp;
use serde_json::Map;
use serde_json::Value;

use crate::domain::catalog::Catalog;
use crate::domain::catalog::Filter;
use crate::domain::catalog::Intent;
use crate::domain::catalog::Kind;
use crate::domain::catalog::Target;
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

} // verus!
