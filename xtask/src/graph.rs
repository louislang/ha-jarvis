//! Builds the module graph from the source with `syn`.

use std::collections::BTreeMap;
use std::fs;
use std::hash::{DefaultHasher, Hash, Hasher};
use std::mem;
use std::path::{Path, PathBuf};

use syn::visit::{self, Visit};
use syn::{Attribute, Ident, ImplItemFn, Item, TraitItemFn, UseTree};

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
#[derive(Default)]
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

    /// Runs `visit` one test level deeper when `attributes` has `#[cfg(test)]`.
    fn scoped(&mut self, attributes: &[Attribute], visit: impl FnOnce(&mut Self)) {
        let test = usize::from(attributes.iter().any(is_cfg_test));
        self.test_depth += test;
        visit(self);
        self.test_depth -= test;
    }
}

impl Visit<'_> for FileVisitor {
    fn visit_item(&mut self, item: &Item) {
        self.scoped(attributes(item), |visitor| match item {
            Item::Mod(module) => {
                visitor.module.push(module.ident.to_string());
                if module.content.is_some() {
                    visit::visit_item_mod(visitor, module);
                } else {
                    visitor.declared.push(visitor.found(visitor.module.clone()));
                }
                visitor.module.pop();
            }
            Item::Use(item_use) => visitor.add_use(&[], &item_use.tree),
            _ => visit::visit_item(visitor, item),
        });
    }

    fn visit_impl_item_fn(&mut self, item: &ImplItemFn) {
        self.scoped(&item.attrs, |visitor| {
            visit::visit_impl_item_fn(visitor, item);
        });
    }

    fn visit_trait_item_fn(&mut self, item: &TraitItemFn) {
        self.scoped(&item.attrs, |visitor| {
            visit::visit_trait_item_fn(visitor, item);
        });
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

/// The first segment of `module` after `crate`, or `root` for `crate`.
fn layer(module: &[String]) -> String {
    module.get(1).map_or("root", String::as_str).to_owned()
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

/// Adds the node and the unresolved `use` edges of `module` to `graph`. Returns its declared modules.
fn add_module(root: &Path, module: &Found, graph: &mut Graph) -> Result<Vec<Found>, String> {
    let file = file_of(&module.path);
    let content = fs::read_to_string(root.join(&file))
        .map_err(|error| format!("{}: {error}", file.display()))?;
    let syntax =
        syn::parse_file(&content).map_err(|error| format!("{}: {error}", file.display()))?;
    let mut visitor = FileVisitor {
        module: module.path.clone(),
        test_depth: usize::from(module.test),
        ..FileVisitor::default()
    };
    visitor.visit_file(&syntax);
    let from = module.path.join("::");
    graph
        .edges
        .extend(visitor.uses.into_iter().map(|found| Edge {
            from: from.clone(),
            to: found.path.join("::"),
            test: found.test,
        }));
    let layer = layer(&module.path);
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

    /// The edges of a fixture crate where the file `path` has `source`.
    fn edges(path: &str, source: &str) -> Vec<Edge> {
        graph(&[
            (
                "src/lib.rs",
                "pub mod ports;\npub mod domain { pub mod b; }\npub mod app { pub mod x; }\n#[cfg(test)]\npub mod testing { pub mod fakes; }\n",
            ),
            ("src/ports.rs", ""),
            ("src/domain/b.rs", ""),
            ("src/app/x.rs", ""),
            ("src/testing/fakes.rs", ""),
            (path, source),
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
        assert_eq!(graph.nodes.len(), 2);
        assert_eq!(graph.nodes["crate"].layer, "root");
        assert_eq!(graph.nodes["crate::domain::intent"].layer, "domain");
    }

    /// A `use` of a port type gives an edge to the ports module.
    #[test]
    fn ac_0_7_2_use_gives_edge() {
        assert_eq!(
            edges("src/app/x.rs", "use crate::ports::Home;\n"),
            [edge("crate::app::x", "crate::ports", false)]
        );
    }

    /// Each leaf of a `use` group gives one edge.
    #[test]
    fn ac_0_7_3_group_gives_edge_for_each_leaf() {
        assert_eq!(
            edges("src/app/x.rs", "use crate::{ports::A, domain::b::C};\n"),
            [
                edge("crate::app::x", "crate::ports", false),
                edge("crate::app::x", "crate::domain::b", false),
            ]
        );
    }

    /// A `use` inside a `#[cfg(test)]` module, method or trait method gives a test edge.
    #[test]
    fn ac_0_7_4_use_in_test_module() {
        assert_eq!(
            edges(
                "src/app/x.rs",
                "#[cfg(test)]\nmod tests {\n    use crate::testing::fakes::FakeHome;\n}\nimpl A {\n    #[cfg(test)]\n    fn f() {\n        use crate::ports::Home;\n    }\n}\ntrait T {\n    #[cfg(test)]\n    fn g() {\n        use crate::domain::b::C;\n    }\n}\n"
            ),
            [
                edge("crate::app::x", "crate::testing::fakes", true),
                edge("crate::app::x", "crate::ports", true),
                edge("crate::app::x", "crate::domain::b", true),
            ]
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
            edges("src/app/x.rs", "use super::super::ports::Home;\n"),
            [edge("crate::app::x", "crate::ports", false)]
        );
    }

    /// Every edge of a module that `src/lib.rs` declares under `#[cfg(test)]` is a test edge.
    #[test]
    fn ac_0_7_7_test_module_edges() {
        assert_eq!(
            edges("src/testing/fakes.rs", "use crate::ports::Home;\n"),
            [edge("crate::testing::fakes", "crate::ports", true)]
        );
    }

    /// Each `use` leaf binds its `as` name or its last segment to its full path.
    #[test]
    fn ac_0_7_8_alias_table() {
        let file: syn::File =
            syn::parse_str("use crate::ports::Home as H;\nuse tokio::sync::Mutex;\n")
                .expect("the source parses");
        let mut visitor = FileVisitor::default();
        visitor.visit_file(&file);
        assert_eq!(visitor.aliases.len(), 2);
        assert_eq!(visitor.aliases["H"], "crate::ports::Home");
        assert_eq!(visitor.aliases["Mutex"], "tokio::sync::Mutex");
    }
}
