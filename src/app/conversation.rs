//! The `handle` use case, dispatch of decisions, the announce queues and the pools task.

use std::collections::BTreeMap;
use std::mem;
use std::ops::ControlFlow;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::MutexGuard;
use std::sync::mpsc;
use std::time::Duration;

use arc_swap::ArcSwap;
use futures::FutureExt;
use futures::channel::oneshot;
use futures::future::BoxFuture;
use jiff::Timestamp;
use rand::rngs::StdRng;
use serde_json::Map;
use serde_json::Value;
use tokio::sync::watch;
use tokio::task::JoinHandle;

use crate::DialogConfig;
use crate::app::background::Backfill;
use crate::app::background::Habits;
use crate::app::plugins::Plugins;
use crate::domain::catalog::Catalog;
use crate::domain::decide;
use crate::domain::decide::AskCall;
use crate::domain::decide::Chosen;
use crate::domain::decide::Decision;
use crate::domain::decide::Job;
use crate::domain::decide::MediaItem;
use crate::domain::decide::PluginCall;
use crate::domain::decide::Question;
use crate::domain::decide::Request;
use crate::domain::decide::SearchResults;
use crate::domain::dialog;
use crate::domain::dialog::Dialog;
use crate::domain::dialog::Dialogs;
use crate::domain::dialog::Key;
use crate::domain::dialog::Pending;
use crate::domain::intent;
use crate::domain::model::Prediction;
use crate::domain::model::Slot;
use crate::domain::responses;
use crate::domain::responses::Pools;
use crate::domain::responses::Response;
use crate::domain::state::AreaId;
use crate::domain::state::ServiceCall;
use crate::domain::state_cache::StateCache;
use crate::domain::training;
use crate::ports::Classifier;
use crate::ports::Clock;
use crate::ports::Context;
use crate::ports::Handled;
use crate::ports::Home;
use crate::ports::Reasoner;
use crate::ports::Step;
use crate::ports::Store;
use crate::ports::Work;

/// One change to the pools, applied by the pools task.
pub type Change = Box<dyn FnOnce(Pools) -> Pools + Send>;

/// System Two, the time of its questions and the longest wait for each step of an answer.
#[derive(Clone)]
pub struct SystemTwo {
    /// The remote model.
    pub reasoner: Arc<dyn Reasoner>,
    /// The plugins that the remote model can call.
    pub plugins: Arc<Plugins>,
    /// The current time.
    pub clock: Arc<dyn Clock>,
    /// The longest wait for each step of an answer.
    pub timeout: Duration,
    /// The domains of the services and the targets that a call can have.
    pub allowed_domains: Vec<String>,
    /// The services that a call runs with `return_response`.
    pub read_services: Vec<String>,
}

/// Decides requests with System One and dispatches the decisions to the home, System Two, the
/// announce queues and the pools task.
#[derive(Clone)]
pub struct Conversation {
    /// The current pools.
    pools: Arc<ArcSwap<Pools>>,
    /// The use records and the backfills of the pools.
    backfill: Arc<Backfill>,
    /// The announce queues, their house and the hot cache.
    queues: Arc<Queues>,
    /// The random source of the response selection.
    rng: Arc<Mutex<StdRng>>,
    /// The System One model.
    classifier: Arc<dyn Classifier>,
    /// The intent catalog.
    catalog: Arc<Catalog>,
    /// The least top probability that System One acts on.
    threshold: f32,
    /// System Two.
    two: SystemTwo,
    /// The request habit model, in which each new spoken request counts.
    habits: Arc<Habits>,
}

impl Conversation {
    /// Decides with `classifier`, `catalog` and `threshold`, and asks `two`. Dispatches to
    /// `queues`, their house and `backfill`, which records the uses of `pools`. Selects the
    /// responses with a random source seeded from the time of `two`. Records the requests in
    /// `habits`.
    #[must_use]
    #[expect(
        clippy::too_many_arguments,
        reason = "the use case connects each part of a request"
    )]
    pub fn new(
        pools: Arc<ArcSwap<Pools>>,
        backfill: Arc<Backfill>,
        queues: Arc<Queues>,
        classifier: Arc<dyn Classifier>,
        catalog: Arc<Catalog>,
        threshold: f32,
        two: SystemTwo,
        habits: Arc<Habits>,
    ) -> Self {
        use rand::SeedableRng;
        let seed = two.clock.now().as_millisecond().unsigned_abs();
        Self {
            pools,
            backfill,
            queues,
            rng: Arc::new(Mutex::new(StdRng::seed_from_u64(seed))),
            classifier,
            catalog,
            threshold,
            two,
            habits,
        }
    }

    /// The reply to the transcript `text` in `context` and its work. An answer to the open dialog
    /// of the satellite, else of the conversation, can cancel it, fill it or ask again. Else the
    /// System One prediction of the normalized text in the area name of the satellite device, its
    /// decision and its dispatch, or the answer of a deferred job when `context` has no
    /// satellite. A question opens a dialog. A new request from a satellite counts in the habits.
    ///
    /// # Errors
    ///
    /// The prediction fails. The error text is a `failure` answer.
    pub async fn handle(&self, text: &str, context: Context) -> anyhow::Result<Handled> {
        let (area, name) = self.area(context.device_id.as_deref());
        let now = self.two.clock.now();
        let key = context.satellite_id.clone().or(context.conversation_id);
        let satellite = context.satellite_id.as_deref();
        let open = self.take_dialog(key.clone(), now);
        let (prediction, transcript, new) = match self
            .route(text, open, name.as_deref(), satellite, now)
            .await?
        {
            ControlFlow::Continue(routed) => routed,
            ControlFlow::Break(handled) => return Ok(handled),
        };
        let request = Request {
            transcript,
            area,
            now,
        };
        Ok(self
            .act(&prediction, &request, key.map(Key), satellite, new)
            .await)
    }

    /// The open dialog of `key` at `now` and its key. The dialog map no longer has it.
    fn take_dialog(&self, key: Option<String>, now: Timestamp) -> Option<(Dialog, Key)> {
        let key = Key(key?);
        let timeout = self.queues.dialog.timeout_s;
        let dialog = self.queues.dialogs().take(&key, now, timeout)?;
        Some((dialog, key))
    }

    /// The dispatch of the decision for `prediction` of `request` on `satellite`. A question opens
    /// a dialog for `key`. A `new` request from `satellite` counts in the habits. Logs the request.
    async fn act(
        &self,
        prediction: &Prediction,
        request: &Request,
        key: Option<Key>,
        satellite: Option<&str>,
        new: bool,
    ) -> Handled {
        let decision = self.decide(prediction, request);
        // Only a spoken request counts. Text from the HA UI has no satellite.
        self.record(
            prediction,
            &decision,
            request.now,
            new && satellite.is_some(),
        );
        let (kind, area) = (kind(&decision), request.area.as_ref());
        let pending = match &decision {
            Decision::Ask { pending, .. } => Some(pending.clone()),
            _ => None,
        };
        let handled = self.reply(decision, area, satellite, key.clone()).await;
        if let (Some(key), Some(pending)) = (key, pending) {
            self.queues.open(key, pending, request.now);
        }
        let (top, reply) = (prediction.ranked.first(), handled.text.as_str());
        let transcript = request.transcript.as_str();
        tracing::info!(transcript, ?top, kind, reply, "handled");
        handled
    }

    /// Records the top intent of `prediction` at `now` in the habits when the request is `spoken`
    /// and `decision` does not defer to System Two.
    fn record(&self, prediction: &Prediction, decision: &Decision, now: Timestamp, spoken: bool) {
        let top = prediction
            .ranked
            .first()
            .filter(|_| spoken && !is_llm(decision));
        if let Some((intent, _)) = top {
            self.habits.observe(intent.clone(), now);
        }
    }

    /// The reply to `text` as an answer to the `open` dialog and its key on `satellite` at `now`: a
    /// `cancelled` reply that closes it, or a question again. Else the prediction to decide and its
    /// transcript: the filled intent of a `Slot` dialog, or the System One prediction of `text` in
    /// the area `name`, with true for a new request.
    ///
    /// # Errors
    ///
    /// The prediction fails.
    async fn route(
        &self,
        text: &str,
        open: Option<(Dialog, Key)>,
        name: Option<&str>,
        satellite: Option<&str>,
        now: Timestamp,
    ) -> anyhow::Result<ControlFlow<Handled, (Prediction, String, bool)>> {
        let Some((dialog, key)) = open else {
            return Ok(ControlFlow::Continue((
                self.predict(name, text).await?,
                text.to_owned(),
                true,
            )));
        };
        if dialog::is_cancel(text, &self.queues.dialog.cancel) {
            return Ok(ControlFlow::Break(self.cancel(key)));
        }
        if let Some((prediction, transcript)) = self.filled(&dialog, text) {
            return Ok(ControlFlow::Continue((prediction, transcript, false)));
        }
        let prediction = self.predict(name, text).await?;
        if self.is_clear(&prediction) {
            return Ok(ControlFlow::Continue((prediction, text.to_owned(), true)));
        }
        Ok(ControlFlow::Break(
            self.unclear(text, dialog, key, satellite, now).await,
        ))
    }

    /// The reply to the unclear answer `text` to `dialog` of `key` on `satellite` at `now`: for an
    /// `Llm` dialog, a filler and the System Two answer to the call of the dialog, or the answer
    /// itself without `satellite`. Else a question again.
    async fn unclear(
        &self,
        text: &str,
        dialog: Dialog,
        key: Key,
        satellite: Option<&str>,
        now: Timestamp,
    ) -> Handled {
        let Pending::Llm {
            response_id,
            call_id,
        } = dialog.pending
        else {
            return self.ask_again(dialog, key, satellite, now);
        };
        let thread = Thread {
            request: satellite
                .map(|satellite| (satellite.to_owned(), self.queues.next_generation(satellite))),
            key: Some(key),
            turns: dialog.turns,
            answers: Some(AskCall {
                response_id,
                call_id,
            }),
        };
        if thread.request.is_some() {
            let after = self.llm(text.to_owned(), None, thread);
            let text = self.pick("filler");
            let after = Some(after);
            return Handled { text, after };
        }
        let answer = self.answer(text, None, &thread).await;
        let text = self.or_failure(answer.map(|all| all.join(" ")));
        Handled { text, after: None }
    }

    /// A `cancelled` reply. Closes the dialog of `key`.
    fn cancel(&self, key: Key) -> Handled {
        self.queues.dialogs().close(key);
        let text = self.pick("cancelled");
        Handled { text, after: None }
    }

    /// The System One prediction of the normalized `text` in the area `name`.
    ///
    /// # Errors
    ///
    /// The prediction fails. The error text is a `failure` answer.
    async fn predict(&self, name: Option<&str>, text: &str) -> anyhow::Result<Prediction> {
        let model_context = training::context_text(name);
        let prediction = self
            .classifier
            .predict(&model_context, &intent::normalize(text))
            .await;
        prediction.map_err(|error| {
            tracing::warn!("the prediction failed: {error:#}");
            anyhow::anyhow!(self.pick("failure"))
        })
    }

    /// True when the top label of `prediction` is not `unknown` and has at least the threshold.
    fn is_clear(&self, prediction: &Prediction) -> bool {
        let top = prediction.ranked.first();
        top.is_some_and(|(label, probability)| label != "unknown" && *probability >= self.threshold)
    }

    /// The intent of the `Slot` dialog `dialog` with probability 1 and the value that `text` fills
    /// in place of the missing slot, and the transcript of the dialog with "in" and the value. `None`
    /// when the dialog has no choices or no choice fits.
    fn filled(&self, dialog: &Dialog, text: &str) -> Option<(Prediction, String)> {
        let Pending::Slot {
            intent,
            slots,
            missing,
            choices,
            transcript,
        } = &dialog.pending
        else {
            return None;
        };
        let similarity = self.queues.dialog.choice_similarity;
        let value = dialog::fill(text, choices, similarity).filter(|_| !choices.is_empty())?;
        let transcript = format!("{transcript} in {value}");
        let kept = slots.iter().filter(|slot| &slot.name != missing).cloned();
        let filled = Slot {
            name: missing.clone(),
            value,
        };
        let prediction = Prediction {
            ranked: vec![(intent.clone(), 1.0)],
            slots: kept.chain([filled]).collect(),
        };
        Some((prediction, transcript))
    }

    /// `dialog` of `key` with one more question from `ask_area` on `satellite` at `now`, in its
    /// queue with the current generation, or a `failure` answer at the turn limit.
    fn ask_again(
        &self,
        dialog: Dialog,
        key: Key,
        satellite: Option<&str>,
        now: Timestamp,
    ) -> Handled {
        let Some(next) = dialog::next_turn(dialog, self.queues.dialog.max_turns, now) else {
            let text = self.pick("failure");
            return Handled { text, after: None };
        };
        self.queues.dialogs().dialogs.insert(key, next);
        let request =
            satellite.map(|satellite| (satellite.to_owned(), self.queues.generation(satellite)));
        let (question, after) = self.ask(self.chosen("ask_area"), request);
        Handled {
            text: question.text,
            after,
        }
    }

    /// The ID and the name of the area of the device `device` in the hot cache.
    fn area(&self, device: Option<&str>) -> (Option<AreaId>, Option<String>) {
        let cache = self.queues.cache.borrow();
        let Some(id) = device.and_then(|device| cache.area_of_device(device)) else {
            return (None, None);
        };
        let area = cache.areas.iter().find(|area| &area.area_id == id);
        (Some(id.clone()), area.map(|area| area.name.clone()))
    }

    /// The decision for `prediction` of `request` in the current hot cache and pools.
    fn decide(&self, prediction: &Prediction, request: &Request) -> Decision {
        let (cache, pools) = (self.queues.cache.borrow(), self.pools.load());
        let rng = &mut self.rng.lock().expect("no lock holder panics");
        let catalog = &self.catalog;
        decide::decide(
            prediction,
            request,
            catalog,
            &cache,
            &pools,
            self.threshold,
            rng,
        )
    }

    /// The dispatch of `decision` from `area` on `satellite`. Without `satellite`, the answer of a
    /// deferred job, whose question opens a dialog for `key`, or a `failure` answer when the job
    /// fails.
    async fn reply(
        &self,
        decision: Decision,
        area: Option<&AreaId>,
        satellite: Option<&str>,
        key: Option<Key>,
    ) -> Handled {
        let answer = match (decision, satellite) {
            (Decision::Defer { job, .. }, None) => match job {
                Job::Fetch { call, pool } => self.fetched(&call, &pool).await,
                Job::Llm(text) => {
                    let thread = Thread {
                        key,
                        ..Thread::default()
                    };
                    let answer = self.answer(&text, area, &thread).await;
                    answer.map(|all| all.join(" "))
                }
            },
            (decision, satellite) => return self.dispatch(decision, area, satellite),
        };
        let text = self.or_failure(answer);
        Handled { text, after: None }
    }

    /// The reply, filler or question of `decision` and its service call from `area` as background
    /// work, and its fetch, answer or question only with `satellite`. Records the use of the reply.
    /// Increases the generation of `satellite`, the satellite entity of the request.
    pub fn dispatch(
        &self,
        decision: Decision,
        area: Option<&AreaId>,
        satellite: Option<&str>,
    ) -> Handled {
        let request = satellite
            .map(|satellite| (satellite.to_owned(), self.queues.next_generation(satellite)));
        let (chosen, after) = match decision {
            Decision::Reply { reply, call } => self.replied(reply, call, request),
            Decision::Defer {
                filler,
                job: Job::Fetch { call, pool },
            } => (
                filler,
                request.map(|request| self.fetch(call, pool, request)),
            ),
            Decision::Defer {
                filler,
                job: Job::Llm(text),
            } => {
                let llm = request.map(|request| {
                    let request = Some(request);
                    let thread = Thread {
                        request,
                        ..Thread::default()
                    };
                    self.llm(text, area.cloned(), thread)
                });
                (filler, llm)
            }
            Decision::Ask { question, .. } => self.ask(question, request),
        };
        let Chosen { text, pool, index } = chosen;
        self.backfill.record_use(pool, index);
        Handled { text, after }
    }

    /// `reply` and the work of `call` for `request`, with a search first when the intent of the
    /// pool of `reply` has `search`.
    fn replied(
        &self,
        reply: Chosen,
        call: Option<ServiceCall>,
        request: Option<(String, u64)>,
    ) -> (Chosen, Option<Work>) {
        let mut intents = self.catalog.intents.iter();
        let search = intents.any(|intent| intent.search && intent.name == reply.pool);
        (reply, call.map(|call| self.after(call, search, request)))
    }

    /// Empty text and `question` in the queue of the satellite of `request` with the generation of
    /// `request`, or the text of `question` with no `request`.
    fn ask(&self, mut question: Chosen, request: Option<(String, u64)>) -> (Chosen, Option<Work>) {
        let Some((satellite, generation)) = request else {
            return (question, None);
        };
        let (queues, text) = (Arc::clone(&self.queues), mem::take(&mut question.text));
        let ask = async move {
            let speak = queues.home.ask(&satellite, &text);
            queues.queued(&satellite, generation, &text, speak).await
        };
        (question, Some(ask.boxed()))
    }

    /// Makes the calls of `resolve` of `call` with `search` in order. When one fails, makes no
    /// more calls, logs the error, queues a `failure` answer on the satellite of `request` with
    /// the generation of `request`, and records its use.
    fn after(&self, call: ServiceCall, search: bool, request: Option<(String, u64)>) -> Work {
        let conversation = self.clone();
        async move {
            let calls = conversation.resolve(call, search).await;
            let Err(error) = conversation.make(calls).await else {
                return Ok(());
            };
            tracing::warn!("the call failed: {error:#}");
            let Some((satellite, generation)) = request else {
                return Ok(());
            };
            let text = conversation.pick("failure");
            let queues = &conversation.queues;
            queues.announce(&satellite, generation, &text).await
        }
        .boxed()
    }

    /// Makes `calls` in order, up to the first that fails.
    async fn make(&self, calls: Vec<ServiceCall>) -> anyhow::Result<()> {
        for call in calls {
            self.queues.home.call(call).await?;
        }
        Ok(())
    }

    /// With `search`, the `plays` of `call` for the items of `search` of its `media_id` in the
    /// config entry of its first target, else `call`. Logs a failed search.
    async fn resolve(&self, call: ServiceCall, search: bool) -> Vec<ServiceCall> {
        let name = call.data.get("media_id").and_then(Value::as_str);
        let entry = {
            let cache = self.queues.cache.borrow();
            let mut entities = cache.entities.iter();
            let found = entities.find(|entity| Some(&entity.entity_id.0) == call.target.first());
            found.and_then(|entity| entity.config_entry_id.clone())
        };
        let (true, Some(name), Some(entry)) = (search, name.map(str::to_owned), entry) else {
            return vec![call];
        };
        let items = self.search(&name, &entry).await.unwrap_or_else(|error| {
            tracing::warn!("the music search failed: {error:#}");
            Vec::new()
        });
        plays(call, items)
    }

    /// The items to play for the best match of `name` in the library playlists of the Music
    /// Assistant config entry `entry`, else in its artists, albums and tracks: the first track of
    /// an artist match and then the match, or only the match. An error when a search fails or its
    /// response does not parse.
    async fn search(&self, name: &str, entry: &str) -> anyhow::Result<Vec<MediaItem>> {
        let library = serde_json::json!({ "config_entry_id": entry, "name": name,
            "media_type": ["playlist"], "library_only": true });
        let all = serde_json::json!({ "config_entry_id": entry, "name": name,
            "media_type": ["artist", "album", "track"] });
        for data in [library, all] {
            let call = ServiceCall {
                domain: "music_assistant".to_owned(),
                service: "search".to_owned(),
                target: Vec::new(),
                data: serde_json::from_value(data)?,
                return_response: true,
            };
            let response = self.queues.home.call(call).await?.unwrap_or_default();
            let results: SearchResults = serde_json::from_value(response)?;
            if let Some(found) = decide::best_match(&results, name) {
                return Ok(items(&results, found));
            }
        }
        Ok(Vec::new())
    }

    /// Fetches `call` while `request` is the newest of its satellite. Queues its answer of `pool`,
    /// or a `failure` answer when it fails, with the generation of `request`.
    fn fetch(&self, call: PluginCall, pool: String, request: (String, u64)) -> Work {
        let conversation = self.clone();
        async move {
            let (satellite, generation) = request;
            if !conversation.queues.is_current(&satellite, generation) {
                return Ok(());
            }
            let answer = conversation.fetched(&call, &pool).await;
            let text = conversation.or_failure(answer);
            let queues = &conversation.queues;
            queues.announce(&satellite, generation, &text).await
        }
        .boxed()
    }

    /// Announces each sentence of the System Two answer to `text` from `area` in `thread` on the
    /// satellite of its request as it arrives, or a `failure` answer when the answer fails.
    fn llm(&self, text: String, area: Option<AreaId>, thread: Thread) -> Work {
        let conversation = self.clone();
        async move {
            let answer = conversation.answer(&text, area.as_ref(), &thread).await;
            let (Err(error), Some((satellite, generation))) = (answer, &thread.request) else {
                return Ok(());
            };
            let text = conversation.or_failure(Err(error));
            let queues = &conversation.queues;
            queues.announce(satellite, *generation, &text).await
        }
        .boxed()
    }

    /// The System Two question for `text` from `area` with the local time of the house.
    ///
    /// # Errors
    ///
    /// The HA configuration or its time zone is not known.
    fn question(&self, text: &str, area: Option<&AreaId>) -> anyhow::Result<Question> {
        use anyhow::Context as _;
        let cache = self.queues.cache.borrow();
        let config = cache.config().context("no HA configuration")?;
        let now = self.two.clock.now().in_tz(&config.time_zone)?.datetime();
        let location = &config.location_name;
        let domains = self.read_domains();
        Ok(decide::question(
            text, area, location, now, &cache, &domains,
        ))
    }

    /// The domain of each service of `read_services`.
    fn read_domains(&self) -> Vec<&str> {
        let services = self.two.read_services.iter();
        services
            .filter_map(|service| Some(service.split_once('.')?.0))
            .collect()
    }

    /// The sentences of the System Two answer to `text` from `area` in `thread`. With a request,
    /// announces each sentence on its satellite as it arrives instead. Makes each call before the
    /// next step and sends back its result. Ends before a call when the request is no longer the
    /// newest of its satellite.
    ///
    /// # Errors
    ///
    /// The HA configuration or its time zone is not known, the answer fails, no step comes
    /// within the timeout, or an announce or a question fails.
    async fn answer(
        &self,
        text: &str,
        area: Option<&AreaId>,
        thread: &Thread,
    ) -> anyhow::Result<Vec<String>> {
        use anyhow::Context as _;
        use futures::StreamExt;
        let question = Question {
            answers: thread.answers.clone(),
            ..self.question(text, area)?
        };
        let mut steps = self.two.reasoner.answer(&question);
        let (mut sentences, mut more) = (Vec::new(), true);
        while more {
            let next = tokio::time::timeout(self.two.timeout, steps.next()).await;
            more = match next.context("no step of the answer within the timeout")? {
                Some(step) => self.step(step?, thread, &mut sentences).await?,
                None => false,
            };
        }
        Ok(sentences)
    }

    /// Runs `step` of the System Two answer in `thread`, as `answer` describes. False when the
    /// answer ends.
    ///
    /// # Errors
    ///
    /// An announce or a question fails.
    async fn step(
        &self,
        step: Step,
        thread: &Thread,
        sentences: &mut Vec<String>,
    ) -> anyhow::Result<bool> {
        let request = thread.request.as_ref();
        match step {
            Step::Sentence(sentence) => self.say(sentence, request, sentences).await?,
            Step::Call(call, reply) => return Ok(self.step_call(call, reply, request).await),
            Step::States(area, reply) => {
                // The send fails only when the answer has ended, so the result is not needed.
                let _ = reply.send(self.states(&area));
            }
            Step::Plugin(call, reply) => {
                // The send fails only when the answer has ended, so the result is not needed.
                let _ = reply.send(self.plugin(&call).await.map(Some));
            }
            Step::Ask {
                question,
                response_id,
                call_id,
            } => {
                let pending = Pending::Llm {
                    response_id,
                    call_id,
                };
                self.asked(question, pending, thread, sentences).await?;
                return Ok(false);
            }
            Step::Done => return Ok(false),
        }
        Ok(true)
    }

    /// Asks `question` of System Two in `thread` with one more turn of its dialog, which then waits
    /// for `pending`, or gives a `failure` answer at the turn limit. With a request that is still
    /// the newest of its satellite, puts it in the queue of the satellite with a new generation.
    /// Without a request, adds it to `sentences`.
    ///
    /// # Errors
    ///
    /// The `failure` announce fails.
    async fn asked(
        &self,
        question: String,
        pending: Pending,
        thread: &Thread,
        sentences: &mut Vec<String>,
    ) -> anyhow::Result<()> {
        let (now, turns) = (self.two.clock.now(), thread.turns);
        let dialog = Dialog {
            pending,
            turns,
            asked: now,
        };
        let next = dialog::next_turn(dialog, self.queues.dialog.max_turns, now);
        let question = next.is_some().then_some(question);
        let Some((satellite, generation)) = &thread.request else {
            if let (Some(key), Some(next)) = (thread.key.clone(), next) {
                self.queues.dialogs().dialogs.insert(key, next);
            }
            sentences.push(question.unwrap_or_else(|| self.pick("failure")));
            return Ok(());
        };
        match self.queues.reopen(satellite, *generation, next) {
            Some(generation) => self.ask_or_fail(satellite, generation, question).await,
            None => Ok(()),
        }
    }

    /// Asks `question` on `satellite` with `generation` in its queue, or announces a `failure`
    /// answer with `generation` without `question` or when the question fails.
    ///
    /// # Errors
    ///
    /// The `failure` announce fails.
    async fn ask_or_fail(
        &self,
        satellite: &str,
        generation: u64,
        question: Option<String>,
    ) -> anyhow::Result<()> {
        let Some(question) = question else {
            let text = self.pick("failure");
            return self.queues.announce(satellite, generation, &text).await;
        };
        let speak = self.queues.home.ask(satellite, &question);
        let queues = &self.queues;
        let Err(error) = queues.queued(satellite, generation, &question, speak).await else {
            return Ok(());
        };
        let text = self.or_failure(Err(error));
        queues.announce(satellite, generation, &text).await
    }

    /// Announces `sentence` on the satellite of `request`, or adds it to `sentences` with no
    /// `request`.
    ///
    /// # Errors
    ///
    /// The announce fails.
    async fn say(
        &self,
        sentence: String,
        request: Option<&(String, u64)>,
        sentences: &mut Vec<String>,
    ) -> anyhow::Result<()> {
        let Some((satellite, generation)) = request else {
            sentences.push(sentence);
            return Ok(());
        };
        self.queues
            .announce(satellite, *generation, &sentence)
            .await
    }

    /// Makes `call` and sends its result on `reply` while `request` is the newest of its
    /// satellite, or with no `request`. False when it is not.
    async fn step_call(
        &self,
        call: ServiceCall,
        reply: oneshot::Sender<anyhow::Result<Option<Value>>>,
        request: Option<&(String, u64)>,
    ) -> bool {
        let current = request
            .is_none_or(|(satellite, generation)| self.queues.is_current(satellite, *generation));
        if current {
            // The send fails only when the answer has ended, so the result is not needed.
            let _ = reply.send(self.tool(call).await);
        }
        current
    }

    /// The response data of the System Two `call`, with `return_response` when `read_services`
    /// has its service.
    ///
    /// # Errors
    ///
    /// "not permitted" when `allowed_domains` lacks the domain of the service or of a target, the
    /// hot cache lacks a target, or the data has a target key. Else the call fails.
    async fn tool(&self, mut call: ServiceCall) -> anyhow::Result<Option<Value>> {
        let allowed = |domain: &str| self.two.allowed_domains.iter().any(|name| name == domain);
        // HA adds the targets of these data keys to the call, so they would skip the checks.
        let keys = ["entity_id", "device_id", "area_id", "floor_id", "label_id"];
        let untargeted = !keys.iter().any(|&key| call.data.contains_key(key));
        let permitted = untargeted && allowed(&call.domain) && {
            let cache = self.queues.cache.borrow();
            let known = |entity: &String| {
                let domain = entity.split('.').next().unwrap_or_default();
                allowed(domain) && cache.state(entity).is_some()
            };
            call.target.iter().all(known)
        };
        anyhow::ensure!(permitted, "not permitted");
        let service = format!("{}.{}", call.domain, call.service);
        call.return_response = self.two.read_services.contains(&service);
        self.queues.home.call(call).await
    }

    /// The JSON list of the name, the entity ID and the state of each entity of the area `name`
    /// in the hot cache.
    ///
    /// # Errors
    ///
    /// "unknown area" when the hot cache has no area with the name or alias `name`.
    fn states(&self, name: &str) -> anyhow::Result<Option<Value>> {
        use anyhow::Context as _;
        let cache = self.queues.cache.borrow();
        let found = decide::area_states(name, &cache).context("unknown area")?;
        let states = found.into_iter().map(|item| {
            serde_json::json!({"name": item.name, "entity_id": item.entity_id, "state": item.state})
        });
        Ok(Some(Value::Array(states.collect())))
    }

    /// The result of the plugin `call` in the house of the HA configuration of the hot cache.
    ///
    /// # Errors
    ///
    /// The HA configuration is not known, or the registry fails.
    async fn plugin(&self, call: &PluginCall) -> anyhow::Result<Value> {
        use anyhow::Context as _;
        let home = self.queues.cache.borrow().config().cloned();
        let home = home.context("no HA configuration")?;
        self.two.plugins.fetch(call, &home).await
    }

    /// An answer of the pool `pool` rendered with the result of the plugin `call` as `response`.
    ///
    /// # Errors
    ///
    /// The plugin or the render fails.
    async fn fetched(&self, call: &PluginCall, pool: &str) -> anyhow::Result<String> {
        let response = self.plugin(call).await?;
        let template = self.pick(pool);
        let context = Map::from_iter([("response".to_owned(), response)]);
        Ok(responses::render(&template, &context)?)
    }

    /// The text of `answer`, or a `failure` answer when it is an error. Logs the error.
    fn or_failure(&self, answer: anyhow::Result<String>) -> String {
        answer.unwrap_or_else(|error| {
            tracing::warn!("the answer failed: {error:#}");
            self.pick("failure")
        })
    }

    /// The text of an answer that `responses::select` picks from the current pool `pool`.
    /// Records its use.
    fn pick(&self, pool: &str) -> String {
        self.chosen(pool).text
    }

    /// The answer that `responses::select` picks from the current pool `pool`. Records its use.
    fn chosen(&self, pool: &str) -> Chosen {
        let (text, index) = {
            let pools = self.pools.load();
            let found = pools.get(pool).expect("valid pools have each answer pool");
            let index =
                responses::select(found, &mut self.rng.lock().expect("no lock holder panics"));
            (found.responses[index].text.clone(), index)
        };
        self.backfill.record_use(pool.to_owned(), index);
        let pool = pool.to_owned();
        Chosen { text, pool, index }
    }
}

/// Where a System Two answer goes, and the dialog that it continues.
#[derive(Default)]
struct Thread {
    /// The satellite and the generation of the request, or `None` without a satellite.
    request: Option<(String, u64)>,
    /// The key of the dialog that a question opens without a satellite.
    key: Option<Key>,
    /// The questions of the dialog so far.
    turns: u32,
    /// The `ask_user` call that the request answers.
    answers: Option<AskCall>,
}

/// The name of the kind of `decision`.
fn kind(decision: &Decision) -> &'static str {
    match decision {
        Decision::Reply { .. } => "reply",
        Decision::Defer { .. } => "defer",
        Decision::Ask { .. } => "ask",
    }
}

/// True when `decision` defers to System Two.
fn is_llm(decision: &Decision) -> bool {
    matches!(
        decision,
        Decision::Defer {
            job: Job::Llm(_),
            ..
        }
    )
}

/// The items to play for the match `found` in `results`: the first track of an artist match,
/// if any, and then the match.
fn items(results: &SearchResults, found: &MediaItem) -> Vec<MediaItem> {
    let artist = found.media_type == "artist";
    let track = artist.then(|| decide::first_track(results, &found.name));
    let items = track.flatten().into_iter().chain([found]);
    items.cloned().collect()
}

/// `call` for each of `items`, with its `uri` and `media_type`, and `enqueue = "add"` after the
/// first. `call` when `items` is empty.
fn plays(call: ServiceCall, items: Vec<MediaItem>) -> Vec<ServiceCall> {
    if items.is_empty() {
        return vec![call];
    }
    let played = items.into_iter().enumerate().map(|(index, item)| {
        let mut played = call.clone();
        played.data.insert("media_id".to_owned(), item.uri.into());
        played
            .data
            .insert("media_type".to_owned(), item.media_type.into());
        if index > 0 {
            played.data.insert("enqueue".to_owned(), "add".into());
        }
        played
    });
    played.collect()
}

/// The announce queue of one satellite.
#[derive(Default)]
struct Queue {
    /// The turn to announce. Its waiters are the items of the queue, in order.
    turn: tokio::sync::Mutex<()>,
}

/// The announce queue of each satellite, the open dialog of each key and the dialog rules.
pub struct Queues {
    /// The house.
    home: Arc<dyn Home>,
    /// The hot cache.
    cache: watch::Receiver<StateCache>,
    /// The wait after the satellite becomes idle.
    margin: Duration,
    /// The deadline for the satellite to become idle.
    idle_wait: Duration,
    /// The queue of each satellite entity.
    satellites: Mutex<BTreeMap<String, Arc<Queue>>>,
    /// The open dialogs.
    dialogs: Mutex<Dialogs>,
    /// The turn limit, the timeout, the choice similarity and the cancel phrases of a dialog.
    dialog: DialogConfig,
}

impl Queues {
    /// Queues that announce on `home` `margin` after `cache` shows the satellite idle, and drop
    /// an item when the satellite is not idle within `idle_wait`. Their dialogs follow `dialog`.
    #[must_use]
    pub fn new(
        home: Arc<dyn Home>,
        cache: watch::Receiver<StateCache>,
        margin: Duration,
        idle_wait: Duration,
        dialog: DialogConfig,
    ) -> Self {
        Self {
            home,
            cache,
            margin,
            idle_wait,
            satellites: Mutex::default(),
            dialogs: Mutex::default(),
            dialog,
        }
    }

    /// The queue of `satellite`.
    fn queue(&self, satellite: &str) -> Arc<Queue> {
        let mut satellites = self.satellites.lock().expect("no lock holder panics");
        Arc::clone(satellites.entry(satellite.to_owned()).or_default())
    }

    /// The open dialogs.
    fn dialogs(&self) -> MutexGuard<'_, Dialogs> {
        self.dialogs.lock().expect("no lock holder panics")
    }

    /// Opens a dialog on `key` that waits for `pending`, asked at `now`.
    pub fn open(&self, key: Key, pending: Pending, now: Timestamp) {
        self.dialogs().open(key, pending, now);
    }

    /// True when `text` is a cancel phrase and a dialog is open at `now`, or when `text` fills an
    /// open `Slot` dialog. An answer to System Two is free text, so it fills nothing here.
    pub fn is_answer(&self, text: &str, now: Timestamp) -> bool {
        let (mut dialogs, rules) = (self.dialogs().clone(), &self.dialog);
        let keys: Vec<Key> = dialogs.dialogs.keys().cloned().collect();
        let open: Vec<Dialog> = keys
            .iter()
            .filter_map(|key| dialogs.take(key, now, rules.timeout_s))
            .collect();
        // As in `filled`, a `Slot` dialog with no choices takes no answer.
        let fills = |dialog: &Dialog| match &dialog.pending {
            Pending::Slot { choices, .. } if !choices.is_empty() => {
                dialog::fill(text, choices, rules.choice_similarity).is_some()
            }
            _ => false,
        };
        (!open.is_empty() && dialog::is_cancel(text, &rules.cancel)) || open.iter().any(fills)
    }

    /// The generation of the dialog key of `satellite`.
    fn generation(&self, satellite: &str) -> u64 {
        self.dialogs().generation(&Key(satellite.to_owned()))
    }

    /// True when the generation of `satellite` is `generation`.
    fn is_current(&self, satellite: &str, generation: u64) -> bool {
        self.generation(satellite) == generation
    }

    /// Closes the dialog of `satellite`, which increases its generation, and returns the
    /// generation.
    pub fn next_generation(&self, satellite: &str) -> u64 {
        let (mut dialogs, key) = (self.dialogs(), Key(satellite.to_owned()));
        dialogs.close(key.clone());
        dialogs.generation(&key)
    }

    /// When the generation of `satellite` is `generation`, closes its dialog, which increases the
    /// generation, and stores `next` in its place. The new generation, else `None`.
    fn reopen(&self, satellite: &str, generation: u64, next: Option<Dialog>) -> Option<u64> {
        let (mut dialogs, key) = (self.dialogs(), Key(satellite.to_owned()));
        if dialogs.generation(&key) != generation {
            return None;
        }
        dialogs.close(key.clone());
        if let Some(next) = next {
            dialogs.dialogs.insert(key.clone(), next);
        }
        Some(dialogs.generation(&key))
    }

    /// Announces `text` on `satellite` after the earlier items of its queue, when the satellite
    /// is still idle `margin` after it became idle. Drops it when the satellite is not idle
    /// within `idle_wait`, or when the generation of the satellite is no longer `generation`.
    ///
    /// # Errors
    ///
    /// The announce fails.
    pub async fn announce(
        &self,
        satellite: &str,
        generation: u64,
        text: &str,
    ) -> anyhow::Result<()> {
        let speak = self.home.announce(satellite, text);
        self.queued(satellite, generation, text, speak).await
    }

    /// Runs `speak`, which speaks `text` on `satellite`, as `announce` describes.
    async fn queued(
        &self,
        satellite: &str,
        generation: u64,
        text: &str,
        speak: BoxFuture<'_, anyhow::Result<()>>,
    ) -> anyhow::Result<()> {
        let queue = self.queue(satellite);
        let _turn = queue.turn.lock().await;
        let mut cache = self.cache.clone();
        let idle = |cache: &StateCache| {
            cache
                .state(satellite)
                .is_some_and(|state| state.state == "idle")
        };
        let deadline = tokio::time::Instant::now() + self.idle_wait;
        loop {
            let wait = tokio::time::timeout_at(deadline, cache.wait_for(idle));
            if !matches!(wait.await, Ok(Ok(_))) {
                tracing::warn!("{satellite} is not idle, so the announce of {text:?} is dropped");
                return Ok(());
            }
            tokio::time::sleep(self.margin).await;
            if idle(&cache.borrow()) {
                break;
            }
        }
        if !self.is_current(satellite, generation) {
            return Ok(());
        }
        speak.await
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

/// Starts the pools task with the pools `source` merged into the pools of `store`. The task
/// applies each change in order, publishes the result and saves it to `store`.
///
/// # Errors
///
/// `store` cannot load.
pub fn start_pools(source: Pools, store: Arc<dyn Store<Pools>>) -> anyhow::Result<PoolsTask> {
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

/// The pools of `source` with the use count of each response that `saved` has too, then the
/// generated responses of `saved` that `source` does not have.
fn merge(mut source: Pools, saved: &Pools) -> Pools {
    for (name, pool) in &mut source {
        let old = saved
            .get(name)
            .map_or(&[][..], |pool| pool.responses.as_slice());
        for response in &mut pool.responses {
            let same = old.iter().find(|other| other.text == response.text);
            response.uses = same.map_or(0, |other| other.uses);
        }
        let kept = generated(old, &pool.responses);
        pool.responses.extend(kept);
    }
    source
}

/// The generated responses of `saved` whose text no response of `source` has.
fn generated(saved: &[Response], source: &[Response]) -> Vec<Response> {
    let new = |other: &&Response| {
        let same = |response: &Response| response.text == other.text;
        other.generated && !source.iter().any(same)
    };
    saved.iter().filter(new).cloned().collect()
}

/// Tests of dispatch and the pools task.
#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;
    use std::fs;
    use std::path::Path;
    use std::thread;
    use std::time::Duration;

    use jiff::Timestamp;
    use quint_connect::Driver;
    use quint_connect::State as ModelState;
    use quint_connect::Step;
    use quint_connect::quint_run;
    use quint_connect::switch;
    use serde::Deserialize;
    use serde_json::json;
    use tokio::runtime::Runtime;
    use tokio::time::Instant;
    use tracing::instrument::WithSubscriber;

    use super::*;
    use crate::app::speech::Speech;
    use crate::domain::habits::Bucket;
    use crate::domain::habits::DayType;
    use crate::domain::habits::RequestHabits;
    use crate::domain::model::Slot;
    use crate::domain::responses::Pool;
    use crate::domain::state::ServiceCall;
    use crate::domain::state::State;
    use crate::ports::Plugin;
    use crate::testing::fakes::FakeClassifier;
    use crate::testing::fakes::FakeClock;
    use crate::testing::fakes::FakeHome;
    use crate::testing::fakes::FakePlugin;
    use crate::testing::fakes::FakeReasoner;
    use crate::testing::fakes::FakeStore;
    use crate::testing::fakes::FakeTts;
    use crate::testing::fakes::Scripted;
    use crate::testing::fakes::SetClock;
    use crate::testing::fakes::house;
    use crate::testing::fakes::log_dispatch;

    /// The satellite entity of the tests.
    const SATELLITE: &str = "assist_satellite.carson_kitchen";

    /// The `announce.margin_ms` of the tests.
    const MARGIN: Duration = Duration::from_millis(300);

    /// The `announce.idle_wait_s` of the tests. No conformance trace runs this long.
    const IDLE_WAIT: Duration = Duration::from_secs(3600);

    /// The device of the kitchen satellite.
    const KITCHEN: &str = "98f4aacf9c4152ffe32dff212df91521";

    /// The house, the hot cache, the reasoner and the work of a System Two job.
    type Reasoning = (
        Arc<FakeHome>,
        watch::Sender<StateCache>,
        Arc<FakeReasoner>,
        Work,
    );

    /// The `openai.timeout_ms` of the tests.
    const TIMEOUT: Duration = Duration::from_millis(8000);

    /// A backfill on `changes`, `reasoner`, the hot cache of `queues` and `FakeTts`, with the
    /// catalog of `intents.toml` and the `[backfill]` of `jarvis.toml` up to `max_pool` responses.
    fn backfill(
        changes: mpsc::Sender<Change>,
        reasoner: Arc<FakeReasoner>,
        queues: &Queues,
        max_pool: usize,
    ) -> Arc<Backfill> {
        let mut config = crate::Config::parse(include_str!("../../jarvis.toml")).expect("valid");
        config.backfill.max_pool = max_pool;
        let catalog = toml::from_str(include_str!("../../intents.toml")).expect("it is a catalog");
        let speech = Arc::new(Speech::new(Arc::new(FakeTts::default())));
        let (catalog, cache) = (Arc::new(catalog), queues.cache.clone());
        let backfill = Backfill::new(config.backfill, reasoner, changes, speech, catalog, cache);
        Arc::new(backfill)
    }

    /// A conversation with `pools`, `queues`, `classifier` and `reasoner`, the backfill of
    /// `changes` up to `max_pool` responses, the catalog of `intents.toml` and the
    /// `model.threshold` of `jarvis.toml`.
    fn conversation(
        pools: Arc<ArcSwap<Pools>>,
        changes: mpsc::Sender<Change>,
        max_pool: usize,
        queues: Queues,
        classifier: Arc<dyn Classifier>,
        reasoner: Arc<FakeReasoner>,
    ) -> Conversation {
        let catalog = toml::from_str(include_str!("../../intents.toml")).expect("it is a catalog");
        let clock = Arc::new(FakeClock(Timestamp::UNIX_EPOCH));
        let backfill = backfill(changes, Arc::clone(&reasoner), &queues, max_pool);
        let habits = Habits::new(RequestHabits::default(), 14.0, queues.cache.clone());
        let (queues, timeout) = (Arc::new(queues), TIMEOUT);
        Conversation::new(
            pools,
            backfill,
            queues,
            classifier,
            Arc::new(catalog),
            0.6,
            SystemTwo {
                reasoner,
                plugins: registry(Vec::new()),
                clock,
                timeout,
                allowed_domains: ALLOWED.map(str::to_owned).to_vec(),
                read_services: vec!["calendar.get_events".to_owned()],
            },
            Arc::new(habits),
        )
    }

    /// The pool `lights_on` with the responses and use counts `lights`, and the pool `failure` with
    /// one response used `failure` times.
    fn pools(lights: &[(&str, u64)], failure: u64) -> Pools {
        let pool = |texts: &[(&str, u64)]| {
            let responses = texts.iter().map(|&(text, uses)| Response {
                text: text.to_owned(),
                uses,
                generated: false,
            });
            Pool {
                responses: responses.collect(),
                last: None,
            }
        };
        Pools::from([
            ("lights_on".to_owned(), pool(lights)),
            ("failure".to_owned(), pool(&[("Failed.", failure)])),
            ("filler".to_owned(), pool(&[("One moment.", 0)])),
            ("ask_area".to_owned(), pool(&[("Which room?", 0)])),
            ("cancelled".to_owned(), pool(&[("Cancelled.", 0)])),
            (
                "weather".to_owned(),
                pool(&[("It is {{ response.sky }}.", 0)]),
            ),
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

    /// A call of `domain.service` on `target`.
    fn service(domain: &str, service: &str, target: &str) -> ServiceCall {
        ServiceCall {
            domain: domain.to_owned(),
            service: service.to_owned(),
            target: vec![target.to_owned()],
            data: serde_json::Map::new(),
            return_response: false,
        }
    }

    /// A call that turns on the kitchen light.
    fn call() -> ServiceCall {
        service("light", "turn_on", "light.kitchen")
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
        let task = start_pools(unused(), Arc::clone(&store) as _)?;
        let (pools, changes) = (Arc::clone(&task.pools), task.changes.clone());
        let classifier = Arc::new(FakeClassifier::default());
        let queues = queues(home, "idle").1;
        // No backfill starts: a held `generate` would keep the pools task running.
        let conversation = conversation(pools, changes, 0, queues, classifier, Arc::default());
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
        let handled = conversation.dispatch(reply(Some(call())), None, None);
        assert_eq!(Instant::now(), start, "dispatch waited for the call");
        assert!(handled.after.is_some(), "the call is not in after");
        Ok(())
    }

    /// AC-4.8-2.
    #[tokio::test(start_paused = true)]
    async fn ac_4_8_2_call_only_after_start() -> anyhow::Result<()> {
        let home = Arc::new(FakeHome::default());
        let (_task, _store, conversation) = start(&home)?;
        let after = conversation.dispatch(reply(Some(call())), None, None).after;
        assert_eq!(home.calls(), [], "a call came before the start");
        after.ok_or_else(|| anyhow::anyhow!("no after"))?.await?;
        assert_eq!(home.calls(), [call()]);
        Ok(())
    }

    /// AC-4.8-3.
    #[tokio::test(start_paused = true)]
    async fn ac_4_8_3_store_receives_new_use_count() -> anyhow::Result<()> {
        let started = start(&Arc::new(FakeHome::default()))?;
        let _ = started.2.dispatch(reply(Some(call())), None, None);
        let expected = responses::record_use(unused(), "lights_on", 1);
        assert_eq!(last_save(started).await?, expected);
        Ok(())
    }

    /// AC-4.8-4.
    #[tokio::test(start_paused = true)]
    async fn ac_4_8_4_reply_without_call_makes_no_call() -> anyhow::Result<()> {
        let home = Arc::new(FakeHome::default());
        let (_task, _store, conversation) = start(&home)?;
        let handled = conversation.dispatch(reply(None), None, None);
        assert!(handled.after.is_none(), "a reply with no call has work");
        assert_eq!(home.calls(), []);
        Ok(())
    }

    /// AC-4.8-6.
    #[tokio::test(start_paused = true)]
    async fn ac_4_8_6_concurrent_uses_are_all_saved() -> anyhow::Result<()> {
        let started = start(&Arc::new(FakeHome::default()))?;
        thread::scope(|scope| {
            for _ in 0..100 {
                scope.spawn(|| started.2.dispatch(reply(None), None, None));
            }
        });
        let saved = last_save(started).await?;
        assert_eq!(saved["lights_on"].responses[1].uses, 100);
        Ok(())
    }

    /// AC-4.8-7.
    #[tokio::test(start_paused = true)]
    async fn ac_4_8_7_merge_keeps_counts_of_source_responses() -> anyhow::Result<()> {
        let source = pools(&[("Lights on.", 0), ("New.", 0)], 0);
        let saved = pools(&[("Lights on.", 3), ("Done.", 2)], 5);
        let task = start_pools(source, Arc::new(FakeStore::new(Some(saved))))?;
        let expected = pools(&[("Lights on.", 3), ("New.", 0)], 5);
        assert_eq!(**task.pools.load(), expected);
        Ok(())
    }

    /// The pools that the startup merge gives for the source response `Lights on.` and the saved
    /// responses `lights`.
    fn merged(lights: Vec<Response>) -> anyhow::Result<Pools> {
        let mut saved = pools(&[], 0);
        saved
            .get_mut("lights_on")
            .expect("the pool exists")
            .responses = lights;
        let store = Arc::new(FakeStore::new(Some(saved)));
        let task = start_pools(pools(&[("Lights on.", 0)], 0), store)?;
        Ok(Pools::clone(&task.pools.load()))
    }

    /// A saved generated response stays after the startup merge, with its use count.
    #[tokio::test(start_paused = true)]
    async fn startup_merge_keeps_generated_response() -> anyhow::Result<()> {
        let text = "Done.".to_owned();
        let generated = Response {
            text,
            uses: 2,
            generated: true,
        };
        let mut expected = pools(&[("Lights on.", 0)], 0);
        let pool = expected.get_mut("lights_on").expect("the pool exists");
        pool.responses.push(generated.clone());
        assert_eq!(merged(vec![generated])?, expected);
        Ok(())
    }

    /// A saved source response that is no longer in the source leaves after the startup merge.
    #[tokio::test(start_paused = true)]
    async fn startup_merge_drops_removed_source_response() -> anyhow::Result<()> {
        let removed = pools(&[("Done.", 2)], 0)["lights_on"].responses.clone();
        assert_eq!(merged(removed)?, pools(&[("Lights on.", 0)], 0));
        Ok(())
    }

    /// Sets the state of the satellite in `cache` to `value`.
    fn set(cache: &watch::Sender<StateCache>, value: &str) {
        cache.send_modify(|cache| {
            let state = cache.states.get_mut(SATELLITE).expect("the house has it");
            Arc::make_mut(state).state = value.to_owned();
        });
    }

    /// Queues on `home` and the hot cache of the reference house, with the satellite in the
    /// state `value` and the `[dialog]` of `jarvis.toml`.
    fn queues(home: &Arc<FakeHome>, value: &str) -> (watch::Sender<StateCache>, Queues) {
        let (cache, receiver) = watch::channel(StateCache::load(house()));
        set(&cache, value);
        let config = crate::Config::parse(include_str!("../../jarvis.toml")).expect("valid");
        let home = Arc::clone(home) as _;
        let queues = Queues::new(home, receiver, MARGIN, IDLE_WAIT, config.dialog);
        (cache, queues)
    }

    /// The texts that `home` announced, in order.
    fn announces(home: &FakeHome) -> Vec<String> {
        let calls = home.calls();
        let texts = calls
            .iter()
            .filter_map(|call| call.data.get("message")?.as_str());
        texts.map(str::to_owned).collect()
    }

    /// AC-4.9b-5.
    #[tokio::test(start_paused = true)]
    async fn ac_4_9b_5_busy_satellite_drops_announce() -> anyhow::Result<()> {
        let home = Arc::new(FakeHome::default());
        let (_cache, queues) = queues(&home, "responding");
        let generation = queues.next_generation(SATELLITE);
        let start = Instant::now();
        queues.announce(SATELLITE, generation, "Done.").await?;
        assert_eq!(Instant::now() - start, IDLE_WAIT, "the item ended early");
        assert_eq!(announces(&home), [] as [&str; 0]);
        Ok(())
    }

    /// Runs the tasks of `runtime` while its paused clock advances by `duration`. The clock
    /// advances only when every task waits.
    fn advance(runtime: &Runtime, duration: Duration) {
        runtime.block_on(async { tokio::time::sleep(duration).await });
    }

    /// AC-4.9b-6.
    #[tokio::test(start_paused = true)]
    async fn ac_4_9b_6_busy_during_margin_waits_again() -> anyhow::Result<()> {
        let home = Arc::new(FakeHome::default());
        let (cache, queues) = queues(&home, "idle");
        let generation = queues.next_generation(SATELLITE);
        let (start, busy_for) = (Instant::now(), Duration::from_secs(1));
        let busy = async {
            tokio::time::sleep(MARGIN / 3).await;
            set(&cache, "responding");
            tokio::time::sleep(busy_for).await;
            assert_eq!(
                announces(&home),
                [] as [&str; 0],
                "an announce came while busy"
            );
            set(&cache, "idle");
        };
        tokio::join!(queues.announce(SATELLITE, generation, "Done."), busy).0?;
        assert_eq!(Instant::now() - start, MARGIN / 3 + busy_for + MARGIN);
        assert_eq!(announces(&home), ["Done."]);
        Ok(())
    }

    /// A conversation on `home`, `classifier` and `reasoner` with the queues of
    /// `queues(home, value)`, the pools `unused()` and the receiver of its changes. It has no
    /// pools task, because a running blocking task stops the paused clock.
    fn still_with(
        home: &Arc<FakeHome>,
        value: &str,
        classifier: Arc<dyn Classifier>,
        reasoner: Arc<FakeReasoner>,
    ) -> (
        watch::Sender<StateCache>,
        mpsc::Receiver<Change>,
        Conversation,
    ) {
        let (cache, queues) = queues(home, value);
        let pools = Arc::new(ArcSwap::from_pointee(unused()));
        let (changes, receiver) = mpsc::channel();
        let conversation = conversation(pools, changes, usize::MAX, queues, classifier, reasoner);
        (cache, receiver, conversation)
    }

    /// `still_with` and a classifier that gives no prediction.
    fn still(
        home: &Arc<FakeHome>,
        value: &str,
    ) -> (
        watch::Sender<StateCache>,
        mpsc::Receiver<Change>,
        Conversation,
    ) {
        still_with(
            home,
            value,
            Arc::<FakeClassifier>::default(),
            Arc::default(),
        )
    }

    /// The work of a dispatch of `decision` on `satellite`.
    fn work(
        conversation: &Conversation,
        decision: Decision,
        satellite: Option<&str>,
    ) -> anyhow::Result<Work> {
        let handled = conversation.dispatch(decision, None, satellite);
        handled.after.ok_or_else(|| anyhow::anyhow!("no after"))
    }

    /// The work of a dispatch on the satellite of a reply with `call()`, whose call fails.
    fn failed(home: &FakeHome, conversation: &Conversation) -> anyhow::Result<Work> {
        drop(home.delay());
        work(conversation, reply(Some(call())), Some(SATELLITE))
    }

    /// AC-4.9c-1.
    #[tokio::test(start_paused = true)]
    async fn ac_4_9c_1_failure_announce_starts_margin_after_idle() -> anyhow::Result<()> {
        let home = Arc::new(FakeHome::default());
        let (cache, _changes, conversation) = still(&home, "responding");
        let (start, busy_for) = (Instant::now(), Duration::from_millis(1500));
        let after = failed(&home, &conversation)?;
        let idle = async {
            tokio::time::sleep(busy_for).await;
            set(&cache, "idle");
        };
        tokio::join!(after, idle).0?;
        assert_eq!(Instant::now() - start, busy_for + MARGIN);
        assert_eq!(announces(&home), ["Failed."]);
        Ok(())
    }

    /// AC-4.9c-2.
    #[tokio::test(start_paused = true)]
    async fn ac_4_9c_2_failed_call_announces_failure_once() -> anyhow::Result<()> {
        let home = Arc::new(FakeHome::default());
        let (_cache, _changes, conversation) = still(&home, "idle");
        failed(&home, &conversation)?.await?;
        let calls = home.calls();
        assert_eq!(calls.len(), 2, "not one call and one announce: {calls:?}");
        assert_eq!(calls[0], call());
        assert_eq!(announces(&home), ["Failed."]);
        Ok(())
    }

    /// AC-4.9c-3.
    #[tokio::test(start_paused = true)]
    async fn ac_4_9c_3_new_dispatch_drops_old_failure() -> anyhow::Result<()> {
        let home = Arc::new(FakeHome::default());
        let (_cache, _changes, conversation) = still(&home, "idle");
        let first = failed(&home, &conversation)?;
        let _ = conversation.dispatch(reply(None), None, Some(SATELLITE));
        first.await?;
        assert_eq!(announces(&home), [] as [&str; 0]);
        Ok(())
    }

    /// AC-4.9c-4.
    #[tokio::test(start_paused = true)]
    async fn ac_4_9c_4_failure_answer_records_use() -> anyhow::Result<()> {
        let home = Arc::new(FakeHome::default());
        let (_cache, changes, conversation) = still(&home, "idle");
        failed(&home, &conversation)?.await?;
        let applied = changes
            .try_iter()
            .fold(unused(), |pools, change| change(pools));
        let reply = responses::record_use(unused(), "lights_on", 1);
        assert_eq!(applied, responses::record_use(reply, "failure", 0));
        Ok(())
    }

    /// AC-8.4b-2.
    #[tokio::test(start_paused = true)]
    async fn ac_8_4b_2_select_does_not_wait_for_generate() {
        let (home, reasoner) = (
            Arc::new(FakeHome::default()),
            Arc::new(FakeReasoner::default()),
        );
        let classifier = Arc::<FakeClassifier>::default();
        let (_cache, changes, conversation) =
            still_with(&home, "idle", classifier, Arc::clone(&reasoner));
        let _ = conversation.dispatch(reply(None), None, None);
        let _ = changes
            .try_iter()
            .fold(unused(), |pools, change| change(pools));
        tokio::time::sleep(Duration::from_millis(1)).await;
        let waiting = reasoner.generated();
        assert_eq!(waiting, ["lights_on"], "no generate call waits");
        let start = Instant::now();
        let _ = conversation.pick("lights_on");
        assert_eq!(Instant::now(), start, "select waited for the backfill");
    }

    /// The call of the plugin `plugin` with `when = today`.
    fn today(plugin: &str) -> PluginCall {
        PluginCall {
            plugin: plugin.to_owned(),
            arguments: Map::from_iter([("when".to_owned(), "today".into())]),
        }
    }

    /// The `get_weather` plugin with `result` scripted, in the registry of `conversation`.
    fn weather(conversation: &mut Conversation, result: anyhow::Result<Value>) -> Arc<FakePlugin> {
        with_plugin(conversation, "get_weather", result)
    }

    /// The plugin `name` with `result` scripted, in the registry of `conversation`.
    fn with_plugin(
        conversation: &mut Conversation,
        name: &str,
        result: anyhow::Result<Value>,
    ) -> Arc<FakePlugin> {
        let plugin = Arc::new(FakePlugin::new(name));
        plugin.script(result);
        conversation.two.plugins = registry(vec![Arc::clone(&plugin) as Arc<dyn Plugin>]);
        plugin
    }

    /// The work of a dispatch on the satellite of a `weather` fetch of `get_weather`.
    fn fetch(conversation: &Conversation) -> anyhow::Result<Work> {
        let job = Job::Fetch {
            call: today("get_weather"),
            pool: "weather".to_owned(),
        };
        let defer = Decision::Defer {
            filler: chosen("filler", 0),
            job,
        };
        work(conversation, defer, Some(SATELLITE))
    }

    /// AC-17.4-4.
    #[tokio::test(start_paused = true)]
    async fn ac_17_4_4_fetch_runs_the_plugin_and_no_call() -> anyhow::Result<()> {
        let home = Arc::new(FakeHome::default());
        let (_cache, _changes, mut conversation) = still(&home, "idle");
        let plugin = weather(&mut conversation, Ok(serde_json::json!({ "sky": "sunny" })));
        fetch(&conversation)?.await?;
        let fetches = plugin.fetches();
        assert_eq!(fetches.len(), 1, "{fetches:?}");
        assert_eq!(fetches[0].0, today("get_weather").arguments);
        assert_eq!(announces(&home), ["It is sunny."]);
        assert_eq!(home.calls().len(), 1, "a call that is not the announce");
        Ok(())
    }

    /// AC-4.9d-2.
    #[tokio::test(start_paused = true)]
    async fn ac_4_9d_2_fetch_announces_rendered_answer_when_idle() -> anyhow::Result<()> {
        let home = Arc::new(FakeHome::default());
        let (cache, _changes, mut conversation) = still(&home, "responding");
        weather(&mut conversation, Ok(serde_json::json!({ "sky": "sunny" })));
        let idle = async {
            tokio::time::sleep(Duration::from_secs(1)).await;
            assert_eq!(
                announces(&home),
                [] as [&str; 0],
                "an announce came while busy"
            );
            set(&cache, "idle");
        };
        tokio::join!(fetch(&conversation)?, idle).0?;
        assert_eq!(announces(&home), ["It is sunny."]);
        let preannounce = home
            .calls()
            .pop()
            .map(|call| call.data["preannounce"].clone());
        assert_eq!(preannounce, Some(serde_json::json!(false)));
        Ok(())
    }

    /// AC-4.9d-3.
    #[tokio::test(start_paused = true)]
    async fn ac_4_9d_3_failed_fetch_announces_failure() -> anyhow::Result<()> {
        let home = Arc::new(FakeHome::default());
        let (_cache, _changes, mut conversation) = still(&home, "idle");
        weather(&mut conversation, Err(anyhow::anyhow!("server down")));
        fetch(&conversation)?.await?;
        assert_eq!(announces(&home), ["Failed."]);
        Ok(())
    }

    /// A classifier that gives `label` with 0.9, or fails without `label`.
    fn classifier(label: Option<&str>) -> Arc<FakeClassifier> {
        scripted(label.as_slice())
    }

    /// A classifier that gives each of `labels` with 0.9 in order, then fails.
    fn scripted(labels: &[&str]) -> Arc<FakeClassifier> {
        let classifier = Arc::new(FakeClassifier::default());
        for &label in labels {
            let ranked = vec![(label.to_owned(), 0.9)];
            classifier.script(Prediction {
                ranked,
                slots: Vec::new(),
            });
        }
        classifier
    }

    /// The result of `handle` for `text` from the satellite device `device` when the classifier
    /// gives `label` with 0.9, or fails without `label`. Also the `(context, text)` of each
    /// prediction.
    async fn handled(
        label: Option<&str>,
        text: &str,
        device: Option<&str>,
    ) -> (anyhow::Result<Handled>, Vec<(String, String)>) {
        let classifier = classifier(label);
        let home = Arc::new(FakeHome::default());
        let (_cache, _changes, conversation) =
            still_with(&home, "idle", Arc::clone(&classifier) as _, Arc::default());
        let context = Context {
            device_id: device.map(str::to_owned),
            satellite_id: device.map(|_| SATELLITE.to_owned()),
            ..Context::default()
        };
        let result = conversation.handle(text, context).await;
        (result, classifier.calls())
    }

    /// AC-5.3-1.
    #[tokio::test(start_paused = true)]
    async fn ac_5_3_1_lights_on_in_the_kitchen_gives_a_lights_on_reply() -> anyhow::Result<()> {
        let (handled, _) = handled(Some("lights_on"), "turn on the lights", Some(KITCHEN)).await;
        let text = handled?.text;
        let replies = &unused()["lights_on"].responses;
        assert!(
            replies.iter().any(|reply| reply.text == text),
            "{text:?} is not a lights_on reply"
        );
        Ok(())
    }

    /// AC-11.5-3.
    #[tokio::test(start_paused = true)]
    async fn ac_11_5_3_kitchen_device_gives_the_kitchen_context() {
        let (_, calls) = handled(Some("lights_on"), "turn on the lights", Some(KITCHEN)).await;
        assert_eq!(calls[0].0, "area: Kitchen");
    }

    /// AC-5.3-3.
    #[tokio::test(start_paused = true)]
    async fn ac_5_3_3_no_device_gives_no_area() {
        let (_, calls) = handled(Some("lights_on"), "turn on the lights", None).await;
        assert_eq!(calls[0].0, "area: none");
    }

    /// AC-5.3-4.
    #[tokio::test(start_paused = true)]
    async fn ac_5_3_4_defer_on_a_satellite_gives_a_filler() -> anyhow::Result<()> {
        let (handled, _) = handled(Some("unknown"), "tell me a joke", Some(KITCHEN)).await;
        assert_eq!(handled?.text, "One moment.");
        Ok(())
    }

    /// AC-5.3-5.
    #[tokio::test(start_paused = true)]
    async fn ac_5_3_5_failed_prediction_gives_a_failure_error() {
        let (handled, _) = handled(None, "turn on the lights", Some(KITCHEN)).await;
        let error = handled.err().expect("the prediction fails");
        assert_eq!(error.to_string(), "Failed.");
    }

    /// AC-5.3-7.
    #[tokio::test(start_paused = true)]
    async fn ac_5_3_7_prediction_gets_the_normalized_text() {
        let (_, calls) = handled(Some("lights_on"), "Turn on the Lights.", Some(KITCHEN)).await;
        assert_eq!(calls[0].1, "turn on the lights");
    }

    /// AC-6.11-2.
    #[tokio::test(start_paused = true)]
    async fn ac_6_11_2_handle_logs_one_info_event() -> anyhow::Result<()> {
        let log = tempfile::NamedTempFile::new()?;
        let (handled, _) = handled(Some("lights_on"), "turn on the lights", Some(KITCHEN))
            .with_subscriber(log_dispatch(&log)?)
            .await;
        let reply = handled?.text;
        let text = fs::read_to_string(log.path())?;
        let events: Vec<&str> = text
            .lines()
            .filter(|line| line.contains(" INFO "))
            .collect();
        assert_eq!(events.len(), 1, "the log is {text:?}");
        for part in ["turn on the lights", "lights_on", &reply] {
            assert!(
                events[0].contains(part),
                "{part:?} is not in {:?}",
                events[0]
            );
        }
        Ok(())
    }

    /// AC-7.5-1.
    #[tokio::test(start_paused = true)]
    async fn ac_7_5_1_unknown_gives_a_filler_before_time_advances() -> anyhow::Result<()> {
        let start = Instant::now();
        let (handled, _) = handled(Some("unknown"), "tell me a joke", Some(KITCHEN)).await;
        assert_eq!(Instant::now(), start, "handle waited");
        assert_eq!(handled?.text, "One moment.");
        Ok(())
    }

    /// The hot cache and the result of `handle` for "turn on the lights" from no device on the
    /// satellite in the state `value`, when the classifier gives `lights_on`.
    async fn unplaced(
        home: &Arc<FakeHome>,
        value: &str,
    ) -> anyhow::Result<(watch::Sender<StateCache>, Handled)> {
        let classifier = classifier(Some("lights_on"));
        let (cache, _changes, conversation) = still_with(home, value, classifier, Arc::default());
        let handled = conversation
            .handle("turn on the lights", on_satellite())
            .await?;
        Ok((cache, handled))
    }

    /// The context of the satellite with no device.
    fn on_satellite() -> Context {
        Context {
            satellite_id: Some(SATELLITE.to_owned()),
            ..Context::default()
        }
    }

    /// The questions that `home` asked, in order.
    fn asks(home: &FakeHome) -> Vec<String> {
        let calls = home.calls();
        let texts = calls
            .iter()
            .filter_map(|call| call.data.get("start_message")?.as_str());
        texts.map(str::to_owned).collect()
    }

    /// AC-10.4-4.
    #[tokio::test(start_paused = true)]
    async fn ac_10_4_4_ask_on_a_satellite_gives_empty_text_and_asks() -> anyhow::Result<()> {
        let home = Arc::new(FakeHome::default());
        let (_cache, handled) = unplaced(&home, "idle").await?;
        assert_eq!(handled.text, "");
        let after = handled.after.ok_or_else(|| anyhow::anyhow!("no after"))?;
        after.await?;
        assert_eq!(asks(&home), ["Which room?"]);
        Ok(())
    }

    /// AC-10.4-5.
    #[tokio::test(start_paused = true)]
    async fn ac_10_4_5_ask_without_a_satellite_returns_the_question() -> anyhow::Result<()> {
        let (handled, _) = handled(Some("lights_on"), "turn on the lights", None).await;
        assert_eq!(handled?.text, "Which room?");
        Ok(())
    }

    /// AC-10.4-8.
    #[tokio::test(start_paused = true)]
    async fn ac_10_4_8_ask_waits_for_the_idle_satellite() -> anyhow::Result<()> {
        let home = Arc::new(FakeHome::default());
        let (cache, handled) = unplaced(&home, "responding").await?;
        let after = handled.after.ok_or_else(|| anyhow::anyhow!("no after"))?;
        let (start, busy_for) = (Instant::now(), Duration::from_secs(1));
        let idle = async {
            tokio::time::sleep(busy_for).await;
            assert_eq!(asks(&home), [] as [&str; 0], "a question came while busy");
            set(&cache, "idle");
        };
        tokio::join!(after, idle).0?;
        assert_eq!(Instant::now() - start, busy_for + MARGIN);
        assert_eq!(asks(&home), ["Which room?"]);
        Ok(())
    }

    /// The replies of a conversation on `home` and `reasoner` to each of `texts` from the satellite
    /// with no device, when the classifier gives `labels` in order, and the conversation.
    async fn talk(
        home: &Arc<FakeHome>,
        reasoner: Arc<FakeReasoner>,
        texts: &[&str],
        labels: &[&str],
    ) -> anyhow::Result<(Vec<Handled>, Conversation)> {
        let (_cache, _changes, conversation) = still_with(home, "idle", scripted(labels), reasoner);
        let mut replies = Vec::new();
        for text in texts {
            replies.push(conversation.handle(text, on_satellite()).await?);
        }
        Ok((replies, conversation))
    }

    /// The open dialogs of `conversation`.
    fn open_dialogs(conversation: &Conversation) -> BTreeMap<Key, Dialog> {
        conversation.queues.dialogs().dialogs.clone()
    }

    /// AC-10.5-1.
    #[tokio::test(start_paused = true)]
    async fn ac_10_5_1_the_kitchen_fills_the_area() -> anyhow::Result<()> {
        let home = Arc::new(FakeHome::default());
        let texts = ["turn on the lights", "the kitchen"];
        let (mut replies, conversation) =
            talk(&home, Arc::default(), &texts, &["lights_on"]).await?;
        let after = replies.pop().and_then(|handled| handled.after);
        after.ok_or_else(|| anyhow::anyhow!("no after"))?.await?;
        let lights = ["light.kitchen", "light.strawberry_lamp"];
        let expected = ServiceCall {
            target: lights.map(str::to_owned).to_vec(),
            ..service("homeassistant", "turn_on", "")
        };
        assert_eq!(home.calls(), [expected]);
        assert_eq!(open_dialogs(&conversation), BTreeMap::new());
        Ok(())
    }

    /// AC-10.5c-2.
    #[tokio::test(start_paused = true)]
    async fn ac_10_5c_2_filled_system_two_job_has_the_request_and_area() -> anyhow::Result<()> {
        let reasoner = Arc::new(FakeReasoner::default());
        let texts = ["turn on the fan", "kitchen"];
        let (mut replies, _) =
            talk(&Arc::default(), Arc::clone(&reasoner), &texts, &["fan_on"]).await?;
        let after = replies.pop().and_then(|handled| handled.after);
        after.ok_or_else(|| anyhow::anyhow!("no after"))?.await?;
        assert_eq!(reasoner.questions(), ["turn on the fan in Kitchen"]);
        Ok(())
    }

    /// AC-10.5-3.
    #[tokio::test(start_paused = true)]
    async fn ac_10_5_3_clear_new_request_replaces_the_dialog() -> anyhow::Result<()> {
        let texts = ["turn on the lights", "turn on the fan"];
        let labels = ["lights_on", "fan_on"];
        let (_, conversation) = talk(&Arc::default(), Arc::default(), &texts, &labels).await?;
        let dialog = &open_dialogs(&conversation)[&Key(SATELLITE.to_owned())];
        let fan = matches!(&dialog.pending, Pending::Slot { intent, .. } if intent == "fan_on");
        assert!(fan && dialog.turns == 1, "{dialog:?}");
        Ok(())
    }

    /// AC-10.5-4.
    #[tokio::test(start_paused = true)]
    async fn ac_10_5_4_never_mind_cancels() -> anyhow::Result<()> {
        let home = Arc::new(FakeHome::default());
        let texts = ["turn on the lights", "never mind"];
        let (replies, conversation) = talk(&home, Arc::default(), &texts, &["lights_on"]).await?;
        assert_eq!(replies[1].text, "Cancelled.");
        assert!(replies[1].after.is_none(), "the cancel has work");
        assert_eq!(home.calls(), []);
        assert_eq!(open_dialogs(&conversation), BTreeMap::new());
        Ok(())
    }

    /// AC-10.5-5.
    #[tokio::test(start_paused = true)]
    async fn ac_10_5_5_max_turns_of_unclear_answers_fail() -> anyhow::Result<()> {
        let texts = ["turn on the lights", "zzz", "zzz", "zzz"];
        let labels = ["lights_on", "unknown", "unknown", "unknown"];
        let (replies, conversation) =
            talk(&Arc::default(), Arc::default(), &texts, &labels).await?;
        let texts: Vec<&str> = replies
            .iter()
            .map(|handled| handled.text.as_str())
            .collect();
        assert_eq!(texts, ["", "", "", "Failed."]);
        assert_eq!(open_dialogs(&conversation), BTreeMap::new());
        Ok(())
    }

    /// AC-10.5-7.
    #[tokio::test(start_paused = true)]
    async fn ac_10_5_7_llm_dialog_sends_the_answer_to_system_two() -> anyhow::Result<()> {
        let reasoner = Arc::new(FakeReasoner::default());
        let classifier = scripted(&["unknown"]);
        let (_cache, _changes, conversation) =
            still_with(&Arc::default(), "idle", classifier, Arc::clone(&reasoner));
        open_llm(&conversation);
        let after = conversation.handle("four", on_satellite()).await?.after;
        after.ok_or_else(|| anyhow::anyhow!("no after"))?.await?;
        assert_eq!(reasoner.questions(), ["four"]);
        Ok(())
    }

    /// A conversation from `still_with` whose classifier gives each of `labels` in order, and its
    /// clock at `time`.
    fn at(time: &str, labels: &[&str]) -> (Arc<SetClock>, Conversation) {
        let (_cache, _changes, mut conversation) =
            still_with(&Arc::default(), "idle", scripted(labels), Arc::default());
        let clock = Arc::new(SetClock(Mutex::new(time.parse().expect("a time"))));
        conversation.two.clock = Arc::clone(&clock) as _;
        (clock, conversation)
    }

    /// Monday 08:10 in the HA time zone of the reference house.
    const MONDAY: &str = "2026-01-05T08:10:00-06:00";

    /// The keys and buckets of the habits of `conversation`.
    fn observed(conversation: &Conversation) -> Vec<(String, Bucket)> {
        let habits = conversation.habits.current().unwrap_or_default();
        habits.strengths.into_keys().collect()
    }

    /// The key `intent` in the weekday bucket of `hour`.
    fn weekday(intent: &str, hour: i8) -> (String, Bucket) {
        let day = DayType::Weekdays;
        (intent.to_owned(), Bucket { day, hour })
    }

    /// The context of the kitchen satellite and its device.
    fn in_kitchen() -> Context {
        Context {
            device_id: Some(KITCHEN.to_owned()),
            ..on_satellite()
        }
    }

    /// AC-11.4-1.
    #[tokio::test(start_paused = true)]
    async fn ac_11_4_1_spoken_request_counts_in_its_bucket() -> anyhow::Result<()> {
        let (_clock, conversation) = at(MONDAY, &["lights_on"]);
        let _ = conversation
            .handle("turn on the lights", in_kitchen())
            .await?;
        assert_eq!(observed(&conversation), [weekday("lights_on", 8)]);
        Ok(())
    }

    /// AC-11.4-2.
    #[tokio::test(start_paused = true)]
    async fn ac_11_4_2_request_without_a_satellite_does_not_count() -> anyhow::Result<()> {
        let (_clock, conversation) = at(MONDAY, &["lights_on"]);
        let context = Context {
            satellite_id: None,
            ..in_kitchen()
        };
        let _ = conversation.handle("turn on the lights", context).await?;
        assert_eq!(observed(&conversation), []);
        Ok(())
    }

    /// AC-11.4-3.
    #[tokio::test(start_paused = true)]
    async fn ac_11_4_3_system_two_request_does_not_count() -> anyhow::Result<()> {
        let (_clock, conversation) = at(MONDAY, &["unknown"]);
        let handled = conversation.handle("plan dinner", in_kitchen()).await?;
        assert_eq!(handled.text, "One moment.");
        assert_eq!(observed(&conversation), []);
        Ok(())
    }

    /// AC-11.4-4. The answer comes in the next hour, so a count of it would add a bucket.
    #[tokio::test(start_paused = true)]
    async fn ac_11_4_4_area_answer_does_not_count() -> anyhow::Result<()> {
        let (clock, conversation) = at("2026-01-05T08:59:55-06:00", &["lights_on"]);
        let _ = conversation
            .handle("turn on the lights", on_satellite())
            .await?;
        *clock.0.lock().expect("no lock holder panics") = "2026-01-05T09:00:05-06:00".parse()?;
        let _ = conversation.handle("the kitchen", on_satellite()).await?;
        assert_eq!(open_dialogs(&conversation), BTreeMap::new());
        assert_eq!(observed(&conversation), [weekday("lights_on", 8)]);
        Ok(())
    }

    /// Opens a System Two dialog on the satellite of `conversation` at the Unix epoch.
    fn open_llm(conversation: &Conversation) {
        let pending = Pending::Llm {
            response_id: "response".to_owned(),
            call_id: "call".to_owned(),
        };
        let key = Key(SATELLITE.to_owned());
        conversation
            .queues
            .open(key, pending, Timestamp::UNIX_EPOCH);
    }

    /// AC-10.5b-1.
    #[tokio::test(start_paused = true)]
    async fn ac_10_5b_1_cancel_stops_the_system_two_job() -> anyhow::Result<()> {
        let (home, reasoner) = (Arc::default(), Arc::new(FakeReasoner::default()));
        reasoner.script(vec![Scripted::Sentence("One."), Scripted::Call(call())]);
        let (_cache, _changes, conversation) =
            still_with(&home, "idle", Arc::<FakeClassifier>::default(), reasoner);
        let (filler, job) = (chosen("filler", 0), Job::Llm("plan dinner".to_owned()));
        let job = work(
            &conversation,
            Decision::Defer { filler, job },
            Some(SATELLITE),
        )?;
        open_llm(&conversation);
        let cancelled = conversation.handle("never mind", on_satellite()).await?;
        assert_eq!(cancelled.text, "Cancelled.");
        job.await?;
        assert_eq!(home.calls(), []);
        Ok(())
    }

    /// Runs `after` while the satellite of `cache` is busy for 1 s, then idle, and checks that
    /// `home` asks nothing while it is busy.
    async fn busy_then_idle(
        home: &FakeHome,
        cache: &watch::Sender<StateCache>,
        after: Work,
    ) -> anyhow::Result<()> {
        let idle = async {
            tokio::time::sleep(Duration::from_secs(1)).await;
            assert_eq!(asks(home), [] as [&str; 0], "a question came while busy");
            set(cache, "idle");
        };
        tokio::join!(after, idle).0
    }

    /// AC-10.6-3.
    #[tokio::test(start_paused = true)]
    async fn ac_10_6_3_question_asks_when_idle_and_opens_an_llm_dialog() -> anyhow::Result<()> {
        let (home, reasoner) = (
            Arc::new(FakeHome::default()),
            Arc::new(FakeReasoner::default()),
        );
        reasoner.script(vec![Scripted::Ask("How many people?")]);
        let classifier = scripted(&["unknown"]);
        let (cache, _changes, conversation) = still_with(&home, "responding", classifier, reasoner);
        let handled = conversation.handle("plan dinner", on_satellite()).await?;
        let after = handled.after.ok_or_else(|| anyhow::anyhow!("no after"))?;
        busy_then_idle(&home, &cache, after).await?;
        assert_eq!(asks(&home), ["How many people?"]);
        let dialog = &open_dialogs(&conversation)[&Key(SATELLITE.to_owned())];
        assert!(matches!(dialog.pending, Pending::Llm { .. }), "{dialog:?}");
        Ok(())
    }

    /// A System Two question that fails gives a `failure` announce with the new generation.
    #[tokio::test(start_paused = true)]
    async fn failed_question_announces_failure() -> anyhow::Result<()> {
        let (home, reasoner) = (
            Arc::new(FakeHome::default()),
            Arc::new(FakeReasoner::default()),
        );
        reasoner.script(vec![Scripted::Ask("How many people?")]);
        let classifier = scripted(&["unknown"]);
        let (_cache, _changes, conversation) = still_with(&home, "idle", classifier, reasoner);
        let handled = conversation.handle("plan dinner", on_satellite()).await?;
        drop(home.delay());
        handled
            .after
            .ok_or_else(|| anyhow::anyhow!("no after"))?
            .await?;
        assert_eq!(announces(&home), ["Failed."]);
        Ok(())
    }

    /// Without a satellite, a System Two question is the reply, and its dialog has the key of the
    /// conversation.
    #[tokio::test(start_paused = true)]
    async fn question_without_a_satellite_is_the_reply() -> anyhow::Result<()> {
        let reasoner = Arc::new(FakeReasoner::default());
        reasoner.script(vec![Scripted::Ask("How many people?")]);
        let classifier = scripted(&["unknown"]);
        let (_cache, _changes, conversation) =
            still_with(&Arc::default(), "idle", classifier, reasoner);
        let context = Context {
            conversation_id: Some("chat".to_owned()),
            ..Context::default()
        };
        let handled = conversation.handle("plan dinner", context).await?;
        assert_eq!(handled.text, "How many people?");
        let dialog = &open_dialogs(&conversation)[&Key("chat".to_owned())];
        assert!(matches!(dialog.pending, Pending::Llm { .. }), "{dialog:?}");
        Ok(())
    }

    /// The house, the reasoner and the conversation after "plan dinner" and each of `answers` on
    /// the idle satellite, with the work of each reply done, when the classifier gives `unknown`
    /// and the reasoner gives each of `scripts` in order.
    async fn dinner(
        scripts: Vec<Vec<Scripted>>,
        answers: &[&str],
    ) -> anyhow::Result<(Arc<FakeHome>, Arc<FakeReasoner>, Conversation)> {
        let (home, reasoner) = (
            Arc::new(FakeHome::default()),
            Arc::new(FakeReasoner::default()),
        );
        for steps in scripts {
            reasoner.script(steps);
        }
        let classifier = scripted(&vec!["unknown"; answers.len() + 1]);
        let (_cache, _changes, conversation) =
            still_with(&home, "idle", classifier, Arc::clone(&reasoner));
        let texts = ["plan dinner"].into_iter().chain(answers.iter().copied());
        for text in texts {
            if let Some(after) = conversation.handle(text, on_satellite()).await?.after {
                after.await?;
            }
        }
        Ok((home, reasoner, conversation))
    }

    /// AC-10.6b-3.
    #[tokio::test(start_paused = true)]
    async fn ac_10_6b_3_answer_gives_a_filler_before_system_two() -> anyhow::Result<()> {
        let reasoner = Arc::new(FakeReasoner::default());
        let classifier = scripted(&["unknown"]);
        let (_cache, _changes, conversation) =
            still_with(&Arc::default(), "idle", classifier, Arc::clone(&reasoner));
        open_llm(&conversation);
        let handled = conversation.handle("four", on_satellite()).await?;
        assert_eq!(handled.text, "One moment.");
        assert_eq!(
            reasoner.questions(),
            [] as [&str; 0],
            "System Two came first"
        );
        handled
            .after
            .ok_or_else(|| anyhow::anyhow!("no after"))?
            .await?;
        assert_eq!(reasoner.answered(), [Some("call".to_owned())]);
        Ok(())
    }

    /// AC-10.6b-4.
    #[tokio::test(start_paused = true)]
    async fn ac_10_6b_4_three_questions_then_the_answer() -> anyhow::Result<()> {
        let questions = ["How many?", "When?", "Where?"];
        let mut scripts: Vec<Vec<Scripted>> =
            questions.map(|text| vec![Scripted::Ask(text)]).into();
        scripts.push(vec![Scripted::Sentence("Dinner is planned.")]);
        let (home, reasoner, _) = dinner(scripts, &["four", "seven", "home"]).await?;
        let calls = home.calls();
        let services: Vec<&str> = calls.iter().map(|call| call.service.as_str()).collect();
        let ask = "start_conversation";
        assert_eq!(services, [ask, ask, ask, "announce"]);
        assert_eq!(asks(&home), questions);
        assert_eq!(announces(&home), ["Dinner is planned."]);
        let answered = questions.map(|text| Some(text.to_owned()));
        assert_eq!(reasoner.answered()[1..], answered);
        Ok(())
    }

    /// AC-10.6b-5.
    #[tokio::test(start_paused = true)]
    async fn ac_10_6b_5_more_than_max_turns_questions_fail() -> anyhow::Result<()> {
        let scripts = (0..4).map(|_| vec![Scripted::Ask("How many?")]).collect();
        let (home, _, conversation) = dinner(scripts, &["four", "four", "four"]).await?;
        assert_eq!(asks(&home).len(), 3);
        assert_eq!(announces(&home), ["Failed."]);
        assert_eq!(open_dialogs(&conversation), BTreeMap::new());
        Ok(())
    }

    /// AC-10.6-7.
    #[tokio::test(start_paused = true)]
    async fn ac_10_6_7_cancel_ends_the_system_two_dialog() -> anyhow::Result<()> {
        let scripts = vec![vec![Scripted::Ask("How many?")]];
        let (home, reasoner, conversation) = dinner(scripts, &["cancel"]).await?;
        assert_eq!(reasoner.questions(), ["plan dinner"]);
        assert_eq!(asks(&home), ["How many?"]);
        assert_eq!(open_dialogs(&conversation), BTreeMap::new());
        Ok(())
    }

    /// A System Two job on the satellite in the state `value` whose reasoner gives `steps`: its
    /// house, hot cache, reasoner and work.
    fn reasoning(value: &str, steps: Vec<Scripted>) -> anyhow::Result<Reasoning> {
        plugged(value, steps, Vec::new())
    }

    /// The plugin registry of `plugins` with the `[plugins]` of `jarvis.toml` at the Unix epoch.
    fn registry(plugins: Vec<Arc<dyn Plugin>>) -> Arc<Plugins> {
        let config = crate::Config::parse(include_str!("../../jarvis.toml")).expect("valid");
        let clock = Arc::new(FakeClock(Timestamp::UNIX_EPOCH));
        Arc::new(Plugins::new(config.plugins, clock, plugins))
    }

    /// `reasoning` with the plugin registry of `plugins`.
    fn plugged(
        value: &str,
        steps: Vec<Scripted>,
        plugins: Vec<Arc<dyn Plugin>>,
    ) -> anyhow::Result<Reasoning> {
        let (home, reasoner) = (Arc::default(), Arc::new(FakeReasoner::default()));
        reasoner.script(steps);
        let classifier = Arc::new(FakeClassifier::default());
        let (cache, _, mut conversation) =
            still_with(&home, value, classifier, Arc::clone(&reasoner));
        conversation.two.plugins = registry(plugins);
        let (filler, job) = (chosen("filler", 0), Job::Llm("tell me".to_owned()));
        let defer = Decision::Defer { filler, job };
        Ok((
            home,
            cache,
            reasoner,
            work(&conversation, defer, Some(SATELLITE))?,
        ))
    }

    /// AC-7.5-2.
    #[tokio::test(start_paused = true)]
    async fn ac_7_5_2_two_sentences_give_two_announces_in_order() -> anyhow::Result<()> {
        let steps = vec![Scripted::Sentence("One."), Scripted::Sentence("Two.")];
        let (home, _cache, _, work) = reasoning("idle", steps)?;
        work.await?;
        assert_eq!(announces(&home), ["One.", "Two."]);
        Ok(())
    }

    /// AC-7.5-3.
    #[tokio::test(start_paused = true)]
    async fn ac_7_5_3_first_announce_starts_margin_after_idle() -> anyhow::Result<()> {
        let (home, cache, _, work) = reasoning("responding", vec![Scripted::Sentence("One.")])?;
        let (start, busy_for) = (Instant::now(), Duration::from_millis(1500));
        let idle = async {
            tokio::time::sleep(busy_for).await;
            set(&cache, "idle");
        };
        tokio::join!(work, idle).0?;
        assert!(
            Instant::now() - start >= busy_for + MARGIN,
            "it started early"
        );
        assert_eq!(announces(&home), ["One."]);
        Ok(())
    }

    /// AC-7.5-5.
    #[tokio::test(start_paused = true)]
    async fn ac_7_5_5_answer_error_announces_failure() -> anyhow::Result<()> {
        let (home, _cache, _, work) = reasoning("idle", vec![Scripted::Error("broken")])?;
        work.await?;
        assert_eq!(announces(&home), ["Failed."]);
        Ok(())
    }

    /// AC-7.5-9.
    #[tokio::test(start_paused = true)]
    async fn ac_7_5_9_sentence_is_announced_while_the_reasoner_waits() -> anyhow::Result<()> {
        let wait = Duration::from_secs(1);
        let steps = vec![Scripted::Sentence("One."), Scripted::Wait(wait)];
        let (home, _cache, _, work) = reasoning("idle", steps)?;
        let waiting = async {
            tokio::time::sleep(wait / 2).await;
            assert_eq!(announces(&home), ["One."], "no announce during the wait");
        };
        tokio::join!(work, waiting).0
    }

    /// AC-7.5-10.
    #[tokio::test(start_paused = true)]
    async fn ac_7_5_10_no_step_within_timeout_announces_failure() -> anyhow::Result<()> {
        let start = Instant::now();
        let (home, _cache, _, work) = reasoning("idle", vec![Scripted::Wait(TIMEOUT * 2)])?;
        work.await?;
        assert_eq!(Instant::now() - start, TIMEOUT + MARGIN, "no timeout");
        assert_eq!(announces(&home), ["Failed."]);
        Ok(())
    }

    /// AC-7.5b-1.
    #[tokio::test(start_paused = true)]
    async fn ac_7_5b_1_call_reaches_home_before_the_next_step() -> anyhow::Result<()> {
        let steps = vec![Scripted::Call(call()), Scripted::Sentence("One.")];
        let (home, _cache, _, work) = reasoning("idle", steps)?;
        let gate = home.delay();
        let waiting = async {
            tokio::time::sleep(Duration::from_secs(1)).await;
            assert_eq!(home.calls(), [call()], "a step came before the call ended");
            gate.send(())
        };
        let (done, sent) = tokio::join!(work, waiting);
        done?;
        sent.map_err(|()| anyhow::anyhow!("the call ended early"))?;
        assert_eq!(home.calls()[0], call());
        assert_eq!(announces(&home), ["One."]);
        Ok(())
    }

    /// The `openai.allowed_domains` of the tests.
    const ALLOWED: [&str; 6] = [
        "light",
        "switch",
        "fan",
        "media_player",
        "climate",
        "calendar",
    ];

    /// Makes sure that the System Two `call` makes no call and gets "not permitted".
    async fn not_permitted(call: ServiceCall) -> anyhow::Result<()> {
        let (home, _cache, reasoner, work) = reasoning("idle", vec![Scripted::Call(call)])?;
        work.await?;
        assert_eq!(home.calls(), []);
        assert_eq!(reasoner.results(), ["not permitted"]);
        Ok(())
    }

    /// AC-7.5b-2.
    #[tokio::test(start_paused = true)]
    async fn ac_7_5b_2_lock_domain_is_not_permitted() -> anyhow::Result<()> {
        not_permitted(service("lock", "unlock", "lock.front_door")).await
    }

    /// AC-7.5b-3.
    #[tokio::test(start_paused = true)]
    async fn ac_7_5b_3_lock_target_is_not_permitted() -> anyhow::Result<()> {
        not_permitted(service("light", "turn_on", "lock.front_door")).await
    }

    /// A newer request on the satellite stops the calls of an older answer.
    #[tokio::test(start_paused = true)]
    async fn newer_request_stops_the_calls_of_an_older_answer() -> anyhow::Result<()> {
        let (home, reasoner) = (
            Arc::new(FakeHome::default()),
            Arc::new(FakeReasoner::default()),
        );
        reasoner.script(vec![Scripted::Call(call())]);
        let classifier = Arc::new(FakeClassifier::default());
        let (_cache, _changes, conversation) = still_with(&home, "idle", classifier, reasoner);
        let (filler, job) = (chosen("filler", 0), Job::Llm("tell me".to_owned()));
        let old = work(
            &conversation,
            Decision::Defer { filler, job },
            Some(SATELLITE),
        )?;
        let _ = conversation.dispatch(reply(None), None, Some(SATELLITE));
        old.await?;
        assert_eq!(home.calls(), []);
        Ok(())
    }

    /// A target key in the data would add targets that the checks do not see.
    #[tokio::test(start_paused = true)]
    async fn target_key_in_data_is_not_permitted() -> anyhow::Result<()> {
        let mut call = call();
        call.data.insert("area_id".to_owned(), "kitchen".into());
        not_permitted(call).await
    }

    /// Adds `entity` to `cache` with the state of the satellite.
    fn add(cache: &watch::Sender<StateCache>, entity: &str) {
        cache.send_modify(|cache| {
            let mut state = State::clone(&cache.states[SATELLITE]);
            state.entity_id = entity.to_owned();
            cache.states.insert(Arc::from(entity), Arc::new(state));
        });
    }

    /// AC-7.5b-4.
    #[tokio::test(start_paused = true)]
    async fn ac_7_5b_4_read_service_returns_response_data() -> anyhow::Result<()> {
        let events = service("calendar", "get_events", "calendar.alex");
        let light = ServiceCall {
            return_response: true,
            ..call()
        };
        let steps = vec![Scripted::Call(events.clone()), Scripted::Call(light)];
        let (home, cache, reasoner, work) = reasoning("idle", steps)?;
        add(&cache, "calendar.alex");
        let data = serde_json::json!({ "calendar.alex": { "events": [] } });
        home.respond(data.clone());
        work.await?;
        let read = ServiceCall {
            return_response: true,
            ..events
        };
        assert_eq!(home.calls(), [read, call()]);
        let result = format!("{:?}", Ok::<_, ()>(Some(Ok::<_, ()>(Some(data)))));
        assert_eq!(reasoner.results()[0], result);
        Ok(())
    }

    /// AC-7.6-4.
    #[tokio::test(start_paused = true)]
    async fn ac_7_6_4_states_come_from_the_hot_cache() -> anyhow::Result<()> {
        let steps = vec![Scripted::States("Garage"), Scripted::States("attic")];
        let (home, _cache, reasoner, work) = reasoning("idle", steps)?;
        work.await?;
        assert_eq!(home.calls(), []);
        let light = serde_json::json!([
            { "name": "Garage Light", "entity_id": "switch.garage_light", "state": "off" },
        ]);
        let states = format!("{:?}", Ok::<_, ()>(Some(Ok::<_, ()>(Some(light)))));
        assert_eq!(reasoner.results(), [states, "unknown area".to_owned()]);
        Ok(())
    }

    /// The test plugin with `result` scripted, and a System Two job on an idle satellite whose
    /// reasoner gives a call of it with `when = today` and then `more`.
    fn plugin_job(
        result: anyhow::Result<Value>,
        more: Vec<Scripted>,
    ) -> anyhow::Result<(Arc<FakePlugin>, Reasoning)> {
        let plugin = Arc::new(FakePlugin::new("get_test"));
        plugin.script(result);
        let mut steps = vec![Scripted::Plugin(today("get_test"))];
        steps.extend(more);
        let reasoning = plugged("idle", steps, vec![Arc::clone(&plugin) as Arc<dyn Plugin>])?;
        Ok((plugin, reasoning))
    }

    /// AC-17.2-4.
    #[tokio::test(start_paused = true)]
    async fn ac_17_2_4_plugin_step_fetches_with_the_hot_cache_config() -> anyhow::Result<()> {
        let result = serde_json::json!({ "high": 72 });
        let (plugin, (home, _cache, reasoner, work)) = plugin_job(Ok(result.clone()), Vec::new())?;
        work.await?;
        let fetches = plugin.fetches();
        assert_eq!(fetches.len(), 1, "{fetches:?}");
        assert_eq!(fetches[0].1, house().config);
        assert_eq!(home.calls(), []);
        let sent = format!("{:?}", Ok::<_, ()>(Some(Ok::<_, ()>(Some(result)))));
        assert_eq!(reasoner.results(), [sent]);
        Ok(())
    }

    /// AC-17.2-5.
    #[tokio::test(start_paused = true)]
    async fn ac_17_2_5_plugin_error_is_the_result_and_the_answer_goes_on() -> anyhow::Result<()> {
        let error = Err(anyhow::anyhow!("server down"));
        let more = vec![Scripted::Sentence("One.")];
        let (_plugin, (home, _cache, reasoner, work)) = plugin_job(error, more)?;
        work.await?;
        assert_eq!(reasoner.results(), ["server down"]);
        assert_eq!(announces(&home), ["One."]);
        Ok(())
    }

    /// The text of `handle` for `text` on `home` from the kitchen device with no satellite, when
    /// the classifier gives `label`, the reasoner gives `steps` and `get_weather` gives
    /// `{ sky: sunny }`.
    async fn typed(
        label: &str,
        text: &str,
        home: &Arc<FakeHome>,
        steps: Vec<Scripted>,
    ) -> anyhow::Result<String> {
        let reasoner = Arc::new(FakeReasoner::default());
        reasoner.script(steps);
        let (_cache, _changes, mut conversation) =
            still_with(home, "idle", classifier(Some(label)), reasoner);
        weather(&mut conversation, Ok(serde_json::json!({ "sky": "sunny" })));
        let context = Context {
            device_id: Some(KITCHEN.to_owned()),
            ..Context::default()
        };
        Ok(conversation.handle(text, context).await?.text)
    }

    /// AC-7.5c-1.
    #[tokio::test(start_paused = true)]
    async fn ac_7_5c_1_no_satellite_returns_the_sentences() -> anyhow::Result<()> {
        let home = Arc::new(FakeHome::default());
        let steps = vec![Scripted::Sentence("One."), Scripted::Sentence("Two.")];
        let text = typed("unknown", "tell me a joke", &home, steps).await?;
        assert_eq!(text, "One. Two.");
        assert_eq!(announces(&home), [] as [&str; 0]);
        Ok(())
    }

    /// AC-7.5c-2.
    #[tokio::test(start_paused = true)]
    async fn ac_7_5c_2_no_satellite_returns_the_fetch_answer() -> anyhow::Result<()> {
        let home = Arc::new(FakeHome::default());
        let text = typed("weather", "what is the weather like", &home, Vec::new()).await?;
        assert_eq!(text, "It is sunny.");
        Ok(())
    }

    /// The announces after a conversation with an idle satellite and the catalog and pools of
    /// `intents.toml` and `responses.toml` handles `text` from the kitchen satellite, when the
    /// classifier gives `label` and the plugin `plugin` gives `result`.
    async fn announced(
        label: &str,
        plugin: &str,
        result: Value,
        text: &str,
    ) -> anyhow::Result<Vec<String>> {
        let home = Arc::new(FakeHome::default());
        let classifier = classifier(Some(label));
        let (_cache, _changes, mut conversation) =
            still_with(&home, "idle", classifier, Arc::default());
        with_plugin(&mut conversation, plugin, Ok(result));
        spoken(conversation, text).await?;
        Ok(announces(&home))
    }

    /// The reply after `conversation` with the pools of `responses.toml` handles `text` from the
    /// kitchen satellite and its work ends.
    async fn spoken(mut conversation: Conversation, text: &str) -> anyhow::Result<String> {
        let catalog = crate::load_catalog(Path::new("intents.toml"))?;
        let pools = crate::load_pools(Path::new("responses.toml"), &catalog)?;
        conversation.pools = Arc::new(ArcSwap::from_pointee(pools));
        let context = Context {
            device_id: Some(KITCHEN.to_owned()),
            satellite_id: Some(SATELLITE.to_owned()),
            ..Context::default()
        };
        let Handled { text, after } = conversation.handle(text, context).await?;
        after.ok_or_else(|| anyhow::anyhow!("no after"))?.await?;
        Ok(text)
    }

    /// AC-17.4-5.
    #[tokio::test(start_paused = true)]
    async fn ac_17_4_5_weather_announces_the_forecast() -> anyhow::Result<()> {
        let forecast = serde_json::json!({
            "condition": "partly cloudy", "high": 72, "low": 54, "precipitation_chance": 20
        });
        let text = "what is the weather like";
        let announces = announced("weather", "get_weather", forecast, text).await?;
        let answer = "Today will be partly cloudy, with a high of 72 degrees.";
        assert_eq!(announces, [answer]);
        Ok(())
    }

    /// The announces of a `new_shows` request when `get_new_shows` gives `added` and `on_deck`.
    async fn new_shows(added: &[&str], on_deck: &[&str]) -> anyhow::Result<Vec<String>> {
        let shows = serde_json::json!({ "added": added, "on_deck": on_deck });
        let text = "are there any new shows";
        announced("new_shows", "get_new_shows", shows, text).await
    }

    /// AC-18.12-2.
    #[tokio::test(start_paused = true)]
    async fn ac_18_12_2_new_shows_announces_the_added_and_continued_shows() -> anyhow::Result<()> {
        let answer = "New on Plex: Bluey. You can continue The Office, Severance.";
        let announces = new_shows(&["Bluey"], &["The Office", "Severance"]).await?;
        assert_eq!(announces, [answer]);
        Ok(())
    }

    /// AC-18.12-3.
    #[tokio::test(start_paused = true)]
    async fn ac_18_12_3_no_new_shows_announces_nothing_new() -> anyhow::Result<()> {
        let answer = "Nothing new was added to Plex in the last day.";
        assert_eq!(new_shows(&[], &[]).await?, [answer]);
        Ok(())
    }

    /// The speaker of the kitchen satellite.
    const SPEAKER: &str = "media_player.carson_kitchen";

    /// The config entry of Music Assistant in the tests.
    const ENTRY: Option<&str> = Some("music_assistant");

    /// The reply and the calls of `home` after a conversation with the catalog of `intents.toml`
    /// handles "play `media`" from the kitchen satellite, when the classifier gives `label` with
    /// the slot `media` and [`SPEAKER`] has the config entry `entry`.
    async fn played(
        home: &Arc<FakeHome>,
        label: &str,
        media: &str,
        entry: Option<&str>,
    ) -> anyhow::Result<(String, Vec<ServiceCall>)> {
        let classifier = Arc::new(FakeClassifier::default());
        let (name, value) = ("media".to_owned(), media.to_owned());
        let (ranked, slots) = (vec![(label.to_owned(), 0.9)], vec![Slot { name, value }]);
        classifier.script(Prediction { ranked, slots });
        let (cache, _changes, conversation) = still_with(home, "idle", classifier, Arc::default());
        cache.send_modify(|cache| set_entry(cache, entry));
        let reply = spoken(conversation, &format!("play {media}")).await?;
        Ok((reply, home.calls()))
    }

    /// Sets the config entry of [`SPEAKER`] in `cache` to `entry`.
    fn set_entry(cache: &mut StateCache, entry: Option<&str>) {
        let mut entities = Arc::make_mut(&mut cache.entities).iter_mut();
        let speaker = entities.find(|entity| entity.entity_id.0 == SPEAKER);
        speaker.expect("the house has it").config_entry_id = entry.map(str::to_owned);
    }

    /// The `music_assistant` call of `service` on `target` with `data`, with the response data of
    /// a search.
    fn music(service: &str, target: &[&str], data: Value) -> ServiceCall {
        ServiceCall {
            domain: "music_assistant".to_owned(),
            service: service.to_owned(),
            target: target.iter().map(|&entity| entity.to_owned()).collect(),
            data: serde_json::from_value(data).expect("the data is an object"),
            return_response: service == "search",
        }
    }

    /// The search with `data` and the `play_media` call of [`SPEAKER`] with the `uri` and the
    /// `media_type` of `item`.
    fn searched(data: Value, item: &Value) -> [ServiceCall; 2] {
        let play = json!({ "media_id": item["uri"], "media_type": item["media_type"] });
        [
            music("search", &[], data),
            music("play_media", &[SPEAKER], play),
        ]
    }

    /// AC-18.13-5.
    #[tokio::test(start_paused = true)]
    async fn ac_18_13_5_play_music_plays_the_library_playlist_of_the_name() -> anyhow::Result<()> {
        let home = Arc::new(FakeHome::default());
        let hip_hop =
            json!({ "media_type": "playlist", "uri": "library://playlist/82", "name": "Hip Hop" });
        home.respond_next(json!({ "playlists": [hip_hop] }));
        let (reply, calls) = played(&home, "play_music", "hip hop", ENTRY).await?;
        assert_eq!(reply, "Playing hip hop.");
        let search = json!({ "config_entry_id": "music_assistant", "name": "hip hop",
            "media_type": ["playlist"], "library_only": true });
        assert_eq!(calls, searched(search, &hip_hop));
        Ok(())
    }

    /// AC-18.13-6.
    #[tokio::test(start_paused = true)]
    async fn ac_18_13_6_no_playlist_plays_the_artist_of_the_name() -> anyhow::Result<()> {
        let home = Arc::new(FakeHome::default());
        let uri = "spotify--x7LaiKmM://artist/4Z8W4fKeB5YxbusRsdQVPb";
        let radiohead = json!({ "media_type": "artist", "uri": uri, "name": "Radiohead" });
        home.respond_next(json!({ "playlists": [] }));
        home.respond_next(json!({ "artists": [radiohead] }));
        let (_, calls) = played(&home, "play_music", "radiohead", ENTRY).await?;
        let search = json!({ "config_entry_id": "music_assistant", "name": "radiohead",
            "media_type": ["artist", "album", "track"] });
        assert_eq!(calls[1..], searched(search, &radiohead));
        Ok(())
    }

    /// AC-18.13-7.
    #[tokio::test(start_paused = true)]
    async fn ac_18_13_7_no_match_failed_search_or_no_entry_plays_the_name() -> anyhow::Result<()> {
        let unmatched = Arc::new(FakeHome::default());
        let creep = json!({ "media_type": "track", "uri": "library://track/1", "name": "Creep" });
        unmatched.respond(json!({ "tracks": [creep] }));
        let (failed, garbled) = (Arc::new(FakeHome::default()), Arc::new(FakeHome::default()));
        drop(failed.delay());
        garbled.respond(json!("busy"));
        let unconfigured = (Arc::new(FakeHome::default()), None);
        let homes = [
            (unmatched, ENTRY),
            (failed, ENTRY),
            (garbled, ENTRY),
            unconfigured,
        ];
        for (home, entry) in homes {
            plays_the_name(&home, entry).await?;
        }
        Ok(())
    }

    /// Checks that the last call of `home` after "play radiohead" with `entry` plays the name.
    async fn plays_the_name(home: &Arc<FakeHome>, entry: Option<&str>) -> anyhow::Result<()> {
        let play = music("play_media", &[SPEAKER], json!({ "media_id": "radiohead" }));
        let (_, calls) = played(home, "play_music", "radiohead", entry).await?;
        assert_eq!(calls.last(), Some(&play), "{entry:?}");
        Ok(())
    }

    /// AC-18.13-8.
    #[tokio::test(start_paused = true)]
    async fn ac_18_13_8_radio_and_queue_make_no_search() -> anyhow::Result<()> {
        for label in ["play_radio", "queue_music"] {
            let home = Arc::new(FakeHome::default());
            let (_, calls) = played(&home, label, "radiohead", ENTRY).await?;
            let services: Vec<&str> = calls.iter().map(|call| call.service.as_str()).collect();
            assert_eq!(services, ["play_media"], "{label}");
        }
        Ok(())
    }

    /// The calls after the searches of "play `media`", when the second search gives the artist
    /// `artist` at `uri` and `track`.
    async fn artist_played(
        media: &str,
        artist: &str,
        uri: &str,
        track: Value,
    ) -> anyhow::Result<Vec<ServiceCall>> {
        let home = Arc::new(FakeHome::default());
        let artist = json!({ "media_type": "artist", "uri": uri, "name": artist });
        home.respond_next(json!({ "playlists": [] }));
        home.respond_next(json!({ "artists": [artist], "tracks": [track] }));
        let (_, calls) = played(&home, "play_music", media, ENTRY).await?;
        Ok(calls[2..].to_vec())
    }

    /// The track `name` at `uri` by the artist `artist`.
    fn track(name: &str, uri: &str, artist: &str) -> Value {
        let by = json!({ "media_type": "artist", "uri": "spotify--x7LaiKmM://artist/1", "name": artist });
        json!({ "media_type": "track", "uri": uri, "name": name, "artists": [by] })
    }

    /// AC-18.14-1.
    #[tokio::test(start_paused = true)]
    async fn ac_18_14_1_artist_plays_its_track_then_queues_the_artist() -> anyhow::Result<()> {
        let uri = "spotify--x7LaiKmM://track/63OQupATfueTdZMWTxW03A";
        let karma_police = track("Karma Police", uri, "Radiohead");
        let calls = artist_played("radiohead", "Radiohead", "library://artist/5", karma_police);
        let track = json!({ "media_id": uri, "media_type": "track" });
        let artist =
            json!({ "media_id": "library://artist/5", "media_type": "artist", "enqueue": "add" });
        let plays = [track, artist].map(|data| music("play_media", &[SPEAKER], data));
        assert_eq!(calls.await?, plays);
        Ok(())
    }

    /// AC-18.14-2.
    #[tokio::test(start_paused = true)]
    async fn ac_18_14_2_artist_without_its_track_plays_the_artist() -> anyhow::Result<()> {
        let anthem = track("The National Anthem", "library://track/2", "Radiohead");
        let uri = "library://artist/7";
        let calls = artist_played("the national", "The National", uri, anthem).await?;
        let artist = json!({ "media_id": uri, "media_type": "artist" });
        assert_eq!(calls, [music("play_media", &[SPEAKER], artist)]);
        Ok(())
    }

    /// The model state that the conformance test compares.
    #[derive(Debug, Deserialize, PartialEq)]
    struct Announce {
        /// The started announces: request, sentence and the newest request at the start.
        started: Vec<(i64, i64, i64)>,
    }

    impl ModelState<AnnounceDriver> for Announce {
        /// The announces after every task waits. `NoStaleAnnounce` makes the newest request at
        /// the start the request of the announce.
        fn from_driver(driver: &AnnounceDriver) -> anyhow::Result<Self> {
            advance(&driver.runtime, Duration::from_millis(1));
            let started = announces(&driver.home).into_iter().map(|text| {
                let (request, sentence) = text.split_once(' ').expect("the text is \"r s\"");
                let request = request.parse()?;
                Ok((request, sentence.parse()?, request))
            });
            Ok(Self {
                started: started.collect::<anyhow::Result<_>>()?,
            })
        }
    }

    /// A current-thread runtime with time and a paused clock.
    fn paused() -> Runtime {
        let mut runtime = tokio::runtime::Builder::new_current_thread();
        let runtime = runtime.enable_time().start_paused(true).build();
        runtime.expect("the runtime builds")
    }

    /// Replays `announce.qnt` against the queue of one satellite, with `FakeHome` as HA.
    struct AnnounceDriver {
        /// The paused runtime that runs the items.
        runtime: Runtime,
        /// HA.
        home: Arc<FakeHome>,
        /// The hot cache.
        cache: watch::Sender<StateCache>,
        /// The queues.
        queues: Arc<Queues>,
        /// The generation of each request.
        generations: BTreeMap<i64, u64>,
    }

    impl AnnounceDriver {
        /// A driver with an idle satellite, on a new runtime.
        fn new() -> Self {
            let home = Arc::new(FakeHome::default());
            let (cache, queues) = queues(&home, "idle");
            Self {
                runtime: paused(),
                home,
                cache,
                queues: Arc::new(queues),
                generations: BTreeMap::new(),
            }
        }
    }

    impl Driver for AnnounceDriver {
        type State = Announce;

        fn step(&mut self, step: &Step) -> anyhow::Result<()> {
            switch!(step {
                init => *self = Self::new(),
                request(r: i64) => {
                    self.generations.insert(r, self.queues.next_generation(SATELLITE));
                    set(&self.cache, "responding");
                },
                pipelineEnd => set(&self.cache, "idle"),
                arrive(r: i64, s: i64) => {
                    // Generations start at 1, so a request that never started has an older one.
                    let generation = self.generations.get(&r).copied().unwrap_or(0);
                    let (queues, text) = (Arc::clone(&self.queues), format!("{r} {s}"));
                    self.runtime.spawn(async move { queues.announce(SATELLITE, generation, &text).await });
                },
                start => advance(&self.runtime, MARGIN),
                finish => {},
            })
        }
    }

    /// AC-4.9b-2. `quint_run` replays 100 traces by default.
    #[quint_run(spec = "quint/announce.qnt", max_steps = 30, seed = "1")]
    fn ac_4_9b_2_conformance() -> impl Driver {
        AnnounceDriver::new()
    }

    /// The satellites of `dialog.qnt`.
    const KEYS: [&str; 2] = ["s1", "s2"];

    /// The generations and the calls of `dialog.qnt`.
    #[derive(Debug, Deserialize, PartialEq)]
    struct Jobs {
        /// The generation of each key.
        #[serde(rename = "gen")]
        generations: BTreeMap<String, u64>,
        /// True for each request that acted.
        calls: BTreeMap<i64, bool>,
    }

    impl ModelState<JobDriver> for Jobs {
        /// The generation of each satellite without the extra generations of the System Two
        /// questions again, and the requests whose calls `FakeHome` recorded.
        fn from_driver(driver: &JobDriver) -> anyhow::Result<Self> {
            let queues = &driver.conversation.queues;
            let generation =
                |key: &str| queues.generation(key) - driver.extra.get(key).copied().unwrap_or(0);
            let generations = KEYS.map(|key| (key.to_owned(), generation(key)));
            let calls = (1..=3).map(|request| (request, driver.calls.contains(&request)));
            Ok(Self {
                generations: generations.into(),
                calls: calls.collect(),
            })
        }
    }

    /// Replays `dialog.qnt` against `handle` on two satellites, with `FakeHome` as HA.
    struct JobDriver {
        /// The paused runtime that runs `handle` and the jobs.
        runtime: Runtime,
        /// HA.
        home: Arc<FakeHome>,
        /// The System One model.
        classifier: Arc<FakeClassifier>,
        /// System Two.
        reasoner: Arc<FakeReasoner>,
        /// The clock of the dialogs.
        clock: Arc<SetClock>,
        /// The use case.
        conversation: Conversation,
        /// The number of requests so far, which is the ID of the last request.
        requests: i64,
        /// The request of the dialog of each satellite.
        request: BTreeMap<String, i64>,
        /// The satellite and the work of the deferred job of each request.
        jobs: BTreeMap<i64, (String, Work)>,
        /// The requests whose calls `FakeHome` recorded.
        calls: BTreeSet<i64>,
        /// The generations of each satellite that `askAgain` of the model does not have.
        extra: BTreeMap<String, u64>,
    }

    impl JobDriver {
        /// A driver with no dialogs, on a new runtime, with a dialog timeout of 3 s.
        fn new() -> Self {
            let (home, reasoner) = (Arc::default(), Arc::new(FakeReasoner::default()));
            let (_cache, mut queues) = queues(&home, "idle");
            queues.dialog.timeout_s = 3;
            let classifier = Arc::new(FakeClassifier::default());
            let pools = Arc::new(ArcSwap::from_pointee(unused()));
            let changes = mpsc::channel().0;
            let reasoning = Arc::clone(&reasoner);
            let classifying = Arc::clone(&classifier) as _;
            let mut conversation =
                conversation(pools, changes, usize::MAX, queues, classifying, reasoning);
            let clock = Arc::new(SetClock(Mutex::new(Timestamp::UNIX_EPOCH)));
            conversation.two.clock = Arc::clone(&clock) as _;
            Self {
                runtime: paused(),
                home,
                classifier,
                reasoner,
                clock,
                conversation,
                requests: 0,
                request: BTreeMap::new(),
                jobs: BTreeMap::new(),
                calls: BTreeSet::new(),
                extra: BTreeMap::new(),
            }
        }

        /// The work of `handle` for `text` on `satellite` when the classifier gives `label`.
        fn handle(&self, satellite: &str, text: &str, label: Option<&str>) -> Option<Work> {
            if let Some(label) = label {
                let ranked = vec![(label.to_owned(), 0.9)];
                let slots = Vec::new();
                self.classifier.script(Prediction { ranked, slots });
            }
            let context = Context {
                satellite_id: Some(satellite.to_owned()),
                ..Context::default()
            };
            let handled = self
                .runtime
                .block_on(self.conversation.handle(text, context));
            handled.expect("the prediction is scripted").after
        }

        /// A new request on `satellite` whose decision is a question, for `request`.
        fn ask(&mut self, satellite: String, request: i64) {
            let _ = self.handle(&satellite, "turn on the lights", Some("lights_on"));
            self.request.insert(satellite, request);
        }

        /// A new request on `satellite` whose decision is a System Two job.
        fn defer(&mut self, satellite: String) {
            self.requests += 1;
            let work = self.handle(&satellite, "tell me a joke", Some("removed"));
            self.jobs
                .insert(self.requests, (satellite, work.expect("a job")));
        }

        /// Runs the job of `request`, whose answer has one call. A job that asked has ended.
        fn act_job(&mut self, request: i64) -> anyhow::Result<()> {
            let Some((_, work)) = self.jobs.remove(&request) else {
                return Ok(());
            };
            self.reasoner.script(vec![Scripted::Call(call())]);
            self.act(Some(work), request)
        }

        /// Runs the job of `request`, whose answer asks. The answer of its dialog continues
        /// `request`.
        fn ask_job(&mut self, request: i64) -> anyhow::Result<()> {
            let (satellite, work) = self.jobs.remove(&request).expect("the job runs");
            self.reasoner.script(vec![Scripted::Ask("How many?")]);
            self.runtime.block_on(work)?;
            self.request.insert(satellite, request);
            Ok(())
        }

        /// True when the dialog of `satellite` waits for System Two.
        fn waits_for_llm(&self, satellite: &str) -> bool {
            let dialogs = self.conversation.queues.dialogs();
            let dialog = dialogs.dialogs.get(&Key(satellite.to_owned()));
            dialog.is_some_and(|dialog| matches!(dialog.pending, Pending::Llm { .. }))
        }

        /// The work of the answer `text` on `satellite`: to System Two, whose next answer gives
        /// `steps`, when the dialog waits for it, else to System One with `label`.
        fn answer(
            &self,
            satellite: &str,
            text: &str,
            label: Option<&str>,
            steps: Vec<Scripted>,
        ) -> Option<Work> {
            if !self.waits_for_llm(satellite) {
                return self.handle(satellite, text, label);
            }
            self.reasoner.script(steps);
            self.handle(satellite, text, Some("unknown"))
        }

        /// An unclear answer on `satellite`. A System Two answer asks again, which is a new
        /// request and a new question: two generations that `askAgain` of the model does not have.
        fn ask_again(&mut self, satellite: &str) -> anyhow::Result<()> {
            if self.waits_for_llm(satellite) {
                *self.extra.entry(satellite.to_owned()).or_default() += 2;
            }
            let ask = vec![Scripted::Ask("How many?")];
            if let Some(work) = self.answer(satellite, "zzz", Some("unknown"), ask) {
                self.runtime.block_on(work)?;
            }
            Ok(())
        }

        /// Runs `work`, and marks `request` when `FakeHome` records a call that is not an
        /// announce or a question.
        fn act(&mut self, work: Option<Work>, request: i64) -> anyhow::Result<()> {
            let calls = |home: &FakeHome| {
                let calls = home.calls();
                calls
                    .iter()
                    .filter(|call| call.domain != "assist_satellite")
                    .count()
            };
            let before = calls(&self.home);
            if let Some(work) = work {
                self.runtime.block_on(work)?;
            }
            if calls(&self.home) > before {
                self.calls.insert(request);
            }
            Ok(())
        }
    }

    impl Driver for JobDriver {
        type State = Jobs;

        fn step(&mut self, step: &Step) -> anyhow::Result<()> {
            switch!(step {
                init => *self = Self::new(),
                open(k: String) => {
                    self.requests += 1;
                    self.ask(k, self.requests);
                },
                defer(k: String) => self.defer(k),
                ask(r: i64) => self.ask_job(r)?,
                jobAct(r: i64) => self.act_job(r)?,
                askAgain(k: String) => self.ask_again(&k)?,
                fill(k: String) => {
                    let work = self.answer(&k, "kitchen", None, vec![Scripted::Call(call())]);
                    self.act(work, self.request[&k])?;
                },
                cancel(k: String) => _ = self.handle(&k, "never mind", None),
                tick => {
                    let mut now = self.clock.0.lock().expect("no lock holder panics");
                    *now += Duration::from_secs(1);
                },
            })
        }
    }

    /// AC-10.5b-2. `quint_run` replays 100 traces by default.
    #[quint_run(spec = "quint/dialog.qnt", max_steps = 30, seed = "1")]
    fn ac_10_5b_2_conformance() -> impl Driver {
        JobDriver::new()
    }
}
