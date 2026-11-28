// Training: the trigger, the run, the local and RunPod attempts, the pause, the accept rule and the swap.
module training {
  /// Where the current run trains.
  type Run = Idle | Local | RunPod

  /// A step of the model. A done step has the accuracy of the new model.
  type Event = Init | Change | AutoStart | VoiceStart | LocalDone(int) | LocalFail | RunpodDone(int) | RunpodFail

  /// The change count that starts a run.
  pure val MIN = 2

  /// The accuracies of a model.
  pure val ACC = Set(0, 1, 2)

  /// The largest drop of the accuracy that a new model can have: one level of `ACC`.
  pure val MARGIN = 1

  /// A RunPod fallback is configured. `init` chooses it, `initLocal` sets false and no action changes it, so each check covers both values.
  var RUNPOD: bool

  /// The change count.
  var changes: int

  /// The changes since the run started.
  var during: int

  /// Where the current run trains.
  var runState: Run

  /// Jarvis has released its models for the local attempt.
  var paused: bool

  /// The number of started runs.
  var starts: int

  /// The number of ended runs.
  var ends: int

  /// The accuracy of the installed model.
  var modelAcc: int

  /// `modelAcc` before the last step.
  var prevAcc: int

  /// The swaps, as the old and the new accuracy.
  var swaps: List[(int, int)]

  /// The attempts of the current run, in order.
  var attempts: List[Run]

  /// The last step.
  var lastAction: Event

  /// Sets the history of step `e` and keeps `RUNPOD`.
  action record(e: Event): bool = all {
    RUNPOD' = RUNPOD,
    prevAcc' = modelAcc,
    lastAction' = e,
  }

  /// No run and no changes, with the fallback `runpod`. The installed model has the lowest accuracy.
  action initWith(runpod: bool): bool = all {
    RUNPOD' = runpod,
    changes' = 0,
    during' = 0,
    runState' = Idle,
    paused' = false,
    starts' = 0,
    ends' = 0,
    modelAcc' = 0,
    prevAcc' = 0,
    swaps' = List(),
    attempts' = List(),
    lastAction' = Init,
  }

  /// The start with or without a RunPod fallback.
  action init = {
    nondet runpod = Bool.oneOf()
    initWith(runpod)
  }

  /// The start without a RunPod fallback, which the conformance test replays.
  action initLocal = initWith(false)

  /// The start with a RunPod fallback, which the conformance test replays.
  action initRunPod = initWith(true)

  /// A new action mark.
  action change = all {
    record(Change),
    changes' = changes + 1,
    during' = if (runState != Idle) during + 1 else during,
    runState' = runState,
    paused' = paused,
    starts' = starts,
    ends' = ends,
    modelAcc' = modelAcc,
    swaps' = swaps,
    attempts' = attempts,
  }

  /// Starts a run with a local attempt and pauses Jarvis.
  action start(e: Event): bool = all {
    runState == Idle,
    record(e),
    runState' = Local,
    paused' = true,
    starts' = starts + 1,
    during' = 0,
    attempts' = List(Local),
    changes' = changes,
    ends' = ends,
    modelAcc' = modelAcc,
    swaps' = swaps,
  }

  /// The change count reaches `MIN`.
  action autoStart = all {
    changes >= MIN,
    start(AutoStart),
  }

  /// A voice request starts a run at any count.
  action voiceStart = start(VoiceStart)

  /// Ends the run with a new model of accuracy `a`: swaps when it is at most `MARGIN` lower, and keeps only the changes made during the run.
  action finish(a: int): bool = all {
    swaps' = if (a + MARGIN >= modelAcc) swaps.append((modelAcc, a)) else swaps,
    modelAcc' = if (a + MARGIN >= modelAcc) a else modelAcc,
    runState' = Idle,
    ends' = ends + 1,
    changes' = during,
    during' = during,
    starts' = starts,
    attempts' = attempts,
  }

  /// Ends the run with a failure. The model and the change count stay.
  action failRun = all {
    runState' = Idle,
    ends' = ends + 1,
    changes' = changes,
    during' = during,
    starts' = starts,
    modelAcc' = modelAcc,
    swaps' = swaps,
    attempts' = attempts,
  }

  /// The local attempt gives a model of accuracy `a`.
  action localDone(a: int): bool = all {
    runState == Local,
    record(LocalDone(a)),
    paused' = false,
    finish(a),
  }

  /// The local attempt fails. With `RUNPOD`, the run continues on RunPod.
  action localFail = all {
    runState == Local,
    record(LocalFail),
    paused' = false,
    if (RUNPOD) all {
      runState' = RunPod,
      attempts' = attempts.append(RunPod),
      changes' = changes,
      during' = during,
      starts' = starts,
      ends' = ends,
      modelAcc' = modelAcc,
      swaps' = swaps,
    } else failRun,
  }

  /// The RunPod attempt gives a model of accuracy `a`.
  action runpodDone(a: int): bool = all {
    runState == RunPod,
    record(RunpodDone(a)),
    paused' = paused,
    finish(a),
  }

  /// The RunPod attempt fails.
  action runpodFail = all {
    runState == RunPod,
    record(RunpodFail),
    paused' = paused,
    failRun,
  }

  /// Any enabled event.
  action step = {
    nondet a = ACC.oneOf()
    any {
      change,
      autoStart,
      voiceStart,
      localDone(a),
      localFail,
      runpodDone(a),
      runpodFail,
    }
  }

  /// The last step ended the run with a failure.
  val finalFailure = lastAction == RunpodFail or (lastAction == LocalFail and runState == Idle)

  /// At most one run occurs at a time.
  val OneRun = starts - ends <= 1

  /// A swap never installs a model more than `MARGIN` lower.
  val SwapWithinMargin = swaps.foldl(true, (ok, s) => ok and s._2 + MARGIN >= s._1)

  /// After a run, the count holds only the changes made during the run.
  val CountResetsAfterRun = match lastAction {
    | LocalDone(_) => changes == during
    | RunpodDone(_) => changes == during
    | _ => true
  }

  /// A failed run keeps the installed model.
  val FailureKeepsModel = finalFailure implies modelAcc == prevAcc

  /// A run has at most one RunPod attempt, and only after a local failure.
  val FallbackOnce = attempts.length() <= 2
    and attempts.indices().forall(i => attempts[i] == (if (i == 0) Local else RunPod))
    and (runState == RunPod implies attempts.length() == 2)

  /// Jarvis pauses only for a local attempt.
  val PauseOnlyLocal = paused == (runState == Local)

  /// The invariants that `just quint` checks.
  val allInvariants = OneRun and SwapWithinMargin and CountResetsAfterRun and FailureKeepsModel and FallbackOnce and PauseOnlyLocal

  /// A rejected run keeps the model and counts only the changes made during it.
  run ac_13_4_3_rejected_run_resets_count = init
    .then(voiceStart)
    .then(localDone(2))
    .then(change)
    .then(change)
    .then(autoStart)
    .then(change)
    .then(localDone(0))
    .expect(all {
      modelAcc == 2,
      swaps == List((0, 2)),
      changes == 1,
    })

  /// A run one level below the model swaps, and a run two levels below it does not.
  run ac_13_13_4_drop_within_margin_swaps = init
    .then(voiceStart)
    .then(localDone(2))
    .then(voiceStart)
    .then(localDone(0))
    .expect(modelAcc == 2)
    .then(voiceStart)
    .then(localDone(1))
    .expect(all {
      modelAcc == 1,
      swaps == List((0, 2), (2, 1)),
    })
}
