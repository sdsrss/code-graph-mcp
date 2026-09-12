//! Generic AST helpers shared by language-specific extractors:
//! callee-name extraction across multiple call-expression shapes,
//! depth-bounded string-literal lookup inside an arbitrary subtree.

use super::super::node_text;

pub(super) const MAX_SUBTREE_DEPTH: usize = 32;

pub(super) fn extract_callee_name(node: &tree_sitter::Node, source: &str) -> Option<String> {
    let function = node
        .child_by_field_name("function")
        .or_else(|| node.named_child(0))?;

    match function.kind() {
        "identifier" | "simple_identifier" => Some(node_text(&function, source).to_string()),
        "member_expression" | "field_expression" | "attribute" => {
            // e.g., obj.method — extract "method" or "obj.method"
            // Python tree-sitter uses node kind `attribute` with field `attribute`
            // for the method name; JS/TS use `member_expression` with `property`;
            // Go uses `field_expression` with `field`.
            if let Some(prop) = function
                .child_by_field_name("property")
                .or_else(|| function.child_by_field_name("field"))
                .or_else(|| function.child_by_field_name("attribute"))
            {
                Some(node_text(&prop, source).to_string())
            } else {
                Some(node_text(&function, source).to_string())
            }
        }
        "scoped_identifier" => {
            // Rust: Self::method(), Module::func(), std::collections::HashMap::new()
            // Extract the rightmost name component (the actual function being called)
            function
                .child_by_field_name("name")
                .map(|n| node_text(&n, source).to_string())
        }
        "selector_expression" => {
            // Go: receiver.Method(), http.HandleFunc(), etc.
            function
                .child_by_field_name("field")
                .map(|n| node_text(&n, source).to_string())
        }
        "qualified_identifier" => {
            // C++: Foo::bar(), ns::A::make() — extract the final identifier (the
            // function actually called). `name` may itself nest for A::B::c, so
            // take the rightmost `::` segment of the node text. Same-language
            // resolution then binds it like any bare call, mirroring Rust's
            // scoped_identifier handling above. Without this arm the call falls
            // to `_ => None` and the edge is silently dropped.
            node_text(&function, source)
                .rsplit("::")
                .next()
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
        }
        "navigation_expression" => {
            // Kotlin/Swift: obj.method() — last named child is the method name
            // Swift wraps it in navigation_suffix → simple_identifier
            let count = function.named_child_count();
            if count > 0 {
                let last = function.named_child(count - 1)?;
                if last.kind() == "navigation_suffix" {
                    // Swift: navigation_suffix -> simple_identifier
                    last.named_child(0)
                        .map(|n| node_text(&n, source).to_string())
                } else {
                    Some(node_text(&last, source).to_string())
                }
            } else {
                None
            }
        }
        _ => None, // Unknown callee expression — skip to avoid noise in call graph
    }
}

/// Shape of a callee's qualifier. Drives same-language candidate
/// disambiguation in the edge resolver. See
/// `docs/superpowers/specs/2026-05-11-bare-name-call-qualifier-design.md`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum CalleeQualifier {
    /// `foo()` — no qualifier (also: any non-Rust language)
    Bare,
    /// `crate::snapshot::create()` / `Module::foo()` / `Type::method()`
    /// Stored with leading `crate`/`super`/`self` segments stripped.
    /// Empty after strip → caller must convert to Bare before serialization.
    Path(Vec<String>),
    /// `Self::method()` — payload is the enclosing impl block's type name.
    SelfType(String),
    /// `self.method()` — payload is the enclosing impl block's type name.
    SelfRecv(String),
    /// `obj.method()` where receiver is a plain identifier of unknown type.
    Receiver(String),
    /// `OpenOptions::new().create(true)` — receiver is a call_expression
    /// (any chain).
    Chain,
}

/// Like `extract_callee_name` but also returns the qualifier shape.
/// Non-Rust languages always return `Bare`. Rust dispatches on the
/// function-node kind to detect scoped_identifier paths.
pub(crate) fn extract_callee(
    node: &tree_sitter::Node,
    source: &str,
    language: &str,
    current_rust_impl: Option<&str>,
) -> Option<(String, CalleeQualifier)> {
    let _ = current_rust_impl; // used in Task 8+
    if language != "rust" {
        // JS-family: capture a SIMPLE-identifier receiver (`m.foo()`) so the
        // indexer can bind the call to a require-namespaced module
        // (`const m = require('./x')`). Every other shape — `this.x()`,
        // `a.b.x()`, `chain().x()`, bare `foo()` — keeps the Bare qualifier so
        // its resolution path is unchanged (only `identifier.method()` is newly
        // routed through the receiver-aware branch, which falls back to the
        // identical default resolution when the receiver is not a namespace).
        if matches!(language, "javascript" | "typescript" | "tsx") {
            if let Some(function) = node.child_by_field_name("function") {
                if function.kind() == "member_expression" {
                    if let (Some(obj), Some(prop)) = (
                        function.child_by_field_name("object"),
                        function.child_by_field_name("property"),
                    ) {
                        if obj.kind() == "identifier" {
                            let name = node_text(&prop, source).to_string();
                            if !name.is_empty() {
                                let recv = node_text(&obj, source).to_string();
                                return Some((name, CalleeQualifier::Receiver(recv)));
                            }
                        }
                    }
                }
            }
        }
        return extract_callee_name(node, source).map(|n| (n, CalleeQualifier::Bare));
    }

    let function = node
        .child_by_field_name("function")
        .or_else(|| node.named_child(0))?;

    match function.kind() {
        // Rust grammar uses "identifier" for bare callees. Other grammars
        // (e.g. Kotlin) use "simple_identifier"; if we ever share this match
        // arm with them, intentionally let "simple_identifier" fall through
        // to the `_` arm where extract_callee_name handles it generically.
        "identifier" => Some((
            node_text(&function, source).to_string(),
            CalleeQualifier::Bare,
        )),
        "scoped_identifier" => extract_rust_scoped(&function, source),
        "field_expression" => extract_rust_field(&function, source),
        _ => extract_callee_name(node, source).map(|n| (n, CalleeQualifier::Bare)),
    }
}

/// Extract the bare target and written receiver path from a Python call.
///
/// Direct `self` and `cls` calls carry the enclosing class. Other attribute
/// chains remain syntactic paths here; the indexer later decides whether the
/// leading segment is an import binding, an exact class, or a runtime receiver.
/// Keeping that decision out of the parser prevents filenames from being
/// mistaken for imports.
pub(crate) fn extract_python_callee(
    node: &tree_sitter::Node,
    source: &str,
    current_class: Option<&str>,
) -> Option<(String, CalleeQualifier)> {
    let function = node
        .child_by_field_name("function")
        .or_else(|| node.named_child(0))?;

    if function.kind() != "attribute" {
        return extract_callee_name(node, source).map(|n| (n, CalleeQualifier::Bare));
    }

    let attribute = function.child_by_field_name("attribute")?;
    let name = node_text(&attribute, source).to_string();
    let object = function.child_by_field_name("object")?;

    if object.kind() == "identifier" {
        let receiver = node_text(&object, source);
        if matches!(receiver, "self" | "cls") {
            return Some((
                name,
                current_class
                    .map(|class| CalleeQualifier::SelfRecv(class.to_string()))
                    .unwrap_or_else(|| CalleeQualifier::Receiver(receiver.to_string())),
            ));
        }
    }

    if let Some(path) = collect_python_attribute_segments(&object, source) {
        if !path.is_empty() {
            return Some((name, CalleeQualifier::Path(path)));
        }
    }

    Some((name, CalleeQualifier::Chain))
}

/// Flatten an identifier/attribute-only Python receiver into dotted segments.
/// Calls and subscripts return `None` because their runtime value is not a
/// statically named path.
fn collect_python_attribute_segments(
    node: &tree_sitter::Node,
    source: &str,
) -> Option<Vec<String>> {
    match node.kind() {
        "identifier" => Some(vec![node_text(node, source).to_string()]),
        "attribute" => {
            let object = node.child_by_field_name("object")?;
            let attribute = node.child_by_field_name("attribute")?;
            let mut segments = collect_python_attribute_segments(&object, source)?;
            segments.push(node_text(&attribute, source).to_string());
            Some(segments)
        }
        _ => None,
    }
}

/// Walk a scoped_identifier collecting all path segments + final name.
/// `crate::a::b::foo` → segments=["crate","a","b"], name="foo"
fn collect_scoped_path_segments(node: &tree_sitter::Node, source: &str, out: &mut Vec<String>) {
    if node.kind() == "scoped_identifier" {
        if let Some(path) = node.child_by_field_name("path") {
            collect_scoped_path_segments(&path, source, out);
        }
        if let Some(name) = node.child_by_field_name("name") {
            out.push(node_text(&name, source).to_string());
        }
    } else if matches!(node.kind(), "identifier" | "type_identifier") {
        out.push(node_text(node, source).to_string());
    }
}

/// Handle Rust scoped_identifier callee. Returns name + Path qualifier with
/// reserved prefixes (crate/super/self) stripped; SelfType detected when first
/// segment is "Self" (added in Task 10 by overriding the qualifier).
fn extract_rust_scoped(
    function: &tree_sitter::Node,
    source: &str,
) -> Option<(String, CalleeQualifier)> {
    let mut all = Vec::new();
    collect_scoped_path_segments(function, source, &mut all);
    if all.is_empty() {
        return None;
    }
    let name = all.pop()?;
    let mut path: Vec<String> = all;

    // `Self::method()` → SelfType (payload filled by mod.rs from current_rust_impl).
    // Detected before the lowercase-reserved strip because `Self` is uppercase
    // and would otherwise pass through as a Path qualifier with v="Self".
    if path.first().is_some_and(|s| s == "Self") {
        return Some((name, CalleeQualifier::SelfType(String::new())));
    }

    let skip = path
        .iter()
        .take_while(|s| matches!(s.as_str(), "crate" | "super" | "self"))
        .count();
    path.drain(..skip);
    if path.is_empty() {
        Some((name, CalleeQualifier::Bare))
    } else {
        Some((name, CalleeQualifier::Path(path)))
    }
}

/// Handle Rust field_expression callee (`obj.method()`, `self.method()`,
/// `chain().method()`). Returns name + qualifier shape:
///   value=self / self_expression → SelfRecv (payload filled by caller via current_rust_impl in T9)
///   value=identifier             → Receiver(<text>)
///   value=call_expression        → Chain
///   else                         → Bare (unknown shape, conservative)
fn extract_rust_field(
    function: &tree_sitter::Node,
    source: &str,
) -> Option<(String, CalleeQualifier)> {
    let field = function.child_by_field_name("field")?;
    let name = node_text(&field, source).to_string();
    let value = function.child_by_field_name("value");
    let qualifier = match value.as_ref().map(|v| v.kind()) {
        Some("self") | Some("self_expression") => {
            // SelfRecv with empty payload here; mod.rs call_expression arm
            // overwrites payload from current_rust_impl context (T9).
            CalleeQualifier::SelfRecv(String::new())
        }
        Some("identifier") => {
            CalleeQualifier::Receiver(node_text(&value.unwrap(), source).to_string())
        }
        Some("call_expression") => CalleeQualifier::Chain,
        _ => CalleeQualifier::Bare,
    };
    Some((name, qualifier))
}

pub(super) fn extract_string_from_subtree(
    node: &tree_sitter::Node,
    source: &str,
) -> Option<String> {
    extract_string_from_subtree_inner(node, source, 0)
}

fn extract_string_from_subtree_inner(
    node: &tree_sitter::Node,
    source: &str,
    depth: usize,
) -> Option<String> {
    if depth > MAX_SUBTREE_DEPTH {
        return None;
    }
    if node.kind() == "string" {
        let text = node_text(node, source);
        let text = text.trim_start_matches(['f', 'r', 'b', 'u', 'F', 'R', 'B', 'U']);
        return Some(text.trim_matches(|c| c == '\'' || c == '"').to_string());
    }
    // tree-sitter-php gives a DOUBLE-quoted string its own kind, `encapsed_string`,
    // because that form can interpolate; only the single-quoted form is `string`.
    // So `require_once "lib.php"` extracted nothing while `require_once 'lib.php'`
    // worked — all four include keywords, and double quotes are the more common
    // spelling (`require_once "vendor/autoload.php"`). PHP is the only grammar
    // using this kind, so no other language's walk is affected.
    //
    // An interpolated path (`require_once "$dir/lib.php"`) is deliberately NOT
    // extracted: its value is not known statically, and guessing the stem would
    // bind a real edge to whatever file happens to share the literal tail.
    // Precision over recall, the same call the resolver makes elsewhere.
    if node.kind() == "encapsed_string" {
        let text = node_text(node, source);
        let unquoted = text.trim_matches(|c| c == '"');
        if !unquoted.contains('$') && !unquoted.contains('{') {
            return Some(unquoted.to_string());
        }
        return None;
    }
    for i in 0..node.child_count() {
        if let Some(child) = node.child(i) {
            if let Some(s) = extract_string_from_subtree_inner(&child, source, depth + 1) {
                return Some(s);
            }
        }
    }
    None
}
