use anyhow::{anyhow, Result};
use std::borrow::Cow;
use std::cell::RefCell;
use std::collections::HashMap;
use super::languages::get_language;
use super::node_text;
use crate::domain::{MAX_AST_DEPTH, max_code_content_len, parse_timeout_ms};

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
    let lang = get_language(language)
        .ok_or_else(|| anyhow!("unsupported language: {}", language))?;

    PARSER_CACHE.with(|cache| {
        let mut cache = cache.borrow_mut();
        if !cache.contains_key(language) {
            let mut p = tree_sitter::Parser::new();
            p.set_timeout_micros(parse_timeout_ms() * 1000);
            p.set_language(&lang)?;
            cache.insert(language.to_string(), p);
        }
        let parser = cache.get_mut(language)
            .ok_or_else(|| anyhow!("parser cache inconsistency for {}", language))?;
        parser.parse(source, None)
            .ok_or_else(|| anyhow!("parse failed or timed out"))
    })
}

pub fn parse_code(source: &str, language: &str) -> Result<Vec<ParsedNode>> {
    let tree = parse_tree(source, language)?;
    Ok(extract_nodes_from_tree(&tree, source, language))
}

/// Extract nodes from a pre-parsed tree (avoids re-parsing).
pub fn extract_nodes_from_tree(tree: &tree_sitter::Tree, source: &str, language: &str) -> Vec<ParsedNode> {
    let mut nodes = Vec::new();
    extract_nodes(tree.root_node(), source, language, None, &mut nodes, 0, false);
    nodes
}

/// Check if a node has a preceding `#[cfg(test)]` or `#[test]` attribute.
fn has_test_attribute(node: &tree_sitter::Node, source: &str) -> bool {
    let mut sibling = node.prev_sibling();
    while let Some(s) = sibling {
        match s.kind() {
            "attribute_item" | "inner_attribute_item" => {
                let text = node_text(&s, source);
                if text.contains("cfg(test)") || text == "#[test]" {
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

fn extract_nodes(
    node: tree_sitter::Node,
    source: &str,
    language: &str,
    parent_class: Option<&str>,
    results: &mut Vec<ParsedNode>,
    depth: usize,
    in_test_context: bool,
) {
    if depth > MAX_AST_DEPTH { return; }
    let kind = node.kind();

    // Detect Rust mod items (e.g., `mod tests { ... }`)
    if kind == "mod_item" {
        let mod_name = node.child_by_field_name("name")
            .map(|n| node_text(&n, source).to_string());
        let is_test_mod = mod_name.as_deref() == Some("tests")
            || has_test_attribute(&node, source);
        // Recurse into the module body with updated test context
        if let Some(body) = node.child_by_field_name("body") {
            for i in 0..body.named_child_count() {
                if let Some(child) = body.named_child(i) {
                    extract_nodes(child, source, language, parent_class, results, depth + 1,
                        in_test_context || is_test_mod);
                }
            }
        }
        return;
    }

    // Check if this specific node has #[test] or #[cfg(test)]
    let node_is_test = in_test_context || has_test_attribute(&node, source);

    match kind {
        // Functions: shared across TS/JS/Go (function_declaration), Python/C/C++ (function_definition)
        "function_declaration" | "function" => {
            if let Some(mut parsed) = extract_function_node(&node, source, "function", parent_class) {
                parsed.is_test = node_is_test;
                results.push(parsed);
            }
        }
        // Python async functions
        "async_function_definition" => {
            let nt = if parent_class.is_some() { "method" } else { "function" };
            if let Some(mut parsed) = extract_function_node(&node, source, nt, parent_class) {
                parsed.is_test = node_is_test;
                results.push(parsed);
            }
        }

        "function_definition" => {
            if language == "c" || language == "cpp" {
                // C/C++: name is in declarator child, not name field
                if let Some(declarator) = node.child_by_field_name("declarator") {
                    if let Some(name) = extract_declarator_name(&declarator, source) {
                        let sig_info = extract_c_signature_info(&node, source);
                        results.push(ParsedNode {
                            node_type: "function".into(),
                            name: name.clone(),
                            qualified_name: Some(name),
                            start_line: node.start_position().row as u32 + 1,
                            end_line: node.end_position().row as u32 + 1,
                            code_content: truncate_code_content(node_text(&node, source)).into_owned(),
                            signature: sig_info.signature,
                            doc_comment: get_preceding_comment(&node, source),
                            return_type: sig_info.return_type,
                            param_types: sig_info.param_types,
                            is_test: node_is_test,
                        });
                    }
                }
            } else {
                // Python and others: name is in "name" field
                let nt = if parent_class.is_some() { "method" } else { "function" };
                if let Some(mut parsed) = extract_function_node(&node, source, nt, parent_class) {
                    parsed.is_test = node_is_test;
                    results.push(parsed);
                }
            }
        }
        "function_item" => {
            // Rust functions
            if let Some(mut parsed) = extract_function_node(&node, source, "function", parent_class) {
                parsed.is_test = node_is_test;
                results.push(parsed);
            }
        }

        // Arrow functions (TS/JS): covers const/let (lexical) and var (variable)
        "lexical_declaration" | "variable_declaration" => {
            if let Some(mut parsed) = extract_named_arrow(&node, source) {
                parsed.is_test = node_is_test;
                results.push(parsed);
            }
        }

        // Classes: shared across TS/JS/Java (class_declaration), Python (class_definition)
        // Kotlin: both classes and interfaces use class_declaration — distinguish by first child kind
        "class_declaration" | "class" | "class_definition" => {
            if let Some(name) = get_child_by_field(&node, "name", source) {
                // Kotlin interfaces are class_declaration with first child kind "interface"
                let node_type_str = if node.child(0).map(|c| c.kind()) == Some("interface") {
                    "interface"
                } else {
                    "class"
                };
                results.push(ParsedNode {
                    node_type: node_type_str.into(),
                    name: name.clone(),
                    qualified_name: Some(name.clone()),
                    start_line: node.start_position().row as u32 + 1,
                    end_line: node.end_position().row as u32 + 1,
                    code_content: truncate_code_content(node_text(&node, source)).into_owned(),
                    signature: None,
                    doc_comment: get_preceding_comment(&node, source),
                    return_type: None,
                    param_types: None,
                    is_test: node_is_test,
                });
                extract_children(node, source, language, Some(&name), results, depth, node_is_test);
                return;
            }
        }

        // Methods: TS/JS (method_definition), Go/Java (method_declaration), Ruby (method, singleton_method)
        "method_definition" | "method_declaration" => {
            if let Some(mut parsed) = extract_function_node(&node, source, "method", parent_class) {
                parsed.is_test = node_is_test;
                results.push(parsed);
            }
        }
        "method" | "singleton_method" if language == "ruby" => {
            if let Some(mut parsed) = extract_function_node(&node, source, "method", parent_class) {
                parsed.is_test = node_is_test;
                results.push(parsed);
            }
        }

        // Ruby modules — mapped to "interface" type
        "module" if language == "ruby" => {
            if let Some(name) = get_child_by_field(&node, "name", source) {
                results.push(make_simple_node("interface", name.clone(), &node, source, node_is_test));
                extract_children(node, source, language, Some(&name), results, depth, node_is_test);
                return;
            }
        }

        // Interfaces (TS/Java/PHP)
        "interface_declaration" => {
            if let Some(name) = get_child_by_field(&node, "name", source) {
                results.push(make_simple_node("interface", name.clone(), &node, source, node_is_test));
                extract_children(node, source, language, Some(&name), results, depth, node_is_test);
                return;
            }
        }

        // PHP traits — mapped to "interface" type
        "trait_declaration" => {
            if let Some(name) = get_child_by_field(&node, "name", source) {
                results.push(make_simple_node("interface", name.clone(), &node, source, node_is_test));
                extract_children(node, source, language, Some(&name), results, depth, node_is_test);
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
                results.push(make_simple_node("struct", name.clone(), &node, source, node_is_test));
                extract_children(node, source, language, Some(&name), results, depth, node_is_test);
                return;
            }
        }

        // Kotlin object declaration (singleton)
        "object_declaration" => {
            if let Some(name) = get_child_by_field(&node, "name", source) {
                results.push(make_simple_node("class", name.clone(), &node, source, node_is_test));
                extract_children(node, source, language, Some(&name), results, depth, node_is_test);
                return;
            }
        }

        // C# constructor
        "constructor_declaration" => {
            if let Some(mut parsed) = extract_function_node(&node, source, "function", parent_class) {
                parsed.is_test = node_is_test;
                results.push(parsed);
            }
        }

        // C++ class/struct
        "class_specifier" | "struct_specifier" => {
            if let Some(name) = get_child_by_field(&node, "name", source) {
                let nt = if kind == "class_specifier" { "class" } else { "struct" };
                results.push(make_simple_node(nt, name.clone(), &node, source, node_is_test));
                extract_children(node, source, language, Some(&name), results, depth, node_is_test);
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
                                code_content: truncate_code_content(node_text(&child, source)).into_owned(),
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
                results.push(make_simple_node("struct", name, &node, source, node_is_test));
            }
        }
        "enum_item" => {
            if let Some(name) = get_child_by_field(&node, "name", source) {
                results.push(make_simple_node("enum", name, &node, source, node_is_test));
            }
        }
        "impl_item" => {
            if let Some(type_node) = node.child_by_field_name("type") {
                let impl_name = node_text(&type_node, source);
                extract_children(node, source, language, Some(impl_name), results, depth, node_is_test);
                return;
            }
        }
        "trait_item" => {
            if let Some(name) = get_child_by_field(&node, "name", source) {
                results.push(make_simple_node("interface", name.clone(), &node, source, node_is_test));
                extract_children(node, source, language, Some(&name), results, depth, node_is_test);
                return;
            }
        }

        _ => {}
    }

    // Recurse into children
    extract_children(node, source, language, parent_class, results, depth, node_is_test);
}

fn extract_children(
    node: tree_sitter::Node,
    source: &str,
    language: &str,
    parent_class: Option<&str>,
    results: &mut Vec<ParsedNode>,
    depth: usize,
    in_test_context: bool,
) {
    for i in 0..node.named_child_count() {
        if let Some(child) = node.named_child(i) {
            extract_nodes(child, source, language, parent_class, results, depth + 1, in_test_context);
        }
    }
}

fn truncate_code_content(content: &str) -> Cow<'_, str> {
    if content.len() <= max_code_content_len() {
        Cow::Borrowed(content)
    } else {
        let mut end = max_code_content_len();
        while end > 0 && !content.is_char_boundary(end) {
            end -= 1;
        }
        let mut truncated = content[..end].to_string();
        truncated.push_str("...");
        Cow::Owned(truncated)
    }
}

fn make_simple_node(node_type: &str, name: String, node: &tree_sitter::Node, source: &str, is_test: bool) -> ParsedNode {
    ParsedNode {
        node_type: node_type.into(),
        name: name.clone(),
        qualified_name: Some(name),
        start_line: node.start_position().row as u32 + 1,
        end_line: node.end_position().row as u32 + 1,
        code_content: truncate_code_content(node_text(node, source)).into_owned(),
        signature: None,
        doc_comment: get_preceding_comment(node, source),
        return_type: None,
        param_types: None,
        is_test,
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
    let sig_info = extract_signature_info(node, source);

    Some(ParsedNode {
        node_type: node_type.into(),
        name,
        qualified_name,
        start_line: node.start_position().row as u32 + 1,
        end_line: node.end_position().row as u32 + 1,
        code_content: truncate_code_content(node_text(node, source)).into_owned(),
        signature: sig_info.signature,
        doc_comment: get_preceding_comment(node, source),
        return_type: sig_info.return_type,
        param_types: sig_info.param_types,
        is_test: false,
    })
}

fn extract_named_arrow(node: &tree_sitter::Node, source: &str) -> Option<ParsedNode> {
    // lexical_declaration -> variable_declarator -> arrow_function
    for i in 0..node.named_child_count() {
        let child = match node.named_child(i) {
            Some(c) => c,
            None => continue,
        };
        if child.kind() == "variable_declarator" {
            let name = get_child_by_field(&child, "name", source)?;
            let value = child.child_by_field_name("value")?;
            if value.kind() == "arrow_function" {
                let sig_info = extract_signature_info(&value, source);
                return Some(ParsedNode {
                    node_type: "function".into(),
                    name: name.clone(),
                    qualified_name: Some(name),
                    start_line: node.start_position().row as u32 + 1,
                    end_line: node.end_position().row as u32 + 1,
                    code_content: truncate_code_content(node_text(node, source)).into_owned(),
                    signature: sig_info.signature,
                    doc_comment: get_preceding_comment(node, source),
                    return_type: sig_info.return_type,
                    param_types: sig_info.param_types,
                    is_test: false,
                });
            }
        }
    }
    None
}

struct SignatureInfo {
    signature: Option<String>,
    return_type: Option<String>,
    param_types: Option<String>,
}

fn extract_signature_info(node: &tree_sitter::Node, source: &str) -> SignatureInfo {
    let params = node.child_by_field_name("parameters")
        .map(|p| node_text(&p, source).to_string());
    let ret = node.child_by_field_name("return_type")
        .map(|r| node_text(&r, source).to_string());

    let signature = match (&params, &ret) {
        (Some(p), Some(r)) => Some(format!("{} -> {}", p, r)),
        (Some(p), None) => Some(p.clone()),
        _ => None,
    };

    SignatureInfo {
        signature,
        return_type: ret,
        param_types: params,
    }
}

fn extract_c_signature_info(node: &tree_sitter::Node, source: &str) -> SignatureInfo {
    let declarator = match node.child_by_field_name("declarator") {
        Some(d) => d,
        None => return SignatureInfo { signature: None, return_type: None, param_types: None },
    };
    let params = declarator.child_by_field_name("parameters")
        .map(|p| node_text(&p, source).to_string());
    let ret_type = node.child_by_field_name("type")
        .map(|t| node_text(&t, source).to_string());

    let signature = match (&ret_type, &params) {
        (Some(t), Some(p)) => Some(format!("{} {}", t, p)),
        (Some(t), None) => Some(t.clone()),
        (None, Some(p)) => Some(p.clone()),
        _ => None,
    };

    SignatureInfo {
        signature,
        return_type: ret_type,
        param_types: params,
    }
}

fn extract_declarator_name(node: &tree_sitter::Node, source: &str) -> Option<String> {
    extract_declarator_name_inner(node, source, 0)
}

fn extract_declarator_name_inner(node: &tree_sitter::Node, source: &str, depth: usize) -> Option<String> {
    if depth > MAX_AST_DEPTH { return None; }
    // C/C++ function_declarator -> identifier
    if node.kind() == "function_declarator" {
        return get_child_by_field(node, "declarator", source)
            .or_else(|| {
                node.named_child(0).map(|c| node_text(&c, source).to_string())
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

fn get_preceding_comment(node: &tree_sitter::Node, source: &str) -> Option<String> {
    let mut comments = Vec::new();
    let mut current = node.prev_sibling();
    while let Some(prev) = current {
        if prev.kind() == "comment" || prev.kind() == "line_comment" || prev.kind() == "block_comment" {
            comments.push(node_text(&prev, source).to_string());
            current = prev.prev_sibling();
        } else {
            break;
        }
    }
    if comments.is_empty() {
        None
    } else {
        comments.reverse();
        Some(comments.join("\n"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
        assert!(names.contains(&"handleLogin"), "missing handleLogin, got: {:?}", names);
        assert!(names.contains(&"processPayment"), "missing processPayment, got: {:?}", names);
        assert!(names.contains(&"UserService"), "missing UserService, got: {:?}", names);
        assert!(names.contains(&"findUser"), "missing findUser, got: {:?}", names);
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
        let code = "package main\nfunc handleRequest(w http.ResponseWriter, r *http.Request) {\n}\n";
        let nodes = parse_code(code, "go").unwrap();
        assert!(nodes.iter().any(|n| n.name == "handleRequest"), "got: {:?}", nodes.iter().map(|n| &n.name).collect::<Vec<_>>());
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
        let code = "pub fn calculate(x: i32, y: i32) -> i32 { x + y }\nstruct Config { name: String }\n";
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
        assert!(nodes.iter().any(|n| n.name == "main"), "got: {:?}", nodes.iter().map(|n| &n.name).collect::<Vec<_>>());
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
        assert!(names.contains(&"App"), "TSX function with JSX should be parsed, got: {:?}", names);
        assert!(names.contains(&"Container"), "TSX function with complex JSX should be parsed, got: {:?}", names);
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
        assert!(nodes.iter().find(|n| n.name == "Comparable").unwrap().node_type == "interface");
        assert!(nodes.iter().find(|n| n.name == "Color").unwrap().node_type == "enum");
    }

    #[test]
    fn test_parse_cpp_class_and_struct() {
        let code = "class MyClass {\npublic:\n    void doSomething() {}\n};\nstruct Point {\n    int x;\n    int y;\n};\n";
        let nodes = parse_code(code, "cpp").unwrap();
        let names: Vec<&str> = nodes.iter().map(|n| n.name.as_str()).collect();
        assert!(names.contains(&"MyClass"), "got: {:?}", names);
        assert!(names.contains(&"Point"), "got: {:?}", names);
        assert!(nodes.iter().find(|n| n.name == "MyClass").unwrap().node_type == "class");
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
        assert_eq!(greet.return_type.as_deref(), Some(": string"));

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
}
