//! Builds the module graph from the source with `syn`.

use std::collections::BTreeMap;
use std::collections::BTreeSet;
use std::fs;
use std::hash::{DefaultHasher, Hash, Hasher};
use std::iter;
use std::mem;
use std::path::{Path, PathBuf};
use std::process::Command;

use syn::buffer::{Cursor, TokenBuffer};
use syn::punctuated::Punctuated;
use syn::visit::{self, Visit};
use syn::{
    Attribute, Expr, Ident, ImplItemFn, Item, Macro, Token, TraitItemFn, UseTree, VisRestricted,
};
use toml::Table;
use toml::Value;

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

/// A path that one module names.
#[derive(Clone, Debug, PartialEq)]
pub struct Edge {
    /// The module that names the path.
    pub from: String,
    /// The longest prefix of the path that is a module, or the full path of an outside item.
    pub to: String,
    /// True inside test code.
    pub test: bool,
    /// True when `to` is in an outside crate.
    pub external: bool,
}

/// The modules of a crate and the paths that they name.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Graph {
    /// Each module path to its node.
    pub nodes: BTreeMap<String, Node>,
    /// One edge for each path that names a module or an outside crate.
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

/// Collects the declared modules, the paths and the alias table of one file.
#[derive(Default)]
struct FileVisitor {
    /// The current module path, with the inline modules.
    module: Vec<String>,
    /// The number of enclosing `#[cfg(test)]` items.
    test_depth: usize,
    /// The modules that `mod` items without a body declare.
    declared: Vec<Found>,
    /// Each path that the file names, with a leading `self` or `super` resolved.
    paths: Vec<Found>,
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
            UseTree::Glob(_) => self.add_path(prefix),
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
        self.paths.push(self.found(path));
    }

    /// Records `segments` with a leading `self` or `super` resolved.
    fn add_path(&mut self, segments: &[String]) {
        let found = self.found(resolve(&self.module, segments));
        self.paths.push(found);
    }

    /// Records each run of `name :: name ...` tokens from `cursor`, also inside groups.
    fn add_token_paths(&mut self, mut cursor: Cursor<'_>) {
        while let Some((_, mut next)) = cursor.token_tree() {
            if let Some((inside, ..)) = cursor.any_group() {
                self.add_token_paths(inside);
            } else if let Some((ident, mut after)) = cursor.ident() {
                let mut segments = vec![ident.to_string()];
                while let Some((ident, rest)) = path_separator(after).and_then(Cursor::ident) {
                    segments.push(ident.to_string());
                    after = rest;
                }
                self.add_path(&segments);
                next = after;
            }
            cursor = next;
        }
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

    fn visit_path(&mut self, path: &syn::Path) {
        let segments: Vec<String> = path
            .segments
            .iter()
            .map(|segment| segment.ident.to_string())
            .collect();
        self.add_path(&segments);
        visit::visit_path(self, path);
    }

    fn visit_macro(&mut self, item: &Macro) {
        visit::visit_macro(self, item);
        match item.parse_body_with(Punctuated::<Expr, Token![,]>::parse_terminated) {
            Ok(expressions) => {
                for expression in &expressions {
                    self.visit_expr(expression);
                }
            }
            Err(_) => self.add_token_paths(TokenBuffer::new2(item.tokens.clone()).begin()),
        }
    }

    // `pub(crate)` and `pub(super)` name no dependency.
    fn visit_vis_restricted(&mut self, _: &VisRestricted) {}
}

/// The cursor after a `::` at `cursor`.
fn path_separator(cursor: Cursor<'_>) -> Option<Cursor<'_>> {
    let (first, next) = cursor.punct()?;
    let (second, next) = next.punct()?;
    (first.as_char() == ':' && second.as_char() == ':').then_some(next)
}

/// The attributes of `item`.
fn attributes(item: &Item) -> &[Attribute] {
    match item {
        Item::Const(syn::ItemConst { attrs, .. })
        | Item::Enum(syn::ItemEnum { attrs, .. })
        | Item::ExternCrate(syn::ItemExternCrate { attrs, .. })
        | Item::Fn(syn::ItemFn { attrs, .. })
        | Item::ForeignMod(syn::ItemForeignMod { attrs, .. })
        | Item::Impl(syn::ItemImpl { attrs, .. })
        | Item::Macro(syn::ItemMacro { attrs, .. })
        | Item::Mod(syn::ItemMod { attrs, .. })
        | Item::Static(syn::ItemStatic { attrs, .. })
        | Item::Struct(syn::ItemStruct { attrs, .. })
        | Item::Trait(syn::ItemTrait { attrs, .. })
        | Item::TraitAlias(syn::ItemTraitAlias { attrs, .. })
        | Item::Type(syn::ItemType { attrs, .. })
        | Item::Union(syn::ItemUnion { attrs, .. })
        | Item::Use(syn::ItemUse { attrs, .. }) => attrs,
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

/// The edge from `from` to `found`, with its first segment resolved through `aliases`.
/// `None` when the path starts in no module of the crate and in no crate of `dependencies`.
fn resolve_edge(
    from: &str,
    found: &Found,
    aliases: &BTreeMap<String, String>,
    dependencies: &BTreeSet<String>,
) -> Option<Edge> {
    let (first, rest) = found.path.split_first()?;
    let head = aliases.get(first).unwrap_or(first);
    let to = if rest.is_empty() {
        head.clone()
    } else {
        format!("{head}::{}", rest.join("::"))
    };
    let start = head.split("::").next()?;
    let external = dependencies.contains(start);
    (external || start == "crate").then(|| Edge {
        from: from.to_owned(),
        to,
        test: found.test,
        external,
    })
}

/// The longest prefix of `path` that is a node of `nodes`.
fn module_prefix(nodes: &BTreeMap<String, Node>, path: &str) -> Option<String> {
    let segments: Vec<&str> = path.split("::").collect();
    (1..=segments.len())
        .rev()
        .map(|length| segments[..length].join("::"))
        .find(|prefix| nodes.contains_key(prefix))
}

/// The content of the file `file` in `root`. The error names the file.
fn read(root: &Path, file: &Path) -> Result<String, String> {
    fs::read_to_string(root.join(file)).map_err(|error| format!("{}: {error}", file.display()))
}

/// The content of the file `file` at the Git revision `reference` of the repository in `root`.
/// The error names the revision and the file.
fn show(root: &Path, reference: &str, file: &Path) -> Result<String, String> {
    let object = format!("{reference}:{}", file.display());
    let output = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["show", &object])
        .output()
        .map_err(|error| format!("{object}: {error}"))?;
    if !output.status.success() {
        return Err(format!(
            "{object}: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    String::from_utf8(output.stdout).map_err(|error| format!("{object}: {error}"))
}

/// The crate names that start an external path: the dependencies of `Cargo.toml`, also of each
/// target, `std`, `core` and `alloc`.
fn dependencies(
    read: &impl Fn(&Path) -> Result<String, String>,
) -> Result<BTreeSet<String>, String> {
    let manifest: Table = read(Path::new("Cargo.toml"))?
        .parse()
        .map_err(|error| format!("Cargo.toml: {error}"))?;
    let targets = manifest.get("target").and_then(Value::as_table);
    let tables = iter::once(&manifest).chain(
        targets
            .into_iter()
            .flat_map(Table::values)
            .filter_map(Value::as_table),
    );
    let sections = ["dependencies", "dev-dependencies", "build-dependencies"];
    let names = tables
        .flat_map(|table| {
            sections
                .iter()
                .filter_map(|section| table.get(*section)?.as_table())
        })
        .flat_map(Table::keys)
        .map(|name| name.replace('-', "_"));
    Ok(names
        .chain(["std", "core", "alloc"].map(str::to_owned))
        .collect())
}

/// Adds the node and the edges of `module` to `graph`. A module edge keeps its full path.
/// Returns the declared modules.
fn add_module(
    read: &impl Fn(&Path) -> Result<String, String>,
    dependencies: &BTreeSet<String>,
    module: &Found,
    graph: &mut Graph,
) -> Result<Vec<Found>, String> {
    let file = file_of(&module.path);
    let content = read(&file)?;
    let syntax =
        syn::parse_file(&content).map_err(|error| format!("{}: {error}", file.display()))?;
    let mut visitor = FileVisitor {
        module: module.path.clone(),
        test_depth: usize::from(module.test),
        ..FileVisitor::default()
    };
    visitor.visit_file(&syntax);
    let from = module.path.join("::");
    graph.edges.extend(
        visitor
            .paths
            .iter()
            .filter_map(|found| resolve_edge(&from, found, &visitor.aliases, dependencies)),
    );
    let layer = layer(&module.path);
    let hash = hash(&content);
    graph.nodes.insert(from, Node { file, layer, hash });
    Ok(visitor.declared)
}

/// Builds the graph of the crate in `root` from the `mod` items of `src/lib.rs`.
///
/// # Errors
///
/// When `Cargo.toml` or a module file cannot be read or parsed. The error names the file.
pub fn build(root: &Path) -> Result<Graph, String> {
    build_from(&|file| read(root, file))
}

/// Builds the graph of the crate in `root` from its files at the Git revision `reference`.
///
/// # Errors
///
/// When `git show` fails for `Cargo.toml` or a module file, or the file cannot be parsed. The
/// error names the file.
pub fn build_at(root: &Path, reference: &str) -> Result<Graph, String> {
    build_from(&|file| show(root, reference, file))
}

/// Builds the graph of the crate whose files `read` gives.
fn build_from(read: &impl Fn(&Path) -> Result<String, String>) -> Result<Graph, String> {
    let dependencies = dependencies(read)?;
    let mut graph = Graph::default();
    let mut pending = vec![Found {
        path: vec!["crate".to_owned()],
        test: false,
    }];
    while let Some(module) = pending.pop() {
        pending.extend(add_module(read, &dependencies, &module, &mut graph)?);
    }
    let edges = mem::take(&mut graph.edges);
    graph.edges = edges
        .into_iter()
        .filter_map(|edge| {
            if edge.external {
                return Some(edge);
            }
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
        let manifest = (
            "Cargo.toml",
            "[dependencies]\ntokio = \"1\"\n[target.'cfg(unix)'.dependencies]\nlibc = \"1\"\n",
        );
        for (path, content) in [&[manifest], files].concat() {
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
            external: false,
        }
    }

    /// An external edge from `from` to `to` outside test code.
    fn external(from: &str, to: &str) -> Edge {
        Edge {
            external: true,
            ..edge(from, to, false)
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

    /// A path inside a `#[cfg(test)]` module, method, trait method or struct gives a test edge.
    #[test]
    fn ac_0_7_4_use_in_test_module() {
        assert_eq!(
            edges(
                "src/app/x.rs",
                "#[cfg(test)]\nmod tests {\n    use crate::testing::fakes::FakeHome;\n}\nimpl A {\n    #[cfg(test)]\n    fn f() {\n        use crate::ports::Home;\n    }\n}\ntrait T {\n    #[cfg(test)]\n    fn g() {\n        use crate::domain::b::C;\n    }\n}\n#[cfg(test)]\nstruct S(crate::ports::Home);\n"
            ),
            [
                edge("crate::app::x", "crate::testing::fakes", true),
                edge("crate::app::x", "crate::ports", true),
                edge("crate::app::x", "crate::domain::b", true),
                edge("crate::app::x", "crate::ports", true),
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

    /// A `crate` path in an expression gives an edge to its longest module prefix.
    #[test]
    fn ac_0_7b_1_expression_path() {
        let graph = graph(&[
            (
                "src/lib.rs",
                "pub mod adapters { pub mod ha; }\nfn main() {\n    crate::adapters::ha::HaHome::connect();\n}\n",
            ),
            ("src/adapters/ha.rs", ""),
        ]);
        assert_eq!(graph.edges, [edge("crate", "crate::adapters::ha", false)]);
    }

    /// A dependency path in a `use` item or in an expression gives an external edge, also for a
    /// target dependency.
    #[test]
    fn ac_0_7b_2_external_edges() {
        assert_eq!(
            edges(
                "src/domain/b.rs",
                "use tokio::sync::Mutex;\nfn f() {\n    std::fs::read(\"a\");\n    libc::getpid();\n}\n"
            ),
            [
                external("crate::domain::b", "tokio::sync::Mutex"),
                external("crate::domain::b", "std::fs::read"),
                external("crate::domain::b", "libc::getpid"),
            ]
        );
    }

    /// A type path that starts with a module alias gives an edge to that module.
    #[test]
    fn ac_0_7b_3_module_alias() {
        assert_eq!(
            edges(
                "src/app/x.rs",
                "use crate::ports as p;\nfn f(home: p::Home) {}\n"
            ),
            [
                edge("crate::app::x", "crate::ports", false),
                edge("crate::app::x", "crate::ports", false),
            ]
        );
    }

    /// A path that starts with an alias of a dependency item gives an external edge.
    #[test]
    fn ac_0_7b_4_dependency_alias() {
        assert_eq!(
            edges(
                "src/domain/b.rs",
                "use tokio::sync::Mutex;\nfn f() {\n    Mutex::new();\n}\n"
            ),
            [
                external("crate::domain::b", "tokio::sync::Mutex"),
                external("crate::domain::b", "tokio::sync::Mutex::new"),
            ]
        );
    }

    /// Prelude names and local items give no edge.
    #[test]
    fn ac_0_7b_5_local_names() {
        let edges = edges(
            "src/domain/b.rs",
            "struct Intent;\nimpl Intent {\n    fn parse() {}\n}\nfn f() {\n    Vec::new();\n    Ok(1);\n    Intent::parse();\n}\n",
        );
        assert!(edges.is_empty(), "expected no edge, got {edges:?}");
    }

    /// A path in a macro body gives an edge, also when the body is not expressions.
    #[test]
    fn ac_0_7b_6_macro_body() {
        assert_eq!(
            edges(
                "src/domain/b.rs",
                "fn f(p: &str) {\n    let _ = format!(\"{}\", std::fs::read_to_string(p)?);\n    m!(fn g() { std::fs::write });\n}\n"
            ),
            [
                external("crate::domain::b", "std::fs::read_to_string"),
                external("crate::domain::b", "std::fs::write"),
            ]
        );
    }
}
