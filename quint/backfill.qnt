// Backfill: response use, and the backfill jobs that it starts, succeed or fail.
module backfill {
  /// A pool ID.
  type POOL = str

  /// A step of the model.
  type Event = Init | Use(POOL) | Succeed(POOL) | Fail(POOL)

  /// The pools.
  pure val POOLS: Set[POOL] = Set("a", "b")

  /// The fewest unused responses that a pool keeps without a backfill.
  pure val MIN = 3

  /// The number of responses that one backfill adds.
  pure val BATCH = 5

  /// The unused responses of each pool.
  var fresh: POOL -> int

  /// The backfill jobs of each pool.
  var running: POOL -> int

  /// The number of changes to each pool.
  var version: POOL -> int

  /// `version` before the last step.
  var prevVersion: POOL -> int

  /// The last step.
  var lastAction: Event

  /// Sets the history of step `e`.
  action record(e: Event): bool = all {
    prevVersion' = version,
    lastAction' = e,
  }

  /// Pools with `MIN` unused responses and no backfill.
  action init = all {
    fresh' = POOLS.mapBy(_ => MIN),
    running' = POOLS.mapBy(_ => 0),
    version' = POOLS.mapBy(_ => 0),
    prevVersion' = POOLS.mapBy(_ => 0),
    lastAction' = Init,
  }

  /// A request uses a response of pool `p`. Too few unused responses start a backfill when none runs.
  action use(p: POOL): bool = {
    val left = if (fresh.get(p) > 0) fresh.get(p) - 1 else 0
    val start = left < MIN and running.get(p) == 0
    all {
      record(Use(p)),
      fresh' = fresh.set(p, left),
      running' = if (start) running.set(p, running.get(p) + 1) else running,
      version' = version,
    }
  }

  /// A backfill of pool `p` adds `BATCH` responses.
  action succeed(p: POOL): bool = all {
    running.get(p) > 0,
    record(Succeed(p)),
    fresh' = fresh.set(p, fresh.get(p) + BATCH),
    version' = version.set(p, version.get(p) + 1),
    running' = running.set(p, running.get(p) - 1),
  }

  /// A backfill of pool `p` fails.
  action failBackfill(p: POOL): bool = all {
    running.get(p) > 0,
    record(Fail(p)),
    running' = running.set(p, running.get(p) - 1),
    fresh' = fresh,
    version' = version,
  }

  /// Any enabled event.
  action step = {
    nondet p = POOLS.oneOf()
    any {
      use(p),
      succeed(p),
      failBackfill(p),
    }
  }

  /// A pool has at most one backfill job.
  val OneBackfillPerPool = POOLS.forall(p => running.get(p) <= 1)

  /// Only a successful backfill of a pool changes that pool.
  val PoolsChangeOnlyOnSuccess = POOLS.forall(p => version.get(p) != prevVersion.get(p) implies lastAction == Succeed(p))

  /// The invariants that `just quint` checks.
  val allInvariants = OneBackfillPerPool and PoolsChangeOnlyOnSuccess
}
