//! Trusted Verus contracts and axioms for the `std` and domain crate functions that the domain calls.

use std::borrow::Borrow;
use std::borrow::Cow;
use std::cmp::Ordering;
use std::convert::Infallible;
use std::hash::Hash;
use std::num::ParseIntError;
use std::slice::SliceIndex;
use std::str::FromStr;
use std::str::pattern::Pattern;
use std::sync::Arc;
use std::time::Duration;

use jiff::SignedDuration;
use jiff::Timestamp;
use jiff::civil::DateTime;
use minijinja::Environment;
use minijinja::Error;
use minijinja::ErrorKind;
use minijinja::Expression;
use minijinja::Template;
use minijinja::UndefinedBehavior;
use minijinja::functions::Function;
use minijinja::value::FunctionArgs;
use minijinja::value::FunctionResult;
use minijinja::value::Serde;
use rand::Rng;
use rand::TryRng;
use rand::rngs::StdRng;
use serde::Serialize;
use serde_json::Map;
use serde_json::Value;
use text2num::LangInterpreter;
use text2num::Language;

use crate::domain::catalog::Catalog;
use crate::domain::catalog::Filter;
use crate::domain::catalog::Intent;
use crate::domain::catalog::Kind;
use crate::domain::catalog::Target;
use crate::domain::decide::MediaItem;
use crate::domain::decide::SearchResults;
use crate::domain::dialog::Key;
use crate::domain::intent::Span;
use crate::domain::responses::Pool;
use crate::domain::responses::Response;
use crate::domain::state::Area;
use crate::domain::state::AreaId;
use crate::domain::state::Context;
use crate::domain::state::Device;
use crate::domain::state::Entity;
use crate::domain::state::EntityId;
use crate::domain::state::Event;
use crate::domain::state::HomeConfig;
use crate::domain::state::ServiceCall;
use crate::domain::state::Snapshot;
use crate::domain::state::State;
use crate::domain::state::UnitSystem;
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

/// A date and a time of day, without a time zone. <https://docs.rs/jiff/0.2/jiff/civil/struct.DateTime.html>
#[verifier::external_type_specification]
#[verifier::external_body]
pub struct ExDateTime(DateTime);

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

/// An HA service call.
#[verifier::external_type_specification]
pub struct ExServiceCall(ServiceCall);

/// The HA configuration that Jarvis uses.
#[verifier::external_type_specification]
pub struct ExHomeConfig(HomeConfig);

/// The units of the HA configuration that Jarvis uses.
#[verifier::external_type_specification]
pub struct ExUnitSystem(UnitSystem);

/// The intent catalog of `intents.toml`.
#[verifier::external_type_specification]
pub struct ExCatalog(Catalog);

/// One action that Jarvis can take.
#[verifier::external_type_specification]
pub struct ExIntent(Intent);

/// The responses of one intent or one filler type.
#[verifier::external_type_specification]
pub struct ExPool(Pool);

/// One response template and its use count.
#[verifier::external_type_specification]
pub struct ExResponse(Response);

/// How Jarvis replies to an intent and acts on it.
#[verifier::external_type_specification]
pub struct ExKind(Kind);

/// The entities that the service of an intent gets.
#[verifier::external_type_specification]
pub struct ExTarget(Target);

/// An entity filter of `select`.
#[verifier::external_type_specification]
pub struct ExFilter(Filter);

/// One slot value in the text of an example.
#[verifier::external_type_specification]
pub struct ExSpan(Span);

/// The lists of a Music Assistant search response.
#[verifier::external_type_specification]
pub struct ExSearchResults(SearchResults);

/// One item of a Music Assistant search response.
#[verifier::external_type_specification]
pub struct ExMediaItem(MediaItem);

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

/// How a template handles undefined values. <https://docs.rs/minijinja/3/minijinja/enum.UndefinedBehavior.html>
#[verifier::external_type_specification]
pub struct ExUndefinedBehavior(UndefinedBehavior);

/// A value that converts to a template value through Serde. <https://docs.rs/minijinja/3/minijinja/value/struct.Serde.html>
#[verifier::external_type_specification]
#[verifier::reject_recursive_types(T)]
pub struct ExSerde<T>(Serde<T>);

/// A template value. <https://docs.rs/minijinja/3/minijinja/value/struct.Value.html>
#[verifier::external_type_specification]
#[verifier::external_body]
pub struct ExTemplateValue(minijinja::Value);

/// Sets how templates handle undefined values. <https://docs.rs/minijinja/3/minijinja/struct.Environment.html#method.set_undefined_behavior>
#[verifier::external_fn_specification]
pub fn ex_environment_set_undefined_behavior<'source>(environment: &mut Environment<'source>, behavior: UndefinedBehavior) {
    environment.set_undefined_behavior(behavior)
}

/// Turns debug mode on or off. <https://docs.rs/minijinja/3/minijinja/struct.Environment.html#method.set_debug>
#[verifier::external_fn_specification]
pub fn ex_environment_set_debug<'source>(environment: &mut Environment<'source>, enabled: bool) {
    environment.set_debug(enabled)
}

/// A compiled expression. <https://docs.rs/minijinja/3/minijinja/struct.Expression.html>
#[verifier::external_type_specification]
#[verifier::external_body]
pub struct ExExpression<'env, 'source>(Expression<'env, 'source>);

/// Compiles the expression `source`. <https://docs.rs/minijinja/3/minijinja/struct.Environment.html#method.compile_expression>
#[verifier::external_fn_specification]
pub fn ex_environment_compile_expression<'source, 'env>(environment: &'env Environment<'source>, source: &'source str) -> Result<Expression<'env, 'source>, Error> {
    environment.compile_expression(source)
}

/// Evaluates `expression` with `context` and gives its result. <https://docs.rs/minijinja/3/minijinja/struct.Expression.html#method.eval>
#[verifier::external_fn_specification]
pub fn ex_expression_eval<'env, 'source, Context: Into<minijinja::Value>>(expression: &Expression<'env, 'source>, context: Context) -> Result<minijinja::Value, Error> {
    expression.eval(context)
}

/// True when `value` is undefined. <https://docs.rs/minijinja/3/minijinja/value/struct.Value.html#method.is_undefined>
#[verifier::external_fn_specification]
pub fn ex_template_value_is_undefined(value: &minijinja::Value) -> bool {
    value.is_undefined()
}

/// A JSON error. <https://docs.rs/serde_json/1/serde_json/struct.Error.html>
#[verifier::external_type_specification]
#[verifier::external_body]
pub struct ExJsonError(serde_json::Error);

/// A type that serializes with Serde. <https://docs.rs/serde/1/serde/trait.Serialize.html>
#[verifier::external_trait_specification]
pub trait ExSerialize {
    /// The trait that this specification is for.
    type ExternalTraitSpecificationFor: Serialize;
}

/// Converts `value` to a JSON value. <https://docs.rs/serde_json/1/serde_json/fn.to_value.html>
#[verifier::external_fn_specification]
pub fn ex_to_value<T: Serialize>(value: T) -> Result<Value, serde_json::Error> {
    serde_json::to_value(value)
}

/// Compiles and renders `source` with `context`. <https://docs.rs/minijinja/3/minijinja/struct.Environment.html#method.render_str>
#[verifier::external_fn_specification]
pub fn ex_environment_render_str<'source, Context: Into<minijinja::Value>>(
    environment: &Environment<'source>,
    source: &str,
    context: Context,
) -> Result<String, Error> {
    environment.render_str(source, context)
}

/// The arguments of a template function. <https://docs.rs/minijinja/3/minijinja/value/trait.FunctionArgs.html>
#[verifier::external_trait_specification]
pub trait ExFunctionArgs<'a> {
    /// The trait that this specification is for.
    type ExternalTraitSpecificationFor: FunctionArgs<'a>;

    /// The type of the arguments.
    type Output;
}

/// The result of a template function. <https://docs.rs/minijinja/3/minijinja/value/trait.FunctionResult.html>
#[verifier::external_trait_specification]
pub trait ExFunctionResult {
    /// The trait that this specification is for.
    type ExternalTraitSpecificationFor: FunctionResult;
}

/// A function that a template can call. <https://docs.rs/minijinja/3/minijinja/functions/trait.Function.html>
#[verifier::external_trait_specification]
pub trait ExFunction<Rv, Args: for<'a> FunctionArgs<'a>>: Send + Sync + 'static {
    /// The trait that this specification is for.
    type ExternalTraitSpecificationFor: Function<Rv, Args>;
}

/// Adds the filter `f` with the name `name`. <https://docs.rs/minijinja/3/minijinja/struct.Environment.html#method.add_filter>
#[verifier::external_fn_specification]
pub fn ex_environment_add_filter<'source, N, F, Rv, Args>(environment: &mut Environment<'source>, name: N, f: F)
where
    N: Into<Cow<'source, str>>,
    F: Function<Rv, Args>,
    Rv: FunctionResult,
    Args: for<'a> FunctionArgs<'a>,
{
    environment.add_filter(name, f)
}

/// The kind of a template error. <https://docs.rs/minijinja/3/minijinja/enum.ErrorKind.html>
#[verifier::external_type_specification]
pub struct ExErrorKind(ErrorKind);

/// Creates a template error of the kind `kind` with the text `detail`. <https://docs.rs/minijinja/3/minijinja/struct.Error.html#method.new>
#[verifier::external_fn_specification]
pub fn ex_error_new<Detail: Into<Cow<'static, str>>>(kind: ErrorKind, detail: Detail) -> Error {
    Error::new(kind, detail)
}

/// A language of number words. <https://docs.rs/text2num/2/text2num/lang/enum.Language.html>
#[verifier::external_type_specification]
#[verifier::external_body]
pub struct ExLanguage(Language);

/// An error of number word conversion. <https://docs.rs/text2num/2/text2num/error/enum.Error.html>
#[verifier::external_type_specification]
#[verifier::external_body]
pub struct ExNumberError(text2num::error::Error);

/// A language that converts number words. <https://docs.rs/text2num/2/text2num/lang/trait.LangInterpreter.html>
#[verifier::external_trait_specification]
pub trait ExLangInterpreter {
    /// The trait that this specification is for.
    type ExternalTraitSpecificationFor: LangInterpreter;
}

/// Gives the English language. <https://docs.rs/text2num/2/text2num/lang/enum.Language.html#method.english>
#[verifier::external_fn_specification]
pub fn ex_language_english() -> Language {
    Language::english()
}

/// Converts the number words of `text` into digits, or fails when `text` is not a number. <https://docs.rs/text2num/2/text2num/word_to_digit/fn.text2digits.html>
#[verifier::external_fn_specification]
pub fn ex_text2digits<T: LangInterpreter>(text: &str, lang: &T) -> Result<String, text2num::error::Error> {
    text2num::text2digits(text, lang)
}

/// A type that parses from a string. <https://doc.rust-lang.org/std/str/trait.FromStr.html>
#[verifier::external_trait_specification]
pub trait ExFromStr: Sized {
    /// The trait that this specification is for.
    type ExternalTraitSpecificationFor: FromStr;

    /// The type of a parse error.
    type Err;
}

/// An error of integer parsing. <https://doc.rust-lang.org/std/num/struct.ParseIntError.html>
#[verifier::external_type_specification]
#[verifier::external_body]
pub struct ExParseIntError(ParseIntError);

/// Parses `text` into a value of the type `F`. <https://doc.rust-lang.org/std/primitive.str.html#method.parse>
#[verifier::external_fn_specification]
pub fn ex_str_parse<F: FromStr>(text: &str) -> Result<F, F::Err> {
    text.parse()
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

/// A `jiff` error. <https://docs.rs/jiff/0.2/jiff/struct.Error.html>
#[verifier::external_type_specification]
#[verifier::external_body]
pub struct ExJiffError(jiff::Error);

/// The instant that `text` gives, or `None` when it does not parse.
pub uninterp spec fn instant(text: Seq<char>) -> Option<Timestamp>;

/// Parses `text` into an instant. <https://docs.rs/jiff/0.2/jiff/struct.Timestamp.html#impl-FromStr-for-Timestamp>
#[verifier::external_fn_specification]
pub fn ex_timestamp_from_str(text: &str) -> (result: Result<Timestamp, jiff::Error>)
    ensures
        match result {
            Ok(parsed) => instant(text@) == Some(parsed),
            Err(_) => instant(text@) is None,
        },
{
    <Timestamp as FromStr>::from_str(text)
}

/// A signed duration in nanoseconds. <https://docs.rs/jiff/0.2/jiff/struct.SignedDuration.html>
#[verifier::external_type_specification]
#[verifier::external_body]
pub struct ExSignedDuration(SignedDuration);

/// The duration from `earlier` to `later`.
pub uninterp spec fn since(later: Timestamp, earlier: Timestamp) -> SignedDuration;

/// Gives the duration from `other` to `timestamp`. <https://docs.rs/jiff/0.2/jiff/struct.Timestamp.html#method.duration_since>
#[verifier::external_fn_specification]
pub fn ex_timestamp_duration_since(timestamp: Timestamp, other: Timestamp) -> (duration: SignedDuration)
    ensures
        duration == since(timestamp, other),
{
    timestamp.duration_since(other)
}

/// The seconds of `duration`, with the fraction.
pub uninterp spec fn seconds(duration: SignedDuration) -> f64;

/// Gives the seconds of `duration`, with the fraction, as an `f64`. <https://docs.rs/jiff/0.2/jiff/struct.SignedDuration.html#method.as_secs_f64>
#[verifier::external_fn_specification]
pub fn ex_signed_duration_as_secs_f64(duration: &SignedDuration) -> (result: f64)
    ensures
        result == seconds(*duration),
{
    duration.as_secs_f64()
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

/// `Key` derives `Ord`, so it compares by its `String`, which has a total order. <https://doc.rust-lang.org/std/cmp/trait.Ord.html#derivable>
pub broadcast axiom fn axiom_key_obeys_cmp()
    ensures
        #[trigger] vstd::laws_cmp::obeys_cmp::<Key>(),
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

/// Makes an empty JSON object. <https://docs.rs/serde_json/1/serde_json/struct.Map.html#method.new>
#[verifier::external_fn_specification]
pub fn ex_map_new() -> Map<String, Value> {
    Map::new()
}

/// Inserts `value` at `key` and gives the old value of `key`. <https://docs.rs/serde_json/1/serde_json/struct.Map.html#method.insert>
#[verifier::external_fn_specification]
pub fn ex_map_insert(map: &mut Map<String, Value>, key: String, value: Value) -> Option<Value>
    ensures
        attribute(*final(map), &key) == Some(value),
        forall|other: String| other != key ==> #[trigger] attribute(*final(map), &other) == attribute(*old(map), &other),
{
    map.insert(key, value)
}

/// Makes the JSON string `text`. <https://docs.rs/serde_json/1/serde_json/enum.Value.html#impl-From%3CString%3E-for-Value>
#[verifier::external_fn_specification]
pub fn ex_value_from_string(string: String) -> (value: Value)
    ensures
        text(value) == Some(string@),
{
    <Value as From<String>>::from(string)
}

/// Copies the JSON object `map`. <https://docs.rs/serde_json/1/serde_json/struct.Map.html#impl-Clone-for-Map%3CString,+Value%3E>
#[verifier::external_fn_specification]
pub fn ex_map_clone(map: &Map<String, Value>) -> Map<String, Value> {
    map.clone()
}

/// Makes the JSON object `map`. <https://docs.rs/serde_json/1/serde_json/enum.Value.html#impl-From%3CMap%3CString,+Value%3E%3E-for-Value>
#[verifier::external_fn_specification]
pub fn ex_value_from_map(map: Map<String, Value>) -> Value {
    <Value as From<Map<String, Value>>>::from(map)
}

/// Makes the JSON array `items`. <https://docs.rs/serde_json/1/serde_json/enum.Value.html#impl-From%3CVec%3CT%3E%3E-for-Value>
#[verifier::external_fn_specification]
pub fn ex_value_from_vec<Item: Into<Value>>(items: Vec<Item>) -> Value {
    <Value as From<Vec<Item>>>::from(items)
}

/// The value of the JSON number `value` as an `f64`, or `None` for another JSON type.
pub uninterp spec fn number(value: Value) -> Option<f64>;

/// Gives the value of a JSON number as an `f64`. <https://docs.rs/serde_json/1/serde_json/enum.Value.html#method.as_f64>
#[verifier::external_fn_specification]
pub fn ex_value_as_f64(value: &Value) -> (result: Option<f64>)
    ensures
        result == number(*value),
{
    value.as_f64()
}

/// Makes the JSON number `number`, or `null` when it is infinite or NaN. <https://docs.rs/serde_json/1/serde_json/enum.Value.html#impl-From%3Cf64%3E-for-Value>
#[verifier::external_fn_specification]
pub fn ex_value_from_f64(number: f64) -> Value {
    <Value as From<f64>>::from(number)
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

/// The character that the pattern `delimiter` matches, or `None` for another pattern.
pub uninterp spec fn pattern_char<Delimiter>(delimiter: Delimiter) -> Option<char>;

/// A `char` pattern matches that character. <https://doc.rust-lang.org/std/str/pattern/trait.Pattern.html>
pub broadcast axiom fn axiom_char_pattern(c: char)
    ensures
        #[trigger] pattern_char(c) == Some(c),
;

/// The text that the pattern `delimiter` matches, or `None` for another pattern.
pub uninterp spec fn pattern_text<Delimiter>(delimiter: Delimiter) -> Option<Seq<char>>;

/// A `&str` pattern matches that text. <https://doc.rust-lang.org/std/str/pattern/trait.Pattern.html>
pub broadcast axiom fn axiom_str_pattern(text: &str)
    ensures
        #[trigger] pattern_text(text) == Some(text@),
;

/// Splits `text` at the first match of `delimiter` and gives the text before and after it, or
/// `None` when `text` has no match. <https://doc.rust-lang.org/std/primitive.str.html#method.split_once>
#[verifier::external_fn_specification]
pub fn ex_str_split_once<Delimiter: Pattern>(text: &str, delimiter: Delimiter) -> (parts: Option<(&str, &str)>)
    ensures
        pattern_char(delimiter) matches Some(c) ==> match parts {
            Some((before, after)) => text@ == before@ + seq![c] + after@ && !before@.contains(c),
            None => !text@.contains(c),
        },
        pattern_text(delimiter) matches Some(found) ==> (parts matches Some((before, after)) ==> text@ == before@ + found + after@),
{
    text.split_once(delimiter)
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

/// `c` with `A` to `Z` mapped to `a` to `z`. <https://doc.rust-lang.org/std/primitive.char.html#method.to_ascii_lowercase>
pub open spec fn ascii_lowercase(c: char) -> char {
    if 'A' <= c && c <= 'Z' { ((c as u32 + 32) as u32) as char } else { c }
}

/// Maps `A` to `Z` to `a` to `z` and gives other characters unchanged. <https://doc.rust-lang.org/std/primitive.char.html#method.to_ascii_lowercase>
#[verifier::external_fn_specification]
pub fn ex_char_to_ascii_lowercase(c: &char) -> (lower: char)
    ensures
        lower == ascii_lowercase(*c),
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

/// The IEEE 754 totalOrder of two `f64` floats.
pub uninterp spec fn total_order_f64() -> spec_fn(f64, f64) -> Ordering;

/// Gives the IEEE 754 totalOrder of `left` and `right`. <https://doc.rust-lang.org/std/primitive.f64.html#method.total_cmp>
#[verifier::external_fn_specification]
pub fn ex_f64_total_cmp(left: &f64, right: &f64) -> (ordering: Ordering)
    ensures
        ordering == total_order_f64()(*left, *right),
{
    left.total_cmp(right)
}

/// IEEE 754 totalOrder is a total order. <https://doc.rust-lang.org/std/primitive.f64.html#method.total_cmp>
pub axiom fn axiom_total_order_f64_is_total()
    ensures
        total(total_order_f64()),
;

/// The Jaro-Winkler similarity of `a` and `b`.
pub uninterp spec fn jaro_winkler(a: Seq<char>, b: Seq<char>) -> f64;

/// Gives the Jaro-Winkler similarity of `a` and `b`. <https://docs.rs/strsim/0.11/strsim/fn.jaro_winkler.html>
#[verifier::external_fn_specification]
pub fn ex_jaro_winkler(a: &str, b: &str) -> (similarity: f64)
    ensures
        similarity == jaro_winkler(a@, b@),
{
    strsim::jaro_winkler(a, b)
}

/// Gives `text` without the prefix `prefix`, or `None` when `text` does not start with it. <https://doc.rust-lang.org/std/primitive.str.html#method.strip_prefix>
#[verifier::external_fn_specification]
pub fn ex_str_strip_prefix<'text, Prefix: Pattern>(text: &'text str, prefix: Prefix) -> (rest: Option<&'text str>)
    ensures
        pattern_text(prefix) matches Some(found) ==> match rest {
            Some(rest) => text@ == found + rest@,
            None => !found.is_prefix_of(text@),
        },
{
    text.strip_prefix(prefix)
}

/// True when `part` is a sequence of consecutive characters of `text`.
pub open spec fn infix(part: Seq<char>, text: Seq<char>) -> bool {
    exists|start: int| 0 <= start <= text.len() - part.len()
        && #[trigger] text.subrange(start, start + part.len()) == part
}

/// True when `pattern` matches a part of `text`. <https://doc.rust-lang.org/std/primitive.str.html#method.contains>
#[verifier::external_fn_specification]
pub fn ex_str_contains<Part: Pattern>(text: &str, pattern: Part) -> (found: bool)
    ensures
        pattern_text(pattern) matches Some(part) ==> found == infix(part, text@),
{
    text.contains(pattern)
}

/// The length of `duration` in nanoseconds.
pub uninterp spec fn nanoseconds(duration: Duration) -> nat;

/// Makes a duration of `secs` whole seconds. <https://doc.rust-lang.org/std/time/struct.Duration.html#method.from_secs>
#[verifier::external_fn_specification]
pub fn ex_duration_from_secs(secs: u64) -> (duration: Duration)
    ensures
        nanoseconds(duration) == secs * 1_000_000_000,
{
    Duration::from_secs(secs)
}

/// Divides `duration` by `rhs`, or gives `None` when `rhs` is 0. <https://doc.rust-lang.org/std/time/struct.Duration.html#method.checked_div>
#[verifier::external_fn_specification]
pub fn ex_duration_checked_div(duration: Duration, rhs: u32) -> (quotient: Option<Duration>)
    ensures
        match quotient {
            Some(quotient) => rhs != 0 && nanoseconds(quotient) == nanoseconds(duration) / rhs as nat,
            None => rhs == 0,
        },
{
    duration.checked_div(rhs)
}

} // verus!
