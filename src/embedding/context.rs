pub struct NodeContext {
    pub node_type: String,
    pub name: String,
    pub file_path: String,
    pub signature: Option<String>,
    pub code_content: Option<String>,
    pub routes: Vec<String>,
    pub callees: Vec<String>,
    pub callers: Vec<String>,
    pub inherits: Vec<String>,
    pub imports: Vec<String>,
    pub implements: Vec<String>,
    pub exports: Vec<String>,
    pub doc_comment: Option<String>,
}

pub fn build_context_string(info: &NodeContext) -> String {
    let mut parts = Vec::new();

    // 1. Signature (always short, high value)
    if let Some(sig) = &info.signature {
        parts.push(format!("signature: {}", sig));
    }

    // 2. Code content (truncated to fit 512-token budget)
    if let Some(code) = &info.code_content {
        parts.push(format!("code: {}", code));
    }

    // 3. Doc comment
    if let Some(doc) = &info.doc_comment {
        parts.push(format!("doc: {}", doc));
    }

    // 4. Identity: type + name + file
    parts.push(format!("{} {}", info.node_type, info.name));
    parts.push(format!("in {}", info.file_path));

    // 5. Graph relations (fill remaining space, truncated last)
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
    if !info.implements.is_empty() {
        parts.push(format!("implements: {}", info.implements.join(", ")));
    }
    if !info.exports.is_empty() {
        parts.push(format!("exports: {}", info.exports.join(", ")));
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
            code_content: Some("function validateToken(token: string) { return jwt.verify(token); }".into()),
            routes: vec!["POST /api/login".into(), "GET /api/profile".into()],
            callees: vec!["jwt.verify".into(), "UserRepo.findById".into()],
            callers: vec!["authMiddleware".into(), "handleLogin".into()],
            inherits: vec![],
            imports: vec!["jwt".into(), "UserRepo".into()],
            implements: vec![],
            exports: vec![],
            doc_comment: Some("Validates JWT token and returns the associated user".into()),
        };

        let ctx = build_context_string(&info);
        assert!(ctx.contains("function validateToken"));
        assert!(ctx.contains("in src/auth/middleware.ts"));
        assert!(ctx.contains("calls: jwt.verify, UserRepo.findById"));
        assert!(ctx.contains("called_by: authMiddleware, handleLogin"));
        assert!(ctx.contains("routes: POST /api/login, GET /api/profile"));
        assert!(ctx.contains("imports: jwt, UserRepo"));
        assert!(ctx.contains("code: function validateToken(token: string)"));
    }

    #[test]
    fn test_context_string_code_before_graph() {
        let info = NodeContext {
            node_type: "function".into(),
            name: "handler".into(),
            file_path: "api.ts".into(),
            signature: Some("(req: Request) -> Response".into()),
            code_content: Some("function handler(req: Request) { return ok(); }".into()),
            routes: vec![],
            callees: vec!["ok".into()],
            callers: vec!["router".into()],
            inherits: vec![],
            imports: vec![],
            implements: vec![],
            exports: vec![],
            doc_comment: Some("Handles requests".into()),
        };
        let ctx = build_context_string(&info);
        let sig_pos = ctx.find("signature:").unwrap();
        let code_pos = ctx.find("code:").unwrap();
        let identity_pos = ctx.find("function handler").unwrap();
        let calls_pos = ctx.find("calls:").unwrap();
        // Per spec 2.3: signature → code → doc → identity → graph relations
        assert!(sig_pos < code_pos, "signature before code");
        assert!(code_pos < identity_pos, "code before identity");
        assert!(identity_pos < calls_pos, "identity before calls");
    }

    #[test]
    fn test_build_context_string_minimal() {
        let info = NodeContext {
            node_type: "function".into(),
            name: "helper".into(),
            file_path: "utils.ts".into(),
            signature: None,
            code_content: None,
            routes: vec![],
            callees: vec![],
            callers: vec![],
            inherits: vec![],
            imports: vec![],
            implements: vec![],
            exports: vec![],
            doc_comment: None,
        };

        let ctx = build_context_string(&info);
        assert!(ctx.contains("function helper"));
        assert!(ctx.contains("in utils.ts"));
        assert!(!ctx.contains("calls:"));
        assert!(!ctx.contains("routes:"));
    }
}
