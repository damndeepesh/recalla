//! Definition finder: maps a source file to the symbols it defines, using
//! tree-sitter grammars behind the `lang-*` cargo features.

use std::ops::{ControlFlow, Range};
use std::path::Path;
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use streaming_iterator::StreamingIterator;
use tree_sitter::{Language, Node, ParseOptions, ParseState, Parser, Query, QueryCursor};

/// Files above this size are never parsed.
pub const MAX_SOURCE_BYTES: usize = 2 * 1024 * 1024;
const PARSE_BUDGET: Duration = Duration::from_millis(500);

/// A language with a definition query. Compiled in only if its feature is on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Lang {
    Rust,
    Python,
    Go,
    TypeScript,
    Tsx,
    JavaScript,
}

impl Lang {
    /// Name stored in `anchors.lang`.
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Self::Rust => "rust",
            Self::Python => "python",
            Self::Go => "go",
            Self::TypeScript | Self::Tsx => "typescript",
            Self::JavaScript => "javascript",
        }
    }

    const fn index(self) -> usize {
        self as usize
    }

    fn grammar(self) -> Option<Language> {
        match self {
            #[cfg(feature = "lang-rust")]
            Self::Rust => Some(tree_sitter_rust::LANGUAGE.into()),
            #[cfg(feature = "lang-python")]
            Self::Python => Some(tree_sitter_python::LANGUAGE.into()),
            #[cfg(feature = "lang-go")]
            Self::Go => Some(tree_sitter_go::LANGUAGE.into()),
            #[cfg(feature = "lang-ts")]
            Self::TypeScript => Some(tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into()),
            #[cfg(feature = "lang-ts")]
            Self::Tsx => Some(tree_sitter_typescript::LANGUAGE_TSX.into()),
            #[cfg(feature = "lang-js")]
            Self::JavaScript => Some(tree_sitter_javascript::LANGUAGE.into()),
            #[allow(unreachable_patterns)]
            _ => None,
        }
    }

    fn query_src(self) -> &'static str {
        match self {
            Self::Rust => RUST_Q,
            Self::Python => PYTHON_Q,
            Self::Go => GO_Q,
            Self::TypeScript | Self::Tsx => TS_Q,
            Self::JavaScript => JS_Q,
        }
    }
}

const RUST_Q: &str = "
(function_item name: (_) @name) @def
(struct_item name: (_) @name) @def
(enum_item name: (_) @name) @def
(trait_item name: (_) @name) @def
(type_item name: (_) @name) @def
(const_item name: (_) @name) @def
(static_item name: (_) @name) @def
(mod_item name: (_) @name) @def
(macro_definition name: (_) @name) @def
";
const PYTHON_Q: &str = "
(function_definition name: (_) @name) @def
(class_definition name: (_) @name) @def
";
const GO_Q: &str = "
(function_declaration name: (_) @name) @def
(method_declaration name: (_) @name) @def
(type_spec name: (_) @name) @def
";
const JS_Q: &str = "
(function_declaration name: (_) @name) @def
(class_declaration name: (_) @name) @def
(method_definition name: (_) @name) @def
(lexical_declaration (variable_declarator name: (identifier) @name value: (_) @value)) @def
";
const TS_Q: &str = "
(function_declaration name: (_) @name) @def
(class_declaration name: (_) @name) @def
(abstract_class_declaration name: (_) @name) @def
(method_definition name: (_) @name) @def
(interface_declaration name: (_) @name) @def
(type_alias_declaration name: (_) @name) @def
(enum_declaration name: (_) @name) @def
(lexical_declaration (variable_declarator name: (identifier) @name value: (_) @value)) @def
";

/// Initializer kinds that make `const x = ...` a function definition.
const FN_VALUES: [&str; 4] = [
    "arrow_function",
    "function_expression",
    "function",
    "generator_function",
];
const CLASS_KINDS: [&str; 3] = ["class_declaration", "abstract_class_declaration", "class"];

/// Language for a file, by extension. `None` for unknown extensions and for
/// languages whose cargo feature is off.
#[must_use]
pub fn language_for(path: &Path) -> Option<Lang> {
    let ext = path.extension()?.to_str()?.to_ascii_lowercase();
    let lang = match ext.as_str() {
        "rs" => Lang::Rust,
        "py" => Lang::Python,
        "go" => Lang::Go,
        "ts" | "mts" | "cts" => Lang::TypeScript,
        "tsx" => Lang::Tsx,
        "js" | "jsx" | "mjs" | "cjs" => Lang::JavaScript,
        _ => return None,
    };
    lang.grammar().map(|_| lang)
}

/// A named definition found in a source file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Def {
    pub name: String,
    /// `Type::method` (Rust), `Class.method` (Python, TS/JS), `Recv.Method`
    /// (Go); equals `name` for free items.
    pub qualified: String,
    /// Byte range of the whole definition in the source.
    pub range: Range<usize>,
}

fn query(lang: Lang) -> Option<&'static Query> {
    static QUERIES: [OnceLock<Option<Query>>; 6] = [const { OnceLock::new() }; 6];
    QUERIES[lang.index()]
        .get_or_init(|| Query::new(&lang.grammar()?, lang.query_src()).ok())
        .as_ref()
}

/// Definitions in `src`, ordered by position. Empty on an oversized input,
/// parse failure or timeout; never panics.
#[must_use]
pub fn definitions(lang: Lang, src: &str) -> Vec<Def> {
    if src.len() > MAX_SOURCE_BYTES {
        return Vec::new();
    }
    let (Some(grammar), Some(query)) = (lang.grammar(), query(lang)) else {
        return Vec::new();
    };
    let mut parser = Parser::new();
    if parser.set_language(&grammar).is_err() {
        return Vec::new();
    }
    let deadline = Instant::now() + PARSE_BUDGET;
    let mut progress = |_: &ParseState| {
        if Instant::now() > deadline {
            ControlFlow::Break(())
        } else {
            ControlFlow::Continue(())
        }
    };
    let bytes = src.as_bytes();
    let options = ParseOptions::new().progress_callback(&mut progress);
    let Some(tree) = parser.parse_with_options(
        &mut |i, _| bytes.get(i..).unwrap_or_default(),
        None,
        Some(options),
    ) else {
        return Vec::new();
    };
    let (Some(def_ix), Some(name_ix)) = (
        query.capture_index_for_name("def"),
        query.capture_index_for_name("name"),
    ) else {
        return Vec::new();
    };
    let value_ix = query.capture_index_for_name("value");

    let mut cursor = QueryCursor::new();
    let mut matches = cursor.matches(query, tree.root_node(), bytes);
    let mut defs = Vec::new();
    while let Some(m) = matches.next() {
        let node_of = |ix| m.captures().iter().find(|c| c.index == ix).map(|c| c.node);
        let (Some(def), Some(name)) = (node_of(def_ix), node_of(name_ix)) else {
            continue;
        };
        if let Some(value) = value_ix.and_then(node_of)
            && !FN_VALUES.contains(&value.kind())
        {
            continue;
        }
        let Some(name) = text(name, src) else {
            continue;
        };
        let qualified = qualifier(lang, def, src)
            .map_or_else(|| name.to_owned(), |q| format!("{q}{}{name}", sep(lang)));
        defs.push(Def {
            name: name.to_owned(),
            qualified,
            range: def.byte_range(),
        });
    }
    defs.sort_by_key(|d| (d.range.start, d.range.end));
    defs
}

fn sep(lang: Lang) -> &'static str {
    if lang == Lang::Rust { "::" } else { "." }
}

fn text<'s>(node: Node<'_>, src: &'s str) -> Option<&'s str> {
    src.get(node.byte_range())
}

/// Enclosing type for a method, or `None` for free items.
fn qualifier(lang: Lang, def: Node<'_>, src: &str) -> Option<String> {
    match lang {
        Lang::Rust => rust_owner(def, src),
        Lang::Go => go_receiver(def, src),
        Lang::Python => python_classes(def, src),
        Lang::TypeScript | Lang::Tsx | Lang::JavaScript => class_owner(def, src),
    }
}

/// Last path segment of a type, without generics, pointers or references.
fn bare_type(t: &str) -> &str {
    let t = t.split(['<', '[']).next().unwrap_or(t);
    let t = t.rsplit("::").next().unwrap_or(t);
    t.trim_start_matches(['*', '&']).trim()
}

fn rust_owner(def: Node<'_>, src: &str) -> Option<String> {
    let owner = def.parent()?.parent()?;
    let ty = match owner.kind() {
        "impl_item" => owner.child_by_field_name("type")?,
        "trait_item" => owner.child_by_field_name("name")?,
        _ => return None,
    };
    Some(bare_type(text(ty, src)?).to_owned())
}

fn go_receiver(def: Node<'_>, src: &str) -> Option<String> {
    if def.kind() != "method_declaration" {
        return None;
    }
    let ty = def
        .child_by_field_name("receiver")?
        .named_child(0)?
        .child_by_field_name("type")?;
    Some(bare_type(text(ty, src)?).to_owned())
}

fn python_classes(def: Node<'_>, src: &str) -> Option<String> {
    let mut names = Vec::new();
    let mut p = def.parent();
    while let Some(n) = p {
        match n.kind() {
            "function_definition" => break,
            "class_definition" => names.push(text(n.child_by_field_name("name")?, src)?),
            _ => {}
        }
        p = n.parent();
    }
    names.reverse();
    (!names.is_empty()).then(|| names.join("."))
}

fn class_owner(def: Node<'_>, src: &str) -> Option<String> {
    if def.kind() != "method_definition" {
        return None;
    }
    let mut p = def.parent();
    while let Some(n) = p {
        if CLASS_KINDS.contains(&n.kind()) {
            return text(n.child_by_field_name("name")?, src).map(str::to_owned);
        }
        p = n.parent();
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn defs(lang: Lang, src: &str) -> Vec<(String, String)> {
        definitions(lang, src)
            .into_iter()
            .map(|d| (d.name, d.qualified))
            .collect()
    }

    fn pairs(v: &[(&str, &str)]) -> Vec<(String, String)> {
        v.iter()
            .map(|(a, b)| ((*a).to_owned(), (*b).to_owned()))
            .collect()
    }

    #[test]
    fn extensions_map_to_languages() {
        let l = |p: &str| language_for(Path::new(p));
        assert_eq!(l("a/b.rs"), Some(Lang::Rust));
        assert_eq!(l("x.PY"), Some(Lang::Python));
        assert_eq!(l("x.go"), Some(Lang::Go));
        for e in ["ts", "mts", "cts"] {
            assert_eq!(l(&format!("x.{e}")), Some(Lang::TypeScript));
        }
        assert_eq!(l("x.tsx"), Some(Lang::Tsx));
        for e in ["js", "jsx", "mjs", "cjs"] {
            assert_eq!(l(&format!("x.{e}")), Some(Lang::JavaScript));
        }
        assert_eq!(l("x.md"), None);
        assert_eq!(l("Makefile"), None);
    }

    #[test]
    fn rust_items_and_methods() {
        let src = "\
mod m { pub fn inner() {} }
struct Store;
enum Kind { A }
trait Greet { fn hi(&self) {} fn sig(&self); }
type Alias = u8;
const MAX: u8 = 1;
static NAME: &str = \"x\";
macro_rules! mk { () => {} }
fn free() {}
impl<T> Store<T> { fn insert(&self) {} }
impl Greet for Store { fn hi(&self) {} }
";
        let want = pairs(&[
            ("m", "m"),
            ("inner", "inner"),
            ("Store", "Store"),
            ("Kind", "Kind"),
            ("Greet", "Greet"),
            ("hi", "Greet::hi"),
            ("Alias", "Alias"),
            ("MAX", "MAX"),
            ("NAME", "NAME"),
            ("mk", "mk"),
            ("free", "free"),
            ("insert", "Store::insert"),
            ("hi", "Store::hi"),
        ]);
        assert_eq!(defs(Lang::Rust, src), want);
    }

    #[test]
    fn rust_range_covers_definition() {
        let src = "// c\nfn a() { 1 }\nfn b() {}\n";
        let d = &definitions(Lang::Rust, src)[0];
        assert_eq!(&src[d.range.clone()], "fn a() { 1 }");
    }

    #[test]
    fn python_classes_and_methods() {
        let src = "\
def top():
    def nested(): pass
class A:
    def m(self): pass
    class B:
        def n(self): pass
";
        assert_eq!(
            defs(Lang::Python, src),
            pairs(&[
                ("top", "top"),
                ("nested", "nested"),
                ("A", "A"),
                ("m", "A.m"),
                ("B", "A.B"),
                ("n", "A.B.n"),
            ])
        );
        let d = &definitions(Lang::Python, src)[0];
        assert!(src[d.range.clone()].starts_with("def top"));
    }

    #[test]
    fn go_funcs_methods_types() {
        let src = "\
package p
type Store struct{}
type Reader interface{ Read() }
func Free() {}
func (s *Store) Insert() {}
func (s Store) Get() {}
";
        assert_eq!(
            defs(Lang::Go, src),
            pairs(&[
                ("Store", "Store"),
                ("Reader", "Reader"),
                ("Free", "Free"),
                ("Insert", "Store.Insert"),
                ("Get", "Store.Get"),
            ])
        );
    }

    #[test]
    fn typescript_definitions() {
        let src = "\
function f() {}
export class Store { insert(): void {} }
interface I { x: number }
type T = string;
enum E { A }
const arrow = () => 1;
const fe = function () {};
const num = 3;
abstract class Base { run() {} }
";
        assert_eq!(
            defs(Lang::TypeScript, src),
            pairs(&[
                ("f", "f"),
                ("Store", "Store"),
                ("insert", "Store.insert"),
                ("I", "I"),
                ("T", "T"),
                ("E", "E"),
                ("arrow", "arrow"),
                ("fe", "fe"),
                ("Base", "Base"),
                ("run", "Base.run"),
            ])
        );
    }

    #[test]
    fn tsx_parses_jsx() {
        let src = "const App = () => <div/>;\nfunction B() { return <p/>; }\n";
        assert_eq!(defs(Lang::Tsx, src), pairs(&[("App", "App"), ("B", "B")]));
    }

    #[test]
    fn javascript_definitions() {
        let src = "\
function f() {}
class Store { insert() {} }
const arrow = () => 1;
const x = 2;
";
        assert_eq!(
            defs(Lang::JavaScript, src),
            pairs(&[
                ("f", "f"),
                ("Store", "Store"),
                ("insert", "Store.insert"),
                ("arrow", "arrow"),
            ])
        );
    }

    #[test]
    fn broken_and_oversized_input_never_panics() {
        assert!(definitions(Lang::Rust, "").is_empty());
        let _ = definitions(Lang::Rust, "fn ((( {{{ impl");
        let big = "x".repeat(MAX_SOURCE_BYTES + 1);
        assert!(definitions(Lang::Python, &big).is_empty());
    }
}
