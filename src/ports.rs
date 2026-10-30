//! Port traits and their data types.

use async_trait::async_trait;
use futures::stream::BoxStream;
use serde_json::Value;

use crate::domain::state::Event;
use crate::domain::state::ServiceCall;
use crate::domain::state::Snapshot;

/// The house: HA states, registries, events and service calls.
#[async_trait]
pub trait Home: Send + Sync {
    /// Loads the states, the registries, the configuration and the user of the token.
    async fn snapshot(&self) -> anyhow::Result<Snapshot>;

    /// The changes in HA, in order.
    fn events(&self) -> BoxStream<'static, Event>;

    /// Calls a service. Gives the response data when `call.return_response` is true.
    async fn call(&self, call: ServiceCall) -> anyhow::Result<Option<Value>>;
}
