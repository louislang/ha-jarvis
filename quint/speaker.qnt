// Speaker identification from presence, proximity and voice, and passive voice enrollment.
module speaker {
  /// A value that can be absent.
  type Option[a] = Some(a) | None

  /// A `person` entity.
  type PERSON = str

  /// The signal that identified the speaker.
  type Source = Presence | Proximity | Voice | Assigned | NoSource

  /// The people.
  pure val PEOPLE: Set[PERSON] = Set("alex", "sam")

  /// The samples that one profile keeps.
  pure val MAX_SAMPLES = 2

  /// The step bound of `quint verify`: past 6 steps, the int-keyed maps take Apalache hours.
  pure val VERIFY_STEPS = 6

  /// The proximity and voice similarity levels, in tenths.
  pure val LEVELS = Set(0, 5, 10)

  /// The combined score that the top score must exceed, in tenths.
  pure val THRESHOLD = 7

  /// The lead of the top score over the second score, in tenths.
  pure val MARGIN = 2

  /// The proximity that gives the source `Proximity`, in tenths: 1 m or less.
  pure val ENROLL_THRESHOLD = 5

  /// The voice similarity that an enroll into a profile with samples needs, in tenths.
  pure val VOICE_THRESHOLD = 6

  /// The sample count from which an enroll needs a voice match.
  pure val MIN_PROFILE = 1

  /// The people at home, or `None` when presence is not known.
  var home: Option[Set[PERSON]]

  /// The satellite presence sensor, or `None` when the area has none.
  var occupied: Option[bool]

  /// The proximity of each person to the satellite.
  var proximity: PERSON -> int

  /// The voice similarity of the utterance to each profile.
  var voice: PERSON -> int

  /// The identified speaker.
  var result: Option[PERSON]

  /// The signal that identified `result`.
  var source: Source

  /// The interaction ID of the current request.
  var utterance: int

  /// The stored voice samples by interaction ID.
  var samples: int -> (PERSON, Source)

  /// The last pick of each interaction ID: a person or none.
  var picks: int -> Option[PERSON]

  /// The interaction IDs of the samples of person `p`.
  pure def ids(stored: int -> (PERSON, Source), p: PERSON): Set[int] =
    stored.keys().filter(i => stored.get(i)._1 == p)

  /// The candidates: the people at home, or all people when presence is not known.
  val candidates: Set[PERSON] = match home {
    | Some(s) => s
    | None => PEOPLE
  }

  /// Presence alone identifies the speaker: presence is known and one person is at home.
  val byPresence: bool = match home {
    | Some(s) => s.size() == 1
    | None => false
  }

  /// The proximity of person `p` that counts: 0 when the satellite area is not occupied.
  def counted(p: PERSON): int = match occupied {
    | Some(o) => if (o) proximity.get(p) else 0
    | None => proximity.get(p)
  }

  /// The combined score of person `p`.
  def score(p: PERSON): int = counted(p) + voice.get(p)

  /// The candidates whose score is more than `THRESHOLD` and more than each other score plus `MARGIN`.
  val winners: Set[PERSON] =
    candidates.filter(p => score(p) > THRESHOLD and candidates.forall(q => q == p or score(p) > score(q) + MARGIN))

  /// Removes the sample of person `p` with the lowest ID when `p` has more than `MAX_SAMPLES`. One added sample needs one removal.
  pure def trim(stored: int -> (PERSON, Source), p: PERSON): int -> (PERSON, Source) = {
    val own = ids(stored, p)
    val over = own.size() > MAX_SAMPLES
    stored.keys().filter(i => not(over and own.contains(i) and own.forall(j => i <= j))).mapBy(i => stored.get(i))
  }

  /// Keeps the signals of the request.
  action keepSignals: bool = all {
    home' = home,
    occupied' = occupied,
    proximity' = proximity,
    voice' = voice,
    utterance' = utterance,
  }

  /// No request and no samples.
  action init = all {
    home' = None,
    occupied' = None,
    proximity' = PEOPLE.mapBy(_ => 0),
    voice' = PEOPLE.mapBy(_ => 0),
    result' = None,
    source' = NoSource,
    utterance' = 0,
    samples' = Map(),
    picks' = Map(),
  }

  /// A new request with the signals `h`, `o`, `prox` and `v`.
  action setSignals(h: Option[Set[PERSON]], o: Option[bool], prox: PERSON -> int, v: PERSON -> int): bool = all {
    home' = h,
    occupied' = o,
    proximity' = prox,
    voice' = v,
    result' = None,
    source' = NoSource,
    utterance' = utterance + 1,
    samples' = samples,
    picks' = picks,
  }

  /// Rules 1 to 4 of sub-phase 15.6.
  action identify = {
    val person = (if (byPresence) candidates else winners).fold(None, (_, p) => Some(p))
    all {
      keepSignals,
      result' = person,
      source' = match person {
        | Some(p) => if (byPresence) Presence else if (counted(p) >= ENROLL_THRESHOLD) Proximity else Voice
        | None => NoSource
      },
      samples' = samples,
      picks' = picks,
    }
  }

  /// Rule 5 of sub-phase 15.6 for speaker `p`, then the trim of the store of sub-phase 15.3b. Not taking it models a short utterance.
  action enroll(p: PERSON): bool = all {
    result == Some(p),
    source == Presence or source == Proximity,
    not(samples.keys().contains(utterance)),
    not(picks.keys().contains(utterance)),
    ids(samples, p).size() >= MIN_PROFILE implies
      (voice.get(p) >= VOICE_THRESHOLD and PEOPLE.forall(q => q == p or voice.get(p) > voice.get(q))),
    keepSignals,
    result' = result,
    source' = source,
    samples' = trim(samples.put(utterance, (p, source)), p),
    picks' = picks,
  }

  /// A pick of `p` for the request `id`: it moves the sample of `id` to `p`, or removes it.
  action assignVoice(id: int, p: Option[PERSON]): bool = all {
    1 <= id and id <= utterance,
    keepSignals,
    result' = result, source' = source,
    samples' = match p { | Some(q) => trim(samples.put(id, (q, Assigned)), q) | None => samples.keys().exclude(Set(id)).mapBy(i => samples.get(i)) },
    picks' = picks.put(id, p),
  }

  /// Any enabled event.
  action step = {
    nondet known = Bool.oneOf()
    nondet atHome = PEOPLE.powerset().oneOf()
    nondet sensor = Bool.oneOf()
    nondet state = Bool.oneOf()
    val h = if (known) Some(atHome) else None
    val o = if (sensor) Some(state) else None
    nondet prox = PEOPLE.setOfMaps(LEVELS).oneOf()
    nondet v = PEOPLE.setOfMaps(LEVELS).oneOf()
    nondet p = PEOPLE.oneOf()
    nondet back = 0.to(2).oneOf()
    nondet chosen = PEOPLE.map(q => Some(q)).union(Set(None)).oneOf()
    any {
      setSignals(h, o, prox, v),
      identify,
      enroll(p),
      assignVoice(utterance - back, chosen),
    }
  }

  /// The identified speaker is at home, or presence is not known.
  val SpeakerIsCandidate = match result {
    | Some(p) => match home {
        | Some(s) => s.contains(p)
        | None => true
      }
    | None => true
  }

  /// No sample comes from a voice match alone.
  val NoVoiceOnlyEnrollment = samples.keys().forall(i => samples.get(i)._2 != Voice)

  /// Each person has at most `MAX_SAMPLES` samples.
  val ProfilesBounded = PEOPLE.forall(p => ids(samples, p).size() <= MAX_SAMPLES)

  /// A sample of a picked ID has the person of the pick and the source `Assigned`, and a pick of none has no sample.
  val PickWins = samples.keys().forall(i => picks.keys().contains(i) implies (picks.get(i) == Some(samples.get(i)._1) and samples.get(i)._2 == Assigned))

  /// The invariants that `just quint` checks.
  val allInvariants = SpeakerIsCandidate and NoVoiceOnlyEnrollment and ProfilesBounded and PickWins
}
