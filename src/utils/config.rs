pub fn detect_language(path: &str) -> Option<&'static str> {
    let p = std::path::Path::new(path);
    // Skip dotfiles (e.g., .gitignore) - file_stem is empty for dotfiles
    let stem = p.file_stem()?.to_str()?;
    if stem.is_empty() {
        return None;
    }
    let ext = p.extension()?.to_str()?;
    match ext {
        "rs" => Some("rust"),
        "ts" => Some("typescript"),
        "tsx" => Some("tsx"),
        "js" | "jsx" | "mjs" | "cjs" => Some("javascript"),
        "go" => Some("go"),
        "py" | "pyi" => Some("python"),
        "java" => Some("java"),
        "c" | "h" => Some("c"),
        "cpp" | "cc" | "cxx" | "hpp" => Some("cpp"),
        "html" | "htm" => Some("html"),
        "css" => Some("css"),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_detect_language_from_extension() {
        assert_eq!(detect_language("src/main.rs"), Some("rust"));
        assert_eq!(detect_language("app.ts"), Some("typescript"));
        assert_eq!(detect_language("app.tsx"), Some("tsx"));
        assert_eq!(detect_language("index.js"), Some("javascript"));
        assert_eq!(detect_language("main.go"), Some("go"));
        assert_eq!(detect_language("app.py"), Some("python"));
        assert_eq!(detect_language("Main.java"), Some("java"));
        assert_eq!(detect_language("main.c"), Some("c"));
        assert_eq!(detect_language("main.cpp"), Some("cpp"));
        assert_eq!(detect_language("index.html"), Some("html"));
        assert_eq!(detect_language("style.css"), Some("css"));
        assert_eq!(detect_language("image.png"), None);
    }

    #[test]
    fn test_detect_language_edge_cases() {
        assert_eq!(detect_language("Makefile"), None);
        assert_eq!(detect_language(".gitignore"), None);
        assert_eq!(detect_language("file.test.ts"), Some("typescript"));
        assert_eq!(detect_language("path/to/no_ext"), None);
    }
}
