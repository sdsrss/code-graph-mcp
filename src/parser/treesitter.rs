use super::lang_config::LanguageConfig;
use super::languages::get_language;
use super::node_text;
use crate::domain::{max_code_content_len, parse_timeout_ms, MAX_AST_DEPTH};
use anyhow::{anyhow, Result};
use std::borrow::Cow;
use std::cell::RefCell;
use std::collections::HashMap;

pub struct ParsedNode {
    pub node_type: String,
    pub name: String,
    pub qualified_name: Option<String>,
    pub start_line: u32,
    pub end_line: u32,
    pub code_content: String,
    pub signature: Option<String>,
    pub doc_comment: Option<String>,
    pub return_type: Option<String>,
    /// Full parameter text from AST, e.g. "(a: number, b: string)" — includes names and types,
    /// not just type annotations. Stored as-is for FTS search (users may search by param names).
    pub param_types: Option<String>,
    /// True if this node is inside a test context (#[cfg(test)], mod tests, #[test], etc.)
    pub is_test: bool,
}

thread_local! {
    static PARSER_CACHE: RefCell<HashMap<String, tree_sitter::Parser>> = RefCell::new(HashMap::new());
}

/// Parse source code into a Tree-sitter tree. Shared by node extraction and relation extraction.
pub fn parse_tree(source: &str, language: &str) -> Result<tree_sitter::Tree> {
    let lang =
        get_language(language).ok_or_else(|| anyhow!("unsupported language: {}", language))?;

    PARSER_CACHE.with(|cache| {
        let mut cache = cache.borrow_mut();
        if !cache.contains_key(language) {
            let mut p = tree_sitter::Parser::new();
            p.set_timeout_micros(parse_timeout_ms() * 1000);
            p.set_language(&lang)?;
            cache.insert(language.to_string(), p);
        }
        let parser = cache
            .get_mut(language)
            .ok_or_else(|| anyhow!("parser cache inconsistency for {}", language))?;
        match parser.parse(source, None) {
            Some(tree) => Ok(tree),
            None => {
                parser.reset();
                Err(anyhow!("parse failed or timed out"))
            }
        }
    })
}

pub fn parse_code(source: &str, language: &str) -> Result<Vec<ParsedNode>> {
    let tree = parse_tree(source, language)?;
    Ok(extract_nodes_from_tree(&tree, source, language))
}

/// Extract nodes from a pre-parsed tree (avoids re-parsing).
pub fn extract_nodes_from_tree(
    tree: &tree_sitter::Tree,
    source: &str,
    language: &str,
) -> Vec<ParsedNode> {
    let mut nodes = Vec::new();
    let config = LanguageConfig::for_language(language);
    extract_nodes(
        tree.root_node(),
        source,
        language,
        &config,
        None,
        &mut nodes,
        0,
        false,
    );
    nodes
}

/// Check if a node has a preceding `#[cfg(test)]` or `#[test]` attribute.
fn has_test_attribute(node: &tree_sitter::Node, source: &str) -> bool {
    let mut sibling = node.prev_sibling();
    while let Some(s) = sibling {
        match s.kind() {
            "attribute_item" | "inner_attribute_item" => {
                let text = node_text(&s, source);
                if text.contains("cfg(test)") || text == "#[test]" || text.contains("::test]") {
                    return true;
                }
            }
            "line_comment" | "block_comment" | "comment" => {}
            _ => break,
        }
        sibling = s.prev_sibling();
    }
    false
}

#[allow(clippy::too_many_arguments)]
fn extract_nodes(
    node: tree_sitter::Node,
    source: &str,
    language: &str,
    config: &LanguageConfig,
    parent_class: Option<&str>,
    results: &mut Vec<ParsedNode>,
    depth: usize,
    in_test_context: bool,
) {
    if depth > MAX_AST_DEPTH {
        return;
    }
    let kind = node.kind();

    // Detect Rust mod items (e.g., `mod tests { ... }`)
    if kind == "mod_item" {
        let mod_name = node
            .child_by_field_name("name")
            .map(|n| node_text(&n, source).to_string());
        let is_test_mod = mod_name.as_deref() == Some("tests") || has_test_attribute(&node, source);
        // Recurse into the module body with updated test context
        if let Some(body) = node.child_by_field_name("body") {
            for i in 0..body.named_child_count() {
                if let Some(child) = body.named_child(i) {
                    extract_nodes(
                        child,
                        source,
                        language,
                        config,
                        parent_class,
                        results,
                        depth + 1,
                        in_test_context || is_test_mod,
                    );
                }
            }
        }
        return;
    }

    // JS/TS test-framework call blocks (Jest/Mocha/Vitest/Node): describe()/it()/
    // test()/beforeEach()/etc. Function definitions inside these callback args are
    // test code, not production. Propagate in_test_context so the AST `is_test`
    // flag (stored on the node and checked in SQL via `n.is_test = 0`) excludes them.
    if matches!(config.name, "javascript" | "typescript" | "tsx")
        && kind == "call_expression"
        && !in_test_context
    {
        if let Some(fn_node) = node.child_by_field_name("function") {
            let fn_text = node_text(&fn_node, source);
            // Match bare names and member forms like `describe.only`, `it.skip`, `test.each`.
            let head = fn_text.split('.').next().unwrap_or(fn_text);
            let is_test_block = matches!(
                head,
                "describe"
                    | "it"
                    | "test"
                    | "suite"
                    | "context"
                    | "beforeEach"
                    | "beforeAll"
                    | "afterEach"
                    | "afterAll"
                    | "before"
                    | "after"
                    | "fdescribe"
                    | "xdescribe"
                    | "fit"
                    | "xit"
            );
            if is_test_block {
                if let Some(args) = node.child_by_field_name("arguments") {
                    for i in 0..args.named_child_count() {
                        if let Some(child) = args.named_child(i) {
                            extract_nodes(
                                child,
                                source,
                                language,
                                config,
                                parent_class,
                                results,
                                depth + 1,
                                true,
                            );
                        }
                    }
                }
                return;
            }
        }
    }

    // Check if this specific node has #[test] or #[cfg(test)] attributes
    let node_is_test =
        in_test_context || (config.has_test_attributes && has_test_attribute(&node, source));

    match kind {
        // Functions: shared across TS/JS/Go (function_declaration), Python/C/C++ (function_definition)
        "function_declaration" | "function" => {
            if let Some(mut parsed) = extract_function_node(&node, source, "function", parent_class)
            {
                parsed.is_test = node_is_test;
                results.push(parsed);
            } else if let Some(name) = super::route_handler_name(&node, source) {
                // Anonymous `function (req, res) { ... }` used as an inline route
                // handler (no name field → extract_function_node returns None):
                // materialize it like the arrow / function_expression case below.
                results.push(make_simple_node(
                    "function",
                    name,
                    &node,
                    source,
                    node_is_test,
                ));
            }
        }
        // Python async functions
        "async_function_definition" => {
            let nt = if parent_class.is_some() {
                "method"
            } else {
                "function"
            };
            if let Some(mut parsed) = extract_function_node(&node, source, nt, parent_class) {
                parsed.is_test = node_is_test;
                results.push(parsed);
            }
        }

        "function_definition" => {
            if config.name == "c" || config.name == "cpp" {
                // C/C++: name is in declarator child, not name field.
                // gtest macros (`TEST(Suite, Name) { ... }`) parse as
                // function_definition with declarator name = the macro;
                // we extract `Suite.Name` instead and mark is_test=true.
                if let Some(declarator) = node.child_by_field_name("declarator") {
                    let gtest_name = extract_gtest_test_name(&declarator, source);
                    let is_gtest = gtest_name.is_some();
                    let name = gtest_name.or_else(|| extract_declarator_name(&declarator, source));
                    if let Some(name) = name {
                        let sig_info = extract_c_signature_info(&node, source);
                        // C/C++ method scope. gtest macro names ("Suite.Name")
                        // and free functions stay bare; an out-of-class
                        // definition `int Foo::bar(){}` (name = "Foo::bar") or an
                        // in-class definition (parent_class = Some) becomes
                        // node_type "method" + qualified_name "Foo.bar".
                        let (bare, qual, nt): (String, String, &str) = if is_gtest {
                            (name.clone(), name.clone(), "function")
                        } else if let Some((cls, method)) = name.rsplit_once("::") {
                            let cls = cls.rsplit("::").next().unwrap_or(cls);
                            (method.to_string(), format!("{}.{}", cls, method), "method")
                        } else if let Some(cls) = parent_class {
                            (name.clone(), format!("{}.{}", cls, name), "method")
                        } else {
                            (name.clone(), name.clone(), "function")
                        };
                        results.push(ParsedNode {
                            node_type: nt.into(),
                            name: bare,
                            qualified_name: Some(qual),
                            start_line: node.start_position().row as u32 + 1,
                            end_line: node.end_position().row as u32 + 1,
                            code_content: truncate_code_content(node_text(&node, source))
                                .into_owned(),
                            signature: sig_info.signature,
                            doc_comment: get_doc_comment(&node, source),
                            return_type: sig_info.return_type,
                            param_types: sig_info.param_types,
                            is_test: node_is_test || is_gtest,
                        });
                    }
                }
            } else {
                // Python and others: name is in "name" field
                let nt = if parent_class.is_some() {
                    "method"
                } else {
                    "function"
                };
                if let Some(mut parsed) = extract_function_node(&node, source, nt, parent_class) {
                    parsed.is_test = node_is_test;
                    results.push(parsed);
                }
            }
        }
        "function_item" => {
            // Rust functions
            if let Some(mut parsed) = extract_function_node(&node, source, "function", parent_class)
            {
                parsed.is_test = node_is_test;
                results.push(parsed);
            }
        }

        // Arrow functions (TS/JS): covers const/let (lexical) and var (variable)
        // A single declaration may contain multiple arrow functions.
        "lexical_declaration" | "variable_declaration" => {
            for mut parsed in extract_named_arrows(&node, source) {
                parsed.is_test = node_is_test;
                results.push(parsed);
            }
        }

        // Classes: shared across TS/JS/Java (class_declaration), Python (class_definition)
        // Kotlin: both classes and interfaces use class_declaration — distinguish by first child kind
        "class_declaration" | "class" | "class_definition" => {
            if let Some(name) = get_child_by_field(&node, "name", source) {
                // Kotlin interfaces are class_declaration with first child kind "interface"
                // Swift reuses class_declaration for class/struct/enum — first child is the keyword
                let node_type_str = match node.child(0).map(|c| c.kind()) {
                    Some("interface") => "interface",
                    Some("struct") => "struct",
                    Some("enum") => "enum",
                    _ => "class",
                };
                // Widen to the `decorated_definition` wrapper so a decorated Python
                // class (`@dataclass class Config:`) retains its decorator in the
                // extent/source (issue #31). No-op for non-Python classes and
                // undecorated Python classes (extent == node).
                let extent = python_decorated_extent(&node);
                results.push(ParsedNode {
                    node_type: node_type_str.into(),
                    name: name.clone(),
                    qualified_name: Some(name.clone()),
                    start_line: extent.start_position().row as u32 + 1,
                    end_line: extent.end_position().row as u32 + 1,
                    code_content: truncate_code_content(node_text(&extent, source)).into_owned(),
                    signature: None,
                    // Same split as extract_function_node: docstring from the
                    // class node itself, comment fallback from the decorated
                    // extent.
                    doc_comment: get_body_docstring(&node, source)
                        .or_else(|| get_preceding_comment(&extent, source)),
                    return_type: None,
                    param_types: None,
                    is_test: node_is_test,
                });
                extract_children(
                    node,
                    source,
                    language,
                    config,
                    Some(&name),
                    results,
                    depth,
                    node_is_test,
                );
                return;
            }
        }

        // Dart mixins: `mixin M {}` parses as `(mixin_declaration (mixin)
        // (identifier) (class_body))` — the name is a POSITIONAL `identifier`
        // child, not a `name:` field, so the class arm above (which reads the
        // `name` field) misses it. Without a node for `M`, the `with M`
        // inherits edge (emitted by relations/inherits.rs) drops at Phase-2
        // same-language resolution because its target has no node to bind to.
        // (`mixin class MC {}` is a `class_definition` with a `mixin` modifier
        // and a real `name:` field — already handled by the class arm.)
        "mixin_declaration" if config.name == "dart" => {
            let name_node = (0..node.named_child_count())
                .filter_map(|i| node.named_child(i))
                .find(|c| c.kind() == "identifier");
            if let Some(name) = name_node.map(|n| node_text(&n, source).to_string()) {
                results.push(make_simple_node(
                    "class",
                    name.clone(),
                    &node,
                    source,
                    node_is_test,
                ));
                extract_children(
                    node,
                    source,
                    language,
                    config,
                    Some(&name),
                    results,
                    depth,
                    node_is_test,
                );
                return;
            }
        }

        // Methods: TS/JS (method_definition), Go/Java (method_declaration), Ruby (method, singleton_method)
        "method_definition" | "method_declaration" => {
            // Go declares methods at file scope with the owner in a RECEIVER
            // rather than by nesting them in a type body, so `parent_class` is
            // always None here and every method was stored with
            // `qualified_name == name`. Two types with a `Start` method were
            // then two indistinguishable `Start` nodes (audit 2026-08-16 P1-4).
            let go_owner = go_receiver_type(&node, source);
            let owner = go_owner.as_deref().or(parent_class);
            if let Some(mut parsed) = extract_function_node(&node, source, "method", owner) {
                parsed.is_test = node_is_test;
                results.push(parsed);
            }
        }
        "method" | "singleton_method" if config.name == "ruby" => {
            if let Some(mut parsed) = extract_function_node(&node, source, "method", parent_class) {
                parsed.is_test = node_is_test;
                results.push(parsed);
            }
        }

        // C#: top-level local functions (C# 9+ top-level statements) and local
        // functions nested in a method are `local_function_statement`, distinct
        // from `method_declaration` (handled above). Without extraction the
        // function has no symbol node, so a top-level `void Greet(){}` invoked via
        // the v49 `<module>` call edge dangled unresolved and the function was
        // invisible to callgraph/impact/dead-code. Extract as a function-kind node
        // (method-kind when nested in a class body, matching the method_declaration
        // arm's parent_class convention). name/params come from the
        // `name`/`parameters` fields, which extract_signature_info already reads.
        "local_function_statement" if config.name == "csharp" => {
            let nt = if parent_class.is_some() {
                "method"
            } else {
                "function"
            };
            if let Some(mut parsed) = extract_function_node(&node, source, nt, parent_class) {
                parsed.is_test = node_is_test;
                results.push(parsed);
            }
        }

        // Dart: method_signature wraps function_signature/constructor_signature/getter_signature
        // Extract the function name from the inner signature node
        "method_signature" if config.method_signature_kind.is_some() => {
            if let Some(mut parsed) = extract_dart_method_signature(&node, source, parent_class) {
                parsed.is_test = node_is_test;
                results.push(parsed);
            }
        }

        // Dart: top-level declarations can contain function_signature (abstract methods, fields)
        "declaration" if config.name == "dart" => {
            if let Some(mut parsed) = extract_dart_declaration(&node, source, parent_class) {
                parsed.is_test = node_is_test;
                results.push(parsed);
            }
        }

        // Dart top-level function: `int helper(int x) { ... }` parses as a bare
        // `function_signature` + `function_body` sibling directly under `program`
        // — NOT wrapped in a `declaration` (handled above) and distinct from a
        // class method (`method_signature`, handled above). Without this arm,
        // top-level Dart functions were never extracted as symbols, leaving
        // callgraph / impact / dead-code blind to them. Guard against the nested
        // forms so a method's inner function_signature isn't double-extracted.
        "function_signature"
            if config.name == "dart"
                && !matches!(
                    node.parent().map(|p| p.kind()),
                    Some("method_signature") | Some("declaration")
                ) =>
        {
            if let Some(mut parsed) = extract_dart_top_level_function(&node, source, parent_class) {
                parsed.is_test = node_is_test;
                results.push(parsed);
            }
        }

        // Ruby modules — mapped to "interface" type
        "module" if config.name == "ruby" => {
            if let Some(name) = get_child_by_field(&node, "name", source) {
                results.push(make_simple_node(
                    "interface",
                    name.clone(),
                    &node,
                    source,
                    node_is_test,
                ));
                extract_children(
                    node,
                    source,
                    language,
                    config,
                    Some(&name),
                    results,
                    depth,
                    node_is_test,
                );
                return;
            }
        }

        // Swift protocol → interface
        "protocol_declaration" => {
            if let Some(name) = get_child_by_field(&node, "name", source) {
                results.push(make_simple_node(
                    "interface",
                    name.clone(),
                    &node,
                    source,
                    node_is_test,
                ));
                extract_children(
                    node,
                    source,
                    language,
                    config,
                    Some(&name),
                    results,
                    depth,
                    node_is_test,
                );
                return;
            }
        }

        // Swift protocol function declarations (method signatures without body)
        "protocol_function_declaration" => {
            if let Some(mut parsed) = extract_function_node(&node, source, "method", parent_class) {
                parsed.is_test = node_is_test;
                results.push(parsed);
            }
        }

        // Interfaces (TS/Java/PHP)
        "interface_declaration" => {
            if let Some(name) = get_child_by_field(&node, "name", source) {
                results.push(make_simple_node(
                    "interface",
                    name.clone(),
                    &node,
                    source,
                    node_is_test,
                ));
                extract_children(
                    node,
                    source,
                    language,
                    config,
                    Some(&name),
                    results,
                    depth,
                    node_is_test,
                );
                return;
            }
        }

        // PHP traits — mapped to "interface" type
        "trait_declaration" => {
            if let Some(name) = get_child_by_field(&node, "name", source) {
                results.push(make_simple_node(
                    "interface",
                    name.clone(),
                    &node,
                    source,
                    node_is_test,
                ));
                extract_children(
                    node,
                    source,
                    language,
                    config,
                    Some(&name),
                    results,
                    depth,
                    node_is_test,
                );
                return;
            }
        }

        // TS type aliases: type Foo = ...
        "type_alias_declaration" => {
            if let Some(name) = get_child_by_field(&node, "name", source) {
                results.push(make_simple_node("type", name, &node, source, node_is_test));
            }
        }

        // Java/C# enums
        "enum_declaration" => {
            if let Some(name) = get_child_by_field(&node, "name", source) {
                results.push(make_simple_node("enum", name, &node, source, node_is_test));
            }
        }

        // C# struct
        "struct_declaration" => {
            if let Some(name) = get_child_by_field(&node, "name", source) {
                results.push(make_simple_node(
                    "struct",
                    name.clone(),
                    &node,
                    source,
                    node_is_test,
                ));
                extract_children(
                    node,
                    source,
                    language,
                    config,
                    Some(&name),
                    results,
                    depth,
                    node_is_test,
                );
                return;
            }
        }

        // Kotlin object declaration (singleton)
        "object_declaration" => {
            if let Some(name) = get_child_by_field(&node, "name", source) {
                results.push(make_simple_node(
                    "class",
                    name.clone(),
                    &node,
                    source,
                    node_is_test,
                ));
                extract_children(
                    node,
                    source,
                    language,
                    config,
                    Some(&name),
                    results,
                    depth,
                    node_is_test,
                );
                return;
            }
        }

        // C# constructor
        "constructor_declaration" => {
            if let Some(mut parsed) = extract_function_node(&node, source, "function", parent_class)
            {
                parsed.is_test = node_is_test;
                results.push(parsed);
            }
        }

        // C++ class/struct
        "class_specifier" | "struct_specifier" => {
            if let Some(name) = get_child_by_field(&node, "name", source) {
                let nt = if kind == "class_specifier" {
                    "class"
                } else {
                    "struct"
                };
                results.push(make_simple_node(
                    nt,
                    name.clone(),
                    &node,
                    source,
                    node_is_test,
                ));
                extract_children(
                    node,
                    source,
                    language,
                    config,
                    Some(&name),
                    results,
                    depth,
                    node_is_test,
                );
                return;
            }
        }

        // Go type declarations (struct, interface)
        "type_declaration" => {
            for i in 0..node.named_child_count() {
                if let Some(child) = node.named_child(i) {
                    if child.kind() == "type_spec" {
                        if let Some(name) = get_child_by_field(&child, "name", source) {
                            let node_type = if child.named_child_count() > 1 {
                                match child.named_child(1).map(|c| c.kind()) {
                                    Some("struct_type") => "struct",
                                    Some("interface_type") => "interface",
                                    _ => "type",
                                }
                            } else {
                                "type"
                            };
                            results.push(ParsedNode {
                                node_type: node_type.into(),
                                name: name.clone(),
                                qualified_name: Some(name),
                                start_line: child.start_position().row as u32 + 1,
                                end_line: child.end_position().row as u32 + 1,
                                code_content: truncate_code_content(node_text(&child, source))
                                    .into_owned(),
                                signature: None,
                                doc_comment: get_preceding_comment(&child, source),
                                return_type: None,
                                param_types: None,
                                is_test: node_is_test,
                            });
                        }
                    }
                }
            }
        }

        // Rust-specific
        "struct_item" => {
            if let Some(name) = get_child_by_field(&node, "name", source) {
                results.push(make_simple_node(
                    "struct",
                    name,
                    &node,
                    source,
                    node_is_test,
                ));
            }
        }
        "enum_item" => {
            if let Some(name) = get_child_by_field(&node, "name", source) {
                results.push(make_simple_node("enum", name, &node, source, node_is_test));
            }
        }
        "impl_item" => {
            if let Some(type_node) = node.child_by_field_name("type") {
                let impl_name_full = node_text(&type_node, source);
                // Strip path prefix so `impl crate::db_a::Db` is captured as
                // "Db" (matching what callers use as `Self`/`self` payload).
                // Mirrors the strip in relations/mod.rs walk_for_relations
                // for impl_item — keeps qualified_name consistent across the
                // two parser walks (treesitter.rs builds nodes; relations/mod.rs
                // builds edges).
                let impl_name = impl_name_full.rsplit("::").next().unwrap_or(impl_name_full);
                // Strip generic parameters so `impl<T> Foo<T>` produces method
                // qualified_names like "Foo.method" not "Foo<T>.method". The
                // self_filter_candidates resolver and impl_method metadata
                // both encode the bare type name (see relations/rust.rs);
                // keeping the impl name bare avoids a LIKE mismatch that would
                // drop every method-level implements edge.
                let impl_name = impl_name.split('<').next().unwrap_or(impl_name).trim();
                extract_children(
                    node,
                    source,
                    language,
                    config,
                    Some(impl_name),
                    results,
                    depth,
                    node_is_test,
                );
                return;
            }
        }
        "trait_item" => {
            if let Some(name) = get_child_by_field(&node, "name", source) {
                results.push(make_simple_node(
                    "interface",
                    name.clone(),
                    &node,
                    source,
                    node_is_test,
                ));
                extract_children(
                    node,
                    source,
                    language,
                    config,
                    Some(&name),
                    results,
                    depth,
                    node_is_test,
                );
                return;
            }
        }
        "const_item" | "static_item" => {
            if let Some(name) = get_child_by_field(&node, "name", source) {
                let type_annotation = node
                    .child_by_field_name("type")
                    .map(|t| node_text(&t, source).to_string());
                let mut pn = make_simple_node("constant", name, &node, source, node_is_test);
                pn.return_type = type_annotation;
                results.push(pn);
            }
        }

        // Markdown ATX heading: `# Title`, `## Subtitle`. Produces h1..h6 nodes so
        // headings are searchable via FTS and browsable via module_overview.
        "atx_heading" if config.name == "markdown" => {
            let mut level = 1usize;
            let mut text: Option<String> = None;
            for i in 0..node.named_child_count() {
                if let Some(child) = node.named_child(i) {
                    match child.kind() {
                        "atx_h1_marker" => level = 1,
                        "atx_h2_marker" => level = 2,
                        "atx_h3_marker" => level = 3,
                        "atx_h4_marker" => level = 4,
                        "atx_h5_marker" => level = 5,
                        "atx_h6_marker" => level = 6,
                        "inline" => text = Some(node_text(&child, source).trim().to_string()),
                        _ => {}
                    }
                }
            }
            if let Some(title) = text.filter(|s| !s.is_empty()) {
                results.push(ParsedNode {
                    node_type: format!("h{}", level),
                    name: title.clone(),
                    qualified_name: Some(title),
                    start_line: node.start_position().row as u32 + 1,
                    end_line: node.end_position().row as u32 + 1,
                    code_content: truncate_code_content(node_text(&node, source)).into_owned(),
                    signature: None,
                    doc_comment: None,
                    return_type: None,
                    param_types: None,
                    is_test: false,
                });
            }
        }

        // Markdown setext heading: `Title\n=====` or `Subtitle\n-----` — paragraph
        // + underline. The paragraph child's inline text is the heading name.
        "setext_heading" if config.name == "markdown" => {
            let mut level = 1usize;
            let mut text: Option<String> = None;
            for i in 0..node.named_child_count() {
                if let Some(child) = node.named_child(i) {
                    match child.kind() {
                        "setext_h1_underline" => level = 1,
                        "setext_h2_underline" => level = 2,
                        "paragraph" => {
                            text = Some(node_text(&child, source).trim().to_string());
                        }
                        _ => {}
                    }
                }
            }
            if let Some(title) = text.filter(|s| !s.is_empty()) {
                results.push(ParsedNode {
                    node_type: format!("h{}", level),
                    name: title.clone(),
                    qualified_name: Some(title),
                    start_line: node.start_position().row as u32 + 1,
                    end_line: node.end_position().row as u32 + 1,
                    code_content: truncate_code_content(node_text(&node, source)).into_owned(),
                    signature: None,
                    doc_comment: None,
                    return_type: None,
                    param_types: None,
                    is_test: false,
                });
            }
        }

        // Inline HTTP route handlers (Express/Fastify/Koa:
        // `app.get('/x', (req, res) => { ... })`) are anonymous arrows /
        // function expressions with no `name` field, so they matched no node arm
        // and their calls collapsed onto the file <module>. Materialize them as
        // function nodes named "METHOD path" so trace/impact/overview resolve
        // per-route; relations::walk_for_relations scopes the handler's calls to
        // the same synthetic name. (INDEX_VERSION bumped in domain.rs.)
        "arrow_function" | "function_expression" => {
            if let Some(name) = super::route_handler_name(&node, source) {
                results.push(make_simple_node(
                    "function",
                    name,
                    &node,
                    source,
                    node_is_test,
                ));
            }
            // fall through to extract_children below so nested fns still extract
        }
        _ => {}
    }

    // Recurse into children
    extract_children(
        node,
        source,
        language,
        config,
        parent_class,
        results,
        depth,
        node_is_test,
    );
}

#[allow(clippy::too_many_arguments)]
fn extract_children(
    node: tree_sitter::Node,
    source: &str,
    language: &str,
    config: &LanguageConfig,
    parent_class: Option<&str>,
    results: &mut Vec<ParsedNode>,
    depth: usize,
    in_test_context: bool,
) {
    for i in 0..node.named_child_count() {
        if let Some(child) = node.named_child(i) {
            extract_nodes(
                child,
                source,
                language,
                config,
                parent_class,
                results,
                depth + 1,
                in_test_context,
            );
        }
    }
}

fn truncate_code_content(content: &str) -> Cow<'_, str> {
    let max = max_code_content_len();
    // Fast path unchanged for the common case: short content with no NUL bytes is
    // borrowed as-is.
    if content.len() <= max && !content.contains('\0') {
        return Cow::Borrowed(content);
    }
    // Owned path. Strip NUL bytes first: the FTS5 tokenizer treats stored TEXT as a
    // C-string and stops at the first NUL, so a source file with an embedded NUL
    // (mis-detected binary / generated blob) would leave everything after it
    // unsearchable. Replace with a space (same byte length) so the tail stays
    // indexed. (L10)
    let mut s = if content.contains('\0') {
        content.replace('\0', " ")
    } else {
        content.to_string()
    };
    if s.len() > max {
        let mut end = max;
        while end > 0 && !s.is_char_boundary(end) {
            end -= 1;
        }
        s.truncate(end);
        s.push_str("...");
    }
    Cow::Owned(s)
}

/// Strip NUL bytes (→ space) from an optional signature-derived field
/// (`return_type` / `param_types` / `signature`). SQLite stores TEXT as a
/// C-string, so a stored NUL silently truncates a `LIKE` / substring match at the
/// NUL — and the same fields flow into the context_string used for those probes.
/// Same NUL→space convention as truncate_code_content / get_preceding_comment
/// (byte-length preserving, all other bytes unchanged). Fast path: no allocation
/// when the value is absent or NUL-free (the overwhelming common case).
fn strip_nul_field(s: Option<String>) -> Option<String> {
    s.map(|v| {
        if v.contains('\0') {
            v.replace('\0', " ")
        } else {
            v
        }
    })
}

fn make_simple_node(
    node_type: &str,
    name: String,
    node: &tree_sitter::Node,
    source: &str,
    is_test: bool,
) -> ParsedNode {
    ParsedNode {
        node_type: node_type.into(),
        name: name.clone(),
        qualified_name: Some(name),
        start_line: node.start_position().row as u32 + 1,
        end_line: node.end_position().row as u32 + 1,
        code_content: truncate_code_content(node_text(node, source)).into_owned(),
        signature: None,
        doc_comment: get_doc_comment(node, source),
        return_type: None,
        param_types: None,
        is_test,
    }
}

/// Python wraps a decorated `def`/`class` in a `decorated_definition` node whose
/// extent spans the decorator stack; the inner `function_definition` /
/// `class_definition` starts at `def`/`class`. Binding a symbol to the inner node
/// drops every decorator from its source and extent — losing the semantic payload
/// of e.g. `@field_validator("lat", mode="before")`, the pydantic contract
/// (issue #31). When a definition is the child of a `decorated_definition`, return
/// that wrapper so `start_line` and `code_content` include the decorators; the
/// decorator text also lets `find_dead_code` recognize framework-registered
/// (edgeless) entry points. No-op for every other language: `decorated_definition`
/// is a Python-only node kind, so a non-Python `def`/`class` returns itself.
fn python_decorated_extent<'a>(node: &tree_sitter::Node<'a>) -> tree_sitter::Node<'a> {
    match node.parent() {
        Some(parent) if parent.kind() == "decorated_definition" => parent,
        _ => *node,
    }
}

/// The owner type of a Go method, from its receiver: `func (s *Server) Start()`
/// → `Server`. `None` for any node without a `receiver` field, which is every
/// non-Go `method_declaration` (Java nests its methods in a class body instead)
/// and every Go `function_declaration`.
///
/// The base name is taken from the receiver's TEXT rather than by walking into
/// `pointer_type` / `generic_type`: a Go receiver base type is always a single
/// local identifier, so stripping a leading `*` and any `[…]` type arguments is
/// exact, and it does not break when a grammar bump renames those inner kinds.
/// The type arguments MUST come off — `Server[T].Generic` would never match a
/// lookup for `Server`.
fn go_receiver_type(node: &tree_sitter::Node, source: &str) -> Option<String> {
    let receiver = node.child_by_field_name("receiver")?;
    let decl = receiver.named_child(0)?;
    let ty = decl.child_by_field_name("type")?;
    let base = node_text(&ty, source)
        .trim()
        .trim_start_matches('*')
        .split('[')
        .next()
        .unwrap_or("")
        .trim()
        .to_string();
    if base.is_empty() {
        None
    } else {
        Some(base)
    }
}

fn extract_function_node(
    node: &tree_sitter::Node,
    source: &str,
    node_type: &str,
    parent_class: Option<&str>,
) -> Option<ParsedNode> {
    let name = get_child_by_field(node, "name", source)?;
    let qualified_name = match parent_class {
        Some(cls) => Some(format!("{}.{}", cls, name)),
        None => Some(name.clone()),
    };
    // Name/signature/params come from the inner definition; the extent (line span
    // + stored source) widens to the enclosing `decorated_definition` so Python
    // decorators are retained (issue #31). extent == node for undecorated defs and
    // every non-Python language.
    let sig_info = extract_signature_info(node, source);
    let extent = python_decorated_extent(node);

    Some(ParsedNode {
        node_type: node_type.into(),
        name,
        qualified_name,
        start_line: extent.start_position().row as u32 + 1,
        end_line: extent.end_position().row as u32 + 1,
        code_content: truncate_code_content(node_text(&extent, source)).into_owned(),
        signature: sig_info.signature,
        // Docstring first (see get_doc_comment); it comes from the inner `node`
        // because a `decorated_definition` has no `body` field of its own. The
        // comment fallback reads the EXTENT so a `# comment` above a DECORATED
        // Python def is still found.
        doc_comment: get_body_docstring(node, source)
            .or_else(|| get_preceding_comment(&extent, source)),
        return_type: sig_info.return_type,
        param_types: sig_info.param_types,
        is_test: false,
    })
}

/// Collect the local binding identifiers introduced by a destructuring pattern
/// (`object_pattern` / `array_pattern`) on the left of a `const`/`let` declaration.
/// Each bound name is an independently-importable symbol, so
/// `export const { host, port } = getConfig()` yields `host` and `port` rather than
/// the literal pattern text `{ host, port }`. Renamed `{ key: local }` binds `local`
/// (the exported name); defaults (`{ x = 1 }` / `[x = 1]`), rest (`{ ...r }` /
/// `[...r]`), and nested patterns recurse to their leaf identifiers. Mirrors the
/// require-destructuring walk in relations/mod.rs (which keys on the export/key side
/// for CJS import edges; extraction wants the local binding, i.e. the value side).
fn collect_binding_names(pattern: &tree_sitter::Node, source: &str, out: &mut Vec<String>) {
    match pattern.kind() {
        "identifier" | "shorthand_property_identifier_pattern" => {
            let t = node_text(pattern, source);
            if !t.is_empty() {
                out.push(t.to_string());
            }
        }
        // `{ key: local }` — the VALUE side is the local binding that gets exported.
        "pair_pattern" => {
            if let Some(v) = pattern.child_by_field_name("value") {
                collect_binding_names(&v, source, out);
            }
        }
        // `{ x = default }` / `[x = default]` — the LEFT side is the binding.
        "object_assignment_pattern" | "assignment_pattern" => {
            if let Some(l) = pattern
                .child_by_field_name("left")
                .or_else(|| pattern.named_child(0))
            {
                collect_binding_names(&l, source, out);
            }
        }
        // Containers + rest: recurse into every named child.
        "object_pattern" | "array_pattern" | "rest_pattern" => {
            for i in 0..pattern.named_child_count() {
                if let Some(c) = pattern.named_child(i) {
                    collect_binding_names(&c, source, out);
                }
            }
        }
        _ => {}
    }
}

fn extract_named_arrows(node: &tree_sitter::Node, source: &str) -> Vec<ParsedNode> {
    // lexical_declaration -> variable_declarator -> arrow_function
    // A single declaration may contain multiple arrow functions: const a = () => {}, b = () => {};
    let mut out = Vec::new();
    // The doc comment sits above the STATEMENT, which owns every declarator, so
    // reading it per-declarator gave `/** DOC */ export const a = 1, b = 2;` the
    // same DOC for both names (INDEX_VERSION 65). Only the first declarator
    // keeps it — the rule this file already applies to Go's `// GROUP_DOC\ntype
    // ( Alpha …; Beta … )`, where the group's doc reaches `Alpha` and stops.
    // Read once here rather than inside the loop: it is the statement's comment,
    // not each declarator's, and computing it per-declarator is what made the
    // shared attribution look intentional.
    let statement_doc = get_preceding_comment(node, source);
    let mut first_declarator_seen = false;
    for i in 0..node.named_child_count() {
        let child = match node.named_child(i) {
            Some(c) => c,
            None => continue,
        };
        if child.kind() == "variable_declarator" {
            // Claimed by position in the statement, NOT by whether the emit
            // below succeeds: a leading declarator that produces no node (a
            // local non-arrow const, say) must still consume the doc, or the
            // comment would slide onto whichever later name happens to be
            // extracted first and document the wrong symbol.
            let doc_comment = if first_declarator_seen {
                None
            } else {
                first_declarator_seen = true;
                statement_doc.clone()
            };
            let name_node = match child.child_by_field_name("name") {
                Some(n) => n,
                None => continue,
            };
            let name = node_text(&name_node, source).to_string();
            let value = match child.child_by_field_name("value") {
                Some(v) => v,
                None => continue,
            };
            if value.kind() == "arrow_function" {
                let sig_info = extract_signature_info(&value, source);
                out.push(ParsedNode {
                    node_type: "function".into(),
                    name: name.clone(),
                    qualified_name: Some(name),
                    start_line: child.start_position().row as u32 + 1,
                    end_line: child.end_position().row as u32 + 1,
                    code_content: truncate_code_content(node_text(&child, source)).into_owned(),
                    signature: sig_info.signature,
                    doc_comment,
                    return_type: sig_info.return_type,
                    param_types: sig_info.param_types,
                    is_test: false,
                });
            } else if !matches!(value.kind(), "function_expression" | "function")
                && node.parent().map(|p| p.kind()) == Some("export_statement")
            {
                // Top-level `export const/let X = <value>` — a config constant, a
                // route/config table, or a widely-imported singleton such as
                // `const store = defineStore(...)`, `const logger = createLogger(...)`,
                // or `const svc = new Service()`. Only EXPORTED top-level declarations
                // are extracted: a local `const x = 5` inside a function body cannot be
                // imported cross-file, so extracting it would be pure noise. Emitting
                // the symbol lets `import { X } from './mod'` resolve to a real node and
                // form a REL_IMPORTS edge — previously the import bound to the
                // `<external>` sentinel and the dependency was invisible to
                // tour/affected/impact/project_map (feedback_const_export_no_import_edge).
                // Type "constant" mirrors the Rust `const_item`/`static_item` extraction
                // (this is TS/JS reaching parity); function-valued consts are handled by
                // the arrow branch above, never here.
                //
                // A destructuring export binds MULTIPLE names: `export const { host,
                // port } = getConfig()` / `export const [a, b] = getPair()`. The
                // declarator's `name` field is then an object/array pattern whose text
                // is the literal `{ host, port }` — not a valid identifier, and no
                // consumer can `import { host }` against it (the import dangles to the
                // `<external>` sentinel). Emit one constant per bound name so each is
                // an importable symbol. Common in the wild: Redux `export const {
                // actions, reducer } = slice`, React `export const { Provider } =
                // createContext()`. A plain identifier yields the single name unchanged.
                let names: Vec<String> =
                    if matches!(name_node.kind(), "object_pattern" | "array_pattern") {
                        let mut v = Vec::new();
                        collect_binding_names(&name_node, source, &mut v);
                        v
                    } else {
                        vec![name.clone()]
                    };
                for nm in names {
                    out.push(ParsedNode {
                        node_type: "constant".into(),
                        name: nm.clone(),
                        qualified_name: Some(nm),
                        start_line: child.start_position().row as u32 + 1,
                        end_line: child.end_position().row as u32 + 1,
                        code_content: truncate_code_content(node_text(&child, source)).into_owned(),
                        signature: None,
                        // Every name bound by ONE destructuring declarator shares
                        // its doc — the split is between declarators, not between
                        // the names inside one.
                        doc_comment: doc_comment.clone(),
                        return_type: None,
                        param_types: None,
                        is_test: false,
                    });
                }
            }
        }
    }
    out
}

struct SignatureInfo {
    signature: Option<String>,
    return_type: Option<String>,
    param_types: Option<String>,
}

fn extract_signature_info(node: &tree_sitter::Node, source: &str) -> SignatureInfo {
    let params = node
        .child_by_field_name("parameters")
        .map(|p| node_text(&p, source).to_string());
    // For TS/JS the return_type field maps to a `type_annotation` node whose
    // text starts with the literal `:` (e.g. `: string`). For Python/Rust/Go
    // it's the bare type (e.g. `str`, `Result<()>`). Strip a single leading
    // colon + whitespace so all languages produce shape-consistent values
    // (no-op when the leading char isn't `:`).
    let ret = node
        .child_by_field_name("return_type")
        .map(|r| node_text(&r, source).to_string())
        .map(|s| s.trim_start_matches(':').trim_start().to_string())
        .filter(|s| !s.is_empty());

    let signature = match (&params, &ret) {
        (Some(p), Some(r)) => Some(format!("{} -> {}", p, r)),
        (Some(p), None) => Some(p.clone()),
        _ => None,
    };

    SignatureInfo {
        signature: strip_nul_field(signature),
        return_type: strip_nul_field(ret),
        param_types: strip_nul_field(params),
    }
}

fn extract_c_signature_info(node: &tree_sitter::Node, source: &str) -> SignatureInfo {
    let declarator = match node.child_by_field_name("declarator") {
        Some(d) => d,
        None => {
            return SignatureInfo {
                signature: None,
                return_type: None,
                param_types: None,
            }
        }
    };
    let params = declarator
        .child_by_field_name("parameters")
        .map(|p| node_text(&p, source).to_string());
    let ret_type = node
        .child_by_field_name("type")
        .map(|t| node_text(&t, source).to_string());

    let signature = match (&ret_type, &params) {
        (Some(t), Some(p)) => Some(format!("{} {}", t, p)),
        (Some(t), None) => Some(t.clone()),
        (None, Some(p)) => Some(p.clone()),
        _ => None,
    };

    SignatureInfo {
        signature: strip_nul_field(signature),
        return_type: strip_nul_field(ret_type),
        param_types: strip_nul_field(params),
    }
}

fn extract_declarator_name(node: &tree_sitter::Node, source: &str) -> Option<String> {
    extract_declarator_name_inner(node, source, 0)
}

/// Detect gtest macro invocations parsed as function_definition.
/// `TEST(Suite, Name) { ... }` has a function_declarator whose inner
/// declarator is `TEST` and parameters are two type_identifiers.
/// Returns `Some("Suite.Name")` when the macro matches; None otherwise.
fn extract_gtest_test_name(declarator: &tree_sitter::Node, source: &str) -> Option<String> {
    if declarator.kind() != "function_declarator" {
        return None;
    }
    let inner = declarator.child_by_field_name("declarator")?;
    if !matches!(inner.kind(), "identifier" | "field_identifier") {
        return None;
    }
    let macro_name = node_text(&inner, source);
    const GTEST_MACROS: &[&str] = &[
        "TEST",
        "TEST_F",
        "TEST_P",
        "TEST_CASE",
        "TYPED_TEST",
        "TYPED_TEST_P",
    ];
    if !GTEST_MACROS.contains(&macro_name) {
        return None;
    }

    let params = declarator.child_by_field_name("parameters")?;
    let mut suite: Option<String> = None;
    let mut test: Option<String> = None;
    let mut idx = 0;
    for i in 0..params.named_child_count() {
        let Some(param) = params.named_child(i) else {
            continue;
        };
        // parameter_declaration > type_identifier (gtest args parsed as types)
        let id_text = (0..param.named_child_count())
            .filter_map(|j| param.named_child(j))
            .find(|c| matches!(c.kind(), "type_identifier" | "identifier"))
            .map(|c| node_text(&c, source).to_string());
        if let Some(t) = id_text {
            match idx {
                0 => suite = Some(t),
                1 => test = Some(t),
                _ => {}
            }
            idx += 1;
        }
    }
    match (suite, test) {
        (Some(s), Some(t)) => Some(format!("{}.{}", s, t)),
        _ => None,
    }
}

fn extract_declarator_name_inner(
    node: &tree_sitter::Node,
    source: &str,
    depth: usize,
) -> Option<String> {
    if depth > MAX_AST_DEPTH {
        return None;
    }
    // C/C++ function_declarator -> identifier
    if node.kind() == "function_declarator" {
        return get_child_by_field(node, "declarator", source).or_else(|| {
            node.named_child(0)
                .map(|c| node_text(&c, source).to_string())
        });
    }
    // Might be a pointer_declarator wrapping a function_declarator
    for i in 0..node.named_child_count() {
        if let Some(child) = node.named_child(i) {
            if let Some(name) = extract_declarator_name_inner(&child, source, depth + 1) {
                return Some(name);
            }
        }
    }
    None
}

fn get_child_by_field(node: &tree_sitter::Node, field: &str, source: &str) -> Option<String> {
    node.child_by_field_name(field)
        .map(|n| node_text(&n, source).to_string())
}

/// Wrapper nodes that sit BETWEEN a declaration and its doc comment.
///
/// A JSDoc block precedes the whole `export function f() {}` statement, so it is
/// a sibling of the `export_statement` — not of the `function_declaration` the
/// extractor hands us. The sibling walk therefore found nothing and every
/// EXPORTED TS/JS symbol lost its documentation, while a plain `function f(){}`
/// and a class method (neither wrapped) kept theirs. In real TS the exported
/// symbols are the documented ones, so the column was empty exactly where it
/// mattered, and `search "issuer allowlist"` — a phrase living only in a JSDoc —
/// returned nothing at all (the block is outside the node, so `code_content`
/// does not carry it either, unlike a Python docstring).
/// Each entry was measured against a real parse tree, not guessed — the sweep
/// that produced this list found Dart, Go and Ruby losing docs for three
/// different wrapper shapes.
const DOC_COMMENT_WRAPPERS: &[&str] = &[
    // TS/JS: the JSDoc precedes `export function f(){}` as a whole.
    "export_statement",
    "lexical_declaration",
    "variable_declaration",
    "variable_declarator",
    // Python: `@decorator` above a def/class.
    "decorated_definition",
    // Go: the comment is a sibling of `type_declaration`, while the extractor
    // sees the inner `type_spec` (same for const/var groups).
    "type_declaration",
    "const_declaration",
    "var_declaration",
    // Ruby: a class body is a `body_statement`, so a method's comment is a
    // sibling of that wrapper rather than of the `method` node.
    "body_statement",
    // Dart: `method_signature` wraps the `function_signature` the extractor reads.
    "method_signature",
];

/// A Python docstring: the first statement of a `def`/`class` body, when it is a
/// bare string literal.
///
/// Python does not put its documentation in a preceding comment, so
/// `get_preceding_comment` found nothing and `doc_comment` was empty for every
/// Python symbol. Less severe than the TS case — the docstring is INSIDE the
/// node, so `code_content` carries it and FTS can still reach it — but the
/// embedding context builder ranks `doc:` above `code:` precisely because code
/// is what gets truncated at 512 tokens, so a long function's docstring was the
/// first thing dropped from its vector.
///
/// Gated on the two Python declaration kinds rather than on "has a `block`
/// child": Rust functions also have `block` bodies, and `fn f() { "x"; }` would
/// otherwise be read as documented.
fn get_body_docstring(node: &tree_sitter::Node, source: &str) -> Option<String> {
    if !matches!(node.kind(), "function_definition" | "class_definition") {
        return None;
    }
    let body = node.child_by_field_name("body")?;
    if body.kind() != "block" {
        return None;
    }
    let first = body.named_child(0)?;
    if first.kind() != "expression_statement" {
        return None;
    }
    let literal = first.named_child(0)?;
    if literal.kind() != "string" {
        return None;
    }
    let text = node_text(&literal, source);
    if text.is_empty() {
        return None;
    }
    Some(truncate_code_content(text).into_owned())
}

/// The documentation attached to a declaration, by whichever convention the
/// language uses: a Python docstring, or a preceding comment block.
///
/// **Docstring first.** The first ordering tried the comment first, which meant a
/// file-level `# Copyright …` / `# pylint: disable=…` header — adjacent to the
/// first `def` in most real Python files — won over that function's actual
/// docstring, so the new channel never fired exactly where it was needed
/// (pre-tag review finding #1, reproduced). A docstring is authored
/// documentation for THIS symbol; a preceding comment may be anything that
/// happens to sit above it. Only the Python kinds have a docstring at all, so
/// every other language keeps comment-only behavior unchanged.
fn get_doc_comment(node: &tree_sitter::Node, source: &str) -> Option<String> {
    get_body_docstring(node, source).or_else(|| get_preceding_comment(node, source))
}

/// Whether a comment starts its own line — nothing but whitespace to its left.
///
/// A comment TRAILING code (`func F() {} // note`, `class R # note`) documents
/// the line it sits on, not whatever declaration follows it. The sibling walk
/// cannot tell the two apart by kind, and widening the walk to wrappers made
/// that visible: a Go trailing comment became the doc of the NEXT `type`, and a
/// Ruby `class Inline # note` became the doc of the class's first method (both
/// reported by the pre-tag review, both reproduced). `tree_sitter::Point::column`
/// is a byte offset within the line, so the line prefix is exactly the bytes
/// between `start_byte - column` and `start_byte`.
fn comment_starts_its_own_line(node: &tree_sitter::Node, source: &str) -> bool {
    let start = node.start_byte();
    let col = node.start_position().column;
    let Some(line_start) = start.checked_sub(col) else {
        return true;
    };
    match source.get(line_start..start) {
        Some(prefix) => prefix.chars().all(char::is_whitespace),
        // Non-UTF8 boundary (should not happen: tree-sitter columns are byte
        // offsets into the same &str). Keep the pre-existing accept.
        None => true,
    }
}

/// A decorator / attribute / annotation that sits BETWEEN a declaration and its
/// documentation comment, as its own sibling node.
///
/// Whether a language lands here is a property of its grammar, not of the
/// decoration syntax: Java, Kotlin and Swift park annotations inside the
/// declaration's `modifiers`, and C#/PHP inside an `attribute_list` field, so
/// the comment stays the declaration's immediate previous sibling and those
/// languages never needed anything. The three spellings below are the ones that
/// do NOT — each measured against a real parse tree, the same way
/// `DOC_COMMENT_WRAPPERS` was:
///
/// - `decorator` — TS/JS. `@Component({}) export class C {}` puts the decorator
///   inside `export_statement` ahead of the declaration, and a member decorator
///   (`@Get() findAll() {}`) sits directly between the comment and the method in
///   `class_body`. This is the Angular/NestJS shape, i.e. most documented
///   declarations in those codebases.
/// - `attribute_item` — Rust. `#[derive(Debug)]` is a sibling, so a documented
///   derived struct or an `#[inline]`/`#[test]` fn lost its `///` block.
/// - `annotation` — Dart. `@override` is a top-level sibling of the signature.
///
/// Skipping is bounded to these kinds rather than "anything that is not a
/// comment": stepping over an arbitrary node would let a comment reach across
/// the declaration it actually documents
/// (`test_decoration_skip_does_not_cross_a_declaration`).
fn is_decoration(node: &tree_sitter::Node) -> bool {
    matches!(node.kind(), "decorator" | "attribute_item" | "annotation")
}

/// An INNER doc comment — Rust's `//!` and `/*!`, which document the module or
/// crate that ENCLOSES them, never the item that follows.
///
/// The sibling walk cannot tell the two apart by node kind (both are
/// `line_comment`), so a file opening with `//! What this module does` handed
/// that text to its first declaration as if it were the declaration's own doc.
/// Pre-existing — an undecorated `//! doc` + `pub struct S;` mis-attributed the
/// same way before decorations were skipped — but stepping over `attribute_item`
/// widened its reach to the `#[derive(…)]`-first files that are the common Rust
/// layout, so it is fixed here rather than left to grow.
fn is_inner_doc_comment(text: &str) -> bool {
    let t = text.trim_start();
    t.starts_with("//!") || t.starts_with("/*!")
}

fn get_preceding_comment(node: &tree_sitter::Node, source: &str) -> Option<String> {
    fn scan_siblings(node: &tree_sitter::Node, source: &str) -> Vec<String> {
        let mut comments = Vec::new();
        let mut current = node.prev_sibling();
        while let Some(prev) = current {
            // Suffix match, not a three-name allowlist: Dart's grammar calls its
            // `///` block `documentation_comment`, which the allowlist did not
            // name, so EVERY Dart symbol carried an empty doc_comment. Any
            // grammar that spells the concept `*_comment` is now covered without
            // another silent per-language gap.
            if prev.kind().ends_with("comment") {
                if !comment_starts_its_own_line(&prev, source) {
                    // Trailing comment: it belongs to the code on its own line.
                    // Stop rather than skip — anything further back is separated
                    // from this declaration by that code.
                    break;
                }
                let text = node_text(&prev, source);
                if is_inner_doc_comment(text) {
                    // Documents the enclosing module, not this declaration. Stop
                    // rather than skip: anything above a module header belongs to
                    // the module too.
                    break;
                }
                comments.push(text.to_string());
                current = prev.prev_sibling();
            } else if is_decoration(&prev) {
                // Step over it: the comment documents the declaration the
                // decoration is attached to, not the decoration.
                current = prev.prev_sibling();
            } else {
                break;
            }
        }
        comments
    }

    let mut comments = scan_siblings(node, source);
    // Climb out of the wrappers and retry, only through a node that is its
    // parent's FIRST named child — in Go's `// GROUP_DOC\ntype ( Alpha …; Beta … )`
    // the spec `Beta` is not first, and without that check the group's doc
    // comment would document it too (`test_wrapper_climb_skips_later_group_members`).
    //
    // This check does NOT handle the multi-declarator case, and cannot: it sees
    // the `lexical_declaration`, which is its parent's first named child no
    // matter how many declarators hang off it, so `/** DOC */ export const a =
    // 1, b = 2;` used to give DOC to both names. That is fixed one level up in
    // `extract_named_arrows`, which now hands the statement's comment to the
    // first declarator only (INDEX_VERSION 65) — the same outcome this check
    // produces for Go's `type ( Alpha; Beta )`, reached by a different route
    // because the declarators are not separate nodes at this level.
    // (The v63 INDEX_VERSION note claimed this check already prevented it,
    // citing the UNEXPORTED `const a = 1, b = 2`; that was vacuous — the
    // unexported form produces no nodes at all.)
    //
    // The 3-level bound is defence-in-depth, NOT a measured chain: the pre-tag
    // review walked every call site and every supported grammar and found that a
    // successful climb always takes exactly ONE level, because the extractor
    // never starts deeper than one wrapper in (`extract_named_arrows` passes the
    // `lexical_declaration` itself, not the declarator). An earlier version of
    // this comment cited a declarator → lexical_declaration → export_statement
    // chain — the code never walks it. The bound exists so a future
    // deeper-starting call site cannot turn this into an unbounded walk; there
    // is deliberately no test for it, because any fixture would have to feed a
    // starting node the extractor does not produce.
    let mut cursor = *node;
    for _ in 0..3 {
        if !comments.is_empty() {
            break;
        }
        let Some(parent) = cursor.parent() else { break };
        if !DOC_COMMENT_WRAPPERS.contains(&parent.kind()) {
            break;
        }
        // First MEANINGFUL named child, not literally the first: a decorated
        // `export` (`@Component({}) export class C {}`) parses with the
        // decorator as `export_statement`'s first named child, so a literal
        // check refused the climb for exactly the Angular/NestJS shape. The
        // check's purpose — keep a group's doc off its later members, e.g. Go's
        // `type ( Alpha; Beta )` — is unaffected, since a `type_spec` is never a
        // decoration.
        let first_meaningful = (0..parent.named_child_count())
            .filter_map(|i| parent.named_child(i))
            .find(|c| !is_decoration(c));
        if first_meaningful.map(|c| c.id()) != Some(cursor.id()) {
            break;
        }
        comments = scan_siblings(&parent, source);
        cursor = parent;
    }
    if comments.is_empty() {
        None
    } else {
        comments.reverse();
        let joined = comments.join("\n");
        // Apply the code_content policy verbatim — NUL→space AND the byte cap.
        //
        // NUL: doc_comment is stored as SQLite TEXT and fed to the FTS5
        // tokenizer, which treats it as a C-string and stops at the first NUL,
        // leaving everything after unsearchable (L10).
        //
        // Cap: doc_comment had none while code_content has been capped since
        // v47, so a comment could outweigh the symbol it documents by orders of
        // magnitude — measured MAX in this repo 20,828 bytes, the INDEX_VERSION
        // changelog tail, which tree-sitter attaches to the 46-byte constant
        // that follows it. Two measured costs: FTS5 recall pollution (that
        // constant answering a query for a word that appears only in the
        // changelog prose) and a 512-token embedding window filled with comment
        // before any code reaches it. Unlike code_content, no SQL guard reads
        // doc_comment via instr/LIKE, so the cap introduces no new
        // truncation-fragile predicate — the `...` sentinel is for the reader.
        //
        // Fast path: `truncate_code_content` borrows when the comment is
        // NUL-free and under the cap (the overwhelming common case), so match on
        // the Cow to hand back the existing allocation instead of cloning it.
        match truncate_code_content(&joined) {
            Cow::Borrowed(_) => Some(joined),
            Cow::Owned(s) => Some(s),
        }
    }
}

/// Extract a Dart method from a `method_signature` node.
/// method_signature wraps function_signature, constructor_signature, getter_signature, etc.
fn extract_dart_method_signature(
    node: &tree_sitter::Node,
    source: &str,
    parent_class: Option<&str>,
) -> Option<ParsedNode> {
    // Find the inner function_signature, constructor_signature, getter_signature, or setter_signature
    for i in 0..node.named_child_count() {
        if let Some(child) = node.named_child(i) {
            match child.kind() {
                "function_signature" | "getter_signature" | "setter_signature" => {
                    let name = get_child_by_field(&child, "name", source)?;
                    let qualified_name = match parent_class {
                        Some(cls) => Some(format!("{}.{}", cls, name)),
                        None => Some(name.clone()),
                    };
                    let params = child
                        .child_by_field_name("parameters")
                        .or_else(|| {
                            // function_signature doesn't use field name for formal_parameter_list
                            (0..child.named_child_count())
                                .filter_map(|j| child.named_child(j))
                                .find(|c| c.kind() == "formal_parameter_list")
                        })
                        .map(|p| node_text(&p, source).to_string());
                    // Return type: first type_identifier, void_type, or function_type child
                    let ret = (0..child.named_child_count())
                        .filter_map(|j| child.named_child(j))
                        .find(|c| {
                            matches!(c.kind(), "type_identifier" | "void_type" | "function_type")
                        })
                        .map(|r| node_text(&r, source).to_string());
                    // Include type_arguments (e.g. <String>) with the return type
                    let ret_with_args = ret.map(|r| {
                        let type_args = (0..child.named_child_count())
                            .filter_map(|j| child.named_child(j))
                            .find(|c| c.kind() == "type_arguments")
                            .map(|a| node_text(&a, source).to_string());
                        match type_args {
                            Some(args) => format!("{}{}", r, args),
                            None => r,
                        }
                    });
                    // NUL→space so return_type/param_types/signature (and the
                    // context_string built from them) stay SQLite-LIKE-searchable
                    // — same convention as the extract_signature_info path.
                    let params = strip_nul_field(params);
                    let ret_with_args = strip_nul_field(ret_with_args);
                    let signature = match (&params, &ret_with_args) {
                        (Some(p), Some(r)) => Some(format!("{} -> {}", p, r)),
                        (Some(p), None) => Some(p.clone()),
                        _ => None,
                    };
                    return Some(ParsedNode {
                        node_type: "method".into(),
                        name,
                        qualified_name,
                        start_line: node.start_position().row as u32 + 1,
                        end_line: node.end_position().row as u32 + 1,
                        code_content: truncate_code_content(node_text(node, source)).into_owned(),
                        signature,
                        doc_comment: get_preceding_comment(node, source),
                        return_type: ret_with_args,
                        param_types: params,
                        is_test: false,
                    });
                }
                "constructor_signature" => {
                    let name = get_child_by_field(&child, "name", source)?;
                    let qualified_name = match parent_class {
                        Some(cls) => Some(format!("{}.{}", cls, name)),
                        None => Some(name.clone()),
                    };
                    let params = strip_nul_field(
                        child
                            .child_by_field_name("parameters")
                            .map(|p| node_text(&p, source).to_string()),
                    );
                    return Some(ParsedNode {
                        node_type: "function".into(),
                        name,
                        qualified_name,
                        start_line: node.start_position().row as u32 + 1,
                        end_line: node.end_position().row as u32 + 1,
                        code_content: truncate_code_content(node_text(node, source)).into_owned(),
                        signature: params.clone(),
                        doc_comment: get_preceding_comment(node, source),
                        return_type: None,
                        param_types: params,
                        is_test: false,
                    });
                }
                _ => {}
            }
        }
    }
    None
}

/// Extract a Dart function/constructor from a `declaration` node (class-body or top-level).
/// Extract a top-level Dart function from a bare `function_signature` node (its
/// `function_body` is the next sibling). Mirrors the `function_signature` branch
/// of `extract_dart_declaration` but the span covers the signature + body so
/// `code_content` carries the body (the dead-code same-file probe and FTS rely
/// on it).
fn extract_dart_top_level_function(
    sig: &tree_sitter::Node,
    source: &str,
    parent_class: Option<&str>,
) -> Option<ParsedNode> {
    let name = get_child_by_field(sig, "name", source)?;
    let node_type = if parent_class.is_some() {
        "method"
    } else {
        "function"
    };
    let qualified_name = match parent_class {
        Some(cls) => Some(format!("{}.{}", cls, name)),
        None => Some(name.clone()),
    };
    let params = (0..sig.named_child_count())
        .filter_map(|j| sig.named_child(j))
        .find(|c| c.kind() == "formal_parameter_list")
        .map(|p| node_text(&p, source).to_string());
    let ret = (0..sig.named_child_count())
        .filter_map(|j| sig.named_child(j))
        .find(|c| matches!(c.kind(), "type_identifier" | "void_type" | "function_type"))
        .map(|r| node_text(&r, source).to_string());
    // NUL→space (same convention as extract_signature_info) so the stored
    // return_type/param_types/signature and the derived context_string stay
    // SQLite-LIKE-searchable.
    let params = strip_nul_field(params);
    let ret = strip_nul_field(ret);
    let signature = match (&params, &ret) {
        (Some(p), Some(r)) => Some(format!("{} -> {}", p, r)),
        (Some(p), None) => Some(p.clone()),
        _ => None,
    };
    // Span = signature .. function_body sibling (block `{...}` or arrow `=> e;`).
    let end_node = sig
        .next_named_sibling()
        .filter(|s| s.kind() == "function_body")
        .unwrap_or(*sig);
    let code = source
        .get(sig.start_byte()..end_node.end_byte())
        .unwrap_or("");
    Some(ParsedNode {
        node_type: node_type.into(),
        name,
        qualified_name,
        start_line: sig.start_position().row as u32 + 1,
        end_line: end_node.end_position().row as u32 + 1,
        code_content: truncate_code_content(code).into_owned(),
        signature,
        doc_comment: get_preceding_comment(sig, source),
        return_type: ret,
        param_types: params,
        is_test: false,
    })
}

/// declaration can contain function_signature, constructor_signature, etc.
fn extract_dart_declaration(
    node: &tree_sitter::Node,
    source: &str,
    parent_class: Option<&str>,
) -> Option<ParsedNode> {
    for i in 0..node.named_child_count() {
        if let Some(child) = node.named_child(i) {
            match child.kind() {
                "function_signature" => {
                    let name = get_child_by_field(&child, "name", source)?;
                    let node_type = if parent_class.is_some() {
                        "method"
                    } else {
                        "function"
                    };
                    let qualified_name = match parent_class {
                        Some(cls) => Some(format!("{}.{}", cls, name)),
                        None => Some(name.clone()),
                    };
                    let params = (0..child.named_child_count())
                        .filter_map(|j| child.named_child(j))
                        .find(|c| c.kind() == "formal_parameter_list")
                        .map(|p| node_text(&p, source).to_string());
                    let ret = (0..child.named_child_count())
                        .filter_map(|j| child.named_child(j))
                        .find(|c| {
                            matches!(c.kind(), "type_identifier" | "void_type" | "function_type")
                        })
                        .map(|r| node_text(&r, source).to_string());
                    let ret_with_args = ret.map(|r| {
                        let type_args = (0..child.named_child_count())
                            .filter_map(|j| child.named_child(j))
                            .find(|c| c.kind() == "type_arguments")
                            .map(|a| node_text(&a, source).to_string());
                        match type_args {
                            Some(args) => format!("{}{}", r, args),
                            None => r,
                        }
                    });
                    // NUL→space so return_type/param_types/signature (and the
                    // context_string built from them) stay SQLite-LIKE-searchable
                    // — same convention as the extract_signature_info path.
                    let params = strip_nul_field(params);
                    let ret_with_args = strip_nul_field(ret_with_args);
                    let signature = match (&params, &ret_with_args) {
                        (Some(p), Some(r)) => Some(format!("{} -> {}", p, r)),
                        (Some(p), None) => Some(p.clone()),
                        _ => None,
                    };
                    return Some(ParsedNode {
                        node_type: node_type.into(),
                        name,
                        qualified_name,
                        start_line: node.start_position().row as u32 + 1,
                        end_line: node.end_position().row as u32 + 1,
                        code_content: truncate_code_content(node_text(node, source)).into_owned(),
                        signature,
                        doc_comment: get_preceding_comment(node, source),
                        return_type: ret_with_args,
                        param_types: params,
                        is_test: false,
                    });
                }
                "constructor_signature" => {
                    let name = get_child_by_field(&child, "name", source)?;
                    let qualified_name = match parent_class {
                        Some(cls) => Some(format!("{}.{}", cls, name)),
                        None => Some(name.clone()),
                    };
                    let params = strip_nul_field(
                        child
                            .child_by_field_name("parameters")
                            .map(|p| node_text(&p, source).to_string()),
                    );
                    return Some(ParsedNode {
                        node_type: "function".into(),
                        name,
                        qualified_name,
                        start_line: node.start_position().row as u32 + 1,
                        end_line: node.end_position().row as u32 + 1,
                        code_content: truncate_code_content(node_text(node, source)).into_owned(),
                        signature: params.clone(),
                        doc_comment: get_preceding_comment(node, source),
                        return_type: None,
                        param_types: params,
                        is_test: false,
                    });
                }
                _ => {}
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    // L10: extracted code_content must not carry NUL bytes into FTS5 (the tokenizer
    // treats stored TEXT as a C-string and stops at the first NUL → the tail is
    // unsearchable). truncate_code_content is the single chokepoint every extraction
    // site wraps node_text in, so testing it directly mirrors the real producer.
    #[test]
    fn test_truncate_code_content_strips_nul_bytes() {
        let got = truncate_code_content("alpha\0betaGamma");
        assert!(!got.contains('\0'), "NUL must be stripped, got {got:?}");
        assert!(
            got.contains("betaGamma"),
            "text after the NUL must survive, got {got:?}"
        );
        assert_eq!(got, "alpha betaGamma");
        // Non-NUL short content stays byte-identical on the borrow fast path.
        assert!(matches!(
            truncate_code_content("plain code"),
            std::borrow::Cow::Borrowed("plain code")
        ));
    }

    #[test]
    fn test_parse_js_describe_it_marks_nested_as_test() {
        let code = r#"
function prodFn() { return 1; }

describe('Suite', () => {
    function helper() { return 2; }
    const arrow = () => 3;
    it('works', () => {
        function innerFn() { return 4; }
    });
    it.skip('skipped', () => {
        function skippedFn() {}
    });
});

beforeEach(() => {
    function setupFn() {}
});
"#;
        let nodes = parse_code(code, "javascript").unwrap();
        let by_name: std::collections::HashMap<&str, bool> =
            nodes.iter().map(|n| (n.name.as_str(), n.is_test)).collect();
        assert_eq!(
            by_name.get("prodFn").copied(),
            Some(false),
            "prodFn outside describe must NOT be is_test; nodes: {:?}",
            nodes
                .iter()
                .map(|n| (&n.name, n.is_test))
                .collect::<Vec<_>>()
        );
        assert_eq!(
            by_name.get("helper").copied(),
            Some(true),
            "helper inside describe → is_test"
        );
        assert_eq!(
            by_name.get("arrow").copied(),
            Some(true),
            "arrow inside describe → is_test"
        );
        assert_eq!(
            by_name.get("innerFn").copied(),
            Some(true),
            "innerFn inside it → is_test"
        );
        assert_eq!(
            by_name.get("skippedFn").copied(),
            Some(true),
            "skippedFn inside it.skip → is_test"
        );
        assert_eq!(
            by_name.get("setupFn").copied(),
            Some(true),
            "setupFn inside beforeEach → is_test"
        );
    }

    #[test]
    fn test_parse_tsx_describe_it_marks_nested_as_test() {
        // TSX went through LanguageConfig::for_language's default arm where `_ => "unknown"`
        // silently disabled every `config.name == "tsx"` match. Regression: confirm the
        // describe/it propagation fires for TSX after lang_config.rs adds the tsx case.
        let code = r#"
function prodFn() { return 1; }
describe('Widget', () => {
    function helper() { return 2; }
    it('renders', () => { function inner() {} });
});
"#;
        let nodes = parse_code(code, "tsx").unwrap();
        let by_name: std::collections::HashMap<&str, bool> =
            nodes.iter().map(|n| (n.name.as_str(), n.is_test)).collect();
        assert_eq!(by_name.get("prodFn").copied(), Some(false));
        assert_eq!(
            by_name.get("helper").copied(),
            Some(true),
            "tsx helper inside describe → is_test; nodes: {:?}",
            nodes
                .iter()
                .map(|n| (&n.name, n.is_test))
                .collect::<Vec<_>>()
        );
        assert_eq!(by_name.get("inner").copied(), Some(true));
    }

    #[test]
    fn test_parse_markdown_headings() {
        let code = "# Project Overview\n\nIntro.\n\n## Module Layout\n\ndetails\n\n### Important Patterns\n\nSubsection X\n--------------\n";
        let nodes = parse_code(code, "markdown").unwrap();
        let by_name: std::collections::HashMap<&str, &str> = nodes
            .iter()
            .map(|n| (n.name.as_str(), n.node_type.as_str()))
            .collect();
        assert_eq!(
            by_name.get("Project Overview").copied(),
            Some("h1"),
            "nodes: {:?}",
            nodes
                .iter()
                .map(|n| (&n.name, &n.node_type))
                .collect::<Vec<_>>()
        );
        assert_eq!(by_name.get("Module Layout").copied(), Some("h2"));
        assert_eq!(by_name.get("Important Patterns").copied(), Some("h3"));
        assert_eq!(
            by_name.get("Subsection X").copied(),
            Some("h2"),
            "setext h2 (dashes) should be detected"
        );
    }

    #[test]
    fn test_parse_cpp_gtest_marks_is_test() {
        let code = "#include <gtest/gtest.h>\n\nTEST(MathSuite, Addition) {\n    EXPECT_EQ(1 + 1, 2);\n}\n\nTEST_F(FixtureSuite, ScopedTest) {\n    EXPECT_TRUE(true);\n}\n\nint regular_func() { return 0; }\n";
        let nodes = parse_code(code, "cpp").unwrap();
        let by_name: std::collections::HashMap<&str, bool> =
            nodes.iter().map(|n| (n.name.as_str(), n.is_test)).collect();
        assert_eq!(
            by_name.get("MathSuite.Addition").copied(),
            Some(true),
            "TEST(MathSuite, Addition) should yield Suite.Name + is_test=true; nodes: {:?}",
            nodes
                .iter()
                .map(|n| (&n.name, n.is_test))
                .collect::<Vec<_>>()
        );
        assert_eq!(
            by_name.get("FixtureSuite.ScopedTest").copied(),
            Some(true),
            "TEST_F should also be detected, got: {:?}",
            nodes
                .iter()
                .map(|n| (&n.name, n.is_test))
                .collect::<Vec<_>>()
        );
        assert_eq!(
            by_name.get("regular_func").copied(),
            Some(false),
            "non-gtest function should not be is_test"
        );
    }

    #[test]
    fn test_parse_cpp_method_scope_qualified() {
        // C++ method scope: in-class methods and out-of-class `Type::method`
        // definitions should carry node_type "method" + qualified_name
        // "Class.method"; free functions stay bare.
        let code = "class Calculator {\n    int add(int a, int b) { return a + b; }\n};\nint Calculator::multiply(int a, int b) { return a * b; }\nint free_fn() { return 0; }\n";
        let nodes = parse_code(code, "cpp").unwrap();
        let dump: Vec<_> = nodes
            .iter()
            .map(|n| {
                (
                    n.name.clone(),
                    n.node_type.clone(),
                    n.qualified_name.clone(),
                )
            })
            .collect();
        let find = |name: &str| {
            nodes
                .iter()
                .find(|n| n.name == name)
                .map(|n| (n.node_type.as_str(), n.qualified_name.as_deref()))
        };
        assert_eq!(
            find("add"),
            Some(("method", Some("Calculator.add"))),
            "in-class method should be Calculator.add; got: {:?}",
            dump
        );
        assert_eq!(
            find("multiply"),
            Some(("method", Some("Calculator.multiply"))),
            "out-of-class def should be Calculator.multiply; got: {:?}",
            dump
        );
        assert_eq!(
            find("free_fn"),
            Some(("function", Some("free_fn"))),
            "free function should stay bare; got: {:?}",
            dump
        );
    }

    /// P1-4: a Go method's RECEIVER is its owner, and it was never folded into
    /// `qualified_name` — so `Server.Start` and `Client.Start` were two nodes
    /// both spelled `Start`, indistinguishable to every symbol lookup. The
    /// visible damage is silent: `callgraph Start` merges the callers of both,
    /// and the ambiguity is folded out by the default confidence floor, so the
    /// answer looks clean (audit 2026-08-16 P1-4). C++ has carried
    /// `Class.method` since v0.91.0; Go is the same shape spelled differently.
    #[test]
    fn test_parse_go_method_receiver_qualifies_the_name() {
        let code = "package p\n\
            type Server struct{}\n\
            type Client struct{}\n\
            func (s *Server) Start() error { return nil }\n\
            func (c Client) Start() error { return nil }\n\
            func (s *Server[T]) Generic() {}\n\
            func Plain() {}\n";
        let nodes = parse_code(code, "go").unwrap();
        let dump: Vec<_> = nodes
            .iter()
            .map(|n| {
                (
                    n.name.clone(),
                    n.node_type.clone(),
                    n.qualified_name.clone(),
                )
            })
            .collect();
        let quals: Vec<&str> = nodes
            .iter()
            .filter_map(|n| n.qualified_name.as_deref())
            .collect();

        // Pointer receiver.
        assert!(
            quals.contains(&"Server.Start"),
            "pointer receiver must qualify the method; got: {dump:?}"
        );
        // Value receiver — same method name, different owner. This pair is the
        // whole point: before the fix both were bare `Start`.
        assert!(
            quals.contains(&"Client.Start"),
            "value receiver must qualify the method; got: {dump:?}"
        );
        // Generic receiver: the type ARGUMENTS are not part of the owner name,
        // or `Server[T].Generic` would never match a lookup for `Server`.
        assert!(
            quals.contains(&"Server.Generic"),
            "a generic receiver must qualify by its BASE type; got: {dump:?}"
        );
        // A plain function has no receiver and must stay bare — otherwise this
        // fix is indistinguishable from "prefix everything".
        let plain = nodes.iter().find(|n| n.name == "Plain").unwrap();
        assert_eq!(plain.qualified_name.as_deref(), Some("Plain"), "{dump:?}");
        assert_eq!(plain.node_type, "function", "{dump:?}");
        // The bare `name` is unchanged: by-name lookup must keep working.
        assert_eq!(
            nodes.iter().filter(|n| n.name == "Start").count(),
            2,
            "both methods keep the bare name `Start`; got: {dump:?}"
        );
    }

    #[test]
    fn test_parse_bash_functions() {
        let code = "#!/usr/bin/env bash\n\ngreet() {\n    echo \"hi\"\n}\n\nfunction backup_files {\n    cp \"$1\" \"$2\"\n}\n";
        let nodes = parse_code(code, "bash").unwrap();
        let names: Vec<&str> = nodes.iter().map(|n| n.name.as_str()).collect();
        assert!(names.contains(&"greet"), "missing greet, got: {:?}", names);
        assert!(
            names.contains(&"backup_files"),
            "missing backup_files, got: {:?}",
            names
        );
    }

    #[test]
    fn test_parse_json_loads_grammar() {
        // JSON has no function/class concept; we verify the grammar links + parses
        // without panicking. Empty symbol list is the expected, correct outcome —
        // file still gets file-level indexing for FTS via the indexer.
        let code =
            "{\n  \"name\": \"foo\",\n  \"version\": \"1.0.0\",\n  \"deps\": [\"a\", \"b\"]\n}\n";
        let nodes = parse_code(code, "json").unwrap();
        assert!(
            nodes.is_empty(),
            "json should yield no symbol nodes, got: {:?}",
            nodes
                .iter()
                .map(|n| (&n.name, &n.node_type))
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn test_parse_typescript_functions() {
        let code = r#"
function handleLogin(req: Request, res: Response): void {
    validateToken(req.token);
    res.send(200);
}

const processPayment = async (amount: number): Promise<void> => {
    await chargeCard(amount);
};

class UserService {
    async findUser(id: string): Promise<User> {
        return db.query(id);
    }
}
"#;
        let nodes = parse_code(code, "typescript").unwrap();
        let names: Vec<&str> = nodes.iter().map(|n| n.name.as_str()).collect();
        assert!(
            names.contains(&"handleLogin"),
            "missing handleLogin, got: {:?}",
            names
        );
        assert!(
            names.contains(&"processPayment"),
            "missing processPayment, got: {:?}",
            names
        );
        assert!(
            names.contains(&"UserService"),
            "missing UserService, got: {:?}",
            names
        );
        assert!(
            names.contains(&"findUser"),
            "missing findUser, got: {:?}",
            names
        );
    }

    #[test]
    fn test_parse_extracts_signatures() {
        let code = "function add(a: number, b: number): number { return a + b; }";
        let nodes = parse_code(code, "typescript").unwrap();
        assert_eq!(nodes.len(), 1);
        assert!(nodes[0].signature.is_some(), "signature should be present");
    }

    #[test]
    fn test_parse_extracts_line_numbers() {
        let code = "// line 1\nfunction foo() {\n  return 1;\n}\n";
        let nodes = parse_code(code, "typescript").unwrap();
        assert_eq!(nodes[0].start_line, 2);
        assert_eq!(nodes[0].end_line, 4);
    }

    #[test]
    fn test_parse_go_functions() {
        let code =
            "package main\nfunc handleRequest(w http.ResponseWriter, r *http.Request) {\n}\n";
        let nodes = parse_code(code, "go").unwrap();
        assert!(
            nodes.iter().any(|n| n.name == "handleRequest"),
            "got: {:?}",
            nodes.iter().map(|n| &n.name).collect::<Vec<_>>()
        );
    }

    #[test]
    fn test_parse_python_functions() {
        let code = "def process_data(items: list) -> dict:\n    return {}\n\nclass DataProcessor:\n    def run(self):\n        pass\n";
        let nodes = parse_code(code, "python").unwrap();
        let names: Vec<&str> = nodes.iter().map(|n| n.name.as_str()).collect();
        assert!(names.contains(&"process_data"), "got: {:?}", names);
        assert!(names.contains(&"DataProcessor"), "got: {:?}", names);
    }

    #[test]
    fn test_parse_rust_functions() {
        let code =
            "pub fn calculate(x: i32, y: i32) -> i32 { x + y }\nstruct Config { name: String }\n";
        let nodes = parse_code(code, "rust").unwrap();
        let names: Vec<&str> = nodes.iter().map(|n| n.name.as_str()).collect();
        assert!(names.contains(&"calculate"), "got: {:?}", names);
        assert!(names.contains(&"Config"), "got: {:?}", names);
    }

    #[test]
    fn test_parse_java_methods() {
        let code = "public class UserController {\n    public void getUser(String id) {}\n}\n";
        let nodes = parse_code(code, "java").unwrap();
        let names: Vec<&str> = nodes.iter().map(|n| n.name.as_str()).collect();
        assert!(names.contains(&"UserController"), "got: {:?}", names);
    }

    #[test]
    fn test_parse_c_functions() {
        let code = "int main(int argc, char *argv[]) { return 0; }\n";
        let nodes = parse_code(code, "c").unwrap();
        assert!(
            nodes.iter().any(|n| n.name == "main"),
            "got: {:?}",
            nodes.iter().map(|n| &n.name).collect::<Vec<_>>()
        );
    }

    #[test]
    fn test_parse_tsx_jsx_syntax() {
        // Use generic arrow + JSX — the TS parser misparses <T> as JSX tag,
        // only the TSX grammar handles the ambiguity correctly.
        let code = r#"
function App() {
    return <div className="app"><span>hello</span></div>;
}

function Container() {
    const items = [1, 2, 3];
    return (
        <ul>
            {items.map(i => <li key={i}>{i}</li>)}
        </ul>
    );
}
"#;
        let nodes = parse_code(code, "tsx").unwrap();
        let names: Vec<&str> = nodes.iter().map(|n| n.name.as_str()).collect();
        assert!(
            names.contains(&"App"),
            "TSX function with JSX should be parsed, got: {:?}",
            names
        );
        assert!(
            names.contains(&"Container"),
            "TSX function with complex JSX should be parsed, got: {:?}",
            names
        );
    }

    #[test]
    fn test_parse_ts_type_alias() {
        let code = "type UserId = string;\ntype Config = { name: string; port: number };\n";
        let nodes = parse_code(code, "typescript").unwrap();
        let names: Vec<&str> = nodes.iter().map(|n| n.name.as_str()).collect();
        assert!(names.contains(&"UserId"), "got: {:?}", names);
        assert!(names.contains(&"Config"), "got: {:?}", names);
        assert!(nodes.iter().find(|n| n.name == "UserId").unwrap().node_type == "type");
    }

    #[test]
    fn test_parse_java_interface_and_enum() {
        let code = "public interface Comparable {\n    int compareTo(Object o);\n}\npublic enum Color { RED, GREEN, BLUE }\n";
        let nodes = parse_code(code, "java").unwrap();
        let names: Vec<&str> = nodes.iter().map(|n| n.name.as_str()).collect();
        assert!(names.contains(&"Comparable"), "got: {:?}", names);
        assert!(names.contains(&"Color"), "got: {:?}", names);
        assert!(
            nodes
                .iter()
                .find(|n| n.name == "Comparable")
                .unwrap()
                .node_type
                == "interface"
        );
        assert!(nodes.iter().find(|n| n.name == "Color").unwrap().node_type == "enum");
    }

    #[test]
    fn test_parse_cpp_class_and_struct() {
        let code = "class MyClass {\npublic:\n    void doSomething() {}\n};\nstruct Point {\n    int x;\n    int y;\n};\n";
        let nodes = parse_code(code, "cpp").unwrap();
        let names: Vec<&str> = nodes.iter().map(|n| n.name.as_str()).collect();
        assert!(names.contains(&"MyClass"), "got: {:?}", names);
        assert!(names.contains(&"Point"), "got: {:?}", names);
        assert!(
            nodes
                .iter()
                .find(|n| n.name == "MyClass")
                .unwrap()
                .node_type
                == "class"
        );
        assert!(nodes.iter().find(|n| n.name == "Point").unwrap().node_type == "struct");
    }

    #[test]
    fn test_parse_python_async_function() {
        let code = "async def fetch_data(url: str) -> dict:\n    return {}\n\nclass Api:\n    async def get(self, path):\n        pass\n";
        let nodes = parse_code(code, "python").unwrap();
        let names: Vec<&str> = nodes.iter().map(|n| n.name.as_str()).collect();
        assert!(names.contains(&"fetch_data"), "got: {:?}", names);
        assert!(names.contains(&"get"), "got: {:?}", names);
    }

    /// Issue #31: `get_ast_node` stripped every decorator because the symbol was
    /// bound to the inner tree-sitter `function_definition`/`class_definition`
    /// (which starts at `def`/`class`) instead of the enclosing
    /// `decorated_definition` wrapper that spans the decorator stack. For pydantic
    /// v2 the decorator IS the contract (`@field_validator("lat", mode="before")`
    /// carries the validated field + mode), so its loss blinds semantic search /
    /// impact analysis. Extent (`start_line`) and `code_content` must include the
    /// decorators for decorated functions, methods, async methods, and classes,
    /// while name/signature still come from the inner definition.
    #[test]
    fn test_parse_python_decorated_extent_includes_decorators() {
        let code = r#"from pydantic import BaseModel, field_validator, computed_field


class Demo(BaseModel):
    lat: str

    @field_validator("lat", mode="before")
    @classmethod
    def pre_validate(cls, v):
        return str(v)

    @computed_field
    @property
    def label(self) -> str:
        return "x"

    @staticmethod
    async def refresh(self):
        return None


@app.route("/health")
def health():
    return "ok"


@dataclass
class Config:
    x: int
"#;
        let nodes = parse_code(code, "python").unwrap();
        let by_name = |n: &str| -> &ParsedNode {
            nodes.iter().find(|x| x.name == n).unwrap_or_else(|| {
                panic!(
                    "{n} not extracted; got: {:?}",
                    nodes.iter().map(|x| &x.name).collect::<Vec<_>>()
                )
            })
        };

        // Decorated classmethod: full decorator stack + start at first decorator.
        let pv = by_name("pre_validate");
        assert!(
            pv.code_content
                .contains("@field_validator(\"lat\", mode=\"before\")"),
            "code_content must include the pydantic decorator (issue #31); got: {:?}",
            pv.code_content
        );
        assert!(
            pv.code_content.contains("@classmethod"),
            "code_content must include the FULL decorator stack; got: {:?}",
            pv.code_content
        );
        assert_eq!(
            pv.start_line, 7,
            "start_line must point at the first decorator, not `def`; got: {}",
            pv.start_line
        );
        assert_eq!(pv.node_type, "method");

        // @property method (issue #31 secondary): decorator retained.
        let label = by_name("label");
        assert!(
            label.code_content.contains("@property"),
            "property decorator must be retained; got: {:?}",
            label.code_content
        );

        // Decorated async staticmethod.
        let refresh = by_name("refresh");
        assert!(
            refresh.code_content.contains("@staticmethod"),
            "async method decorator must be retained; got: {:?}",
            refresh.code_content
        );

        // Top-level decorated function.
        let health = by_name("health");
        assert!(
            health.code_content.contains("@app.route(\"/health\")"),
            "top-level function decorator must be retained; got: {:?}",
            health.code_content
        );

        // Decorated class.
        let config = by_name("Config");
        assert!(
            config.code_content.contains("@dataclass"),
            "class decorator must be retained; got: {:?}",
            config.code_content
        );
        assert_eq!(config.node_type, "class");
    }

    #[test]
    fn test_typescript_return_type_extraction() {
        let code = r#"
function greet(name: string): string {
    return "hello " + name;
}

function noReturn(x: number) {
    console.log(x);
}
"#;
        let nodes = parse_code(code, "typescript").unwrap();
        let greet = nodes.iter().find(|n| n.name == "greet").unwrap();
        // Leading colon + whitespace from the TS `type_annotation` node is
        // stripped at extraction so output matches Python/Rust shape.
        assert_eq!(greet.return_type.as_deref(), Some("string"));

        let no_ret = nodes.iter().find(|n| n.name == "noReturn").unwrap();
        assert!(no_ret.return_type.is_none());
    }

    #[test]
    fn test_typescript_param_types_extraction() {
        let code = "function add(a: number, b: number): number { return a + b; }";
        let nodes = parse_code(code, "typescript").unwrap();
        let add = nodes.iter().find(|n| n.name == "add").unwrap();
        assert!(add.param_types.as_ref().unwrap().contains("number"));
    }

    #[test]
    fn test_parse_rust_constants() {
        let code = r#"
pub const MAX_SIZE: usize = 1024;
static DB_PATH: &str = "/tmp/db";
const NAMES: &[&str] = &["a", "b"];
"#;
        let nodes = parse_code(code, "rust").unwrap();
        let names: Vec<&str> = nodes.iter().map(|n| n.name.as_str()).collect();
        assert!(
            names.contains(&"MAX_SIZE"),
            "should parse const, got: {:?}",
            names
        );
        assert!(
            names.contains(&"DB_PATH"),
            "should parse static, got: {:?}",
            names
        );
        assert!(
            names.contains(&"NAMES"),
            "should parse const array, got: {:?}",
            names
        );

        let max_size = nodes.iter().find(|n| n.name == "MAX_SIZE").unwrap();
        assert_eq!(max_size.node_type, "constant");
        assert_eq!(
            max_size.return_type.as_deref(),
            Some("usize"),
            "should capture type annotation"
        );

        let db_path = nodes.iter().find(|n| n.name == "DB_PATH").unwrap();
        assert_eq!(db_path.node_type, "constant");
    }

    #[test]
    fn test_parse_typescript_exported_consts() {
        // Top-level `export const X = <value>` becomes a `constant` node so a
        // cross-file `import { X }` resolves to it and forms a REL_IMPORTS edge
        // (INDEX_VERSION 39, feedback_const_export_no_import_edge). Arrow-valued
        // consts stay `function`; non-exported consts — top-level OR function-local —
        // are NOT extracted: they can't be imported cross-file, so a node would be
        // pure noise.
        let code = r#"
export const API_URL = "https://example.com";
export const DEFAULT_CONFIG = {
    timeout: 5000,
    retries: 3,
};
export const authStore = createStore({ url: API_URL });
export const buildUrl = (p: string) => API_URL + p;
export function handler() { return 1; }

const NOT_EXPORTED = 42;

function scope() {
    const localOnly = 7;
    return localOnly;
}
"#;
        let nodes = parse_code(code, "typescript").unwrap();
        let by = |n: &str| nodes.iter().find(|x| x.name == n);

        // Value literals, objects, and call-result singletons → `constant`.
        assert_eq!(
            by("API_URL").expect("exported value const").node_type,
            "constant"
        );
        assert_eq!(
            by("DEFAULT_CONFIG")
                .expect("multi-line object const")
                .node_type,
            "constant"
        );
        assert_eq!(
            by("authStore").expect("singleton const").node_type,
            "constant"
        );

        // Arrow-valued const and plain function stay `function` (unchanged).
        assert_eq!(by("buildUrl").expect("arrow const").node_type, "function");
        assert_eq!(
            by("handler").expect("export function").node_type,
            "function"
        );

        // Non-exported consts are never extracted (can't be imported → noise).
        assert!(
            by("NOT_EXPORTED").is_none(),
            "non-exported top-level const must not be a symbol"
        );
        assert!(
            by("localOnly").is_none(),
            "function-local const must not be a symbol"
        );
    }

    #[test]
    fn test_parse_typescript_destructuring_exports() {
        // A destructuring export binds MULTIPLE importable names. Before INDEX_VERSION
        // 41 the declarator's pattern text (`{ host, port }`) became a single garbage
        // node — no valid identifier, so `import { host }` dangled to `<external>`.
        // Now one `constant` node is emitted per bound name (Redux `export const {
        // actions, reducer } = slice`, React `export const { Provider } = createContext()`).
        let code = r#"
export const { host, port } = getConfig();
export const [first, second] = getPair();
export const { renamedFrom: localName } = getObj();
export const { withDefault = 10 } = getObj();
export const { keep, ...theRest } = getObj();
export const SIMPLE = 1;

const { notExported } = getObj();
"#;
        let nodes = parse_code(code, "typescript").unwrap();
        let by = |n: &str| nodes.iter().find(|x| x.name == n);

        // Object-shorthand and array bindings each become their own constant.
        for name in ["host", "port", "first", "second", "keep", "theRest"] {
            let n = by(name).unwrap_or_else(|| {
                panic!("destructured binding `{name}` should be a constant node")
            });
            assert_eq!(
                n.node_type, "constant",
                "binding `{name}` should be a constant"
            );
        }

        // Renamed `{ renamedFrom: localName }` binds the LOCAL name (the exported one),
        // not the source property key.
        assert!(
            by("localName").is_some(),
            "renamed destructure binds the local name"
        );
        assert!(
            by("renamedFrom").is_none(),
            "the source key is not a binding"
        );

        // Default `{ withDefault = 10 }` binds `withDefault`.
        assert!(
            by("withDefault").is_some(),
            "default-valued destructure binds the name"
        );

        // The literal pattern text must NEVER become a node name.
        assert!(
            by("{ host, port }").is_none(),
            "pattern text must not be a symbol name"
        );
        assert!(
            by("[first, second]").is_none(),
            "array pattern text must not be a symbol name"
        );

        // Plain identifier export still works; non-exported destructure is not extracted.
        assert_eq!(by("SIMPLE").expect("plain const").node_type, "constant");
        assert!(
            by("notExported").is_none(),
            "non-exported destructure must not be a symbol"
        );
    }

    // A doc comment had NO length cap while code_content has had one since v47
    // (4 KB + a three-dot sentinel). Measured MAX in this repo: 20,828 bytes —
    // the giant INDEX_VERSION changelog tail, which tree-sitter attaches as the
    // preceding comment of whatever constant follows it. Two concrete costs:
    // FTS5 recall pollution (a 46-byte constant answering a query for a word
    // that appears only in the changelog prose), and a 512-token embedding
    // window filled with comment before any code reaches it.
    #[test]
    fn test_preceding_comment_is_capped_like_code_content() {
        let max = max_code_content_len();
        let filler = "x".repeat(max * 2);
        let src = format!("// {filler}\npub const TAIL: i32 = 1;\n");
        let nodes = parse_code(&src, "rust").unwrap();
        let doc = nodes
            .iter()
            .find(|n| n.name == "TAIL")
            .and_then(|n| n.doc_comment.clone())
            .expect("TAIL must carry the preceding comment");
        assert!(
            doc.len() <= max + 3,
            "doc_comment must be capped at max_code_content_len (+3 sentinel), got {} bytes",
            doc.len()
        );
        assert!(
            doc.ends_with("..."),
            "a truncated doc_comment must carry the same three-dot sentinel code_content uses, got tail {:?}",
            &doc[doc.len().saturating_sub(8)..]
        );
    }

    /// INDEX_VERSION 63: a JSDoc block precedes `export function f(){}` as a
    /// whole, so it is a sibling of the `export_statement` — the sibling walk
    /// started at the inner declaration and found the `export` keyword instead.
    /// Every EXPORTED TS/JS symbol therefore carried an empty `doc_comment`,
    /// while the unwrapped forms (plain function, class method) carried theirs,
    /// which is what made the column look populated. Unlike a Python docstring
    /// the block is OUTSIDE the node, so `code_content` did not hold it either
    /// and the text was unreachable by every channel.
    #[test]
    fn test_exported_ts_declarations_keep_their_jsdoc() {
        let src = "\
/** DOC_PLAIN */
function plainFn(): void {}

/** DOC_EXPORTED */
export function exportedFn(): void {}

/** DOC_CLASS */
export class Cls {
  /** DOC_METHOD */
  method(): void {}
}

/** DOC_ARROW */
export const arrowFn = (): void => {};

/** DOC_DEFAULT */
export default function defaultFn(): void {}
";
        let nodes = parse_code(src, "typescript").unwrap();
        let doc_of = |name: &str| -> String {
            nodes
                .iter()
                .find(|n| n.name == name)
                .unwrap_or_else(|| panic!("{name} must be extracted"))
                .doc_comment
                .clone()
                .unwrap_or_default()
        };
        for (symbol, marker) in [
            ("plainFn", "DOC_PLAIN"),
            ("exportedFn", "DOC_EXPORTED"),
            ("Cls", "DOC_CLASS"),
            ("method", "DOC_METHOD"),
            ("arrowFn", "DOC_ARROW"),
            ("defaultFn", "DOC_DEFAULT"),
        ] {
            assert!(
                doc_of(symbol).contains(marker),
                "{symbol} lost its doc comment: expected {marker}, got {:?}",
                doc_of(symbol)
            );
        }
    }

    /// Negative control for the wrapper climb: it must stop at the first
    /// non-comment sibling, not keep walking until it finds SOME comment.
    /// An undocumented `export` following a documented one is the shape that
    /// would expose an unbounded climb — `second` must stay empty rather than
    /// inherit the block that documents `first`.
    #[test]
    fn test_wrapper_climb_stops_at_the_previous_declaration() {
        let src = "\
/** DOC_FIRST */
export function first(): void {}
export function second(): void {}
";
        let nodes = parse_code(src, "typescript").unwrap();
        let doc_of = |name: &str| -> String {
            nodes
                .iter()
                .find(|n| n.name == name)
                .map(|n| n.doc_comment.clone().unwrap_or_default())
                .unwrap_or_default()
        };
        assert!(
            doc_of("first").contains("DOC_FIRST"),
            "positive half: first must carry its own doc, got {:?}",
            doc_of("first")
        );
        assert!(
            doc_of("second").is_empty(),
            "an undocumented export must not inherit the previous one's doc, got {:?}",
            doc_of("second")
        );
    }

    /// The climb makes `export` invisible to doc attribution — the exported and
    /// local spellings of the same declaration must agree. Pinned as parity
    /// rather than as an absolute: the two forms have to move together, and
    /// since INDEX_VERSION 65 they do so under the first-declarator rule
    /// (`test_multi_declarator_doc_goes_to_the_first_only`).
    #[test]
    fn test_export_wrapper_does_not_change_doc_attribution() {
        let src = "\
/** DOC_LOCAL */
const localA = () => {}, localB = () => {};

/** DOC_EXPORT */
export const expA = () => {}, expB = () => {};
";
        let nodes = parse_code(src, "typescript").unwrap();
        let has_doc = |name: &str| -> bool {
            nodes
                .iter()
                .find(|n| n.name == name)
                .map(|n| n.doc_comment.as_deref().unwrap_or("").contains("DOC_"))
                .unwrap_or(false)
        };
        assert_eq!(
            (has_doc("localA"), has_doc("localB")),
            (has_doc("expA"), has_doc("expB")),
            "exported and local declarations must attribute doc comments identically"
        );
        assert!(has_doc("expA"), "the exported form must carry a doc at all");
    }

    /// INDEX_VERSION 65: `/** DOC */ export const a = 1, b = 2;` handed DOC to
    /// BOTH names. `extract_named_arrows` reads the comment off the
    /// `lexical_declaration`, which owns every declarator, so each one resolved
    /// to the single comment above the statement.
    ///
    /// This is not a cosmetic mis-labelling. `doc_comment` is ranked ABOVE
    /// `code:` in the embedding context builder precisely because it is the
    /// densest description of a symbol, so a wrong doc makes `b` retrievable
    /// under a description of `a` — a phantom bound to a real node, which this
    /// codebase has repeatedly found to be worse than the missing edge or the
    /// missing field it replaces.
    ///
    /// The rule chosen is the one the file already applies to the analogous
    /// shape: Go's `// GROUP_DOC\ntype ( Alpha …; Beta … )` gives the doc to
    /// `Alpha` and withholds it from `Beta`
    /// (`test_wrapper_climb_skips_later_group_members`). A comma-separated JS
    /// declaration is the same construct with different punctuation, and it
    /// simply never reached that check.
    #[test]
    fn test_multi_declarator_doc_goes_to_the_first_only() {
        let doc_of = |src: &str, name: &str| -> String {
            parse_code(src, "typescript")
                .unwrap()
                .iter()
                .find(|n| n.name == name)
                .and_then(|n| n.doc_comment.clone())
                .unwrap_or_default()
        };

        // Constant-valued declarators (the exported-const extraction path).
        let consts = "/** DOC_A */\nexport const a = 1, b = 2;\n";
        assert!(
            doc_of(consts, "a").contains("DOC_A"),
            "the first declarator keeps the doc, got {:?}",
            doc_of(consts, "a")
        );
        assert_eq!(
            doc_of(consts, "b"),
            "",
            "a later declarator must not inherit the statement's doc"
        );

        // Arrow-valued declarators (the other branch of extract_named_arrows —
        // same `lexical_declaration`, different emit path, so it needs its own
        // assertion rather than riding on the one above).
        let arrows = "/** DOC_F */\nexport const f = () => {}, g = () => {};\n";
        assert!(doc_of(arrows, "f").contains("DOC_F"));
        assert_eq!(doc_of(arrows, "g"), "");

        // `export let` takes the same path and must not be a survivor.
        let lets = "/** DOC_L */\nexport let m = 1, n = 2;\n";
        assert!(doc_of(lets, "m").contains("DOC_L"));
        assert_eq!(doc_of(lets, "n"), "");

        // Negative control: a SINGLE declarator must still get its doc — the fix
        // must not be "stop attributing docs to declarators at all", which every
        // assertion above would also pass.
        let single = "/** DOC_S */\nexport const only = 1;\n";
        assert!(
            doc_of(single, "only").contains("DOC_S"),
            "a lone declarator must keep its doc, got {:?}",
            doc_of(single, "only")
        );

        // A destructuring declarator binds several names from ONE declarator, so
        // there is no "later declarator" to withhold from — it is a different
        // shape and deliberately unchanged here. Pinned so the next person sees
        // it was considered, not missed.
        let destructured = "/** DOC_D */\nexport const { host, port } = getConfig();\n";
        assert!(doc_of(destructured, "host").contains("DOC_D"));
        assert!(
            doc_of(destructured, "port").contains("DOC_D"),
            "one declarator's names share its doc; only MULTI-declarator statements split"
        );
    }

    /// INDEX_VERSION 63: Python documents with a docstring, not a preceding
    /// comment, so `doc_comment` was empty for every Python symbol. The text is
    /// inside `code_content` (so FTS still reached it) but the embedding context
    /// builder ranks `doc:` above `code:` exactly because code is what gets
    /// truncated at 512 tokens — a long function's docstring was the first thing
    /// dropped from its vector.
    #[test]
    fn test_python_docstrings_populate_doc_comment() {
        let src = "\
def documented(path):
    \"\"\"DOC_FUNC reads the manifest.\"\"\"
    return {}

class Documented:
    \"\"\"DOC_CLASS holds config.\"\"\"

    def method(self):
        \"\"\"DOC_METHOD does a thing.\"\"\"
        return 1

def undocumented(x):
    return x
";
        let nodes = parse_code(src, "python").unwrap();
        let doc_of = |name: &str| -> String {
            nodes
                .iter()
                .find(|n| n.name == name)
                .unwrap_or_else(|| panic!("{name} must be extracted"))
                .doc_comment
                .clone()
                .unwrap_or_default()
        };
        for (symbol, marker) in [
            ("documented", "DOC_FUNC"),
            ("Documented", "DOC_CLASS"),
            ("method", "DOC_METHOD"),
        ] {
            assert!(
                doc_of(symbol).contains(marker),
                "{symbol} must carry its docstring: expected {marker}, got {:?}",
                doc_of(symbol)
            );
        }
        // Negative control: a body whose first statement is not a string must
        // stay undocumented, rather than picking up the first literal it finds.
        assert!(
            doc_of("undocumented").is_empty(),
            "a function without a docstring must have no doc_comment, got {:?}",
            doc_of("undocumented")
        );
    }

    /// A Rust `fn` also has a `block` body, so `fn f() { "x"; }` is the shape
    /// that would be misread as documentation. Two independent guards stop it —
    /// the Python-kind gate (`function_item` is not `function_definition`) and
    /// the literal-kind check (Rust spells it `string_literal`, not `string`) —
    /// and mutation testing confirmed this goes red only when BOTH are removed,
    /// which is the honest description: it pins the pair, not either one.
    #[test]
    fn test_rust_leading_string_statement_is_not_a_docstring() {
        let src = "pub fn noisy() { \"NOT_A_DOCSTRING\"; }\n";
        let nodes = parse_code(src, "rust").unwrap();
        let doc = nodes
            .iter()
            .find(|n| n.name == "noisy")
            .expect("noisy must be extracted")
            .doc_comment
            .clone()
            .unwrap_or_default();
        assert!(
            !doc.contains("NOT_A_DOCSTRING"),
            "a Rust string statement must not be read as a doc comment, got {doc:?}"
        );
    }

    /// Doc-comment extraction had no per-language guard at all, and the sweep
    /// that added this table found four silent gaps at once — TS `export`,
    /// Python docstrings, Dart's `documentation_comment` spelling, and the
    /// Go/Ruby wrapper nodes — each invisible because the languages that DID
    /// work made the column look populated. The axis is (language, declaration
    /// form); a new language or a new wrapper shape must add a row here.
    #[test]
    fn test_doc_comment_parity_across_languages() {
        /// (language, source, [(symbol, marker it must carry)])
        type DocCase = (
            &'static str,
            &'static str,
            &'static [(&'static str, &'static str)],
        );
        let cases: &[DocCase] = &[
            (
                "typescript",
                "/** M_TS_C */\nexport class C {\n  /** M_TS_M */\n  m(): void {}\n}\n",
                &[("C", "M_TS_C"), ("m", "M_TS_M")],
            ),
            (
                "python",
                "class P:\n    \"\"\"M_PY_C\"\"\"\n\n    def m(self):\n        \"\"\"M_PY_M\"\"\"\n        return 1\n",
                &[("P", "M_PY_C"), ("m", "M_PY_M")],
            ),
            (
                "rust",
                "/// M_RS_S\npub struct S;\n\n/// M_RS_F\npub fn f() {}\n",
                &[("S", "M_RS_S"), ("f", "M_RS_F")],
            ),
            (
                "go",
                "package p\n\n// M_GO_T\ntype T struct{}\n\n// M_GO_F\nfunc F() {}\n",
                &[("T", "M_GO_T"), ("F", "M_GO_F")],
            ),
            (
                "java",
                "/** M_JV_C */\npublic class J {\n  /** M_JV_M */\n  public void m() {}\n}\n",
                &[("J", "M_JV_C"), ("m", "M_JV_M")],
            ),
            (
                "ruby",
                "# M_RB_C\nclass R\n  # M_RB_M\n  def m\n    1\n  end\nend\n",
                &[("R", "M_RB_C"), ("m", "M_RB_M")],
            ),
            (
                "dart",
                "/// M_DT_C\nclass D {\n  /// M_DT_M\n  void m() {}\n}\n",
                &[("D", "M_DT_C"), ("m", "M_DT_M")],
            ),
            (
                "php",
                "<?php\n/** M_PHP_C */\nclass P {\n  /** M_PHP_M */\n  public function m() {}\n}\n",
                &[("P", "M_PHP_C"), ("m", "M_PHP_M")],
            ),
            (
                "csharp",
                "/// M_CS_C\npublic class C {\n  /// M_CS_M\n  public void M() {}\n}\n",
                &[("C", "M_CS_C"), ("M", "M_CS_M")],
            ),
            (
                "kotlin",
                "/** M_KT_C */\nclass K {\n  /** M_KT_M */\n  fun m() {}\n}\n",
                &[("K", "M_KT_C"), ("m", "M_KT_M")],
            ),
            (
                "swift",
                "/// M_SW_C\nclass S {\n  /// M_SW_M\n  func m() {}\n}\n",
                &[("S", "M_SW_C"), ("m", "M_SW_M")],
            ),
        ];
        let mut missing = Vec::new();
        for (lang, src, expectations) in cases {
            let nodes = match parse_code(src, lang) {
                Ok(n) => n,
                Err(e) => {
                    missing.push(format!("{lang}: parse_code failed: {e}"));
                    continue;
                }
            };
            for (symbol, marker) in *expectations {
                let doc = nodes
                    .iter()
                    .find(|n| n.name == *symbol)
                    .and_then(|n| n.doc_comment.clone())
                    .unwrap_or_default();
                if !doc.contains(marker) {
                    missing.push(format!("{lang}/{symbol}: expected {marker}, got {doc:?}"));
                }
            }
        }
        assert!(
            missing.is_empty(),
            "doc_comment lost for {} case(s):\n  {}",
            missing.len(),
            missing.join("\n  ")
        );
    }

    /// Second axis of the same table: a declaration carrying a decorator,
    /// attribute or annotation between it and its documentation.
    ///
    /// Split from the plain-form table above because the two axes fail
    /// independently — every language here passes the plain form, and the
    /// languages that pass this one do so for a reason that has nothing to do
    /// with the extractor: their grammars park the annotation INSIDE the
    /// declaration node (Java/Kotlin/Swift `modifiers`, C#/PHP `attribute_list`),
    /// so the comment stays the immediate previous sibling. The four that failed
    /// put it OUTSIDE, as a sibling of its own (`decorator`, `attribute_item`,
    /// `annotation`) — which is the whole gap. Rows for the already-working
    /// languages are kept so the table shows the entire axis rather than the
    /// half that broke.
    ///
    /// Go and Ruby have no decoration syntax and so have no row.
    #[test]
    fn test_doc_comment_parity_for_decorated_declarations() {
        type DocCase = (
            &'static str,
            &'static str,
            &'static [(&'static str, &'static str)],
        );
        let cases: &[DocCase] = &[
            // Angular/NestJS shape: the decorator is a named child of
            // `export_statement`, ahead of the declaration the extractor reads.
            (
                "typescript",
                "/** M_TS_DC */\n@Component({})\nexport class C {}\n",
                &[("C", "M_TS_DC")],
            ),
            (
                "typescript",
                "/** M_TS_DF */\n@Deco()\nexport function f() {}\n",
                &[("f", "M_TS_DF")],
            ),
            // NestJS controller shape: comment and decorator are both siblings
            // of the method inside `class_body`.
            (
                "typescript",
                "class S {\n  /** M_TS_DM */\n  @Get()\n  findAll() {}\n}\n",
                &[("findAll", "M_TS_DM")],
            ),
            (
                "javascript",
                "/** M_JS_DC */\n@deco\nexport class J {}\n",
                &[("J", "M_JS_DC")],
            ),
            (
                "rust",
                "/// M_RS_DS\n#[derive(Debug)]\npub struct S;\n",
                &[("S", "M_RS_DS")],
            ),
            (
                "rust",
                "impl T {\n    /// M_RS_DM\n    #[inline]\n    fn m(&self) {}\n}\n",
                &[("m", "M_RS_DM")],
            ),
            (
                "dart",
                "/// M_DT_DF\n@override\nvoid f() {}\n",
                &[("f", "M_DT_DF")],
            ),
            // Already working — the annotation lives inside the declaration node.
            (
                "python",
                "# M_PY_DF\n@dec\ndef f():\n    pass\n",
                &[("f", "M_PY_DF")],
            ),
            (
                "java",
                "class C {\n  /** M_JV_DM */\n  @Override\n  public void m() {}\n}\n",
                &[("m", "M_JV_DM")],
            ),
            (
                "kotlin",
                "/** M_KT_DF */\n@Deprecated\nfun f() {}\n",
                &[("f", "M_KT_DF")],
            ),
            (
                "csharp",
                "/// M_CS_DC\n[Obsolete]\npublic class C {}\n",
                &[("C", "M_CS_DC")],
            ),
            (
                "php",
                "<?php\n/** M_PHP_DC */\n#[Attr]\nclass K {}\n",
                &[("K", "M_PHP_DC")],
            ),
            (
                "swift",
                "/// M_SW_DF\n@objc func f() {}\n",
                &[("f", "M_SW_DF")],
            ),
        ];
        let mut missing = Vec::new();
        for (lang, src, expectations) in cases {
            let nodes = match parse_code(src, lang) {
                Ok(n) => n,
                Err(e) => {
                    missing.push(format!("{lang}: parse_code failed: {e}"));
                    continue;
                }
            };
            for (symbol, marker) in *expectations {
                let doc = nodes
                    .iter()
                    .find(|n| n.name == *symbol)
                    .and_then(|n| n.doc_comment.clone())
                    .unwrap_or_default();
                if !doc.contains(marker) {
                    missing.push(format!("{lang}/{symbol}: expected {marker}, got {doc:?}"));
                }
            }
        }
        assert!(
            missing.is_empty(),
            "doc_comment lost for {} decorated case(s):\n  {}",
            missing.len(),
            missing.join("\n  ")
        );
    }

    /// A Rust `//!` block documents the module that CONTAINS it, so it must not
    /// become the doc of the module's first declaration.
    ///
    /// Both arms matter and they fail for different reasons. The undecorated one
    /// is the older bug — the sibling walk cannot tell `//!` from `///` by node
    /// kind, both being `line_comment`. The decorated one is what made it worth
    /// fixing now: stepping over `attribute_item` extended the mis-attribution to
    /// `//!` header + `#[derive(…)]` + type, which is the ordinary Rust file
    /// layout. The `///` arm is the positive control — the fix must not silence
    /// real doc comments.
    #[test]
    fn test_inner_module_doc_is_not_a_declarations_doc() {
        let doc_of = |src: &str| {
            parse_code(src, "rust")
                .unwrap()
                .iter()
                .find(|n| n.name == "S")
                .and_then(|n| n.doc_comment.clone())
                .unwrap_or_default()
        };
        assert!(
            doc_of("//! module doc\npub struct S;\n").is_empty(),
            "an inner doc comment must not document the next item, got {:?}",
            doc_of("//! module doc\npub struct S;\n")
        );
        assert!(
            doc_of("//! module doc\n#[derive(Debug)]\npub struct S;\n").is_empty(),
            "the decoration skip must not carry an inner doc comment across, got {:?}",
            doc_of("//! module doc\n#[derive(Debug)]\npub struct S;\n")
        );
        assert!(
            doc_of("/*! block module doc */\npub struct S;\n").is_empty(),
            "the block spelling of an inner doc must behave the same, got {:?}",
            doc_of("/*! block module doc */\npub struct S;\n")
        );
        assert!(
            doc_of("/// item doc\n#[derive(Debug)]\npub struct S;\n").contains("item doc"),
            "a real outer doc comment must still reach its item"
        );
    }

    /// Negative control for the decoration skip: stepping over an attribute must
    /// not let a declaration reach past whatever sits behind it. Both shapes are
    /// ones the skip could plausibly break — an attributed item following an
    /// undocumented sibling, and one following a DOCUMENTED sibling whose
    /// comment must stay with its own declaration.
    #[test]
    fn test_decoration_skip_does_not_cross_a_declaration() {
        let src = "/// DOC_FOR_A\nfn a() {}\n#[derive(Debug)]\npub struct S;\n";
        let nodes = parse_code(src, "rust").unwrap();
        let doc_of = |name: &str| {
            nodes
                .iter()
                .find(|n| n.name == name)
                .and_then(|n| n.doc_comment.clone())
                .unwrap_or_default()
        };
        assert!(
            doc_of("a").contains("DOC_FOR_A"),
            "the documented function keeps its own doc, got {:?}",
            doc_of("a")
        );
        assert!(
            doc_of("S").is_empty(),
            "an attributed struct must not inherit the previous declaration's doc, got {:?}",
            doc_of("S")
        );
    }

    /// The wrapper climb's first-named-child check, pinned by the one shape the
    /// pre-tag review found that actually exercises it: a Go grouped `type (…)`
    /// declaration carries ONE doc comment above the group, and only the first
    /// spec may claim it. Removing the check makes `Beta` claim it too.
    #[test]
    fn test_wrapper_climb_skips_later_group_members() {
        let src = "package p\n\n// GROUP_DOC\ntype (\n\tAlpha struct{}\n\tBeta struct{}\n)\n";
        let nodes = parse_code(src, "go").unwrap();
        let doc_of = |name: &str| -> String {
            nodes
                .iter()
                .find(|n| n.name == name)
                .unwrap_or_else(|| panic!("{name} must be extracted"))
                .doc_comment
                .clone()
                .unwrap_or_default()
        };
        assert!(
            doc_of("Alpha").contains("GROUP_DOC"),
            "the first spec in the group carries the group's doc, got {:?}",
            doc_of("Alpha")
        );
        assert!(
            doc_of("Beta").is_empty(),
            "a later spec must NOT inherit the group's doc comment, got {:?}",
            doc_of("Beta")
        );
    }

    /// A comment trailing code documents its own line, not the next declaration.
    /// Widening the sibling walk to wrappers made this reachable: the Go case is
    /// a regression that shipped in an earlier draft of this change, the Ruby one
    /// put a `class X # note` comment on the class's first method.
    #[test]
    fn test_trailing_comment_is_not_a_doc_comment() {
        let go = "package b\n\n// DOC_F is F.\nfunc F() {} // trailing on F\ntype AfterTrailing struct{}\n";
        let nodes = parse_code(go, "go").unwrap();
        let go_doc = |name: &str| -> String {
            nodes
                .iter()
                .find(|n| n.name == name)
                .unwrap_or_else(|| panic!("{name} must be extracted"))
                .doc_comment
                .clone()
                .unwrap_or_default()
        };
        assert!(
            go_doc("F").contains("DOC_F"),
            "positive half: an own-line comment is still a doc, got {:?}",
            go_doc("F")
        );
        assert!(
            go_doc("AfterTrailing").is_empty(),
            "a trailing comment must not document the next declaration, got {:?}",
            go_doc("AfterTrailing")
        );

        let ruby = "class Inline # trailing on the class line\n  def firstm\n    1\n  end\nend\n";
        let rb = parse_code(ruby, "ruby").unwrap();
        let rb_doc = rb
            .iter()
            .find(|n| n.name == "firstm")
            .expect("firstm must be extracted")
            .doc_comment
            .clone()
            .unwrap_or_default();
        assert!(
            rb_doc.is_empty(),
            "a comment trailing `class Inline` must not document its first method, got {rb_doc:?}"
        );
    }

    /// A Python docstring must win over a preceding comment: a file-level
    /// license or lint header sits adjacent to the first `def` in most real
    /// files, and reading it as that function's documentation puts a copyright
    /// notice in the `doc:` slot the embedding builder ranks above `code:`.
    #[test]
    fn test_python_docstring_beats_a_preceding_header_comment() {
        let src = "\
# Copyright 2020 Example Corp.
# Licensed under the Apache License.

def main():
    \"\"\"DOC_RUNS_THE_THING.\"\"\"
    return 1

def undocumented():
    return 2
";
        let nodes = parse_code(src, "python").unwrap();
        let doc_of = |name: &str| -> String {
            nodes
                .iter()
                .find(|n| n.name == name)
                .unwrap_or_else(|| panic!("{name} must be extracted"))
                .doc_comment
                .clone()
                .unwrap_or_default()
        };
        assert!(
            doc_of("main").contains("DOC_RUNS_THE_THING"),
            "the docstring must win over the license header, got {:?}",
            doc_of("main")
        );
        assert!(
            !doc_of("main").contains("Copyright"),
            "the license header must not survive into doc_comment, got {:?}",
            doc_of("main")
        );
        // Negative control: with no docstring the comment channel still applies,
        // so this is not "Python ignores comments now".
        assert!(
            doc_of("undocumented").is_empty(),
            "a def with neither docstring nor adjacent comment stays empty, got {:?}",
            doc_of("undocumented")
        );
    }

    #[test]
    fn test_short_preceding_comment_is_untouched() {
        // Negative control: capping must not rewrite ordinary doc comments.
        // Without this, truncating everything to zero would pass the test above.
        let src = "/// Adds two numbers.\npub fn add(a: i32, b: i32) -> i32 { a + b }\n";
        let nodes = parse_code(src, "rust").unwrap();
        let doc = nodes
            .iter()
            .find(|n| n.name == "add")
            .and_then(|n| n.doc_comment.clone())
            .expect("add must carry its doc comment");
        // Byte-identical to the pre-cap behavior, trailing newline included.
        assert_eq!(doc, "/// Adds two numbers.\n");
    }
}
