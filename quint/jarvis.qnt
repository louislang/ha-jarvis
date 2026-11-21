// System model: the request lifecycle on two satellites, with replies, fillers, deferred answers, calls and model swaps.
module jarvis {
  /// A satellite.
  type SAT = str

  /// The ID of a request.
  type REQUEST = int

  /// A version of the intent model.
  type VERSION = int

  /// The phase of a request.
  type Phase = Listening | Committed | Handling | Replied | Deferred | Done

  /// A step of the model. `Drop` is an announce of a request that is not the newest of its satellite.
  type Event = Init | Audio(REQUEST) | Commit(REQUEST) | Handle(REQUEST) | Reply(REQUEST) | Fail(REQUEST) | Defer(REQUEST)
    | Call(REQUEST) | PipelineEnd(SAT) | Announce(REQUEST) | Drop(REQUEST) | Swap(VERSION) | Tick

  /// The satellites.
  pure val SATS: Set[SAT] = Set("s1", "s2")

  /// The requests that users can start.
  pure val REQUESTS: Set[REQUEST] = 1.to(3)

  /// The model versions that training can make active.
  pure val VERSIONS: Set[VERSION] = Set(1, 2)

  /// The last time unit.
  pure val MAX_TIME = 5

  /// The satellite of request `r`: requests 1 and 2 on `s1`, request 3 on `s2`.
  pure def sat(r: REQUEST): SAT = if (r <= 2) "s1" else "s2"

  /// The request before `r` on its satellite, or 0. Each satellite starts its requests in order.
  pure def prev(r: REQUEST): REQUEST = if (r == 2) 1 else 0

  /// The current time.
  var now: int

  /// The model version that `handle` reads.
  var activeModel: VERSION

  /// The phase of each request. A request that did not start is `Listening`.
  var phase: REQUEST -> Phase

  /// The number of `Handled` messages of each request.
  var handled: REQUEST -> int

  /// The time of the reply or the filler of each request. -1 before it.
  var replyTime: REQUEST -> int

  /// The time of the call of each request. -1 when it has none.
  var callTime: REQUEST -> int

  /// The satellite of each request became idle after its reply or filler.
  var idleAfterReply: REQUEST -> bool

  /// The start times of the announces of each request.
  var announceTimes: REQUEST -> Set[int]

  /// The model version of each request. 0 before `handle`.
  var modelVersion: REQUEST -> VERSION

  /// The satellite entity is `idle`.
  var idle: SAT -> bool

  /// The newest request of each satellite. 0 before any.
  var newest: SAT -> REQUEST

  /// `modelVersion` before the last step.
  var prevVersion: REQUEST -> VERSION

  /// The last step.
  var lastAction: Event

  /// Request `r` started and its phase is in `phases`.
  def isIn(r: REQUEST, phases: Set[Phase]): bool = r > 0 and r <= newest.get(sat(r)) and phases.contains(phase.get(r))

  /// Sets the history of step `e`.
  action record(e: Event): bool = all {
    prevVersion' = modelVersion,
    lastAction' = e,
  }

  /// The time and the active model do not change.
  action keepTime: bool = all {
    now' = now,
    activeModel' = activeModel,
  }

  /// The satellites do not change.
  action keepSatellites: bool = all {
    idle' = idle,
    newest' = newest,
    idleAfterReply' = idleAfterReply,
  }

  /// No call or announce starts.
  action keepOutputs: bool = all {
    callTime' = callTime,
    announceTimes' = announceTimes,
  }

  /// The counts, the reply times and the model versions do not change.
  action keepCounts: bool = all {
    handled' = handled,
    replyTime' = replyTime,
    modelVersion' = modelVersion,
  }

  /// No request, idle satellites and model version 1.
  action init = all {
    now' = 0,
    activeModel' = 1,
    phase' = REQUESTS.mapBy(_ => Listening),
    handled' = REQUESTS.mapBy(_ => 0),
    replyTime' = REQUESTS.mapBy(_ => -1),
    callTime' = REQUESTS.mapBy(_ => -1),
    idleAfterReply' = REQUESTS.mapBy(_ => false),
    announceTimes' = REQUESTS.mapBy(_ => Set()),
    modelVersion' = REQUESTS.mapBy(_ => 0),
    idle' = SATS.mapBy(_ => true),
    newest' = SATS.mapBy(_ => 0),
    prevVersion' = REQUESTS.mapBy(_ => 0),
    lastAction' = Init,
  }

  /// A user starts request `r`. Its satellite leaves `idle`.
  action audio(r: REQUEST): bool = all {
    newest.get(sat(r)) == prev(r),
    record(Audio(r)),
    phase' = phase,
    newest' = newest.set(sat(r), r),
    idle' = idle.set(sat(r), false),
    idleAfterReply' = idleAfterReply,
    keepTime,
    keepOutputs,
    keepCounts,
  }

  /// Listening commits the utterance of `r`.
  action commit(r: REQUEST): bool = all {
    isIn(r, Set(Listening)),
    record(Commit(r)),
    phase' = phase.set(r, Committed),
    keepTime,
    keepSatellites,
    keepOutputs,
    keepCounts,
  }

  /// The assistant starts to handle `r` and reads the active model once.
  action handle(r: REQUEST): bool = all {
    isIn(r, Set(Committed)),
    record(Handle(r)),
    phase' = phase.set(r, Handling),
    modelVersion' = modelVersion.set(r, activeModel),
    handled' = handled,
    replyTime' = replyTime,
    keepTime,
    keepSatellites,
    keepOutputs,
  }

  /// In step `e`, request `r` leaves `Handling` for `target` with one `Handled` message and the reply times `times`.
  action finish(e: Event, r: REQUEST, target: Phase, times: REQUEST -> int): bool = all {
    isIn(r, Set(Handling)),
    record(e),
    phase' = phase.set(r, target),
    handled' = handled.set(r, handled.get(r) + 1),
    replyTime' = times,
    modelVersion' = modelVersion,
    keepTime,
    keepSatellites,
    keepOutputs,
  }

  /// System One replies to `r`.
  action reply(r: REQUEST): bool = finish(Reply(r), r, Replied, replyTime.set(r, now))

  /// The handling of `r` fails with the error `not-handled`.
  action failRequest(r: REQUEST): bool = finish(Fail(r), r, Done, replyTime)

  /// Jarvis speaks the filler of `r` and defers its answer to System Two.
  action defer(r: REQUEST): bool = finish(Defer(r), r, Deferred, replyTime.set(r, now))

  /// Jarvis calls HA for `r` after its reply or filler.
  action call(r: REQUEST): bool = all {
    isIn(r, Set(Replied, Deferred)),
    callTime.get(r) < 0,
    record(Call(r)),
    callTime' = callTime.set(r, now),
    announceTimes' = announceTimes,
    phase' = phase,
    keepTime,
    keepSatellites,
    keepCounts,
  }

  /// HA ends the pipeline of `s` after the reply, the filler or the error of its newest request.
  action pipelineEnd(s: SAT): bool = all {
    isIn(newest.get(s), Set(Replied, Deferred, Done)),
    record(PipelineEnd(s)),
    idle' = idle.set(s, true),
    idleAfterReply' = REQUESTS.mapBy(r => idleAfterReply.get(r) or (sat(r) == s and isIn(r, Set(Replied, Deferred)))),
    newest' = newest,
    phase' = phase,
    keepTime,
    keepOutputs,
    keepCounts,
  }

  /// The answer of deferred request `r` starts on its idle satellite. A request that is not the newest drops it.
  action announce(r: REQUEST): bool = all {
    isIn(r, Set(Deferred)),
    idle.get(sat(r)),
    val fresh = newest.get(sat(r)) == r
    all {
      record(if (fresh) Announce(r) else Drop(r)),
      announceTimes' = if (fresh) announceTimes.set(r, announceTimes.get(r).union(Set(now))) else announceTimes,
    },
    callTime' = callTime,
    phase' = phase,
    keepTime,
    keepSatellites,
    keepCounts,
  }

  /// Training makes version `v` active.
  action swap(v: VERSION): bool = all {
    record(Swap(v)),
    activeModel' = v,
    now' = now,
    phase' = phase,
    keepSatellites,
    keepOutputs,
    keepCounts,
  }

  /// Time advances, up to `MAX_TIME`.
  action tick = all {
    now < MAX_TIME,
    record(Tick),
    now' = now + 1,
    activeModel' = activeModel,
    phase' = phase,
    keepSatellites,
    keepOutputs,
    keepCounts,
  }

  /// Any enabled event.
  action step = any {
    nondet r = REQUESTS.oneOf()
    any {
      audio(r),
      commit(r),
      handle(r),
      reply(r),
      failRequest(r),
      defer(r),
      call(r),
      announce(r),
    },
    nondet s = SATS.oneOf()
    pipelineEnd(s),
    nondet v = VERSIONS.oneOf()
    swap(v),
    tick,
  }

  /// Each request has at most one `Handled` message, and exactly one after it leaves `Handling`.
  val OneHandledPerRequest = REQUESTS.forall(r =>
    handled.get(r) <= 1 and (isIn(r, Set(Replied, Deferred, Done)) implies handled.get(r) == 1))

  /// A call follows the reply or the filler of its request.
  val ReplyBeforeCall = REQUESTS.forall(r =>
    callTime.get(r) >= 0 implies (replyTime.get(r) >= 0 and callTime.get(r) >= replyTime.get(r)))

  /// Each announce of a deferred request follows the end of its filler on the satellite.
  val FillerBeforeAnswer = REQUESTS.forall(r => announceTimes.get(r) != Set() implies idleAfterReply.get(r))

  /// Only the `handle` step of a request writes its model version, once, from the active model.
  val ModelSwapAtomic = REQUESTS.forall(r => all {
    modelVersion.get(r) != prevVersion.get(r) implies
      (lastAction == Handle(r) and prevVersion.get(r) == 0 and modelVersion.get(r) == activeModel),
    isIn(r, Set(Handling, Replied, Deferred, Done)) implies VERSIONS.contains(modelVersion.get(r)),
  })

  /// An announce starts only on an idle satellite, for its newest request.
  val NoTalkOver = match lastAction {
    | Announce(r) => idle.get(sat(r)) and newest.get(sat(r)) == r
    | _ => true
  }

  /// The invariants that `just quint` checks.
  val allInvariants = OneHandledPerRequest and ReplyBeforeCall and FillerBeforeAnswer and ModelSwapAtomic and NoTalkOver

  /// Each step of the model.
  pure val EVENTS: Set[Event] = Set(Init, Tick)
    .union(VERSIONS.map(v => Swap(v)))
    .union(SATS.map(s => PipelineEnd(s)))
    .union(REQUESTS.map(r => Set(Audio(r), Commit(r), Handle(r), Reply(r), Fail(r), Defer(r), Call(r), Announce(r), Drop(r))).flatten())

  /// The phase of request `r` is in `phases`, also when `r` did not start.
  def at(r: REQUEST, phases: Set[Phase]): bool = phases.contains(phase.get(r))

  /// An invariant that implies `allInvariants` and that each step keeps.
  val Inductive = all {
    now.in(0.to(MAX_TIME)),
    activeModel.in(VERSIONS),
    phase.in(REQUESTS.setOfMaps(Set(Listening, Committed, Handling, Replied, Deferred, Done))),
    handled.in(REQUESTS.setOfMaps(0.to(1))),
    replyTime.in(REQUESTS.setOfMaps((-1).to(MAX_TIME))),
    callTime.in(REQUESTS.setOfMaps((-1).to(MAX_TIME))),
    idleAfterReply.in(REQUESTS.setOfMaps(Bool)),
    announceTimes.in(REQUESTS.setOfMaps(0.to(MAX_TIME).powerset())),
    modelVersion.in(REQUESTS.setOfMaps(Set(0).union(VERSIONS))),
    idle.in(SATS.setOfMaps(Bool)),
    newest.in(SATS.setOfMaps(0.to(3))),
    prevVersion.in(REQUESTS.setOfMaps(Set(0).union(VERSIONS))),
    lastAction.in(EVENTS),
    allInvariants,
    SATS.forall(s => newest.get(s) == 0 or sat(newest.get(s)) == s),
    // The pipeline of a satellite ends only after its newest request replied, deferred or failed.
    SATS.forall(s => (idle.get(s) and newest.get(s) > 0) implies isIn(newest.get(s), Set(Replied, Deferred, Done))),
    REQUESTS.forall(r => all {
      at(r, Set(Listening)) or isIn(r, Set(Committed, Handling, Replied, Deferred, Done)),
      handled.get(r) == (if (at(r, Set(Replied, Deferred, Done))) 1 else 0),
      modelVersion.get(r) == 0 iff at(r, Set(Listening, Committed)),
      replyTime.get(r) >= 0 iff at(r, Set(Replied, Deferred)),
      replyTime.get(r) <= now,
      callTime.get(r) >= 0 implies at(r, Set(Replied, Deferred)),
      idleAfterReply.get(r) implies at(r, Set(Replied, Deferred)),
      announceTimes.get(r) != Set() implies at(r, Set(Deferred)),
      (isIn(r, Set(Replied, Deferred)) and idle.get(sat(r)) and newest.get(sat(r)) == r) implies idleAfterReply.get(r),
    }),
  }

  /// A swap before `handle` gives the new version, and a swap after `handle` keeps the version of `handle`.
  run ac_16_1_3_swap_during_request = init
    .then(audio(1))
    .then(commit(1))
    .then(swap(2))
    .then(handle(1))
    .then(swap(1))
    .then(reply(1))
    .expect(modelVersion.get(1) == 2 and activeModel == 1)

  /// A deferred request makes no announce after a newer request on its satellite.
  run ac_16_1_4_two_requests_one_satellite = init
    .then(audio(1))
    .then(commit(1))
    .then(handle(1))
    .then(defer(1))
    .then(audio(2))
    .then(commit(2))
    .then(handle(2))
    .then(reply(2))
    .then(pipelineEnd("s1"))
    .then(announce(1))
    .expect(announceTimes.get(1) == Set() and lastAction == Drop(1))
}
