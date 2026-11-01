// Announce queue of one satellite: requests, the end of the HA pipeline, and the sentences of deferred answers.
module announce {
  /// The number of a request on the satellite. 0 is before any request.
  type REQUEST = int

  /// The number of a sentence in the answer of one request.
  type SENTENCE = int

  /// The requests that users start.
  pure val REQUESTS: Set[REQUEST] = Set(1, 2)

  /// The sentences that an answer can have.
  pure val SENTENCES: Set[SENTENCE] = Set(1, 2)

  /// The satellite entity is `idle` in the hot cache.
  var idle: bool

  /// The newest request on the satellite.
  var newest: REQUEST

  /// The sentences in the order they arrived.
  var arrived: List[(REQUEST, SENTENCE)]

  /// The sentences that wait for the satellite.
  var queue: List[(REQUEST, SENTENCE)]

  /// The announces that started, with the newest request at the start.
  var started: List[(REQUEST, SENTENCE, REQUEST)]

  /// An announce plays.
  var busy: bool

  /// The sentences of request `r` in `entries`, in order.
  pure def sentencesOf(entries: List[(REQUEST, SENTENCE)], r: REQUEST): List[SENTENCE] =
    entries.foldl(List(), (acc, e) => if (e._1 == r) acc.append(e._2) else acc)

  /// The request and sentence of each started announce.
  val startedSentences: List[(REQUEST, SENTENCE)] = started.foldl(List(), (acc, e) => acc.append((e._1, e._2)))

  /// An idle satellite before any request.
  action init = all {
    idle' = true,
    newest' = 0,
    arrived' = List(),
    queue' = List(),
    started' = List(),
    busy' = false,
  }

  /// A user starts request `r`. The new pipeline stops a playing announce.
  action request(r: REQUEST): bool = all {
    r > newest,
    newest' = r,
    idle' = false,
    busy' = false,
    arrived' = arrived,
    queue' = queue,
    started' = started,
  }

  /// HA ends the reply or the filler.
  action pipelineEnd = all {
    idle' = true,
    newest' = newest,
    arrived' = arrived,
    queue' = queue,
    started' = started,
    busy' = busy,
  }

  /// Sentence `s` of request `r` arrives. A request has answer sentences only after it starts.
  action arrive(r: REQUEST, s: SENTENCE): bool = all {
    r <= newest,
    s == sentencesOf(arrived, r).length() + 1,
    arrived' = arrived.append((r, s)),
    queue' = queue.append((r, s)),
    idle' = idle,
    newest' = newest,
    started' = started,
    busy' = busy,
  }

  /// The idle satellite takes the head of the queue. It plays the head only when its request is the newest.
  action start = all {
    idle,
    not(busy),
    queue.length() > 0,
    val first = queue.head()
    val fresh = first._1 == newest
    all {
      started' = if (fresh) started.append((first._1, first._2, newest)) else started,
      busy' = fresh,
      queue' = queue.tail(),
      idle' = idle,
      newest' = newest,
      arrived' = arrived,
    },
  }

  /// The announce ends.
  action finish = all {
    busy' = false,
    idle' = idle,
    newest' = newest,
    arrived' = arrived,
    queue' = queue,
    started' = started,
  }

  /// Any enabled event.
  action step = any {
    nondet r = REQUESTS.oneOf()
    nondet s = SENTENCES.oneOf()
    any {
      request(r),
      arrive(r, s),
    },
    pipelineEnd,
    start,
    finish,
  }

  /// An announce plays only on an idle satellite.
  val AnnounceWhenIdle = busy implies idle

  /// Each started announce belongs to the request that was newest at its start.
  val NoStaleAnnounce = started.indices().forall(i => started[i]._1 == started[i]._3)

  /// The started sentences of each request are a prefix of its arrived sentences.
  val SentencesInOrder = REQUESTS.forall(r => {
    val played = sentencesOf(startedSentences, r)
    val received = sentencesOf(arrived, r)
    played.length() <= received.length() and played.indices().forall(i => played[i] == received[i])
  })

  /// The invariants that `just quint` checks.
  val allInvariants = AnnounceWhenIdle and NoStaleAnnounce and SentencesInOrder

  /// A sentence of request 1 that arrives after request 2 starts never plays.
  run ac_4_7_5_new_request_drops_old_answer = init
    .then(request(1))
    .then(request(2))
    .then(arrive(1, 1))
    .then(pipelineEnd)
    .then(start)
    .expect(started == List() and queue == List() and not(busy))
}
