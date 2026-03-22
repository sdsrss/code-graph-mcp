/// Per-language configuration for AST parsing and relation extraction.
/// Centralizes language-specific flags that were previously scattered as
/// inline `if language == "X"` guards in treesitter.rs and relations.rs.
pub struct LanguageConfig {
    /// Language name (e.g., "rust", "typescript")
    pub name: &'static str,
    /// Whether this language has test attributes (e.g., Rust #[test])
    pub has_test_attributes: bool,
    /// AST node kind for method signatures (e.g., "method_signature" for Dart)
    pub method_signature_kind: Option<&'static str>,
    /// Whether to check prev_sibling for method definitions (Dart pattern)
    pub method_via_sibling: bool,
    /// Whether function_body nodes contain method definitions (Dart)
    pub function_body_has_methods: bool,
    /// AST node kind for call expressions (e.g., "call" for Ruby vs "call_expression" for most)
    pub call_node_kind: &'static str,
    /// Whether class context should be propagated for scope qualification
    pub has_class_context: bool,
    /// Interface naming convention detection (C# IFoo pattern)
    pub interface_by_prefix: bool,
}

impl LanguageConfig {
    pub fn for_language(language: &str) -> Self {
        match language {
            "rust" => Self {
                name: "rust",
                has_test_attributes: true,
                method_signature_kind: None,
                method_via_sibling: false,
                function_body_has_methods: false,
                call_node_kind: "call_expression",
                has_class_context: false,
                interface_by_prefix: false,
            },
            "dart" => Self {
                name: "dart",
                has_test_attributes: false,
                method_signature_kind: Some("method_signature"),
                method_via_sibling: true,
                function_body_has_methods: true,
                call_node_kind: "call_expression",
                has_class_context: true,
                interface_by_prefix: false,
            },
            "ruby" => Self {
                name: "ruby",
                has_test_attributes: false,
                method_signature_kind: None,
                method_via_sibling: false,
                function_body_has_methods: false,
                call_node_kind: "call",
                has_class_context: true,
                interface_by_prefix: false,
            },
            "csharp" => Self {
                name: "csharp",
                has_test_attributes: false,
                method_signature_kind: None,
                method_via_sibling: false,
                function_body_has_methods: false,
                call_node_kind: "invocation_expression",
                has_class_context: true,
                interface_by_prefix: true,
            },
            // Default config for most languages — enumerate known names for 'static lifetime,
            // fall through to "unknown" for unsupported languages.
            other => {
                let static_name = match other {
                    "typescript" => "typescript",
                    "javascript" => "javascript",
                    "go" => "go",
                    "python" => "python",
                    "java" => "java",
                    "c" => "c",
                    "cpp" => "cpp",
                    "kotlin" => "kotlin",
                    "php" => "php",
                    "swift" => "swift",
                    "html" => "html",
                    "css" => "css",
                    _ => "unknown",
                };
                Self {
                    name: static_name,
                    has_test_attributes: false,
                    method_signature_kind: None,
                    method_via_sibling: false,
                    function_body_has_methods: false,
                    call_node_kind: "call_expression",
                    has_class_context: true,
                    interface_by_prefix: false,
                }
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_rust_config() {
        let config = LanguageConfig::for_language("rust");
        assert_eq!(config.name, "rust");
        assert!(config.has_test_attributes);
        assert!(config.method_signature_kind.is_none());
        assert!(!config.method_via_sibling);
        assert!(!config.function_body_has_methods);
        assert_eq!(config.call_node_kind, "call_expression");
        assert!(!config.has_class_context);
        assert!(!config.interface_by_prefix);
    }

    #[test]
    fn test_dart_config() {
        let config = LanguageConfig::for_language("dart");
        assert_eq!(config.name, "dart");
        assert!(!config.has_test_attributes);
        assert_eq!(config.method_signature_kind, Some("method_signature"));
        assert!(config.method_via_sibling);
        assert!(config.function_body_has_methods);
        assert_eq!(config.call_node_kind, "call_expression");
        assert!(config.has_class_context);
        assert!(!config.interface_by_prefix);
    }

    #[test]
    fn test_ruby_config() {
        let config = LanguageConfig::for_language("ruby");
        assert_eq!(config.name, "ruby");
        assert!(!config.has_test_attributes);
        assert!(config.method_signature_kind.is_none());
        assert!(!config.method_via_sibling);
        assert!(!config.function_body_has_methods);
        assert_eq!(config.call_node_kind, "call");
        assert!(config.has_class_context);
        assert!(!config.interface_by_prefix);
    }

    #[test]
    fn test_csharp_config() {
        let config = LanguageConfig::for_language("csharp");
        assert_eq!(config.name, "csharp");
        assert!(!config.has_test_attributes);
        assert!(config.method_signature_kind.is_none());
        assert!(!config.method_via_sibling);
        assert!(!config.function_body_has_methods);
        assert_eq!(config.call_node_kind, "invocation_expression");
        assert!(config.has_class_context);
        assert!(config.interface_by_prefix);
    }

    #[test]
    fn test_default_config_typescript() {
        let config = LanguageConfig::for_language("typescript");
        assert_eq!(config.name, "typescript");
        assert!(!config.has_test_attributes);
        assert!(config.method_signature_kind.is_none());
        assert!(!config.method_via_sibling);
        assert!(!config.function_body_has_methods);
        assert_eq!(config.call_node_kind, "call_expression");
        assert!(config.has_class_context);
        assert!(!config.interface_by_prefix);
    }

    #[test]
    fn test_default_config_unknown_language() {
        let config = LanguageConfig::for_language("haskell");
        assert_eq!(config.name, "unknown");
        assert!(!config.has_test_attributes);
        assert_eq!(config.call_node_kind, "call_expression");
    }
}
