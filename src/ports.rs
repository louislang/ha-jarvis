//! Port traits and their data types.

use std::path::Path;
use std::sync::Arc;

use async_trait::async_trait;
use futures::stream::BoxStream;
use jiff::Timestamp;
use serde_json::Value;

use crate::Device;
use crate::domain::model::Prediction;
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

    /// Speaks `text` on the satellite entity `satellite` with no preannounce sound. Returns
    /// after the satellite plays the audio.
    async fn announce(&self, satellite: &str, text: &str) -> anyhow::Result<()>;
}

/// The System One model.
#[async_trait]
pub trait Classifier: Send + Sync {
    /// The label probabilities and the slots of the normalized transcript `text` in `context`.
    async fn predict(&self, context: &str, text: &str) -> anyhow::Result<Prediction>;
}

/// Opens System One model folders.
#[async_trait]
pub trait ModelLoader: Send + Sync {
    /// The classifier of the model folder `dir`, run on `device`.
    async fn load(&self, dir: &Path, device: Device) -> anyhow::Result<Arc<dyn Classifier>>;
}

/// A value that survives a restart.
pub trait Store<T>: Send + Sync {
    /// The saved value, or `None` when no value is saved.
    ///
    /// # Errors
    ///
    /// The value cannot be read or parsed.
    fn load(&self) -> anyhow::Result<Option<T>>;

    /// Saves `value`. A stop during the save keeps the last saved value.
    ///
    /// # Errors
    ///
    /// The value cannot be serialized or written.
    fn save(&self, value: &T) -> anyhow::Result<()>;
}

/// The current time.
pub trait Clock: Send + Sync {
    /// The time now.
    fn now(&self) -> Timestamp;
}
