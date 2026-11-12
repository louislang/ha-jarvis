// Feedback: good and bad marks on decisions, and the decisions that `decide` chooses.
module feedback {
  /// A normalized request text.
  type TEXT = str

  /// A decision: an intent with its targets.
  type DECISION = str

  /// A mark value.
  type Value = Good | Bad

  /// A value that can be absent.
  type Option[a] = Some(a) | None

  /// The request texts.
  pure val TEXTS: Set[TEXT] = Set("t")

  /// The decisions.
  pure val DECISIONS: Set[DECISION] = Set("d1", "d2")

  /// The decisions in candidate order.
  pure val CANDIDATES: List[DECISION] = ["d1", "d2"]

  /// The good marks of each text and decision.
  var good: (TEXT, DECISION) -> int

  /// The bad marks of each text and decision.
  var bad: (TEXT, DECISION) -> int

  /// The decision of the last `decide`.
  var chosen: Option[DECISION]

  /// The last `decide` sent the request to System Two.
  var toSystemTwo: bool

  /// `chosen` was blocked at the last `decide` step.
  var chosenWasBlocked: bool

  /// The decision `d` for text `t` has more bad marks than good marks.
  def blocked(t: TEXT, d: DECISION): bool = bad.get((t, d)) > good.get((t, d))

  /// Keeps the decision state.
  action keepDecision = all {
    chosen' = chosen,
    toSystemTwo' = toSystemTwo,
    chosenWasBlocked' = chosenWasBlocked,
  }

  /// No marks and no decision.
  action init = all {
    good' = tuples(TEXTS, DECISIONS).mapBy(_ => 0),
    bad' = tuples(TEXTS, DECISIONS).mapBy(_ => 0),
    chosen' = None,
    toSystemTwo' = false,
    chosenWasBlocked' = false,
  }

  /// Marks decision `d` for text `t` good.
  action markGood(t: TEXT, d: DECISION): bool = all {
    good' = good.set((t, d), good.get((t, d)) + 1),
    bad' = bad,
    keepDecision,
  }

  /// Marks decision `d` for text `t` bad.
  action markBad(t: TEXT, d: DECISION): bool = all {
    bad' = bad.set((t, d), bad.get((t, d)) + 1),
    good' = good,
    keepDecision,
  }

  /// Removes an earlier mark with `value` on decision `d` for text `t`.
  action unmark(t: TEXT, d: DECISION, value: Value): bool = all {
    match value {
      | Good => all { good.get((t, d)) > 0, good' = good.set((t, d), good.get((t, d)) - 1), bad' = bad }
      | Bad => all { bad.get((t, d)) > 0, bad' = bad.set((t, d), bad.get((t, d)) - 1), good' = good }
    },
    keepDecision,
  }

  /// Chooses the first candidate for text `t` that is not blocked, else sends the request to System Two.
  action decide(t: TEXT): bool = {
    val open = CANDIDATES.select(d => not(blocked(t, d)))
    val first = if (open.length() > 0) Some(open[0]) else None
    all {
      chosen' = first,
      toSystemTwo' = first == None,
      chosenWasBlocked' = match first {
        | Some(d) => blocked(t, d)
        | None => false
      },
      good' = good,
      bad' = bad,
    }
  }

  /// Any enabled event.
  action step = {
    nondet t = TEXTS.oneOf()
    nondet d = DECISIONS.oneOf()
    nondet value = Set(Good, Bad).oneOf()
    any {
      markGood(t, d),
      markBad(t, d),
      unmark(t, d, value),
      decide(t),
    }
  }

  /// `decide` never chooses a blocked decision.
  val DecideAvoidsBlocked = not(chosenWasBlocked)

  /// The invariants that `just quint` checks.
  val allInvariants = DecideAvoidsBlocked

  /// One bad and two good marks leave the decision not blocked.
  run ac_12_2_3_bad_then_two_good = init
    .then(markBad("t", "d1"))
    .expect(blocked("t", "d1"))
    .then(markGood("t", "d1"))
    .then(markGood("t", "d1"))
    .expect(not(blocked("t", "d1")))
}
