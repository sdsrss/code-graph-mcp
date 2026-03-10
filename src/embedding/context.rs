pub struct NodeContext {
    pub node_type: String,
    pub name: String,
    pub file_path: String,
    pub signature: Option<String>,
    pub routes: Vec<String>,
    pub callees: Vec<String>,
    pub callers: Vec<String>,
    pub inherits: Vec<String>,
    pub imports: Vec<String>,
    pub doc_comment: Option<String>,
}

pub fn build_context_string(info: &NodeContext) -> String {
    let mut parts = vec![format!("{} {}", info.node_type, info.name)];
    parts.push(format!("in {}", info.file_path));
    if let Some(sig) = &info.signature {
        parts.push(format!("signature: {}", sig));
    }
    if !info.routes.is_empty() {
        parts.push(format!("routes: {}", info.routes.join(", ")));
    }
    if !info.callees.is_empty() {
        parts.push(format!("calls: {}", info.callees.join(", ")));
    }
    if !info.callers.is_empty() {
        parts.push(format!("called_by: {}", info.callers.join(", ")));
    }
    if !info.inherits.is_empty() {
        parts.push(format!("inherits: {}", info.inherits.join(", ")));
    }
    if !info.imports.is_empty() {
        parts.push(format!("imports: {}", info.imports.join(", ")));
    }
    if let Some(doc) = &info.doc_comment {
        parts.push(format!("doc: {}", doc));
    }
    parts.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_build_context_string() {
        let info = NodeContext {
            node_type: "function".into(),
            name: "validateToken".into(),
            file_path: "src/auth/middleware.ts".into(),
            signature: Some("(token: string) -> Promise<User | null>".into()),
            routes: vec!["POST /api/login".into(), "GET /api/profile".into()],
            callees: vec!["jwt.verify".into(), "UserRepo.findById".into()],
            callers: vec!["authMiddleware".into(), "handleLogin".into()],
            inherits: vec![],
            imports: vec!["jwt".into(), "UserRepo".into()],
            doc_comment: Some("Validates JWT token and returns the associated user".into()),
        };

        let ctx = build_context_string(&info);
        assert!(ctx.contains("function validateToken"));
        assert!(ctx.contains("in src/auth/middleware.ts"));
        assert!(ctx.contains("calls: jwt.verify, UserRepo.findById"));
        assert!(ctx.contains("called_by: authMiddleware, handleLogin"));
        assert!(ctx.contains("routes: POST /api/login, GET /api/profile"));
        assert!(ctx.contains("imports: jwt, UserRepo"));
    }

    #[test]
    fn test_build_context_string_minimal() {
        let info = NodeContext {
            node_type: "function".into(),
            name: "helper".into(),
            file_path: "utils.ts".into(),
            signature: None,
            routes: vec![],
            callees: vec![],
            callers: vec![],
            inherits: vec![],
            imports: vec![],
            doc_comment: None,
        };

        let ctx = build_context_string(&info);
        assert!(ctx.contains("function helper"));
        assert!(ctx.contains("in utils.ts"));
        assert!(!ctx.contains("calls:"));
        assert!(!ctx.contains("routes:"));
    }
}
