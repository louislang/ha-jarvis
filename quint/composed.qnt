// Composed system model: the request lifecycle of `jarvis.qnt` with the models of the commit policy, the announce
// queues, the dialogs, training, learned intents and prefetch. Quint instantiates only a module with a constant, so each
// component has a wrapper module with an instance number.

/// The commit policy of one request.
module commitOf {
  /// The instance number.
  const ID: int
  import commit.* from "./commit"
  export commit.*

  /// One event before the commit: a chunk for `kind` 0, a VAD decision for 1, else a partial.
  action event(kind: int, voiced: bool, nonEmpty: bool, conf: bool): bool =
    all { not(committed), if (kind == 0) tick(voiced) else if (kind == 1) decide else partial(nonEmpty, conf) }

  /// The instance does not change.
  action unchanged = all {
    chunks' = chunks, decided' = decided, voiceEnd' = voiceEnd, text' = text,
    confident' = confident, committed' = committed, reason' = reason, commitPos' = commitPos,
  }
}

/// The announce queue of one satellite.
module announceOf {
  /// The instance number.
  const ID: int
  import announce.* from "./announce"
  export announce.*

  /// The next sentence of request `r` arrives on the idle satellite and starts at once: `arrive` and `start` in one step,
  /// as `Queues::queued` plays it after the margin. It plays only when `r` is the newest request.
  action play(r: REQUEST): bool = {
    val entry = (r, sentencesOf(arrived, r).length() + 1)
    all {
      idle, not(busy), r <= newest, arrived' = arrived.append(entry), queue' = queue, idle' = idle, newest' = newest,
      started' = if (r == newest) started.append((r, entry._2, newest)) else started, busy' = r == newest,
    }
  }

  /// The instance does not change.
  action unchanged = all { idle' = idle, newest' = newest, arrived' = arrived, queue' = queue, started' = started, busy' = busy }
}

/// The dialogs of the satellites.
module dialogOf {
  /// The instance number.
  const ID: int
  import dialog.* from "./dialog"
  export dialog.*

  /// The counts and the jobs do not change.
  action keepCounts: bool = all { keepJobs, now' = now, calls' = calls, cancelled' = cancelled, nextRequest' = nextRequest }

  /// A request on `k` takes its open dialog and closes the dialog of `k`, which increases its generation.
  action take(k: KEY): bool = all { closeDialog(k), keepCounts }

  /// The held dialog of request `d` with `held` questions opens again on `k` with one more question, asked at `time`.
  action putBack(k: KEY, d: REQUEST, held: int, time: int): bool = all {
    turns' = turns.set(k, held + 1), asked' = asked.set(k, time), request' = request.set(k, d), gen' = gen, keepCounts,
  }

  /// A new request on `k` asks a question at `time`.
  action openAt(k: KEY, time: int): bool = all {
    REQUESTS.contains(nextRequest), turns' = turns.set(k, 1), asked' = asked.set(k, time), request' = request.set(k, nextRequest),
    gen' = bump(k), keepJobs, now' = now, calls' = calls, cancelled' = cancelled, nextRequest' = nextRequest + 1,
  }

  /// The instance does not change.
  action unchanged = all {
    now' = now, turns' = turns, asked' = asked, request' = request, gen' = gen, running' = running,
    jobKey' = jobKey, jobGen' = jobGen, calls' = calls, cancelled' = cancelled, nextRequest' = nextRequest,
  }
}

/// The training trigger, run, pause and swap.
module trainingOf {
  /// The instance number.
  const ID: int
  import training.* from "./training"
  export training.*

  /// The current attempt gives a model of accuracy `a`.
  action done(a: int): bool = if (runState == Local) localDone(a) else runpodDone(a)

  /// The instance does not change.
  action unchanged = all {
    RUNPOD' = RUNPOD, changes' = changes, during' = during, stale' = stale, runState' = runState, paused' = paused, starts' = starts,
    ends' = ends, modelAcc' = modelAcc, prevAcc' = prevAcc, swaps' = swaps, attempts' = attempts, lastAction' = lastAction,
  }
}

/// The learned intents.
module learnedOf {
  /// The instance number.
  const ID: int
  import learned.* from "./learned"
  export learned.*

  /// The instance does not change.
  action unchanged = all {
    HAS_SERVICE' = HAS_SERVICE, NEEDS_PLUGIN' = NEEDS_PLUGIN, status' = status, approved' = approved, proposals' = proposals, changes' = changes, requests' = requests,
  }
}

/// The prefetch of one usual request.
module prefetchOf {
  /// The instance number.
  const ID: int
  import prefetch.* from "./prefetch"
  export prefetch.*

  /// The instance does not change.
  action unchanged = all {
    now' = now, answer' = answer, attempt' = attempt, fetches' = fetches, writeCalls' = writeCalls, readCalls' = readCalls, served' = served,
  }
}

/// The lifecycle actions of `jarvis.qnt` with the component effects and the training pause.
module composed {
  import jarvis from "./jarvis"
  import commitOf(ID = 1) as commit1 from "./composed"
  import commitOf(ID = 2) as commit2 from "./composed"
  import commitOf(ID = 3) as commit3 from "./composed"
  import announceOf(ID = 1) as queue1 from "./composed"
  import announceOf(ID = 2) as queue2 from "./composed"
  import dialogOf(ID = 1) as dialogs from "./composed"
  import trainingOf(ID = 1) as training from "./composed"
  import learnedOf(ID = 1) as learned from "./composed"
  import prefetchOf(ID = 1) as prefetches from "./composed"

  /// The step bound of `quint verify`. The check takes about 2 minutes at 5 steps and grows about 3 times with each step.
  pure val VERIFY_STEPS = 5
  /// The simulation samples of `just quint`. A composed step takes about 100 times longer than a step of `jarvis.qnt`.
  pure val SAMPLES = 1000

  /// No held dialog.
  pure val NONE = (0, 0)

  /// The dialog request and the questions of the dialog that each request took at `handle`, or `NONE`.
  var held: jarvis::REQUEST -> (int, int)

  /// The work after a reply: calls, a question that `Queues::queued` plays on the idle satellite, or none.
  type Work = Calls | Asks | Silent

  /// The work after the reply of each request. A cancel and the `failure` reply at the turn limit are `Silent`.
  var work: jarvis::REQUEST -> Work

  /// The time of `handle` of each request, which its question and its unclear answer give the dialog.
  var heard: jarvis::REQUEST -> int

  /// The question of request `r` opens a dialog that has not timed out.
  def unexpired(r: jarvis::REQUEST): bool = jarvis::now - heard.get(r) < dialogs::TIMEOUT

  /// The number of request `r` in the announce queue of its satellite.
  pure def local(r: jarvis::REQUEST): int = if (r == 2) 2 else 1

  /// The lifecycle does not change, and its history keeps the last lifecycle step.
  action keepLifecycle: bool = all {
    jarvis::prevVersion' = jarvis::modelVersion, jarvis::lastAction' = jarvis::lastAction, jarvis::phase' = jarvis::phase,
    jarvis::keepTime, jarvis::keepSatellites, jarvis::keepOutputs, jarvis::keepCounts,
  }

  /// The dialogs and the held dialogs do not change.
  action keepDialogs: bool = all { dialogs::unchanged, held' = held, work' = work, heard' = heard }

  /// The commit policies do not change.
  action keepCommits: bool = all { commit1::unchanged, commit2::unchanged, commit3::unchanged }

  /// The announce queues do not change.
  action keepQueues: bool = all { queue1::unchanged, queue2::unchanged }

  /// No component changes.
  action keepParts: bool = all { keepCommits, keepQueues, keepDialogs, training::unchanged, learned::unchanged, prefetches::unchanged }

  /// Only the announce queues change.
  action withQueues(change: bool): bool = all { change, keepCommits, keepDialogs, training::unchanged, learned::unchanged, prefetches::unchanged }

  /// Only training changes.
  action withTraining(change: bool): bool = all { change, keepCommits, keepQueues, keepDialogs, learned::unchanged, prefetches::unchanged }

  /// Only the learned intents change.
  action withLearned(change: bool): bool = all { change, keepLifecycle, keepCommits, keepQueues, keepDialogs, training::unchanged, prefetches::unchanged }

  /// Only the prefetch changes.
  action withPrefetch(change: bool): bool = all { change, keepLifecycle, keepCommits, keepQueues, keepDialogs, training::unchanged, learned::unchanged }

  /// No started request is `Listening` or `Handling`, so training can pause Jarvis.
  val quiet = jarvis::REQUESTS.forall(r => not(jarvis::isIn(r, Set(jarvis::Listening, jarvis::Handling))))

  /// The lifecycle of `jarvis.qnt`, each component at its start, and a configured RunPod fallback.
  action init = all {
    jarvis::init,
    held' = jarvis::REQUESTS.mapBy(_ => NONE),
    work' = jarvis::REQUESTS.mapBy(_ => Calls),
    heard' = jarvis::REQUESTS.mapBy(_ => 0),
    all { commit1::init, commit2::init, commit3::init, queue1::init, queue2::init },
    all { dialogs::init, training::initRunPod, learned::init, prefetches::init },
  }

  /// A user starts request `r`, unless training paused Jarvis.
  action audio(r: jarvis::REQUEST): bool = all { not(training::paused), jarvis::audio(r), keepParts }

  /// The commit policy of request `r` takes one event: a chunk for `kind` 0, a VAD decision for 1, else a partial.
  action listen(r: jarvis::REQUEST, kind: int, voiced: bool, nonEmpty: bool, conf: bool): bool = all {
    jarvis::isIn(r, Set(jarvis::Listening)),
    keepLifecycle,
    if (r == 1) commit1::event(kind, voiced, nonEmpty, conf) else commit1::unchanged,
    if (r == 2) commit2::event(kind, voiced, nonEmpty, conf) else commit2::unchanged,
    if (r == 3) commit3::event(kind, voiced, nonEmpty, conf) else commit3::unchanged,
    all { keepQueues, keepDialogs, training::unchanged, learned::unchanged, prefetches::unchanged },
  }

  /// The utterance of `r` ends: its commit policy commits, or HA ends the audio. The policy of `r` then stops.
  action commit(r: jarvis::REQUEST): bool = all { jarvis::commit(r), keepParts }

  /// The assistant handles `r`, unless training paused Jarvis. `r` becomes the newest request of its announce queue, and
  /// a fresh prefetched answer serves it.
  action handle(r: jarvis::REQUEST): bool = all {
    not(training::paused),
    jarvis::handle(r),
    if (jarvis::sat(r) == "s1") all { queue1::request(local(r)), queue2::unchanged } else all { queue1::unchanged, queue2::request(local(r)) },
    val k = jarvis::sat(r)
    all { dialogs::take(k), work' = work, heard' = heard.set(r, jarvis::now),
      held' = held.set(r, if (dialogs::isOpen(k)) (dialogs::request.get(k), dialogs::turns.get(k)) else NONE) },
    all { prefetches::request, keepCommits, training::unchanged, learned::unchanged },
  }

  /// Request `r` drops the dialog that it holds, the dialogs take `change`, and the work of `r` is `after`.
  action release(r: jarvis::REQUEST, change: bool, after: Work): bool = all {
    held' = held.set(r, NONE), work' = work.set(r, after), heard' = heard, change, keepCommits, keepQueues, training::unchanged, learned::unchanged, prefetches::unchanged,
  }

  /// System One replies to `r`. A dialog that `r` holds drops: `r` is a new clear request.
  action reply(r: jarvis::REQUEST): bool = all { jarvis::reply(r), release(r, dialogs::unchanged, Calls) }

  /// The handling of `r` fails.
  action failRequest(r: jarvis::REQUEST): bool = all { jarvis::failRequest(r), release(r, dialogs::unchanged, Calls) }

  /// Jarvis speaks the filler of `r` and defers its answer to System Two. With a held dialog, `unknown` is `unclear`.
  action defer(r: jarvis::REQUEST): bool = all { held.get(r) == NONE, jarvis::defer(r), release(r, dialogs::unchanged, Calls) }

  /// Request `r` replies with a question. Only the newest request of its satellite opens a dialog, asked at the time of
  /// `handle`. `jarvis.qnt` records it as a reply.
  action ask(r: jarvis::REQUEST): bool = {
    val k = jarvis::sat(r)
    val opens = jarvis::newest.get(k) == r and unexpired(r)
    all { jarvis::reply(r), release(r, if (opens) dialogs::openAt(k, heard.get(r)) else dialogs::unchanged, Asks) }
  }

  /// Request `r` takes the open dialog of its satellite and replies at once, without a prediction, unless training
  /// paused Jarvis: `handle` and `reply` of `jarvis.qnt` in one step. The dialogs take `change`, and the work is `after`.
  action atOnce(r: jarvis::REQUEST, change: bool, after: Work): bool = all {
    not(training::paused),
    jarvis::isIn(r, Set(jarvis::Committed)),
    jarvis::prev(r) == 0 or not(jarvis::at(jarvis::prev(r), Set(jarvis::Listening, jarvis::Committed))),
    dialogs::isOpen(jarvis::sat(r)),
    jarvis::record(jarvis::Handle(r)),
    jarvis::phase' = jarvis::phase.set(r, jarvis::Replied), jarvis::handled' = jarvis::handled.set(r, 1),
    jarvis::replyTime' = jarvis::replyTime.set(r, jarvis::now), jarvis::newest' = jarvis::newest.set(jarvis::sat(r), r),
    jarvis::modelVersion' = jarvis::modelVersion.set(r, jarvis::activeModel), jarvis::keepTime, jarvis::keepOutputs,
    jarvis::idle' = jarvis::idle, jarvis::started' = jarvis::started, jarvis::idleAfterReply' = jarvis::idleAfterReply,
    if (jarvis::sat(r) == "s1") all { queue1::request(local(r)), queue2::unchanged } else all { queue1::unchanged, queue2::request(local(r)) },
    all { change, held' = held, work' = work.set(r, after), heard' = heard.set(r, jarvis::now), prefetches::request, keepCommits, training::unchanged, learned::unchanged },
  }

  /// A choice of request `r` fills the open dialog of its satellite.
  action fill(r: jarvis::REQUEST): bool = atOnce(r, dialogs::fill(jarvis::sat(r)), Calls)

  /// A cancel phrase of request `r` cancels the open dialog of its satellite.
  action cancel(r: jarvis::REQUEST): bool = atOnce(r, dialogs::cancel(jarvis::sat(r)), Silent)

  /// The prediction of request `r`, which holds a dialog, is not clear. Below the turn limit, `r` asks again and puts the
  /// dialog back with one more question, asked at `handle`, when `r` is the newest request of its satellite. At the limit, the `failure`
  /// reply has no call. `jarvis.qnt` records it as a reply.
  action unclear(r: jarvis::REQUEST): bool = all {
    held.get(r) != NONE,
    val k = jarvis::sat(r)
    val (d, turns) = held.get(r)
    val again = turns < dialogs::MAX_TURNS and jarvis::newest.get(k) == r
    all {
      jarvis::reply(r), held' = held.set(r, NONE), heard' = heard,
      work' = work.set(r, if (turns < dialogs::MAX_TURNS) Asks else Silent),
      if (again and unexpired(r)) dialogs::putBack(k, d, turns, heard.get(r)) else dialogs::unchanged,
      keepCommits, keepQueues, training::unchanged, learned::unchanged, prefetches::unchanged,
    },
  }

  /// Jarvis runs the work after the reply of `r`. A question plays only on the idle satellite and only for the newest
  /// request, because `Queues::queued` waits for `idle` and drops the question of an older request.
  action call(r: jarvis::REQUEST): bool = all {
    val (s, after) = (jarvis::sat(r), work.get(r))
    after == Calls or (after == Asks and jarvis::idle.get(s) and jarvis::newest.get(s) == r),
    jarvis::call(r), keepParts,
  }

  /// HA ends the pipeline of `s`, also in its announce queue.
  action pipelineEnd(s: jarvis::SAT): bool = all {
    jarvis::pipelineEnd(s),
    withQueues(if (s == "s1") all { queue1::pipelineEnd, queue2::unchanged } else all { queue1::unchanged, queue2::pipelineEnd }),
  }

  /// The next sentence of the answer of `r` arrives and starts or drops on its idle satellite.
  action announce(r: jarvis::REQUEST): bool = all {
    jarvis::announce(r),
    withQueues(if (jarvis::sat(r) == "s1") all { queue1::play(local(r)), queue2::unchanged } else all { queue1::unchanged, queue2::play(local(r)) }),
  }

  /// The training attempt gives a model of accuracy `a` that is not worse, and makes version `v` active.
  action swap(v: jarvis::VERSION, a: int): bool = all {
    a >= training::modelAcc,
    jarvis::swap(v),
    withTraining(training::done(a)),
  }

  /// Time advances. The dialogs time out, and each playing announce ends.
  action tick = all {
    jarvis::tick,
    all { queue1::finish, queue2::finish, dialogs::tick, prefetches::tick },
    all { held' = held, work' = work, heard' = heard, keepCommits, training::unchanged, learned::unchanged },
  }

  /// The change count reaches its minimum and a run starts, when no request listens or is handled.
  action autoStart = all { quiet, keepLifecycle, withTraining(training::autoStart) }

  /// A voice request starts a run, when no request listens or is handled.
  action voiceStart = all { quiet, keepLifecycle, withTraining(training::voiceStart) }

  /// A new action mark.
  action change = all { keepLifecycle, withTraining(training::change) }

  /// The local attempt fails.
  action localFail = all { keepLifecycle, withTraining(training::localFail) }

  /// The RunPod attempt fails.
  action runpodFail = all { keepLifecycle, withTraining(training::runpodFail) }

  /// System Two proposes an entry for cluster `c`.
  action propose(c: str, suitable: bool): bool = withLearned(learned::propose(c, suitable))

  /// You approve the pending entry of cluster `c`.
  action approve(c: str): bool = withLearned(learned::approve(c))

  /// You reject the pending entry of cluster `c`.
  action reject(c: str): bool = withLearned(learned::reject(c))

  /// Jarvis writes the plugin request file of cluster `c`.
  action writeRequest(c: str): bool = withLearned(learned::writeRequest(c))

  /// The write of the plugin request file of cluster `c` fails.
  action failWrite(c: str): bool = withLearned(learned::failRequest(c))

  /// A prefetch starts inside its window.
  action prefetch = withPrefetch(prefetches::prefetch)

  /// The prefetch gets `answerStep` from System Two.
  action prefetchStep(answerStep: prefetches::Step): bool = withPrefetch(prefetches::prefetchStep(answerStep))

  /// Any enabled event. The component actions share one branch, so that the lifecycle advances in a trace.
  action step = any {
    nondet r = jarvis::REQUESTS.oneOf()
    nondet kind = 0.to(2).oneOf()
    nondet voiced = Bool.oneOf()
    nondet nonEmpty = Bool.oneOf()
    nondet conf = Bool.oneOf()
    any {
      audio(r), listen(r, kind, voiced, nonEmpty, conf), commit(r), handle(r), reply(r), failRequest(r), defer(r),
      ask(r), fill(r), cancel(r), unclear(r), call(r), announce(r),
    },
    nondet s = jarvis::SATS.oneOf()
    pipelineEnd(s),
    nondet v = jarvis::VERSIONS.oneOf()
    nondet a = training::ACC.oneOf()
    swap(v, a),
    tick,
    any {
      autoStart, voiceStart, change, localFail, runpodFail,
      nondet c = learned::CLUSTERS.oneOf()
      nondet suitable = Bool.oneOf()
      any { propose(c, suitable), approve(c), reject(c), writeRequest(c), failWrite(c) },
      prefetch,
      nondet answerStep = Set(prefetches::Read, prefetches::Write, prefetches::Sentence, prefetches::Fail).oneOf()
      prefetchStep(answerStep),
    },
  }

  /// The five system invariants of `jarvis.qnt`.
  val SystemInvariants = jarvis::allInvariants

  /// The invariants of each imported model.
  val ComponentInvariants = all {
    commit1::allInvariants, commit2::allInvariants, commit3::allInvariants, queue1::allInvariants, queue2::allInvariants,
    dialogs::allInvariants, training::allInvariants, learned::allInvariants, prefetches::allInvariants,
  }

  /// The invariants that `just quint` checks.
  val allInvariants = SystemInvariants and ComponentInvariants
}
