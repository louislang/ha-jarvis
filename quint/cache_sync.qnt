// Hot cache sync: HA state, the event stream, disconnects and the snapshot load.
module cache_sync {
  /// An entity ID.
  type ENTITY = str

  /// An entity state.
  type VALUE = int

  /// The number of the connection that delivers an event.
  type EPOCH = int

  /// A value that can be absent.
  type Option[a] = Some(a) | None

  /// The entities of the house.
  pure val ENTITIES: Set[ENTITY] = Set("a", "b")

  /// The states that an entity can have.
  pure val VALUES: Set[VALUE] = Set(0, 1)

  /// The entity states in HA.
  var home: ENTITY -> VALUE

  /// The entity states in the hot cache.
  var cache: ENTITY -> VALUE

  /// The state change events that HA sent and Jarvis did not apply yet.
  var queue: List[(ENTITY, VALUE, EPOCH)]

  /// The connection to HA is open.
  var connected: bool

  /// The number of the current connection.
  var epoch: EPOCH

  /// The cache waits for a snapshot.
  var loading: bool

  /// The snapshot result that HA sent and Jarvis did not apply yet.
  var snap: Option[ENTITY -> VALUE]

  /// A connected, idle cache that equals HA.
  action init = {
    nondet start = ENTITIES.setOfMaps(VALUES).oneOf()
    all {
      home' = start,
      cache' = start,
      queue' = List(),
      connected' = true,
      epoch' = 0,
      loading' = false,
      snap' = None,
    }
  }

  /// HA sets `e` to `v` and sends the event on an open connection.
  action change(e: ENTITY, v: VALUE): bool = all {
    home' = home.set(e, v),
    queue' = if (connected) queue.append((e, v, epoch)) else queue,
    cache' = cache,
    connected' = connected,
    epoch' = epoch,
    loading' = loading,
    snap' = snap,
  }

  /// Jarvis takes the oldest event and applies it when it is from the current connection.
  action deliver = all {
    not(loading),
    queue.length() > 0,
    val event = queue.head()
    cache' = if (event._3 == epoch) cache.set(event._1, event._2) else cache,
    queue' = queue.tail(),
    home' = home,
    connected' = connected,
    epoch' = epoch,
    loading' = loading,
    snap' = snap,
  }

  /// The connection closes and drops the unsent events and the pending snapshot.
  action disconnect = all {
    connected' = false,
    queue' = List(),
    loading' = false,
    snap' = None,
    home' = home,
    cache' = cache,
    epoch' = epoch,
  }

  /// A new connection opens and subscribes, so later changes enter the queue.
  action reconnect = all {
    not(connected),
    epoch' = epoch + 1,
    connected' = true,
    loading' = true,
    home' = home,
    cache' = cache,
    queue' = queue,
    snap' = snap,
  }

  /// HA reads its states for the snapshot. Changes can still arrive before the result.
  action takeSnapshot = all {
    loading,
    snap == None,
    snap' = Some(home),
    home' = home,
    cache' = cache,
    queue' = queue,
    connected' = connected,
    epoch' = epoch,
    loading' = loading,
  }

  /// Jarvis replaces the cache with the snapshot result.
  action applySnapshot = all {
    snap != None,
    cache' = match snap {
      | Some(s) => s
      | None => cache
    },
    snap' = None,
    loading' = false,
    home' = home,
    queue' = queue,
    connected' = connected,
    epoch' = epoch,
  }

  /// Any enabled event.
  action step = any {
    nondet e = ENTITIES.oneOf()
    nondet v = VALUES.oneOf()
    change(e, v),
    deliver,
    disconnect,
    reconnect,
    takeSnapshot,
    applySnapshot,
  }

  /// The cache equals HA when the connection is open, loaded and has no pending event.
  val CacheEqualsHomeWhenIdle = (connected and not(loading) and queue == List()) implies cache == home

  /// Every queued event belongs to the current connection.
  val NoEventAfterDisconnect = queue.indices().forall(i => queue[i]._3 == epoch)

  /// The invariants that `just quint` checks.
  val allInvariants = CacheEqualsHomeWhenIdle and NoEventAfterDisconnect

  /// A change while disconnected is in the cache after the reconnect and the snapshot.
  run ac_2_6_4_change_while_disconnected = init
    .then(disconnect)
    .then(change("a", 1 - home.get("a")))
    .expect(cache != home)
    .then(reconnect)
    .then(takeSnapshot)
    .then(applySnapshot)
    .expect(cache == home)

  /// A change between the snapshot and its result is in the cache after the queue is delivered.
  run ac_2_6_5_change_during_load = init
    .then(disconnect)
    .then(reconnect)
    .then(takeSnapshot)
    .then(change("a", 1 - home.get("a")))
    .then(applySnapshot)
    .expect(cache != home)
    .then(deliver)
    .expect(cache == home and queue == List())
}
