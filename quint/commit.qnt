// Commit policy over time: audio chunks, a VAD that decides each chunk late, and partial transcripts.
module commit {
  /// The rule that allowed the commit.
  type Reason = Open | Early | Endpoint | Limit

  /// The silence after a confident partial that commits, in 100 ms chunks.
  pure val EARLY = 2

  /// The silence after any partial that commits, in 100 ms chunks.
  pure val ENDPOINT = 6

  /// The utterance length that always commits, in 100 ms chunks.
  pure val MAX = 20

  /// The number of chunks that the VAD can decide late.
  pure val LAG = 2

  /// The audio chunks in arrival order. True is a voiced chunk.
  var chunks: List[bool]

  /// The number of chunks that the VAD decided.
  var decided: int

  /// The number of the last decided voiced chunk, counted from 1. -1 before any.
  var voiceEnd: int

  /// The partial transcript is not empty.
  var text: bool

  /// The speculative prediction of the partial is confident.
  var confident: bool

  /// Jarvis committed the utterance.
  var committed: bool

  /// The rule of the commit, or `Open` before it.
  var reason: Reason

  /// The number of decided chunks at the commit.
  var commitPos: int

  /// The decided chunks after the last voiced chunk.
  val silence = decided - voiceEnd

  /// A commit rule is true in the current state.
  val allowed = (voiceEnd >= 0 and text and ((confident and silence >= EARLY) or silence >= ENDPOINT)) or chunks.length() >= MAX

  /// The first commit rule that the inputs make true, or `Open`.
  pure def ruleOf(chunkCount: int, decidedCount: int, end: int, nonEmpty: bool, conf: bool): Reason = {
    val spoken = end >= 0 and nonEmpty
    if (spoken and conf and decidedCount - end >= EARLY) Early
    else if (spoken and decidedCount - end >= ENDPOINT) Endpoint
    else if (chunkCount >= MAX) Limit
    else Open
  }

  /// Takes the new inputs, then commits when a rule allows it.
  action advance(newChunks: List[bool], newDecided: int, newVoiceEnd: int, newText: bool, newConfident: bool): bool = {
    val rule = ruleOf(newChunks.length(), newDecided, newVoiceEnd, newText, newConfident)
    all {
      chunks' = newChunks,
      decided' = newDecided,
      voiceEnd' = newVoiceEnd,
      text' = newText,
      confident' = newConfident,
      committed' = rule != Open,
      reason' = rule,
      commitPos' = if (rule != Open) newDecided else commitPos,
    }
  }

  /// No audio and an empty partial.
  action init = all {
    chunks' = List(),
    decided' = 0,
    voiceEnd' = -1,
    text' = false,
    confident' = false,
    committed' = false,
    reason' = Open,
    commitPos' = 0,
  }

  /// A chunk arrives. The VAD is at most `LAG` chunks behind.
  action tick(voiced: bool): bool = all {
    chunks.length() - decided < LAG,
    advance(chunks.append(voiced), decided, voiceEnd, text, confident),
  }

  /// The VAD decides the oldest undecided chunk.
  action decide = all {
    decided < chunks.length(),
    advance(chunks, decided + 1, if (chunks[decided]) decided + 1 else voiceEnd, text, confident),
  }

  /// The first pass gives a new partial.
  action partial(nonEmpty: bool, conf: bool): bool = advance(chunks, decided, voiceEnd, nonEmpty, conf)

  /// Any enabled event until the commit ends the utterance.
  action step = all {
    not(committed),
    nondet voiced = Bool.oneOf()
    nondet nonEmpty = Bool.oneOf()
    nondet conf = Bool.oneOf()
    any {
      tick(voiced),
      decide,
      partial(nonEmpty, conf),
    },
  }

  /// A commit has the rule that allowed it.
  val CommitOnlyWhenAllowed = committed implies match reason {
    | Early => voiceEnd >= 0 and text and confident and silence >= EARLY
    | Endpoint => voiceEnd >= 0 and text and silence >= ENDPOINT
    | Limit => chunks.length() >= MAX
    | Open => false
  }

  /// Jarvis commits as soon as a rule allows it.
  val NoLateCommit = not(committed) implies not(allowed)

  /// A commit for silence follows `EARLY` unvoiced chunks.
  val NoCommitDuringSpeech = (committed and reason != Limit) implies
    (commitPos >= EARLY and commitPos <= chunks.length() and
      to(commitPos - EARLY, commitPos - 1).forall(i => not(chunks[i])))

  /// The invariants that `just quint` checks.
  val allInvariants = CommitOnlyWhenAllowed and NoLateCommit and NoCommitDuringSpeech
}
