//! Draws the module graph in Mermaid.

use std::collections::BTreeMap;
use std::collections::BTreeSet;
use std::fs;
use std::path::Path;
use std::process::ExitCode;

use crate::graph;
use crate::graph::Graph;

/// A Mermaid flowchart of `graph`: one subgraph for each layer, one node for each module and one
/// arrow for each distinct module edge outside test code.
#[must_use]
pub fn render(graph: &Graph) -> String {
    let mut layers: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
    for (module, node) in &graph.nodes {
        layers.entry(&node.layer).or_default().push(module);
    }
    // An external edge points to no module, so it has no node to draw.
    let arrows: BTreeSet<(&str, &str)> = graph
        .edges
        .iter()
        .filter(|edge| !edge.test && !edge.external)
        .map(|edge| (edge.from.as_str(), edge.to.as_str()))
        .collect();
    let mut lines = vec!["flowchart LR".to_owned()];
    for (layer, modules) in &layers {
        lines.push(format!("    subgraph {layer}"));
        lines.extend(modules.iter().map(|module| node(module)));
        lines.push("    end".to_owned());
    }
    lines.extend(
        arrows
            .iter()
            .map(|(from, to)| format!("    {} --> {}", node_id(from), node_id(to))),
    );
    lines.push(String::new());
    lines.join("\n")
}

/// The node line of `module`, labeled with its last segment.
fn node(module: &str) -> String {
    let label = module.rsplit("::").next().unwrap_or_default();
    format!("        {}[{label}]", node_id(module))
}

/// The Mermaid node ID of `module`.
fn node_id(module: &str) -> String {
    module.replace("::", "_")
}

/// The content of `docs/architecture.md`: a title, the CI line and the Mermaid block of `graph`.
#[must_use]
pub fn document(graph: &Graph) -> String {
    format!(
        "# Architecture\n\nCI writes this file from the module graph on each commit to `main`.\n\n```mermaid\n{}```\n",
        render(graph)
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

/// Writes the document of the crate in the current folder to `file`.
#[must_use]
pub fn run(file: &str) -> ExitCode {
    match write(Path::new("."), Path::new(file)) {
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
        render(graph).matches(" --> ").count()
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
        let first = render(&graph);
        assert!(!first.is_empty(), "expected a diagram");
        assert_eq!(first, render(&graph));
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
        assert_eq!(render(&graph).matches("subgraph ").count(), 4);
    }

    /// The node ID is the path with `::` replaced by `_`, and the label is the last segment.
    #[test]
    fn ac_0_9_5_node_id() {
        let diagram = render(&graph(&["crate::domain::intent"], &[]));
        assert!(
            diagram.contains("crate_domain_intent[intent]\n"),
            "expected the node crate_domain_intent, got {diagram}"
        );
    }

    /// The diagram of the repository has the title, the CI line and one Mermaid block.
    #[test]
    fn ac_0_9_6_diagram_writes_document() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .expect("xtask is a folder of the repository root");
        let folder = TempDir::new().expect("a temporary folder");
        let file = folder.path().join("architecture.md");
        write(root, &file).expect("the diagram is written");
        let content = fs::read_to_string(&file).expect("the file is read");
        let diagram = render(&graph::build(root).expect("the graph builds"));
        assert!(
            content.starts_with("# Architecture\n"),
            "no title: {content}"
        );
        assert!(
            content.contains("CI writes this file"),
            "no CI line: {content}"
        );
        assert_eq!(content.matches("```mermaid\n").count(), 1, "{content}");
        assert!(
            content.contains(&format!("```mermaid\n{diagram}```\n")),
            "no diagram: {content}"
        );
    }
}
