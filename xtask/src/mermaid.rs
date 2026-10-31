//! Draws the module graph in Mermaid.

use std::collections::BTreeMap;
use std::collections::BTreeSet;
use std::fs;
use std::path::Path;
use std::process::ExitCode;

use crate::graph;
use crate::graph::Graph;

/// The class definitions of the change colors.
const CLASSES: [&str; 3] = [
    "    classDef added fill:#2da44e",
    "    classDef updated fill:#fb8500",
    "    classDef deleted fill:#cf222e",
];

/// The change of a module from the base graph to the head graph.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Status {
    /// Only in head.
    Added,
    /// In both, with different content.
    Updated,
    /// Only in base.
    Deleted,
    /// In both, with the same content.
    Unchanged,
}

impl Status {
    /// The status of `module` from `base` to `head`.
    fn of(base: &Graph, head: &Graph, module: &str) -> Self {
        match (base.nodes.get(module), head.nodes.get(module)) {
            (None, _) => Self::Added,
            (Some(_), None) => Self::Deleted,
            (Some(old), Some(new)) if old.hash != new.hash => Self::Updated,
            _ => Self::Unchanged,
        }
    }

    /// The class assignment of a node with this status.
    fn class(self) -> &'static str {
        match self {
            Self::Added => ":::added",
            Self::Updated => ":::updated",
            Self::Deleted => ":::deleted",
            Self::Unchanged => "",
        }
    }
}

/// A Mermaid flowchart of the modules of `base` and `head`: one subgraph for each layer, one node
/// for each module with the class of its change, and one arrow for each distinct module edge
/// outside test code. The arrows of a deleted module come from `base` and are dashed.
#[must_use]
pub fn render(base: &Graph, head: &Graph) -> String {
    let mut lines = vec!["flowchart LR".to_owned()];
    for (layer, nodes) in layers(base, head) {
        lines.push(format!("    subgraph {layer}"));
        lines.extend(nodes);
        lines.push("    end".to_owned());
    }
    lines.extend(links(base, head));
    // The file and the layer of a node are functions of its module path. Thus the node maps
    // differ exactly when a module is not `Unchanged`.
    if base.nodes != head.nodes {
        lines.extend(CLASSES.map(str::to_owned));
    }
    lines.push(String::new());
    lines.join("\n")
}

/// The node lines of the modules of `base` and `head`, for each layer.
fn layers<'graph>(base: &'graph Graph, head: &'graph Graph) -> BTreeMap<&'graph str, Vec<String>> {
    let modules: BTreeMap<&str, &str> = base
        .nodes
        .iter()
        .chain(&head.nodes)
        .map(|(module, node)| (module.as_str(), node.layer.as_str()))
        .collect();
    let mut layers: BTreeMap<&str, Vec<String>> = BTreeMap::new();
    for (module, layer) in modules {
        let status = Status::of(base, head, module);
        layers.entry(layer).or_default().push(node(module, status));
    }
    layers
}

/// The arrow lines: solid for the edges of `head`, dashed for the edges of `base` that touch a
/// deleted module.
fn links(base: &Graph, head: &Graph) -> Vec<String> {
    let deleted = |module: &str| !head.nodes.contains_key(module);
    let dashed = arrows(base);
    let dashed = dashed
        .iter()
        .filter(|(from, to)| deleted(from) || deleted(to))
        .map(|arrow| line(arrow, "-.->"));
    let solid = arrows(head);
    let solid = solid.iter().map(|arrow| line(arrow, "-->"));
    solid.chain(dashed).collect()
}

/// The distinct module edges of `graph` outside test code.
fn arrows(graph: &Graph) -> BTreeSet<(&str, &str)> {
    // An external edge points to no module, so it has no node to draw.
    graph
        .edges
        .iter()
        .filter(|edge| !edge.test && !edge.external)
        .map(|edge| (edge.from.as_str(), edge.to.as_str()))
        .collect()
}

/// The arrow line from `from` to `to` with the Mermaid link `link`.
fn line((from, to): &(&str, &str), link: &str) -> String {
    format!("    {} {link} {}", node_id(from), node_id(to))
}

/// The node line of `module`, labeled with its last segment, with the class of `status`.
fn node(module: &str, status: Status) -> String {
    let label = module.rsplit("::").next().unwrap_or_default();
    format!("        {}[{label}]{}", node_id(module), status.class())
}

/// The Mermaid node ID of `module`.
fn node_id(module: &str) -> String {
    module.replace("::", "_")
}

/// The content of `docs/architecture.md`: a title, the hook line and the Mermaid block of `graph`.
#[must_use]
pub fn document(graph: &Graph) -> String {
    format!(
        "# Architecture\n\nThe pre-commit hook writes this file from the module graph.\n\n```mermaid\n{}```\n",
        render(graph, graph)
    )
}

/// Writes the document of the crate in `root` to `file`.
///
/// # Errors
///
/// When the graph does not build or `file` cannot be written. The error names the file.
pub fn write(root: &Path, file: &Path) -> Result<(), String> {
    let graph = graph::build(root)?;
    fs::write(file, document(&graph)).map_err(|error| format!("{}: {error}", file.display()))
}

/// The Mermaid block of the crate in `root`, from the Git revision `reference` to the working tree.
///
/// # Errors
///
/// When a graph does not build. The error names the file.
pub fn changes(root: &Path, reference: &str) -> Result<String, String> {
    let base = graph::build_at(root, reference)?;
    let head = graph::build(root)?;
    Ok(format!("```mermaid\n{}```\n", render(&base, &head)))
}

/// Writes the document of the crate in the current folder to `file`.
#[must_use]
pub fn run(file: &str) -> ExitCode {
    exit(write(Path::new("."), Path::new(file)))
}

/// Prints the Mermaid block of the crate in the current folder from the Git revision `reference`.
#[must_use]
pub fn run_base(reference: &str) -> ExitCode {
    exit(changes(Path::new("."), reference).map(|block| print!("{block}")))
}

/// The exit code of `result`. An error goes to the standard error.
fn exit(result: Result<(), String>) -> ExitCode {
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("{error}");
            ExitCode::FAILURE
        }
    }
}

/// Tests of the diagram.
#[cfg(test)]
mod tests {
    use super::*;

    use std::path::PathBuf;

    use tempfile::TempDir;

    use crate::graph::Edge;
    use crate::graph::Node;

    /// A graph of `modules` with the module edges `(from, to, test)`.
    fn graph(modules: &[&str], edges: &[(&str, &str, bool)]) -> Graph {
        let nodes: BTreeMap<String, Node> = modules
            .iter()
            .map(|module| {
                let node = Node {
                    file: PathBuf::new(),
                    layer: module.split("::").nth(1).unwrap_or("root").to_owned(),
                    hash: 0,
                };
                ((*module).to_owned(), node)
            })
            .collect();
        let edges: Vec<Edge> = edges
            .iter()
            .map(|(from, to, test)| Edge {
                from: (*from).to_owned(),
                to: (*to).to_owned(),
                test: *test,
                external: false,
            })
            .collect();
        Graph { nodes, edges }
    }

    /// The number of arrows in the diagram of `graph`.
    fn arrows(graph: &Graph) -> usize {
        render(graph, graph).matches(" --> ").count()
    }

    /// Two renders of one graph are equal.
    #[test]
    fn ac_0_9_1_render_is_deterministic() {
        let graph = graph(
            &["crate", "crate::ports", "crate::domain::intent"],
            &[
                ("crate", "crate::ports", false),
                ("crate::ports", "crate::domain::intent", false),
            ],
        );
        let first = render(&graph, &graph);
        assert!(!first.is_empty(), "expected a diagram");
        assert_eq!(first, render(&graph, &graph));
    }

    /// Two edges with the same `from` and `to` give one arrow.
    #[test]
    fn ac_0_9_2_duplicate_edges_give_one_arrow() {
        let edge = ("crate::app::x", "crate::ports", false);
        assert_eq!(
            arrows(&graph(&["crate::app::x", "crate::ports"], &[edge, edge])),
            1
        );
    }

    /// An edge in test code gives no arrow.
    #[test]
    fn ac_0_9_3_test_edge_gives_no_arrow() {
        let modules = ["crate::app::x", "crate::ports"];
        assert_eq!(arrows(&graph(&modules, &[])), 0);
        assert_eq!(
            arrows(&graph(
                &modules,
                &[("crate::app::x", "crate::ports", false)]
            )),
            1
        );
        assert_eq!(
            arrows(&graph(&modules, &[("crate::app::x", "crate::ports", true)])),
            0
        );
    }

    /// Modules in four layers give four subgraphs.
    #[test]
    fn ac_0_9_4_subgraph_for_each_layer() {
        let graph = graph(
            &[
                "crate",
                "crate::domain::intent",
                "crate::domain::state",
                "crate::app::speech",
                "crate::adapters::ha",
            ],
            &[],
        );
        assert_eq!(render(&graph, &graph).matches("subgraph ").count(), 4);
    }

    /// The node ID is the path with `::` replaced by `_`, and the label is the last segment.
    #[test]
    fn ac_0_9_5_node_id() {
        let graph = graph(&["crate::domain::intent"], &[]);
        let diagram = render(&graph, &graph);
        assert!(
            diagram.contains("crate_domain_intent[intent]\n"),
            "expected the node crate_domain_intent, got {diagram}"
        );
    }

    /// The diagram of the repository has the title, the hook line and one Mermaid block.
    #[test]
    fn ac_0_9_6_diagram_writes_document() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .expect("xtask is a folder of the repository root");
        let folder = TempDir::new().expect("a temporary folder");
        let file = folder.path().join("architecture.md");
        write(root, &file).expect("the diagram is written");
        let content = fs::read_to_string(&file).expect("the file is read");
        let graph = graph::build(root).expect("the graph builds");
        let diagram = render(&graph, &graph);
        assert!(
            content.starts_with("# Architecture\n"),
            "no title: {content}"
        );
        assert!(
            content.contains("The pre-commit hook writes this file"),
            "no hook line: {content}"
        );
        assert_eq!(content.matches("```mermaid\n").count(), 1, "{content}");
        assert!(
            content.contains(&format!("```mermaid\n{diagram}```\n")),
            "no diagram: {content}"
        );
    }

    /// The diagram from `base` to `head`, with a check that it has `line`.
    fn assert_line(base: &Graph, head: &Graph, line: &str) -> String {
        let diagram = render(base, head);
        assert!(
            diagram.lines().any(|candidate| candidate.trim() == line),
            "expected the line {line}, got {diagram}"
        );
        diagram
    }

    /// A module only in head is `added`.
    #[test]
    fn ac_0_10_1_added_module() {
        let base = graph(&["crate"], &[]);
        let head = graph(&["crate", "crate::ports"], &[]);
        assert_line(&base, &head, "crate_ports[ports]:::added");
    }

    /// A module in both with different content is `updated`.
    #[test]
    fn ac_0_10_2_updated_module() {
        let base = graph(&["crate", "crate::ports"], &[]);
        let mut head = base.clone();
        head.nodes
            .get_mut("crate::ports")
            .expect("the module exists")
            .hash = 1;
        assert_line(&base, &head, "crate_ports[ports]:::updated");
        assert_line(&base, &head, "crate[crate]");
    }

    /// A module only in base is `deleted`, and its arrows are dashed.
    #[test]
    fn ac_0_10_3_deleted_module() {
        let base = graph(
            &["crate", "crate::app::x", "crate::ports"],
            &[
                ("crate::app::x", "crate::ports", false),
                ("crate", "crate::app::x", false),
            ],
        );
        let head = graph(
            &["crate", "crate::app::x"],
            &[("crate", "crate::app::x", false)],
        );
        assert_line(&base, &head, "crate_ports[ports]:::deleted");
        assert_line(&base, &head, "crate_app_x -.-> crate_ports");
        assert_line(&base, &head, "crate --> crate_app_x");
    }

    /// A module in both with the same content has no class.
    #[test]
    fn ac_0_10_4_unchanged_module() {
        let base = graph(&["crate", "crate::ports"], &[]);
        let head = graph(&["crate", "crate::ports", "crate::app::x"], &[]);
        assert_line(&base, &head, "crate_ports[ports]");
    }

    /// The diagram of one Git revision against itself has no class assignments.
    #[test]
    fn ac_0_10_5_same_graphs_have_no_classes() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .expect("xtask is a folder of the repository root");
        let graph = graph::build_at(root, "HEAD").expect("the graph builds");
        assert!(graph.nodes.contains_key("crate"), "no root module");
        let diagram = render(&graph, &graph);
        assert!(!diagram.contains(":::"), "expected no class, got {diagram}");
    }

    /// The class definitions have the change colors.
    #[test]
    fn ac_0_10_6_class_fills() {
        let base = graph(&["crate::ports"], &[]);
        let head = graph(&["crate::app::x"], &[]);
        assert_line(&base, &head, "classDef added fill:#2da44e");
        assert_line(&base, &head, "classDef updated fill:#fb8500");
        assert_line(&base, &head, "classDef deleted fill:#cf222e");
    }
}
