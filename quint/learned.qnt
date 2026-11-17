// Learned intents: the proposal of each cluster, approval, activation and plugin requests.
module learned {
  /// A cluster ID.
  type CLUSTER = str

  /// The lifecycle state of the learned entry of a cluster.
  type Status = Absent | Pending | Active | Rejected | None | Requested

  /// The clusters.
  pure val CLUSTERS: Set[CLUSTER] = Set("c1", "c2")

  /// The proposal of each cluster calls an HA service. `init` chooses it and no action changes it, so each check covers all values.
  var HAS_SERVICE: CLUSTER -> bool

  /// The requests of each cluster need remote data that no plugin gives. `init` chooses it and no action changes it.
  var NEEDS_PLUGIN: CLUSTER -> bool

  /// The status of each cluster.
  var status: CLUSTER -> Status

  /// The clusters that you approved.
  var approved: Set[CLUSTER]

  /// The number of proposals.
  var proposals: int

  /// The change count of training.
  var changes: int

  /// The plugin request files of each cluster.
  var requests: CLUSTER -> int

  /// Keeps the constants.
  action keep: bool = all {
    HAS_SERVICE' = HAS_SERVICE,
    NEEDS_PLUGIN' = NEEDS_PLUGIN,
  }

  /// No cluster has a proposal, with any services and plugin needs.
  action init = {
    nondet service = CLUSTERS.setOfMaps(Bool).oneOf()
    nondet plugin = CLUSTERS.setOfMaps(Bool).oneOf()
    all {
      HAS_SERVICE' = service,
      NEEDS_PLUGIN' = plugin,
      status' = CLUSTERS.mapBy(_ => Absent),
      approved' = Set(),
      proposals' = 0,
      changes' = 0,
      requests' = CLUSTERS.mapBy(_ => 0),
    }
  }

  /// System Two proposes an entry for cluster `c`. An unsuitable cluster gets `None`.
  action propose(c: CLUSTER, suitable: bool): bool = {
    val outcome = if (not(suitable)) None
      else if (NEEDS_PLUGIN.get(c)) Requested
      else if (HAS_SERVICE.get(c)) Pending
      else Active
    all {
      status.get(c) == Absent,
      keep,
      status' = status.set(c, outcome),
      proposals' = proposals + 1,
      changes' = if (outcome == Active) changes + 1 else changes,
      approved' = approved,
      requests' = requests,
    }
  }

  /// You approve the pending entry of cluster `c`.
  action approve(c: CLUSTER): bool = all {
    status.get(c) == Pending,
    keep,
    status' = status.set(c, Active),
    approved' = approved.union(Set(c)),
    changes' = changes + 1,
    proposals' = proposals,
    requests' = requests,
  }

  /// You reject the pending entry of cluster `c`.
  action reject(c: CLUSTER): bool = all {
    status.get(c) == Pending,
    keep,
    status' = status.set(c, Rejected),
    approved' = approved,
    changes' = changes,
    proposals' = proposals,
    requests' = requests,
  }

  /// Jarvis writes the plugin request file of cluster `c`.
  action writeRequest(c: CLUSTER): bool = all {
    status.get(c) == Requested,
    requests.get(c) == 0,
    keep,
    requests' = requests.set(c, 1),
    status' = status,
    approved' = approved,
    proposals' = proposals,
    changes' = changes,
  }

  /// The write of the plugin request file of cluster `c` fails.
  action failRequest(c: CLUSTER): bool = all {
    status.get(c) == Requested,
    requests.get(c) == 0,
    keep,
    requests' = requests,
    status' = status,
    approved' = approved,
    proposals' = proposals,
    changes' = changes,
  }

  /// Any enabled event.
  action step = {
    nondet c = CLUSTERS.oneOf()
    nondet suitable = Bool.oneOf()
    any {
      propose(c, suitable),
      approve(c),
      reject(c),
      writeRequest(c),
      failRequest(c),
    }
  }

  /// An active entry with a service has your approval.
  val ServiceNeedsApproval = CLUSTERS.forall(c => (status.get(c) == Active and HAS_SERVICE.get(c)) implies approved.contains(c))

  /// Each cluster has at most one proposal.
  val NoReproposal = proposals == CLUSTERS.filter(c => status.get(c) != Absent).size()

  /// Each activation adds exactly one change.
  val ActivationCountsOnce = changes == CLUSTERS.filter(c => status.get(c) == Active).size()

  /// A cluster has at most one plugin request, and only when it needs a plugin.
  val OneRequestPerCluster = CLUSTERS.forall(c => requests.get(c) <= 1 and (requests.get(c) == 1 implies status.get(c) == Requested))

  /// The invariants that `just quint` checks.
  val allInvariants = ServiceNeedsApproval and NoReproposal and ActivationCountsOnce and OneRequestPerCluster
}
