// Prefetch: answers fetched before the usual time, refreshed while its window is open, and served only when fresh.
module prefetch {
  /// A value that can be absent.
  type Option[a] = Some(a) | None

  /// A step of the System Two stream of a prefetch.
  type Step = Read | Write | Sentence | Fail

  /// The freshness interval of an answer.
  pure val FRESH = 2

  /// The lead of the window before the usual time.
  pure val LEAD = 1

  /// The usual time.
  pure val USUAL = 4

  /// The end of the hour of the usual time.
  pure val END = 6

  /// The current time.
  var now: int

  /// The fetch time of the stored answer.
  var answer: Option[int]

  /// The time of the last prefetch, with or without success.
  var attempt: Option[int]

  /// The prefetch times.
  var fetches: List[int]

  /// The HA calls that act on the house.
  var writeCalls: int

  /// The HA calls that only read.
  var readCalls: int

  /// The served answers: serve time and answer time.
  var served: List[(int, int)]

  /// No answer, no prefetch and no call.
  action init = all {
    now' = 0,
    answer' = None,
    attempt' = None,
    fetches' = List(),
    writeCalls' = 0,
    readCalls' = 0,
    served' = List(),
  }

  /// Time advances by one unit.
  action tick = all {
    now' = now + 1,
    answer' = answer,
    attempt' = attempt,
    fetches' = fetches,
    writeCalls' = writeCalls,
    readCalls' = readCalls,
    served' = served,
  }

  /// A prefetch starts inside the window, at most once each `FRESH`.
  action prefetch = all {
    USUAL - LEAD <= now,
    now < END,
    match attempt {
      | Some(a) => now - a >= FRESH
      | None => true
    },
    attempt' = Some(now),
    fetches' = fetches.append(now),
    now' = now,
    answer' = answer,
    writeCalls' = writeCalls,
    readCalls' = readCalls,
    served' = served,
  }

  /// The prefetch gets `step` from System Two.
  action prefetchStep(step: Step): bool = all {
    readCalls' = if (step == Read) readCalls + 1 else readCalls,
    answer' = match step {
      | Write => None
      | Sentence => Some(now)
      | _ => answer
    },
    now' = now,
    attempt' = attempt,
    fetches' = fetches,
    writeCalls' = writeCalls,
    served' = served,
  }

  /// A request gets the fresh answer. Else System Two answers it.
  action request = all {
    served' = match answer {
      | Some(a) => if (now - a <= FRESH) served.append((now, a)) else served
      | None => served
    },
    now' = now,
    answer' = answer,
    attempt' = attempt,
    fetches' = fetches,
    writeCalls' = writeCalls,
    readCalls' = readCalls,
  }

  /// Any enabled event.
  action step = {
    nondet s = Set(Read, Write, Sentence, Fail).oneOf()
    any {
      tick,
      prefetch,
      prefetchStep(s),
      request,
    }
  }

  /// A prefetch never acts on the house.
  val NoCallFromPrefetch = writeCalls == 0

  /// Every served answer was fresh when Jarvis served it.
  val ServedAnswerIsFresh = served.indices().forall(i => served[i]._1 - served[i]._2 <= FRESH)

  /// Every prefetch is inside the window.
  val PrefetchOnlyInWindow = fetches.indices().forall(i => USUAL - LEAD <= fetches[i] and fetches[i] < END)

  /// Two neighbor prefetches are `FRESH` or more apart.
  val NoEarlyRefresh = fetches.indices().forall(i => i == 0 or fetches[i] - fetches[i - 1] >= FRESH)

  /// The invariants that `just quint` checks.
  val allInvariants = NoCallFromPrefetch and ServedAnswerIsFresh and PrefetchOnlyInWindow and NoEarlyRefresh
}
