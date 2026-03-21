pub fn detect_language(path: &str) -> Option<&'static str> {
    let p = std::path::Path::new(path);
    // file_stem() returns None for paths without a filename component;
    // dotfiles like ".gitignore" are filtered by extension() returning None.
    let _stem = p.file_stem()?.to_str()?;
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
        "cs" => Some("csharp"),
        "kt" | "kts" => Some("kotlin"),
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
        assert_eq!(detect_language("Program.cs"), Some("csharp"));
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
