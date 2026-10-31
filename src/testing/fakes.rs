//! A fake for each port.

use std::collections::VecDeque;
use std::path::Path;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::MutexGuard;
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::Ordering;

use async_trait::async_trait;
use futures::StreamExt;
use futures::channel::mpsc;
use futures::channel::oneshot;
use futures::stream::BoxStream;
use serde_json::Value;

use crate::Device;
use crate::domain::model::Prediction;
use crate::domain::state::Event;
use crate::domain::state::ServiceCall;
use crate::domain::state::Snapshot;
use crate::ports::Classifier;
use crate::ports::Home;
use crate::ports::ModelLoader;

/// The reference house snapshot.
#[must_use]
pub(crate) fn house() -> Snapshot {
    serde_json::from_str(include_str!("house.json")).expect("house.json is a snapshot")
}

/// The contents of `mutex`.
fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().expect("no test panics with the lock")
}

/// A `Home` with a settable snapshot, scripted events, a count of snapshot calls and delayed calls.
pub(crate) struct FakeHome {
    /// The snapshot that `snapshot` returns.
    snapshot: Mutex<Snapshot>,
    /// The number of `snapshot` calls.
    snapshots: AtomicUsize,
    /// The sender of the scripted events.
    sender: mpsc::UnboundedSender<Event>,
    /// The receiver of the scripted events, until `events` takes it.
    receiver: Mutex<Option<mpsc::UnboundedReceiver<Event>>>,
    /// The gates of the next delayed calls, in order.
    gates: Mutex<VecDeque<oneshot::Receiver<()>>>,
}

impl FakeHome {
    /// A house that gives `snapshot`.
    #[must_use]
    pub(crate) fn new(snapshot: Snapshot) -> Self {
        let (sender, receiver) = mpsc::unbounded();
        Self {
            snapshot: Mutex::new(snapshot),
            snapshots: AtomicUsize::new(0),
            sender,
            receiver: Mutex::new(Some(receiver)),
            gates: Mutex::default(),
        }
    }

    /// Makes the later `snapshot` calls give `snapshot`.
    pub(crate) fn set_snapshot(&self, snapshot: Snapshot) {
        *lock(&self.snapshot) = snapshot;
    }

    /// Sends `event` on the event stream.
    pub(crate) fn send(&self, event: Event) {
        self.sender
            .unbounded_send(event)
            .expect("the event stream is open");
    }

    /// Makes the next call wait until the returned sender sends. The call fails when it drops.
    pub(crate) fn delay(&self) -> oneshot::Sender<()> {
        let (sender, receiver) = oneshot::channel();
        lock(&self.gates).push_back(receiver);
        sender
    }

    /// The number of `snapshot` calls.
    #[must_use]
    pub(crate) fn snapshots(&self) -> usize {
        self.snapshots.load(Ordering::SeqCst)
    }

    /// Waits for the gate of this call, if `delay` gave one.
    async fn wait(&self) -> anyhow::Result<()> {
        let gate = lock(&self.gates).pop_front();
        if let Some(gate) = gate {
            gate.await?;
        }
        Ok(())
    }
}

impl Default for FakeHome {
    /// The reference house.
    fn default() -> Self {
        Self::new(house())
    }
}

#[async_trait]
impl Home for FakeHome {
    async fn snapshot(&self) -> anyhow::Result<Snapshot> {
        self.snapshots.fetch_add(1, Ordering::SeqCst);
        let snapshot = lock(&self.snapshot).clone();
        self.wait().await?;
        Ok(snapshot)
    }

    fn events(&self) -> BoxStream<'static, Event> {
        let receiver = lock(&self.receiver).take();
        receiver.expect("events is called once").boxed()
    }

    async fn call(&self, _call: ServiceCall) -> anyhow::Result<Option<Value>> {
        self.wait().await?;
        Ok(None)
    }
}

/// A `Classifier` that gives scripted predictions in order and records each `(context, text)`.
#[derive(Default)]
pub struct FakeClassifier {
    /// The predictions of the next calls, in order.
    predictions: Mutex<VecDeque<Prediction>>,
    /// The `(context, text)` of each call, in order.
    calls: Mutex<Vec<(String, String)>>,
}

impl FakeClassifier {
    /// Makes the next call without an earlier scripted prediction give `prediction`.
    pub fn script(&self, prediction: Prediction) {
        lock(&self.predictions).push_back(prediction);
    }

    /// The `(context, text)` of each call, in order.
    #[must_use]
    pub fn calls(&self) -> Vec<(String, String)> {
        lock(&self.calls).clone()
    }
}

#[async_trait]
impl Classifier for FakeClassifier {
    async fn predict(&self, context: &str, text: &str) -> anyhow::Result<Prediction> {
        lock(&self.calls).push((context.to_owned(), text.to_owned()));
        let prediction = lock(&self.predictions).pop_front();
        prediction.ok_or_else(|| anyhow::anyhow!("no scripted prediction for {text}"))
    }
}

/// A `ModelLoader` that gives scripted classifiers in order.
#[derive(Default)]
pub struct FakeModelLoader {
    /// The classifiers of the next loads, in order.
    classifiers: Mutex<VecDeque<Arc<dyn Classifier>>>,
}

impl FakeModelLoader {
    /// Makes the next load without an earlier scripted classifier give `classifier`.
    pub fn script(&self, classifier: Arc<dyn Classifier>) {
        lock(&self.classifiers).push_back(classifier);
    }
}

#[async_trait]
impl ModelLoader for FakeModelLoader {
    async fn load(&self, dir: &Path, _device: Device) -> anyhow::Result<Arc<dyn Classifier>> {
        let classifier = lock(&self.classifiers).pop_front();
        classifier.ok_or_else(|| anyhow::anyhow!("no scripted classifier for {}", dir.display()))
    }
}
