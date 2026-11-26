// Timer mirrors: the Assist timers that show the progress of the timer helpers on the LED ring of the satellites.
module mirrors {
  /// An HA timer helper.
  type HELPER = str

  /// A satellite. `""` is no satellite.
  type SATELLITE = str

  /// The Assist timer with the name of a helper on a satellite.
  type PAIR = (HELPER, SATELLITE)

  /// A call of the mirror task: `"start"`, `"pause"` or `"cancel"` of the pair `(h, s)`, with its number `n`.
  type Call = { op: str, h: HELPER, s: SATELLITE, n: int }

  /// The calls of the mirror task and the number of the next call.
  type Queue = { calls: Set[Call], next: int }

  /// The kind of the last step.
  type Action = MirrorRing | Other

  /// The helpers in entity ID order, so that `start` takes the first free helper.
  pure val HELPERS: List[HELPER] = List("h1", "h2")

  /// The satellites.
  pure val SATELLITES: Set[SATELLITE] = Set("s1", "s2")

  /// The helpers as a set.
  pure val HELPER_SET: Set[HELPER] = HELPERS.indices().map(i => HELPERS[i])

  /// The step bound of `quint verify`, because 10 steps take hours.
  pure val VERIFY_STEPS = 5

  /// The HA state of each helper: `"idle"`, `"active"` or `"paused"`.
  var ha: HELPER -> str

  /// The state of each helper in the view of `TimerDesk`.
  var seen: HELPER -> str

  /// The satellite of the entry of each helper, `""` for no entry.
  var entry: HELPER -> SATELLITE

  /// The satellite of the mirror of each helper in `TimerDesk`, `""` for no mirror.
  var mirror: HELPER -> SATELLITE

  /// The queue of the mirror task. Its order is `ahead`. A set and not a list, because Apalache checks a list that `foldl` builds too slowly.
  var calls: Set[Call]

  /// The number of the next call.
  var nextCall: int

  /// The Assist timers in HA.
  var assist: Set[PAIR]

  /// The pairs whose state in HA Jarvis does not know.
  var late: Set[PAIR]

  /// Jarvis is connected to HA.
  var connected: bool

  /// The kind of the last step.
  var lastAction: Action

  /// The pair of the last `MirrorRing` was in `late` before the step.
  var lastWasLate: bool

  /// No timers and no mirrors.
  action init = all {
    ha' = HELPER_SET.mapBy(_ => "idle"),
    seen' = HELPER_SET.mapBy(_ => "idle"),
    entry' = HELPER_SET.mapBy(_ => ""),
    mirror' = HELPER_SET.mapBy(_ => ""),
    calls' = Set(),
    nextCall' = 0,
    assist' = Set(),
    late' = Set(),
    connected' = true,
    lastAction' = Other,
    lastWasLate' = false,
  }

  /// A helper in the state `state` runs.
  pure def runs(state: str): bool = state == "active" or state == "paused"

  /// The call `c` of the pair `(h, s)`.
  pure def about(c: Call, h: HELPER, s: SATELLITE): bool = c.h == h and c.s == s

  /// The pair `(h, s)` is legit: `s` has the mirror of `h`, and `h` runs.
  def legit(h: HELPER, s: SATELLITE): bool = mirror.get(h) == s and runs(ha.get(h))

  /// A call of `calls` of the pair `(h, s)` has an op of `ops`.
  def queued(h: HELPER, s: SATELLITE, ops: Set[str]): bool = calls.exists(c => about(c, h, s) and ops.contains(c.op))

  /// Call `c` goes before call `d`: each cancel goes ahead of each call that is not a cancel, and else the older call goes first.
  pure def ahead(c: Call, d: Call): bool = {
    val cancels = c.op == "cancel"
    (cancels and d.op != "cancel") or (cancels == (d.op == "cancel") and c.n < d.n)
  }

  /// Call `c` is the first call of the queue.
  def first(c: Call): bool = calls.contains(c) and calls.forall(d => d == c or ahead(c, d))

  /// The queue of the state.
  val queue: Queue = { calls: calls, next: nextCall }

  /// `q` with the call `op` of `(h, s)`, when `s` is not `""`. A cancel first removes the calls of its pair.
  pure def push(q: Queue, op: str, h: HELPER, s: SATELLITE): Queue = {
    val kept = if (op == "cancel") q.calls.filter(c => not(about(c, h, s))) else q.calls
    if (s == "") q else { calls: kept.union(Set({ op: op, h: h, s: s, n: q.next })), next: q.next + 1 }
  }

  /// Helper `h` is `"idle"` in HA and in the view, and has no entry.
  def free(h: HELPER): bool = ha.get(h) == "idle" and seen.get(h) == "idle" and entry.get(h) == ""

  /// The first free helper, or `""` when none is free.
  val firstFree: HELPER = HELPERS.foldl("", (found, h) => if (found == "" and free(h)) h else found)

  /// Takes the new state and records a step that is not a `MirrorRing`.
  action move(
    newHa: HELPER -> str,
    newSeen: HELPER -> str,
    newEntry: HELPER -> SATELLITE,
    newMirror: HELPER -> SATELLITE,
    newQueue: Queue,
    newAssist: Set[PAIR],
    newLate: Set[PAIR],
    newConnected: bool,
  ): bool = all {
    ha' = newHa,
    seen' = newSeen,
    entry' = newEntry,
    mirror' = newMirror,
    calls' = newQueue.calls,
    nextCall' = newQueue.next,
    assist' = newAssist,
    late' = newLate,
    connected' = newConnected,
    lastAction' = Other,
    lastWasLate' = false,
  }

  /// A timer from `s` starts on the first free helper, with its mirror.
  action start(s: SATELLITE): bool = {
    val h = firstFree
    all {
      connected,
      h != "",
      move(ha.set(h, "active"), seen.set(h, "active"), entry.set(h, s), mirror.set(h, s), push(queue, "start", h, s), assist, late, true),
    }
  }

  /// A person starts `h` in HA.
  action outside(h: HELPER): bool = all {
    connected,
    free(h),
    move(ha.set(h, "active"), seen.set(h, "active"), entry, mirror, queue, assist, late, true),
  }

  /// Helper `h` goes from `before` to `after`, and its mirror gets the call `op`.
  action turn(h: HELPER, before: str, after: str, op: str): bool = all {
    connected,
    ha.get(h) == before,
    move(ha.set(h, after), seen.set(h, after), entry, mirror, push(queue, op, h, mirror.get(h)), assist, late, true),
  }

  /// Helper `h` pauses, and so does its mirror.
  action pause(h: HELPER): bool = turn(h, "active", "paused", "pause")

  /// Helper `h` resumes. The call `start` replaces the mirror with one for the time left of the helper and `MIRROR_PAD`, so a pause that came late leaves no deficit.
  action resume(h: HELPER): bool = turn(h, "paused", "active", "start")

  /// The timer of `h` finishes, or Jarvis or a person cancels it.
  action end(h: HELPER): bool = all {
    connected,
    runs(ha.get(h)),
    move(ha.set(h, "idle"), seen.set(h, "idle"), entry.set(h, ""), mirror.set(h, ""), push(queue, "cancel", h, mirror.get(h)), assist, late, true),
  }

  /// The timer of `h` ends while Jarvis is not connected.
  action quietEnd(h: HELPER): bool = all {
    runs(ha.get(h)),
    move(ha.set(h, "idle"), seen, entry, mirror, queue, assist, if (mirror.get(h) == "") late else late.union(Set((h, mirror.get(h)))), false),
  }

  /// The mirrors, the entries and the calls of Jarvis.
  type Desk = { mirror: HELPER -> SATELLITE, entry: HELPER -> SATELLITE, queue: Queue }

  /// `desk` after the snapshot shows `h`: a new run with an entry starts its mirror, and a helper that does not run loses its mirror and its entry.
  def snap(desk: Desk, h: HELPER, newRun: bool): Desk = {
    val e = desk.entry.get(h)
    val m = desk.mirror.get(h)
    if (runs(ha.get(h))) {
      if (newRun and e != "") desk.with("mirror", desk.mirror.set(h, e)).with("queue", push(desk.queue, "start", h, e))
      else desk
    } else {
      { mirror: desk.mirror.set(h, ""), entry: desk.entry.set(h, ""), queue: push(desk.queue, "cancel", h, if (m != "") m else e) }
    }
  }

  /// The snapshot of a connection, from the mirrors `mirrors` and the queue `pending`, with the new runs `newRun`.
  action connect(mirrors: HELPER -> SATELLITE, pending: Queue, newRun: HELPER -> bool, newAssist: Set[PAIR], newLate: Set[PAIR]): bool = {
    val desk = HELPERS.foldl({ mirror: mirrors, entry: entry, queue: pending }, (acc, h) => snap(acc, h, newRun.get(h)))
    move(ha, ha, desk.entry, desk.mirror, desk.queue, newAssist, newLate, true)
  }

  /// Jarvis connects to HA again.
  action reconnect = connect(mirror, queue, HELPER_SET.mapBy(h => runs(ha.get(h)) and not(runs(seen.get(h)))), assist, late)

  /// Jarvis starts again: the queue and the mirrors go, and a dropped cancel of an Assist timer makes its pair late.
  action restart = {
    val dropped = calls.filter(c => c.op == "cancel" and assist.contains((c.h, c.s))).map(c => (c.h, c.s))
    connect(HELPER_SET.mapBy(_ => ""), { calls: Set(), next: nextCall }, HELPER_SET.mapBy(h => runs(ha.get(h))), assist, late.union(dropped))
  }

  /// HA starts again and drops each Assist timer, and Jarvis connects again.
  action haRestart = connect(mirror, queue, HELPER_SET.mapBy(h => runs(ha.get(h))), Set(), Set())

  /// The satellite `s` starts again and drops its Assist timers. Each running helper with a mirror on `s` gets the call `start`.
  action satRestart(s: SATELLITE): bool = {
    val starts = HELPERS.foldl(queue, (acc, h) => if (mirror.get(h) == s and runs(seen.get(h))) push(acc, "start", h, s) else acc)
    all {
      connected,
      move(ha, seen, entry, mirror, starts, assist.filter(p => p._2 != s), late.filter(p => p._2 != s), true),
    }
  }

  /// The mirror task makes the first call. A `start` adds its pair, a `cancel` removes it, and both make it known.
  action call = {
    nondet c = calls.oneOf()
    val p = (c.h, c.s)
    val shown = if (c.op == "start") assist.union(Set(p)) else if (c.op == "cancel") assist.exclude(Set(p)) else assist
    all {
      connected,
      first(c),
      move(ha, seen, entry, mirror, queue.with("calls", calls.exclude(Set(c))), shown, if (c.op == "pause") late else late.exclude(Set(p)), true),
    }
  }

  /// The first call fails, and its pair is late.
  action callFails = {
    nondet c = calls.oneOf()
    all {
      first(c),
      move(ha, seen, entry, mirror, queue.with("calls", calls.exclude(Set(c))), assist, late.union(Set((c.h, c.s))), connected),
    }
  }

  /// A queued cancel or pause of the Assist timer `(h, s)` waits so long that the mirror can end before it.
  action slow(h: HELPER, s: SATELLITE): bool = all {
    queued(h, s, Set("cancel", "pause")),
    assist.contains((h, s)),
    move(ha, seen, entry, mirror, queue, assist, late.union(Set((h, s))), connected),
  }

  /// The mirror `(h, s)` reaches its end, and the firmware rings.
  action mirrorRing(h: HELPER, s: SATELLITE): bool = all {
    assist.contains((h, s)),
    late.contains((h, s)) or not(legit(h, s) or queued(h, s, Set("start", "pause", "cancel"))),
    ha' = ha,
    seen' = seen,
    entry' = entry,
    mirror' = mirror,
    calls' = calls,
    nextCall' = nextCall,
    assist' = assist.exclude(Set((h, s))),
    late' = late,
    connected' = connected,
    lastAction' = MirrorRing,
    lastWasLate' = late.contains((h, s)),
  }

  /// Any enabled event.
  action step = {
    nondet h = HELPER_SET.oneOf()
    nondet s = SATELLITES.oneOf()
    any {
      start(s),
      outside(h),
      pause(h),
      resume(h),
      end(h),
      quietEnd(h),
      reconnect,
      restart,
      haRestart,
      satRestart(s),
      call,
      callFails,
      slow(h, s),
      mirrorRing(h, s),
    }
  }

  /// The firmware rings for a mirror only when Jarvis lost or delayed a call of its pair.
  val NoMirrorRing = lastAction == MirrorRing implies lastWasLate

  /// With no queued call, each pair that is not late is an Assist timer exactly when its helper runs with an entry of its satellite.
  val MirrorsShown = calls == Set() implies HELPER_SET.forall(h => SATELLITES.forall(s =>
    late.contains((h, s)) or (assist.contains((h, s)) iff (entry.get(h) == s and runs(ha.get(h))))
  ))

  /// The invariants that `just quint` checks.
  val allInvariants = NoMirrorRing and MirrorsShown

  /// The queue in its order has the op, the helper and the satellite of each call of `l`.
  def queueIs(l: List[(str, HELPER, SATELLITE)]): bool = calls.size() == l.length() and l.indices().forall(i =>
    calls.exists(c => (c.op, c.h, c.s) == l[i] and calls.filter(d => ahead(d, c)).size() == i)
  )

  /// The mirror starts, pauses and ends with its helper, and an end removes the queued calls of its pair.
  run ac_19_13_3_mirror_follows_the_helper = init
    .then(start("s1"))
    .then(call)
    .expect(assist.contains(("h1", "s1")))
    .then(pause("h1"))
    .expect(queueIs([("pause", "h1", "s1")]))
    .then(call)
    .then(end("h1"))
    .expect(queueIs([("cancel", "h1", "s1")]))
    .then(call)
    .expect(assist == Set())
    .then(mirrorRing("h1", "s1").fail())
    .then(init)
    .then(start("s1"))
    .then(end("h1"))
    .expect(queueIs([("cancel", "h1", "s1")]))

  /// A restart cancels the mirror of a helper that ended unseen, ahead of the new start of a running helper.
  run ac_19_13_4_restart_replaces_the_mirror = init
    .then(start("s1"))
    .then(start("s2"))
    .then(call)
    .then(call)
    .then(quietEnd("h2"))
    .then(restart)
    .expect(queueIs([("cancel", "h2", "s2"), ("start", "h1", "s1")]))

  /// A restart of the satellite or of HA drops the mirror, and a new start shows it again.
  run ac_19_13_5_restarts_show_the_ring_again = {
    val again = assist == Set() and queueIs([("start", "h1", "s1")])
    init
      .then(start("s1"))
      .then(call)
      .then(satRestart("s1"))
      .expect(again)
      .then(call)
      .then(haRestart)
      .expect(again)
      .then(call)
      .expect(assist == Set(("h1", "s1")))
  }

  /// A failed or slow cancel and a failed pause let the mirror ring, and the pair stays late.
  run ac_19_13_6_lost_or_slow_cancel_can_ring = init
    .then(start("s1"))
    .then(call)
    .then(end("h1"))
    .then(callFails)
    .then(mirrorRing("h1", "s1"))
    .expect(lastWasLate)
    .then(init)
    .then(start("s1"))
    .then(call)
    .then(pause("h1"))
    .then(callFails)
    .then(mirrorRing("h1", "s1"))
    .expect(late.contains(("h1", "s1")) and MirrorsShown)
    .then(init)
    .then(start("s1"))
    .then(call)
    .then(end("h1"))
    .then(slow("h1", "s1"))
    .then(mirrorRing("h1", "s1"))
    .expect(queueIs([("cancel", "h1", "s1")]))

  /// A pause that waits behind another call lets the mirror run on, and the resume starts the mirror again from the time left.
  run ac_19_13_7_late_pause_restarts_the_mirror = init
    .then(start("s1"))
    .then(call)
    .then(start("s2"))
    .then(pause("h1"))
    .expect(queueIs([("start", "h2", "s2"), ("pause", "h1", "s1")]))
    .then(slow("h1", "s1"))
    .then(call)
    .then(call)
    .expect(late.contains(("h1", "s1")))
    .then(resume("h1"))
    .expect(queueIs([("start", "h1", "s1")]))
    .then(call)
    .expect(late == Set() and assist == Set(("h1", "s1"), ("h2", "s2")))
    .then(mirrorRing("h1", "s1").fail())
}
