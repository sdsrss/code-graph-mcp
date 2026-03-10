pub mod languages;
pub mod treesitter;
pub mod relations;

/// Safely extract the text corresponding to a tree-sitter node from the source string.
/// Returns `""` if the byte range is out of bounds.
pub fn node_text<'a>(node: &tree_sitter::Node, source: &'a str) -> &'a str {
    source.get(node.byte_range()).unwrap_or("")
}
