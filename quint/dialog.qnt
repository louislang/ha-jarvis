// Dialogs: open dialogs on several satellites, with turns, cancel, timeout and deferred jobs in flight.
module dialog {
  /// The key of a dialog: a satellite.
  type KEY = str

  /// The ID of a request.
  type REQUEST = int

  /// The satellites.
  pure val KEYS: Set[KEY] = Set("s1", "s2")

  /// The requests that users can start.
  pure val REQUESTS: Set[REQUEST] = 1.to(3)

  /// The most questions in one dialog.
  pure val MAX_TURNS = 3

  /// The time after the last question that closes a dialog.
  pure val TIMEOUT = 3

  /// The current time.
  var now: int

  /// The questions of the open dialog of each key. 0 is no open dialog.
  var turns: KEY -> int

  /// The time of the last question of each dialog.
  var asked: KEY -> int

  /// The request of each dialog.
  var request: KEY -> REQUEST

  /// The generation of each key. Each new request, question of a job, fill and cancel increases it.
  var gen: KEY -> int

  /// The deferred job of each request runs.
  var running: REQUEST -> bool

  /// The key of the job of each request.
  var jobKey: REQUEST -> KEY

  /// The generation of the key at the start of the job of each request.
  var jobGen: REQUEST -> int

  /// The request acted.
  var calls: REQUEST -> bool

  /// The user cancelled the request.
  var cancelled: REQUEST -> bool

  /// The ID of the next request.
  var nextRequest: REQUEST

  /// Key `k` has an open dialog.
  def isOpen(k: KEY): bool = turns.get(k) > 0

  /// `gen` with the generation of `k` increased.
  def bump(k: KEY): KEY -> int = gen.set(k, gen.get(k) + 1)

  /// No dialogs, no jobs and no requests.
  action init = all {
    now' = 0,
    turns' = KEYS.mapBy(_ => 0),
    asked' = KEYS.mapBy(_ => 0),
    request' = KEYS.mapBy(_ => 1),
    gen' = KEYS.mapBy(_ => 0),
    running' = REQUESTS.mapBy(_ => false),
    jobKey' = REQUESTS.mapBy(_ => "s1"),
    jobGen' = REQUESTS.mapBy(_ => 0),
    calls' = REQUESTS.mapBy(_ => false),
    cancelled' = REQUESTS.mapBy(_ => false),
    nextRequest' = 1,
  }

  /// Opens a dialog on `k` for request `r` and increases the generation of `k`.
  action openDialog(k: KEY, r: REQUEST): bool = all {
    turns' = turns.set(k, 1),
    asked' = asked.set(k, now),
    request' = request.set(k, r),
    gen' = bump(k),
  }

  /// Closes the dialog on `k` and increases the generation of `k`.
  action closeDialog(k: KEY): bool = all {
    turns' = turns.set(k, 0),
    asked' = asked,
    request' = request,
    gen' = bump(k),
  }

  /// The jobs do not change.
  action keepJobs: bool = all {
    running' = running,
    jobKey' = jobKey,
    jobGen' = jobGen,
  }

  /// A new request on `k` asks a question.
  action open(k: KEY): bool = all {
    REQUESTS.contains(nextRequest),
    openDialog(k, nextRequest),
    keepJobs,
    nextRequest' = nextRequest + 1,
    now' = now,
    calls' = calls,
    cancelled' = cancelled,
  }

  /// A new request on `k` starts a deferred job.
  action defer(k: KEY): bool = all {
    REQUESTS.contains(nextRequest),
    gen' = bump(k),
    running' = running.set(nextRequest, true),
    jobKey' = jobKey.set(nextRequest, k),
    jobGen' = jobGen.set(nextRequest, gen.get(k) + 1),
    nextRequest' = nextRequest + 1,
    now' = now,
    turns' = turns,
    asked' = asked,
    request' = request,
    calls' = calls,
    cancelled' = cancelled,
  }

  /// The job of request `r` asks the user and ends. Only `fill` can then act for `r`.
  action ask(r: REQUEST): bool = all {
    running.get(r),
    jobGen.get(r) == gen.get(jobKey.get(r)),
    openDialog(jobKey.get(r), r),
    keepJobs,
    now' = now,
    calls' = calls,
    cancelled' = cancelled,
    nextRequest' = nextRequest,
  }

  /// The job of request `r` ends. It acts only when no newer request, question, fill or cancel came on its key.
  action jobAct(r: REQUEST): bool = all {
    running.get(r),
    running' = running.set(r, false),
    calls' = if (jobGen.get(r) == gen.get(jobKey.get(r))) calls.set(r, true) else calls,
    jobKey' = jobKey,
    jobGen' = jobGen,
    now' = now,
    turns' = turns,
    asked' = asked,
    request' = request,
    gen' = gen,
    cancelled' = cancelled,
    nextRequest' = nextRequest,
  }

  /// The answer on `k` is not clear. Jarvis asks again, or closes the dialog at the turn limit.
  action askAgain(k: KEY): bool = all {
    isOpen(k),
    turns' = if (turns.get(k) < MAX_TURNS) turns.set(k, turns.get(k) + 1) else turns.set(k, 0),
    asked' = asked.set(k, now),
    request' = request,
    gen' = gen,
    keepJobs,
    now' = now,
    calls' = calls,
    cancelled' = cancelled,
    nextRequest' = nextRequest,
  }

  /// The answer on `k` completes the request of the dialog.
  action fill(k: KEY): bool = all {
    isOpen(k),
    closeDialog(k),
    calls' = calls.set(request.get(k), true),
    keepJobs,
    now' = now,
    cancelled' = cancelled,
    nextRequest' = nextRequest,
  }

  /// The user cancels the dialog on `k`.
  action cancel(k: KEY): bool = all {
    isOpen(k),
    closeDialog(k),
    cancelled' = cancelled.set(request.get(k), true),
    keepJobs,
    now' = now,
    calls' = calls,
    nextRequest' = nextRequest,
  }

  /// Time advances. A dialog closes `TIMEOUT` after its last question.
  action tick = all {
    now' = now + 1,
    turns' = KEYS.mapBy(k => if (now + 1 - asked.get(k) >= TIMEOUT) 0 else turns.get(k)),
    asked' = asked,
    request' = request,
    gen' = gen,
    keepJobs,
    calls' = calls,
    cancelled' = cancelled,
    nextRequest' = nextRequest,
  }

  /// Any enabled event.
  action step = any {
    nondet k = KEYS.oneOf()
    any {
      open(k),
      defer(k),
      askAgain(k),
      fill(k),
      cancel(k),
    },
    nondet r = REQUESTS.oneOf()
    any {
      ask(r),
      jobAct(r),
    },
    tick,
  }

  /// Every dialog has at most `MAX_TURNS` questions.
  val TurnLimit = KEYS.forall(k => turns.get(k) <= MAX_TURNS)

  /// No cancelled request acts.
  val NoCallAfterCancel = REQUESTS.forall(r => not(calls.get(r) and cancelled.get(r)))

  /// The invariants that `just quint` checks.
  val allInvariants = TurnLimit and NoCallAfterCancel

  /// Request `r` has an open dialog.
  def hasDialog(r: REQUEST): bool = KEYS.exists(k => isOpen(k) and request.get(k) == r)

  /// The job of request `r` runs and can still ask or act.
  def live(r: REQUEST): bool = running.get(r) and jobGen.get(r) == gen.get(jobKey.get(r))

  /// An invariant that implies `allInvariants` and that each step keeps.
  val Inductive = all {
    now.in(Nat),
    turns.in(KEYS.setOfMaps(0.to(MAX_TURNS))),
    asked.in(KEYS.setOfMaps(Nat)),
    request.in(KEYS.setOfMaps(REQUESTS)),
    gen.in(KEYS.setOfMaps(Nat)),
    running.in(REQUESTS.setOfMaps(Bool)),
    jobKey.in(REQUESTS.setOfMaps(KEYS)),
    jobGen.in(REQUESTS.setOfMaps(Nat)),
    calls.in(REQUESTS.setOfMaps(Bool)),
    cancelled.in(REQUESTS.setOfMaps(Bool)),
    nextRequest.in(1.to(4)),
    NoCallAfterCancel,
    // A request that has not started has no job, dialog, call or cancel.
    REQUESTS.forall(r => r >= nextRequest implies not(running.get(r) or hasDialog(r) or calls.get(r) or cancelled.get(r))),
    // A job never has a newer generation than its key.
    REQUESTS.forall(r => running.get(r) implies jobGen.get(r) <= gen.get(jobKey.get(r))),
    // A live job has no dialog, call or cancel.
    REQUESTS.forall(r => live(r) implies not(hasDialog(r) or calls.get(r) or cancelled.get(r))),
    // The request of an open dialog has no call or cancel, and no other open dialog.
    KEYS.forall(k => isOpen(k) implies not(calls.get(request.get(k)) or cancelled.get(request.get(k)))),
    KEYS.forall(k => KEYS.forall(l => (k != l and isOpen(k) and isOpen(l)) implies request.get(k) != request.get(l))),
  }

  /// Questions, a deferred job, a cancel and a fill on `s1` leave the dialog of `s2` as it was.
  run ac_10_1_3_two_satellites_independent = init
    .then(open("s2"))
    .then(open("s1"))
    .then(askAgain("s1"))
    .then(cancel("s1"))
    .then(defer("s1"))
    .then(ask(3))
    .then(fill("s1"))
    .expect(turns.get("s2") == 1 and asked.get("s2") == 0 and request.get("s2") == 1)

  /// A job that asked before a cancel makes no call after it.
  run ac_10_1_4_job_after_cancel = init
    .then(defer("s1"))
    .then(ask(1))
    .then(cancel("s1"))
    .then(jobAct(1))
    .expect(not(calls.get(1)) and cancelled.get(1) and not(running.get(1)))
}
