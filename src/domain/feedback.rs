//! Feedback: the logged interactions and their marks.

use jiff::Timestamp;
use serde::Deserialize;
use serde::Serialize;
use vstd::prelude::*;

use crate::domain::state::ServiceCall;

/// A rating.
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
pub enum Value {
    /// The rated part was right.
    Good,
    /// The rated part was wrong.
    Bad,
}

/// A rating of the action of one interaction.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct Mark {
    /// The ID of the interaction.
    pub interaction: u64,
    /// The rating. `None` removes the earlier mark of the interaction.
    pub value: Option<Value>,
    /// The intent that Jarvis should have selected, or `unknown` for System Two.
    pub correction: Option<String>,
    /// The time of the mark.
    pub time: Timestamp,
}

/// The intent of an action and the sorted entity IDs of its targets.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct Action {
    /// The intent name.
    pub intent: String,
    /// The target entity IDs, sorted.
    pub targets: Vec<String>,
}

/// One response of an interaction.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct Spoken {
    /// The spoken text.
    pub text: String,
    /// The pool name.
    pub pool: String,
    /// The response template.
    pub template: String,
}

/// A slot value with its byte offsets in the normalized transcript.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct Tagged {
    /// The slot name.
    pub name: String,
    /// The text of the value.
    pub value: String,
    /// The byte offset of the value.
    pub start: usize,
    /// The byte offset after the value.
    pub end: usize,
}

/// The kind of a decision.
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Serialize)]
pub enum Route {
    /// A reply, with or without a service call.
    Reply,
    /// `Defer` with `Job::Fetch`.
    Fetch,
    /// `Defer` with `Job::Llm`: System Two.
    Llm,
    /// A question.
    Ask,
}

/// One logged request.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct Interaction {
    /// The ID.
    pub id: u64,
    /// The time of the request.
    pub time: Timestamp,
    /// The satellite entity, or `None` for a typed request.
    pub satellite: Option<String>,
    /// The transcript.
    pub transcript: String,
    /// Segment A of the model input.
    pub model_context: String,
    /// The run name of the classifier.
    pub model: String,
    /// The three top labels with their probabilities, highest first.
    pub top_labels: Vec<(String, f32)>,
    /// The slot values.
    pub slots: Vec<Tagged>,
    /// The intent of the decision and its targets.
    pub action: Action,
    /// The kind of the decision.
    pub decision: Route,
    /// The service calls.
    pub service_calls: Vec<ServiceCall>,
    /// The responses, in order.
    pub responses: Vec<Spoken>,
    /// The time from the voice end of the utterance to the end of `handle`, or `None` without
    /// audio.
    pub latency_ms: Option<u64>,
    /// The ID of the first interaction of the dialog, for a dialog answer.
    pub link: Option<u64>,
    /// The marks, at most one.
    pub marks: Vec<Mark>,
}

/// A request that System Two answered.
#[verus_verify]
#[derive(Clone, Debug, PartialEq)]
pub struct Deferred {
    /// The ID of the interaction.
    pub id: u64,
    /// The transcript.
    pub text: String,
    /// The time of the request.
    pub time: Timestamp,
    /// True when the request came from a satellite.
    pub spoken: bool,
}
