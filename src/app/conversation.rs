//! Dispatch of decisions and the pools task.

use std::sync::Arc;
use std::sync::mpsc;

use arc_swap::ArcSwap;
use futures::FutureExt;
use futures::future::BoxFuture;
use tokio::task::JoinHandle;

use crate::domain::decide::Chosen;
use crate::domain::decide::Decision;
use crate::domain::responses;
use crate::domain::responses::Pools;
use crate::ports::Home;
use crate::ports::Store;

/// Background work that the caller starts after it sends the reply.
pub type Work = BoxFuture<'static, anyhow::Result<()>>;

/// One change to the pools, applied by the pools task.
pub type Change = Box<dyn FnOnce(Pools) -> Pools + Send>;

/// The reply to a decision and the work after it.
#[must_use]
pub struct Handled {
    /// The text to speak.
    pub text: String,
    /// The background work, if any.
    pub after: Option<Work>,
}

/// Dispatches decisions to the home and the pools task.
pub struct Conversation {
    /// The house.
    home: Arc<dyn Home>,
    /// The sender of the changes to the pools task.
    changes: mpsc::Sender<Change>,
}

impl Conversation {
    /// Dispatches to `home` and to the pools task of `changes`.
    #[must_use]
    pub fn new(home: Arc<dyn Home>, changes: mpsc::Sender<Change>) -> Self {
        Self { home, changes }
    }

    /// The reply of `decision` and its service call as background work. Records the use of the
    /// reply.
    pub fn dispatch(&self, decision: Decision) -> Handled {
        let (chosen, call) = match decision {
            Decision::Reply { reply, call } => (reply, call),
            Decision::Defer { filler, .. } => (filler, None),
            Decision::Fallback { reply } => (reply, None),
        };
        let Chosen { text, pool, index } = chosen;
        // The task holds the receiver until every sender drops, so the send fails only after a panic.
        let _ = self.changes.send(Box::new(move |pools| {
            responses::record_use(pools, &pool, index)
        }));
        let after = call.map(|call| {
            let home = Arc::clone(&self.home);
            async move {
                home.call(call).await?;
                Ok(())
            }
            .boxed()
        });
        Handled { text, after }
    }
}

/// The pools task and the pools that it publishes.
pub struct PoolsTask {
    /// The current pools.
    pub pools: Arc<ArcSwap<Pools>>,
    /// The sender of the changes.
    pub changes: mpsc::Sender<Change>,
    /// The task. It ends when every sender drops.
    pub task: JoinHandle<()>,
}

/// Starts the pools task with the pools of `source` merged into the pools of `store`. The task
/// applies each change in order, publishes the result and saves it to `store`.
///
/// # Errors
///
/// A store cannot load, or `source` has no pools.
pub fn start_pools(
    source: &dyn Store<Pools>,
    store: Arc<dyn Store<Pools>>,
) -> anyhow::Result<PoolsTask> {
    let source = source
        .load()?
        .ok_or_else(|| anyhow::anyhow!("the source has no pools"))?;
    let saved = store.load()?.unwrap_or_default();
    let pools = Arc::new(ArcSwap::from_pointee(merge(source, &saved)));
    let (changes, receiver) = mpsc::channel::<Change>();
    let published = Arc::clone(&pools);
    // The saves block on file writes, so the task runs on a blocking thread.
    let task = tokio::task::spawn_blocking(move || {
        for change in receiver {
            let next = change(Pools::clone(&published.load()));
            published.store(Arc::new(next));
            // A failed save keeps the last saved file. The next change saves again.
            let _ = store.save(&published.load());
        }
    });
    Ok(PoolsTask {
        pools,
        changes,
        task,
    })
}

/// The pools of `source` with the use count of each response that `saved` has too.
fn merge(mut source: Pools, saved: &Pools) -> Pools {
    for (name, pool) in &mut source {
        let old = saved
            .get(name)
            .map_or(&[][..], |pool| pool.responses.as_slice());
        for response in &mut pool.responses {
            let same = old.iter().find(|other| other.text == response.text);
            response.uses = same.map_or(0, |other| other.uses);
        }
    }
    source
}

/// Tests of dispatch and the pools task.
#[cfg(test)]
mod tests {
    use std::thread;
    use std::time::Duration;

    use tokio::time::Instant;

    use super::*;
    use crate::domain::responses::Pool;
    use crate::domain::responses::Response;
    use crate::domain::state::ServiceCall;
    use crate::testing::fakes::FakeHome;
    use crate::testing::fakes::FakeStore;

    /// The pool `lights_on` with the responses and use counts `lights`, and the pool `fallback`
    /// with one response used `fallback` times.
    fn pools(lights: &[(&str, u64)], fallback: u64) -> Pools {
        let pool = |texts: &[(&str, u64)]| {
            let responses = texts.iter().map(|&(text, uses)| Response {
                text: text.to_owned(),
                uses,
            });
            Pool {
                responses: responses.collect(),
                last: None,
            }
        };
        let (lights, fallback) = (pool(lights), pool(&[("Sorry.", fallback)]));
        Pools::from([
            ("lights_on".to_owned(), lights),
            ("fallback".to_owned(), fallback),
        ])
    }

    /// The pools of the tests, with no uses.
    fn unused() -> Pools {
        pools(&[("Lights on.", 0), ("Done.", 0)], 0)
    }

    /// The response at `index` of the pool `name`.
    fn chosen(name: &str, index: usize) -> Chosen {
        let text = unused()[name].responses[index].text.clone();
        let pool = name.to_owned();
        Chosen { text, pool, index }
    }

    /// A call that turns on the kitchen light.
    fn call() -> ServiceCall {
        ServiceCall {
            domain: "light".to_owned(),
            service: "turn_on".to_owned(),
            target: vec!["light.kitchen".to_owned()],
            data: serde_json::Map::new(),
            return_response: false,
        }
    }

    /// A `lights_on` reply with `call`.
    fn reply(call: Option<ServiceCall>) -> Decision {
        let reply = chosen("lights_on", 1);
        Decision::Reply { reply, call }
    }

    /// The pools task of `unused()`, its store and a conversation with `home`.
    type Started = (PoolsTask, Arc<FakeStore<Pools>>, Conversation);

    /// Starts the pools task of `unused()` and a conversation with `home`.
    fn start(home: &Arc<FakeHome>) -> anyhow::Result<Started> {
        let store = Arc::new(FakeStore::new(None));
        let task = start_pools(&FakeStore::new(Some(unused())), Arc::clone(&store) as _)?;
        let conversation = Conversation::new(Arc::clone(home) as _, task.changes.clone());
        Ok((task, store, conversation))
    }

    /// The last save of `store` after `conversation` drops and `task` ends.
    async fn last_save(started: Started) -> anyhow::Result<Pools> {
        let (task, store, conversation) = started;
        drop((conversation, task.changes));
        task.task.await?;
        Ok(store.saves().pop().expect("the task saves each change"))
    }

    /// AC-4.8-1.
    #[tokio::test(start_paused = true)]
    async fn ac_4_8_1_dispatch_returns_before_time_advances() -> anyhow::Result<()> {
        let home = Arc::new(FakeHome::default());
        let gate = home.delay();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(500)).await;
            gate.send(())
        });
        let (_task, _store, conversation) = start(&home)?;
        let start = Instant::now();
        let handled = conversation.dispatch(reply(Some(call())));
        assert_eq!(Instant::now(), start, "dispatch waited for the call");
        assert!(handled.after.is_some(), "the call is not in after");
        Ok(())
    }

    /// AC-4.8-2.
    #[tokio::test(start_paused = true)]
    async fn ac_4_8_2_call_only_after_start() -> anyhow::Result<()> {
        let home = Arc::new(FakeHome::default());
        let (_task, _store, conversation) = start(&home)?;
        let after = conversation.dispatch(reply(Some(call()))).after;
        assert_eq!(home.calls(), [], "a call came before the start");
        after.ok_or_else(|| anyhow::anyhow!("no after"))?.await?;
        assert_eq!(home.calls(), [call()]);
        Ok(())
    }

    /// AC-4.8-3.
    #[tokio::test(start_paused = true)]
    async fn ac_4_8_3_store_receives_new_use_count() -> anyhow::Result<()> {
        let started = start(&Arc::new(FakeHome::default()))?;
        let _ = started.2.dispatch(reply(Some(call())));
        let expected = responses::record_use(unused(), "lights_on", 1);
        assert_eq!(last_save(started).await?, expected);
        Ok(())
    }

    /// AC-4.8-4.
    #[tokio::test(start_paused = true)]
    async fn ac_4_8_4_reply_without_call_makes_no_call() -> anyhow::Result<()> {
        let home = Arc::new(FakeHome::default());
        let (_task, _store, conversation) = start(&home)?;
        let handled = conversation.dispatch(reply(None));
        assert!(handled.after.is_none(), "a reply with no call has work");
        assert_eq!(home.calls(), []);
        Ok(())
    }

    /// AC-4.8-5.
    #[tokio::test(start_paused = true)]
    async fn ac_4_8_5_fallback_has_text_and_no_after() -> anyhow::Result<()> {
        let (_task, _store, conversation) = start(&Arc::new(FakeHome::default()))?;
        let reply = chosen("fallback", 0);
        let handled = conversation.dispatch(Decision::Fallback { reply });
        assert_eq!(handled.text, "Sorry.");
        assert!(handled.after.is_none(), "a fallback has work");
        Ok(())
    }

    /// AC-4.8-6.
    #[tokio::test(start_paused = true)]
    async fn ac_4_8_6_concurrent_uses_are_all_saved() -> anyhow::Result<()> {
        let started = start(&Arc::new(FakeHome::default()))?;
        thread::scope(|scope| {
            for _ in 0..100 {
                scope.spawn(|| started.2.dispatch(reply(None)));
            }
        });
        let saved = last_save(started).await?;
        assert_eq!(saved["lights_on"].responses[1].uses, 100);
        Ok(())
    }

    /// AC-4.8-7.
    #[tokio::test(start_paused = true)]
    async fn ac_4_8_7_merge_keeps_counts_of_source_responses() -> anyhow::Result<()> {
        let source = FakeStore::new(Some(pools(&[("Lights on.", 0), ("New.", 0)], 0)));
        let saved = pools(&[("Lights on.", 3), ("Done.", 2)], 5);
        let task = start_pools(&source, Arc::new(FakeStore::new(Some(saved))))?;
        let expected = pools(&[("Lights on.", 3), ("New.", 0)], 5);
        assert_eq!(**task.pools.load(), expected);
        assert_eq!(source.saves(), [], "responses.toml changed");
        Ok(())
    }
}
