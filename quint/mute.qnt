// Mute: the mute switches of the satellites, the voice mutes of the desk, their timed end and the training mutes.
module mute {
  /// A satellite.
  type SATELLITE = str

  /// The `switch.turn_on` call of a voice mute. `live = false` is no call.
  type Call = { live: bool, timed: bool, end: int }

  /// The timed voice mute of the desk, with the `version` of the `on` state of its call. `live = false` is no mute.
  type Mute = { live: bool, end: int, version: int }

  /// The satellites.
  pure val SATELLITES: Set[SATELLITE] = Set("s1", "s2")

  /// The time of a timed mute, in ticks.
  pure val LENGTH = 2

  /// The switch of each satellite is `on` (muted).
  var on: SATELLITE -> bool

  /// The `last_updated` of each switch: each change of its state adds 1.
  var version: SATELLITE -> int

  /// The mute call of each satellite.
  var calling: SATELLITE -> Call

  /// The timed voice mute of each satellite.
  var mute: SATELLITE -> Mute

  /// A person changed the switch after the last mute call.
  var pressed: SATELLITE -> bool

  /// The training run turned the switch `on`.
  var trainMuted: SATELLITE -> bool

  /// A local training run holds the desk.
  var training: bool

  /// The time in ticks.
  var now: int

  /// The last step unmuted the switch.
  var unmuted: SATELLITE -> bool

  /// The end of the mute that the last step unmuted.
  var unmutedEnd: SATELLITE -> int

  /// `pressed` at the unmute of the last step.
  var unmutedPressed: SATELLITE -> bool

  /// No call.
  pure val NO_CALL: Call = { live: false, timed: false, end: 0 }

  /// No mute.
  pure val NO_MUTE: Mute = { live: false, end: 0, version: 0 }

  /// Takes the new state. With `runDue`, `due` then ends each live mute whose end passed outside training, and unmutes its switch when it still has the version of the mute. Records the unmutes of the step.
  action settle(
    newOn: SATELLITE -> bool,
    newVersion: SATELLITE -> int,
    newCalling: SATELLITE -> Call,
    newMute: SATELLITE -> Mute,
    newPressed: SATELLITE -> bool,
    newTrainMuted: SATELLITE -> bool,
    newTraining: bool,
    newNow: int,
    runDue: bool,
  ): bool = {
    val ending = if (runDue and not(newTraining)) SATELLITES.filter(s => newMute.get(s).live and newMute.get(s).end <= newNow) else Set()
    val off = ending.filter(s => newOn.get(s) and newVersion.get(s) == newMute.get(s).version)
    all {
      on' = SATELLITES.mapBy(s => newOn.get(s) and not(off.contains(s))),
      version' = SATELLITES.mapBy(s => if (off.contains(s)) newVersion.get(s) + 1 else newVersion.get(s)),
      calling' = newCalling,
      mute' = SATELLITES.mapBy(s => if (ending.contains(s)) newMute.get(s).with("live", false) else newMute.get(s)),
      pressed' = newPressed,
      trainMuted' = newTrainMuted,
      training' = newTraining,
      now' = newNow,
      unmuted' = SATELLITES.mapBy(s => off.contains(s)),
      unmutedEnd' = SATELLITES.mapBy(s => if (off.contains(s)) newMute.get(s).end else 0),
      unmutedPressed' = SATELLITES.mapBy(s => off.contains(s) and newPressed.get(s)),
    }
  }

  /// All switches `off`, no calls, no mutes and no training.
  action init = settle(
    SATELLITES.mapBy(_ => false),
    SATELLITES.mapBy(_ => 0),
    SATELLITES.mapBy(_ => NO_CALL),
    SATELLITES.mapBy(_ => NO_MUTE),
    SATELLITES.mapBy(_ => false),
    SATELLITES.mapBy(_ => false),
    false,
    0,
    false,
  )

  /// A voice mute of `s`, timed or not, starts its `switch.turn_on` call.
  action voiceMute(s: SATELLITE, timed: bool): bool = all {
    not(on.get(s)),
    not(calling.get(s).live),
    not(training),
    settle(on, version, calling.set(s, { live: true, timed: timed, end: now + LENGTH }), mute, pressed, trainMuted, training, now, false),
  }

  /// The call of `s` succeeds: the switch is `on`, a timed call stores its mute and an untimed call removes it, then `due`.
  action callDone(s: SATELLITE): bool = {
    val call = calling.get(s)
    val newVersion = if (on.get(s)) version.get(s) else version.get(s) + 1
    val newMute = if (call.timed) { live: true, end: call.end, version: newVersion } else mute.get(s).with("live", false)
    all {
      call.live,
      settle(
        on.set(s, true),
        version.set(s, newVersion),
        calling.set(s, call.with("live", false)),
        mute.set(s, newMute),
        pressed.set(s, false),
        trainMuted,
        training,
        now,
        true,
      ),
    }
  }

  /// The call of `s` fails.
  action callFailed(s: SATELLITE): bool = all {
    calling.get(s).live,
    settle(on, version, calling.set(s, calling.get(s).with("live", false)), mute, pressed, trainMuted, training, now, false),
  }

  /// A person changes the switch of `s` with the button, in HA or in the Jarvis tab.
  action press(s: SATELLITE): bool = settle(
    on.set(s, not(on.get(s))),
    version.set(s, version.get(s) + 1),
    calling,
    mute,
    pressed.set(s, true),
    trainMuted,
    training,
    now,
    false,
  )

  /// The time passes, then `due`.
  action tick = settle(on, version, calling, mute, pressed, trainMuted, training, now + 1, true)

  /// A local training run holds the desk and turns `on` each switch that is `off`.
  action trainStart = all {
    not(training),
    settle(
      SATELLITES.mapBy(_ => true),
      SATELLITES.mapBy(s => if (on.get(s)) version.get(s) else version.get(s) + 1),
      calling,
      mute,
      pressed,
      SATELLITES.mapBy(s => not(on.get(s))),
      true,
      now,
      false,
    ),
  }

  /// The training run turns `off` each switch that it turned `on` and drops its hold, then `due`. A switch that is `off` already keeps its version.
  action trainEnd = all {
    training,
    settle(
      SATELLITES.mapBy(s => on.get(s) and not(trainMuted.get(s))),
      SATELLITES.mapBy(s => if (on.get(s) and trainMuted.get(s)) version.get(s) + 1 else version.get(s)),
      calling,
      mute,
      pressed,
      SATELLITES.mapBy(_ => false),
      false,
      now,
      true,
    ),
  }

  /// Jarvis restarts: the running calls drop and the stored mutes stay, then `due`.
  action restart = all {
    not(training),
    settle(on, version, SATELLITES.mapBy(s => calling.get(s).with("live", false)), mute, pressed, trainMuted, training, now, true),
  }

  /// Any enabled event.
  action step = {
    nondet s = SATELLITES.oneOf()
    nondet timed = Bool.oneOf()
    any {
      voiceMute(s, timed),
      callDone(s),
      callFailed(s),
      press(s),
      tick,
      trainStart,
      trainEnd,
      restart,
    }
  }

  /// Jarvis never unmutes a switch that a person changed after the mute call.
  val NoUnmuteAfterPress = SATELLITES.forall(s => not(unmuted.get(s) and unmutedPressed.get(s)))

  /// Jarvis never unmutes before the end of the mute.
  val NoEarlyUnmute = SATELLITES.forall(s => unmuted.get(s) implies unmutedEnd.get(s) <= now)

  /// Jarvis never unmutes during a local training run.
  val NoUnmuteInTraining = SATELLITES.forall(s => not(unmuted.get(s) and training))

  /// The invariants that `just quint` checks.
  val allInvariants = NoUnmuteAfterPress and NoEarlyUnmute and NoUnmuteInTraining

  /// A timed voice mute of `s1` whose call succeeded.
  run timedMute = init.then(voiceMute("s1", true)).then(callDone("s1"))

  /// A timed mute unmutes at its end.
  run ac_20_4_3_timed_mute_ends = timedMute
    .then(tick)
    .expect(on.get("s1"))
    .then(tick)
    .expect(not(on.get("s1")))

  /// A change by a person keeps the mute after its end, and an untimed mute never ends.
  run ac_20_4_4_person_wins = timedMute
    .then(press("s1"))
    .then(press("s1"))
    .then(tick)
    .then(tick)
    .expect(on.get("s1"))
    .then(init)
    .then(voiceMute("s1", false))
    .then(callDone("s1"))
    .then(tick)
    .then(tick)
    .then(tick)
    .expect(on.get("s1"))

  /// The end of a timed mute waits for the end of a training run.
  run ac_20_4_5_training_holds = timedMute
    .then(trainStart)
    .then(tick)
    .then(tick)
    .expect(on.get("s1"))
    .then(trainEnd)
    .expect(not(on.get("s1")))

  /// A restart keeps the timed mute, and the actions on `s1` leave `s2` alone.
  run ac_20_4_6_restart_and_satellites = timedMute
    .then(restart)
    .then(tick)
    .then(tick)
    .expect(all { not(on.get("s1")), not(on.get("s2")) })
    .then(voiceMute("s1", false))
    .then(callFailed("s1"))
    .then(press("s1"))
    .expect(all { on.get("s1"), not(on.get("s2")) })
}
