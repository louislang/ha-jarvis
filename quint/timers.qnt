// Voice timers: the timer table of Jarvis, the HA timer helpers and the rings.
module timers {
  /// An HA timer helper.
  type HELPER = str

  /// A satellite.
  type SATELLITE = str

  /// The kind of the last step. `SetTimer` is `Set` of the spec, and `setTimer` is its `set`, because Quint reserves `Set` and `set`.
  type Action = SetTimer | Ring | Other

  /// The entry of the Jarvis table for one helper. `owned = false` is no entry.
  type Entry = { owned: bool, sat: SATELLITE, name: str, order: int }

  /// The ring of the last timer that finished on one helper.
  type Bell = { sat: SATELLITE, name: str, left: int, rung: int, stopped: bool }

  /// The helpers in entity ID order, so that `setTimer` takes the first free helper.
  pure val HELPERS: List[HELPER] = List("h1", "h2")

  /// The satellites.
  pure val SATELLITES: Set[SATELLITE] = Set("s1", "s2")

  /// The timer names. `""` is a generic timer.
  pure val NAMES: Set[str] = Set("", "chicken")

  /// The rings of one finished timer.
  pure val RINGS = 8

  /// The helpers as a set.
  pure val HELPER_SET: Set[HELPER] = HELPERS.indices().map(i => HELPERS[i])

  /// No entry.
  pure val NO_ENTRY: Entry = { owned: false, sat: "", name: "", order: 0 }

  /// No ring.
  pure val NO_RING: Bell = { sat: "", name: "", left: 0, rung: 0, stopped: false }

  /// The HA state of each helper: `"idle"`, `"active"` or `"paused"`.
  var ha: HELPER -> str

  /// The Jarvis table entry of each helper.
  var timer: HELPER -> Entry

  /// The start call of the entry of each helper runs.
  var starting: HELPER -> bool

  /// The order of the next timer.
  var nextOrder: int

  /// The ring of each helper.
  var ring: HELPER -> Bell

  /// The kind of the last step.
  var lastAction: Action

  /// The helper of the last step.
  var lastHelper: HELPER

  /// The helper of the last `SetTimer` was free before the step.
  var lastWasFree: bool

  /// No timers and no rings.
  action init = all {
    ha' = HELPER_SET.mapBy(_ => "idle"),
    timer' = HELPER_SET.mapBy(_ => NO_ENTRY),
    starting' = HELPER_SET.mapBy(_ => false),
    nextOrder' = 0,
    ring' = HELPER_SET.mapBy(_ => NO_RING),
    lastAction' = Other,
    lastHelper' = "h1",
    lastWasFree' = false,
  }

  /// Helper `h` has an entry.
  def owned(h: HELPER): bool = timer.get(h).owned

  /// The HA timer of `h` runs or is paused.
  def live(h: HELPER): bool = ha.get(h) == "active" or ha.get(h) == "paused"

  /// Helper `h` is `idle` in HA and has no entry.
  def free(h: HELPER): bool = ha.get(h) == "idle" and not(owned(h))

  /// The first free helper, or `""` when none is free.
  val firstFree: HELPER = HELPERS.foldl("", (found, h) => if (found == "" and free(h)) h else found)

  /// An entry has the name `n`, and `n` is not `""`.
  def taken(n: str): bool = n != "" and HELPER_SET.exists(h => owned(h) and timer.get(h).name == n)

  /// Ring `r` after a stop: no ring left, and `stopped`.
  pure def hush(r: Bell): Bell = r.with("left", 0).with("stopped", true)

  /// Takes the new tables, keeps `nextOrder`, and records a step of kind `a` on `h`.
  action move(a: Action, h: HELPER, newHa: HELPER -> str, newTimer: HELPER -> Entry, newStarting: HELPER -> bool, newRing: HELPER -> Bell): bool = all {
    ha' = newHa,
    timer' = newTimer,
    starting' = newStarting,
    nextOrder' = nextOrder,
    ring' = newRing,
    lastAction' = a,
    lastHelper' = h,
    lastWasFree' = false,
  }

  /// Keeps every variable but the history.
  action unchanged = move(Other, lastHelper, ha, timer, starting, ring)

  /// A request on `s` sets a timer with the name `n` on the first free helper. A taken name or no free helper changes nothing.
  action setTimer(s: SATELLITE, n: str): bool = {
    val h = firstFree
    if (taken(n) or h == "") unchanged
    else all {
      ha' = ha,
      timer' = timer.set(h, { owned: true, sat: s, name: n, order: nextOrder }),
      starting' = starting.set(h, true),
      nextOrder' = nextOrder + 1,
      ring' = ring,
      lastAction' = SetTimer,
      lastHelper' = h,
      lastWasFree' = free(h),
    }
  }

  /// The start call of `h` succeeds.
  action started(h: HELPER): bool = all {
    starting.get(h),
    move(Other, h, ha.set(h, "active"), timer, starting.set(h, false), ring),
  }

  /// The start call of `h` fails, and its entry goes.
  action startFailed(h: HELPER): bool = all {
    starting.get(h),
    move(Other, h, ha, timer.set(h, NO_ENTRY), starting.set(h, false), ring),
  }

  /// Jarvis cancels the timer of `h`.
  action cancel(h: HELPER): bool = all {
    owned(h),
    not(starting.get(h)),
    live(h),
    move(Other, h, ha.set(h, "idle"), timer.set(h, NO_ENTRY), starting, ring),
  }

  /// Jarvis pauses the timer of `h`.
  action pause(h: HELPER): bool = all {
    owned(h),
    ha.get(h) == "active",
    move(Other, h, ha.set(h, "paused"), timer, starting, ring),
  }

  /// Jarvis resumes the timer of `h`.
  action resume(h: HELPER): bool = all {
    owned(h),
    ha.get(h) == "paused",
    move(Other, h, ha.set(h, "active"), timer, starting, ring),
  }

  /// A person or an automation starts `h` in HA.
  action outside(h: HELPER): bool = all {
    free(h),
    move(Other, h, ha.set(h, "active"), timer, starting, ring),
  }

  /// HA ends the timer of `h` and sends `timer.finished`. A timer of Jarvis starts a ring, and its entry goes.
  action finish(h: HELPER): bool = {
    val e = timer.get(h)
    val bell = { sat: e.sat, name: e.name, left: RINGS, rung: 0, stopped: false }
    all {
      ha.get(h) == "active",
      move(Other, h, ha.set(h, "idle"), timer.set(h, NO_ENTRY), starting, if (e.owned) ring.set(h, bell) else ring),
    }
  }

  /// A person cancels the timer of `h` in HA, and HA sends `timer.cancelled`.
  action haCancel(h: HELPER): bool = all {
    live(h),
    move(Other, h, ha.set(h, "idle"), timer.set(h, NO_ENTRY), starting, ring),
  }

  /// The timer of `h` ends or is cancelled while Jarvis gets no event. The entry stays.
  action quietEnd(h: HELPER): bool = all {
    live(h),
    move(Other, h, ha.set(h, "idle"), timer, starting, ring),
  }

  /// Connects to HA with the start calls `starts` and the rings `rings`: each entry that does not start and whose helper is `idle` goes.
  action connect(starts: HELPER -> bool, rings: HELPER -> Bell): bool = {
    val kept = HELPER_SET.mapBy(h => if (ha.get(h) == "idle" and not(starts.get(h))) NO_ENTRY else timer.get(h))
    move(Other, lastHelper, ha, kept, starts, rings)
  }

  /// Jarvis connects to HA again. The start calls continue.
  action reconnect = connect(starting, ring)

  /// Jarvis starts again with its saved table. No start call and no ring survives.
  action restart = connect(HELPER_SET.mapBy(_ => false), HELPER_SET.mapBy(h => ring.get(h).with("left", 0)))

  /// One ring of `h` on its satellite.
  action ringOnce(h: HELPER): bool = {
    val r = ring.get(h)
    all {
      r.left > 0,
      move(Ring, h, ha, timer, starting, ring.set(h, r.with("left", r.left - 1).with("rung", r.rung + 1))),
    }
  }

  /// The user says "stop" on `s`, which stops each ring of `s`.
  action stop(s: SATELLITE): bool =
    move(Other, lastHelper, ha, timer, starting, HELPER_SET.mapBy(h => if (ring.get(h).sat == s) hush(ring.get(h)) else ring.get(h)))

  /// A request cancels the ringing timer of `h` by name.
  action silence(h: HELPER): bool = {
    val r = ring.get(h)
    all {
      r.left > 0,
      r.name != "",
      move(Other, h, ha, timer, starting, ring.set(h, hush(r))),
    }
  }

  /// Any enabled event.
  action step = {
    nondet h = HELPER_SET.oneOf()
    nondet s = SATELLITES.oneOf()
    nondet n = NAMES.oneOf()
    any {
      setTimer(s, n),
      started(h),
      startFailed(h),
      cancel(h),
      pause(h),
      resume(h),
      outside(h),
      finish(h),
      haCancel(h),
      quietEnd(h),
      reconnect,
      restart,
      ringOnce(h),
      stop(s),
      silence(h),
    }
  }

  /// No two entries have the same name, except `""`.
  val UniqueNames = HELPER_SET.forall(a => HELPER_SET.forall(b => {
    val x = timer.get(a)
    val y = timer.get(b)
    (a != b and x.owned and y.owned and x.name == y.name) implies x.name == ""
  }))

  /// `setTimer` adds an entry only on a free helper.
  val StartOnFreeHelper = lastAction == SetTimer implies lastWasFree

  /// Each ring rings at most `RINGS` times.
  val RingsBounded = HELPER_SET.forall(h => ring.get(h).rung <= RINGS)

  /// A stopped ring never rings again.
  val NoRingAfterStop = lastAction == Ring implies not(ring.get(lastHelper).stopped)

  /// The invariants that `just quint` checks.
  val allInvariants = UniqueNames and StartOnFreeHelper and RingsBounded and NoRingAfterStop

  /// With two entries, `setTimer` changes nothing. With an entry "chicken", `setTimer(s, "chicken")` changes nothing.
  run ac_19_1_3_full_and_taken = init
    .then(setTimer("s1", "chicken"))
    .then(setTimer("s2", "chicken"))
    .expect(all {
      timer.get("h1") == { owned: true, sat: "s1", name: "chicken", order: 0 },
      timer.get("h2") == NO_ENTRY,
      starting.get("h2") == false,
      nextOrder == 1,
    })
    .then(setTimer("s2", ""))
    .then(setTimer("s1", ""))
    .expect(all {
      timer.get("h1") == { owned: true, sat: "s1", name: "chicken", order: 0 },
      timer.get("h2") == { owned: true, sat: "s2", name: "", order: 1 },
      nextOrder == 2,
      lastAction == Other,
    })

  /// After `cancel`, `haCancel`, or `quietEnd` and `reconnect`, the helper has no entry and keeps the ring of its last timer.
  run ac_19_1_4_cancel_never_rings = {
    val first = { sat: "s1", name: "", left: RINGS, rung: 0, stopped: false }
    val gone = timer.get("h1") == NO_ENTRY and ring.get("h1") == first
    init
      .then(setTimer("s1", ""))
      .then(started("h1"))
      .then(finish("h1"))
      .then(setTimer("s2", "chicken"))
      .then(started("h1"))
      .then(cancel("h1"))
      .expect(gone)
      .then(setTimer("s2", "chicken"))
      .then(started("h1"))
      .then(haCancel("h1"))
      .expect(gone)
      .then(setTimer("s2", "chicken"))
      .then(started("h1"))
      .then(quietEnd("h1"))
      .then(reconnect)
      .expect(gone)
  }

  /// The satellite that set the timer rings, and only "stop" on that satellite ends the ring.
  run ac_19_1_5_ring_on_setting_satellite = init
    .then(setTimer("s2", "chicken"))
    .then(started("h1"))
    .then(finish("h1"))
    .expect(ring.get("h1") == { sat: "s2", name: "chicken", left: RINGS, rung: 0, stopped: false })
    .then(stop("s1"))
    .expect(ring.get("h1").left == RINGS and not(ring.get("h1").stopped))
    .then(ringOnce("h1"))
    .then(stop("s2"))
    .then(ringOnce("h1").fail())

  /// A helper takes a new timer while the ring of its last timer continues.
  run ac_19_1_6_reuse_while_ringing = init
    .then(setTimer("s1", ""))
    .then(started("h1"))
    .then(finish("h1"))
    .then(ringOnce("h1"))
    .then(setTimer("s2", "chicken"))
    .expect(all {
      timer.get("h1") == { owned: true, sat: "s2", name: "chicken", order: 1 },
      ring.get("h1").left == RINGS - 1,
    })

  /// A restart keeps the entry of an active helper, and its end rings.
  run ac_19_1_7_restart_keeps_running_timers = init
    .then(setTimer("s1", "chicken"))
    .then(started("h1"))
    .then(restart)
    .expect(timer.get("h1").owned and ha.get("h1") == "active")
    .then(finish("h1"))
    .expect(ring.get("h1") == { sat: "s1", name: "chicken", left: RINGS, rung: 0, stopped: false })
}
