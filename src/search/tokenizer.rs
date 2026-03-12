/// Split identifiers into searchable tokens.
/// "validateToken" → "validate Token validateToken"
/// "PascalCase" → "Pascal Case PascalCase"
/// "get_user_by_id" → "get user by id get_user_by_id"
/// "HTMLParser" → "HTML Parser HTMLParser"
/// "x" → "x"
pub fn split_identifier(name: &str) -> String {
    let mut parts: Vec<String> = Vec::new();
    let mut current = String::new();

    let chars: Vec<char> = name.chars().collect();
    let len = chars.len();

    let mut i = 0;
    while i < len {
        let c = chars[i];

        if c == '_' {
            if !current.is_empty() {
                parts.push(std::mem::take(&mut current));
            }
            i += 1;
            continue;
        }

        if c.is_uppercase() && !current.is_empty() {
            let last_lower = current.chars().last().is_some_and(|lc| lc.is_lowercase());
            let acronym_end = current.chars().last().is_some_and(|lc| lc.is_uppercase())
                && i + 1 < len
                && chars[i + 1].is_lowercase();
            if last_lower || acronym_end {
                parts.push(std::mem::take(&mut current));
            }
        }

        current.push(c);
        i += 1;
    }

    if !current.is_empty() {
        parts.push(current);
    }

    if parts.len() <= 1 {
        return name.to_string();
    }

    let mut result = parts.join(" ");
    result.push(' ');
    result.push_str(name);
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_camel_case() {
        assert_eq!(split_identifier("validateToken"), "validate Token validateToken");
    }

    #[test]
    fn test_pascal_case() {
        assert_eq!(split_identifier("PascalCase"), "Pascal Case PascalCase");
    }

    #[test]
    fn test_snake_case() {
        assert_eq!(split_identifier("get_user_by_id"), "get user by id get_user_by_id");
    }

    #[test]
    fn test_acronym_then_pascal() {
        assert_eq!(split_identifier("HTMLParser"), "HTML Parser HTMLParser");
    }

    #[test]
    fn test_single_word() {
        assert_eq!(split_identifier("hello"), "hello");
    }

    #[test]
    fn test_single_char() {
        assert_eq!(split_identifier("x"), "x");
    }

    #[test]
    fn test_all_upper() {
        assert_eq!(split_identifier("MAX_SIZE"), "MAX SIZE MAX_SIZE");
    }
}
