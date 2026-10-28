//! Checks the layer and modularity rules on the module graph.

use std::fmt;
use std::path::Path;
use std::path::PathBuf;
use std::process::ExitCode;

use crate::graph;
use crate::graph::Edge;
use crate::graph::Graph;

/// The crates of the domain list of `01-architecture.md` section 7, and `std`.
const DOMAIN_CRATES: [&str; 12] = [
    "std",
    "minijinja",
    "rand",
    "jiff",
    "strsim",
    "ndarray",
    "linfa",
    "linfa_clustering",
    "xxhash_rust",
    "vstd",
    "serde",
    "serde_json",
];

/// The paths that the domain does not use (`01-architecture.md` section 1, rule 1).
const BANNED: [&str; 17] = [
    "std::fs",
    "std::net",
    "std::process",
    "std::thread",
    "std::env",
    "std::time::SystemTime",
    "std::time::Instant",
    "std::collections::HashMap",
    "std::collections::HashSet",
    "jiff::Timestamp::now",
    "jiff::Zoned::now",
    "jiff::tz::TimeZone::system",
    "rand::thread_rng",
    "rand::rng",
    "rand::random",
    "rand::rngs::OsRng",
    "rand::rngs::ThreadRng",
];

/// Rules 1, 3, 4 and 6: the rule number, a layer and the layers that it does not use.
const LAYER_RULES: [(u8, &str, &[&str]); 4] = [
    (1, "domain", &["ports", "app", "adapters"]),
    (3, "app", &["adapters"]),
    (4, "adapters", &["app"]),
    (6, "ports", &["app", "adapters"]),
];

/// An edge that breaks a layer or modularity rule.
#[derive(Clone, Debug, PartialEq)]
pub struct RuleViolation {
    /// The rule number of the model of sub-phase 0.8.
    pub rule: u8,
    /// The file of the module that names the path.
    pub file: PathBuf,
    /// The edge that breaks the rule.
    pub edge: Edge,
}

impl fmt::Display for RuleViolation {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "{}: rule {}: {} -> {}",
            self.file.display(),
            self.rule,
            self.edge.from,
            self.edge.to
        )
    }
}

/// The edges outside test code that break a rule, each with one rule that it breaks.
#[must_use]
pub fn violations(graph: &Graph) -> Vec<RuleViolation> {
    graph
        .edges
        .iter()
        .filter(|edge| !edge.test)
        .filter_map(|edge| {
            let rule = broken_rule(graph, edge)?;
            let file = graph.nodes.get(&edge.from)?.file.clone();
            let edge = edge.clone();
            Some(RuleViolation { rule, file, edge })
        })
        .collect()
}

/// A rule that `edge` breaks. Rules 1, 3, 4 and 6 come before rule 5.
fn broken_rule(graph: &Graph, edge: &Edge) -> Option<u8> {
    let from = graph.nodes.get(&edge.from)?.layer.as_str();
    if edge.external {
        return (from == "domain" && !allowed_in_domain(&edge.to)).then_some(2);
    }
    let to = graph.nodes.get(&edge.to)?.layer.as_str();
    let other_adapter = to == "adapters" && from != "root" && edge.from != edge.to;
    LAYER_RULES
        .iter()
        .find(|(_, layer, banned)| *layer == from && banned.contains(&to))
        .map(|(rule, ..)| *rule)
        .or(other_adapter.then_some(5))
}

/// True when `path` is in a crate of the domain list and starts with no banned path.
fn allowed_in_domain(path: &str) -> bool {
    let banned = BANNED.iter().any(|banned| {
        path.strip_prefix(banned)
            .is_some_and(|rest| rest.is_empty() || rest.starts_with("::"))
    });
    let crate_name = path.split("::").next().unwrap_or_default();
    DOMAIN_CRATES.contains(&crate_name) && !banned
}

/// Checks the rules on the graph of the crate in the current folder.
#[must_use]
pub fn run() -> ExitCode {
    let report: Vec<String> = match graph::build(Path::new(".")) {
        Ok(graph) => violations(&graph).iter().map(ToString::to_string).collect(),
        Err(error) => vec![error],
    };
    for line in &report {
        eprintln!("{line}");
    }
    if report.is_empty() {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}

/// Tests of the rules.
#[cfg(test)]
mod tests {
    use super::*;

    use std::collections::BTreeMap;

    use crate::graph::Node;

    /// No rule numbers.
    const NONE: [u8; 0] = [];

    /// The violations of a graph with the edge `from -> to` and its modules.
    fn violations_of(from: &str, to: &str, external: bool, test: bool) -> Vec<RuleViolation> {
        let modules = if external { vec![from] } else { vec![from, to] };
        let nodes: BTreeMap<String, Node> = modules
            .into_iter()
            .map(|module| {
                let node = Node {
                    file: PathBuf::from(format!("{module}.rs")),
                    layer: module.split("::").nth(1).unwrap_or("root").to_owned(),
                    hash: 0,
                };
                (module.to_owned(), node)
            })
            .collect();
        let edges = vec![Edge {
            from: from.to_owned(),
            to: to.to_owned(),
            test,
            external,
        }];
        violations(&Graph { nodes, edges })
    }

    /// The rule numbers of the violations of `from -> to` outside test code.
    fn rules(from: &str, to: &str, external: bool) -> Vec<u8> {
        violations_of(from, to, external, false)
            .iter()
            .map(|violation| violation.rule)
            .collect()
    }

    /// A domain module that uses the ports breaks rule 1, with its file and the edge.
    #[test]
    fn ac_0_8_1_domain_uses_ports() {
        let edge = Edge {
            from: "crate::domain::b".to_owned(),
            to: "crate::ports".to_owned(),
            test: false,
            external: false,
        };
        assert_eq!(
            violations_of("crate::domain::b", "crate::ports", false, false),
            [RuleViolation {
                rule: 1,
                file: PathBuf::from("crate::domain::b.rs"),
                edge,
            }]
        );
    }

    /// The domain breaks rule 2 with `tokio`, not with a crate of the domain list.
    #[test]
    fn ac_0_8_2_domain_crates() {
        assert_eq!(rules("crate::domain::b", "tokio::sync::Mutex", true), [2]);
        assert_eq!(
            rules("crate::domain::b", "minijinja::Environment", true),
            NONE
        );
    }

    /// The domain breaks rule 2 with a banned `std` path, not with `BTreeMap`.
    #[test]
    fn ac_0_8_3_domain_banned_std_paths() {
        for path in [
            "std::fs::read",
            "std::env::var",
            "std::collections::HashMap",
        ] {
            assert_eq!(rules("crate::domain::b", path, true), [2], "{path}");
        }
        assert_eq!(
            rules("crate::domain::b", "std::collections::BTreeMap", true),
            NONE
        );
    }

    /// An application module that uses an adapter breaks rule 3.
    #[test]
    fn ac_0_8_4_app_uses_adapter() {
        assert_eq!(rules("crate::app::x", "crate::adapters::ha", false), [3]);
    }

    /// An adapter that uses the application breaks rule 4.
    #[test]
    fn ac_0_8_5_adapter_uses_app() {
        assert_eq!(rules("crate::adapters::ha", "crate::app::x", false), [4]);
    }

    /// An adapter that uses another adapter breaks rule 5. The root and the same adapter do not.
    #[test]
    fn ac_0_8_6_adapter_uses_other_adapter() {
        let sqlite = "crate::adapters::sqlite";
        assert_eq!(rules("crate::adapters::web", sqlite, false), [5]);
        assert_eq!(rules(sqlite, sqlite, false), NONE);
        assert_eq!(rules("crate", sqlite, false), NONE);
    }

    /// An edge in test code breaks no rule.
    #[test]
    fn ac_0_8_7_test_edge_ignored() {
        let found = violations_of("crate::domain::b", "crate::ports", false, true);
        assert!(found.is_empty(), "expected no violation, got {found:?}");
    }

    /// The ports break rule 6 with the application.
    #[test]
    fn ac_0_8_9_ports_use_app() {
        assert_eq!(rules("crate::ports", "crate::app::x", false), [6]);
    }

    /// The domain breaks rule 2 with a banned path of a domain crate, not with `jiff::civil::Date`.
    /// A banned path matches whole segments, so `rand::rngs::StdRng` is not `rand::rng`.
    #[test]
    fn ac_0_8_10_domain_banned_crate_paths() {
        for path in ["jiff::Zoned::now", "rand::random"] {
            assert_eq!(rules("crate::domain::b", path, true), [2], "{path}");
        }
        for path in ["jiff::civil::Date", "rand::rngs::StdRng"] {
            assert_eq!(rules("crate::domain::b", path, true), NONE, "{path}");
        }
    }
}
