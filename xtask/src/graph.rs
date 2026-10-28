//! Builds the module graph from the source with `syn`.

use std::collections::BTreeMap;
use std::fs;
use std::hash::{DefaultHasher, Hash, Hasher};
use std::mem;
use std::path::{Path, PathBuf};

use syn::visit::{self, Visit};
use syn::{Attribute, Ident, Item, UseTree};

/// A module of the graph.
#[derive(Clone, Debug, PartialEq)]
pub struct Node {
    /// The source file, relative to the crate folder.
    pub file: PathBuf,
    /// The first path segment after `crate`, or `root` for `crate`.
    pub layer: String,
    /// A hash of the file content.
    pub hash: u64,
}

/// A `use` dependency from one module to another.
#[derive(Clone, Debug, PartialEq)]
pub struct Edge {
    /// The module that has the `use` item.
    pub from: String,
    /// The longest prefix of the used path that is a module.
    pub to: String,
    /// True inside test code.
    pub test: bool,
}

/// The modules of a crate and their `use` dependencies.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Graph {
    /// Each module path to its node.
    pub nodes: BTreeMap<String, Node>,
    /// One edge for each leaf of each `use` tree that names a module.
    pub edges: Vec<Edge>,
}

/// A path that a file names, with true when test code names it.
#[derive(Clone, Debug, PartialEq)]
struct Found {
    /// The path segments, from `crate` for a module of the crate.
    path: Vec<String>,
    /// True inside test code.
    test: bool,
}

/// Collects the declared modules, the `use` paths and the alias table of one file.
struct FileVisitor {
    /// The current module path, with the inline modules.
    module: Vec<String>,
    /// The number of enclosing `#[cfg(test)]` items.
    test_depth: usize,
    /// The modules that `mod` items without a body declare.
    declared: Vec<Found>,
    /// The resolved path of each `use` leaf.
    uses: Vec<Found>,
    /// Each name that a `use` leaf binds to its full path.
    aliases: BTreeMap<String, String>,
}

impl FileVisitor {
    /// A visitor for the file of `module`. Test code when `test`.
    fn new(module: Vec<String>, test: bool) -> Self {
        Self {
            module,
            test_depth: usize::from(test),
            declared: Vec::new(),
            uses: Vec::new(),
            aliases: BTreeMap::new(),
        }
    }

    /// A path in the current scope.
    fn found(&self, path: Vec<String>) -> Found {
        Found {
            path,
            test: self.test_depth > 0,
        }
    }

    /// Records each leaf of `tree`, below the path `prefix`.
    fn add_use(&mut self, prefix: &[String], tree: &UseTree) {
        match tree {
            UseTree::Path(path) => {
                self.add_use(&[prefix, &[path.ident.to_string()]].concat(), &path.tree);
            }
            UseTree::Name(name) => self.add_leaf(prefix, &name.ident, &name.ident),
            UseTree::Rename(rename) => self.add_leaf(prefix, &rename.ident, &rename.rename),
            UseTree::Glob(_) if prefix == ["super"] => {}
            UseTree::Glob(_) => self.uses.push(self.found(resolve(&self.module, prefix))),
            UseTree::Group(group) => {
                for tree in &group.items {
                    self.add_use(prefix, tree);
                }
            }
        }
    }

    /// Records the leaf `name` below `prefix`, bound to `bound` (`self` binds the last segment).
    fn add_leaf(&mut self, prefix: &[String], name: &Ident, bound: &Ident) {
        let mut path = resolve(&self.module, prefix);
        if name != "self" {
            path.push(name.to_string());
        }
        let bound = if bound == "self" {
            path.last().cloned().unwrap_or_default()
        } else {
            bound.to_string()
        };
        self.aliases.insert(bound, path.join("::"));
        self.uses.push(self.found(path));
    }
}

impl Visit<'_> for FileVisitor {
    fn visit_item(&mut self, item: &Item) {
        let test = usize::from(attributes(item).iter().any(is_cfg_test));
        self.test_depth += test;
        match item {
            Item::Mod(module) => {
                self.module.push(module.ident.to_string());
                if module.content.is_some() {
                    visit::visit_item_mod(self, module);
                } else {
                    self.declared.push(self.found(self.module.clone()));
                }
                self.module.pop();
            }
            Item::Use(item_use) => self.add_use(&[], &item_use.tree),
            _ => visit::visit_item(self, item),
        }
        self.test_depth -= test;
    }
}

/// The attributes of the items that can hold a `use` item.
fn attributes(item: &Item) -> &[Attribute] {
    match item {
        Item::Fn(item) => &item.attrs,
        Item::Impl(item) => &item.attrs,
        Item::Mod(item) => &item.attrs,
        Item::Trait(item) => &item.attrs,
        Item::Use(item) => &item.attrs,
        _ => &[],
    }
}

/// True for `#[cfg(test)]`.
fn is_cfg_test(attribute: &Attribute) -> bool {
    attribute.path().is_ident("cfg")
        && attribute
            .parse_args::<Ident>()
            .is_ok_and(|argument| argument == "test")
}

/// Resolves a leading `self` or `super` of `path` against `module`.
fn resolve(module: &[String], path: &[String]) -> Vec<String> {
    let supers = path
        .iter()
        .take_while(|segment| *segment == "super")
        .count();
    let skipped = supers + usize::from(path.first().is_some_and(|segment| segment == "self"));
    if skipped == 0 {
        return path.to_vec();
    }
    let base = module
        .get(..module.len().saturating_sub(supers))
        .unwrap_or_default();
    [base, path.get(skipped..).unwrap_or_default()].concat()
}

/// The file of `module`: `src/lib.rs` for `crate`, else `src/<segments>.rs`.
fn file_of(module: &[String]) -> PathBuf {
    match module.get(1..) {
        Some(rest) if !rest.is_empty() => PathBuf::from(format!("src/{}.rs", rest.join("/"))),
        _ => PathBuf::from("src/lib.rs"),
    }
}

/// A hash of `content`.
fn hash(content: &str) -> u64 {
    let mut hasher = DefaultHasher::new();
    content.hash(&mut hasher);
    hasher.finish()
}

/// The longest prefix of `path` that is a node of `nodes`.
fn module_prefix(nodes: &BTreeMap<String, Node>, path: &str) -> Option<String> {
    let segments: Vec<&str> = path.split("::").collect();
    (1..=segments.len())
        .rev()
        .map(|length| segments[..length].join("::"))
        .find(|prefix| nodes.contains_key(prefix))
}

/// Reads and parses the file of `module`, adds its node and its unresolved `use` edges to `graph`.
/// Returns the modules that the file declares.
fn add_module(root: &Path, module: &Found, graph: &mut Graph) -> Result<Vec<Found>, String> {
    let file = file_of(&module.path);
    let content = fs::read_to_string(root.join(&file))
        .map_err(|error| format!("{}: {error}", file.display()))?;
    let syntax =
        syn::parse_file(&content).map_err(|error| format!("{}: {error}", file.display()))?;
    let mut visitor = FileVisitor::new(module.path.clone(), module.test);
    visitor.visit_file(&syntax);
    let from = module.path.join("::");
    graph
        .edges
        .extend(visitor.uses.into_iter().map(|found| Edge {
            from: from.clone(),
            to: found.path.join("::"),
            test: found.test,
        }));
    let layer = module.path.get(1).map_or("root", String::as_str).to_owned();
    let hash = hash(&content);
    graph.nodes.insert(from, Node { file, layer, hash });
    Ok(visitor.declared)
}

/// Builds the graph of the crate in `root` from the `mod` items of `src/lib.rs`.
///
/// # Errors
///
/// When a module file cannot be read or parsed. The error names the file.
pub fn build(root: &Path) -> Result<Graph, String> {
    let mut graph = Graph::default();
    let mut pending = vec![Found {
        path: vec!["crate".to_owned()],
        test: false,
    }];
    while let Some(module) = pending.pop() {
        pending.extend(add_module(root, &module, &mut graph)?);
    }
    let edges = mem::take(&mut graph.edges);
    graph.edges = edges
        .into_iter()
        .filter_map(|edge| {
            let to = module_prefix(&graph.nodes, &edge.to)?;
            Some(Edge { to, ..edge })
        })
        .collect();
    Ok(graph)
}

/// Tests of the module graph.
#[cfg(test)]
mod tests {
    use super::*;

    use tempfile::TempDir;

    /// Builds the graph of a crate with the files `(path, content)`.
    fn graph(files: &[(&str, &str)]) -> Graph {
        let folder = TempDir::new().expect("a temporary folder");
        for (path, content) in files {
            let file = folder.path().join(path);
            fs::create_dir_all(file.parent().expect("a parent folder"))
                .expect("the folder is made");
            fs::write(file, content).expect("the file is written");
        }
        build(folder.path()).expect("the graph builds")
    }

    /// An edge from `from` to `to`.
    fn edge(from: &str, to: &str, test: bool) -> Edge {
        Edge {
            from: from.to_owned(),
            to: to.to_owned(),
            test,
        }
    }

    /// The edges of `src/app/x.rs` with `source`, beside `crate::ports` and `crate::domain::b`.
    fn app_edges(source: &str) -> Vec<Edge> {
        graph(&[
            (
                "src/lib.rs",
                "pub mod ports;\npub mod domain { pub mod b; }\npub mod app { pub mod x; }\n#[cfg(test)]\npub mod testing { pub mod fakes; }\n",
            ),
            ("src/ports.rs", ""),
            ("src/domain/b.rs", ""),
            ("src/testing/fakes.rs", ""),
            ("src/app/x.rs", source),
        ])
        .edges
    }

    /// An inline folder module declares the file module in that folder.
    #[test]
    fn ac_0_7_1_nodes_from_lib() {
        let graph = graph(&[
            ("src/lib.rs", "pub mod domain { pub mod intent; }\n"),
            ("src/domain/intent.rs", ""),
        ]);
        let layers: Vec<(&str, &str)> = graph
            .nodes
            .iter()
            .map(|(path, node)| (path.as_str(), node.layer.as_str()))
            .collect();
        assert_eq!(
            layers,
            [("crate", "root"), ("crate::domain::intent", "domain")]
        );
    }

    /// A `use` of a port type gives an edge to the ports module.
    #[test]
    fn ac_0_7_2_use_gives_edge() {
        assert_eq!(
            app_edges("use crate::ports::Home;\n"),
            [edge("crate::app::x", "crate::ports", false)]
        );
    }

    /// Each leaf of a `use` group gives one edge.
    #[test]
    fn ac_0_7_3_group_gives_edge_for_each_leaf() {
        assert_eq!(
            app_edges("use crate::{ports::A, domain::b::C};\n"),
            [
                edge("crate::app::x", "crate::ports", false),
                edge("crate::app::x", "crate::domain::b", false),
            ]
        );
    }

    /// A `use` inside `#[cfg(test)] mod tests` gives a test edge.
    #[test]
    fn ac_0_7_4_use_in_test_module() {
        assert_eq!(
            app_edges("#[cfg(test)]\nmod tests {\n    use crate::testing::fakes::FakeHome;\n}\n"),
            [edge("crate::app::x", "crate::testing::fakes", true)]
        );
    }

    /// The node hash follows the file content.
    #[test]
    fn ac_0_7_5_hash_follows_content() {
        let hash = |content| graph(&[("src/lib.rs", content)]).nodes["crate"].hash;
        assert_ne!(hash("//! A.\n"), hash("//! B.\n"));
    }

    /// Each `super` goes up one module.
    #[test]
    fn ac_0_7_6_super_resolves() {
        assert_eq!(
            app_edges("use super::super::ports::Home;\n"),
            [edge("crate::app::x", "crate::ports", false)]
        );
    }

    /// Every edge of a module that `src/lib.rs` declares under `#[cfg(test)]` is a test edge.
    #[test]
    fn ac_0_7_7_test_module_edges() {
        let graph = graph(&[
            (
                "src/lib.rs",
                "pub mod ports;\n#[cfg(test)]\npub mod testing { pub mod fakes; }\n",
            ),
            ("src/ports.rs", ""),
            ("src/testing/fakes.rs", "use crate::ports::Home;\n"),
        ]);
        assert_eq!(
            graph.edges,
            [edge("crate::testing::fakes", "crate::ports", true)]
        );
    }

    /// Each `use` leaf binds its `as` name or its last segment to its full path.
    #[test]
    fn ac_0_7_8_alias_table() {
        let file: syn::File =
            syn::parse_str("use crate::ports::Home as H;\nuse tokio::sync::Mutex;\n")
                .expect("the source parses");
        let mut visitor = FileVisitor::new(vec!["crate".to_owned()], false);
        visitor.visit_file(&file);
        let aliases: Vec<(&str, &str)> = visitor
            .aliases
            .iter()
            .map(|(name, path)| (name.as_str(), path.as_str()))
            .collect();
        assert_eq!(
            aliases,
            [("H", "crate::ports::Home"), ("Mutex", "tokio::sync::Mutex")]
        );
    }
}
