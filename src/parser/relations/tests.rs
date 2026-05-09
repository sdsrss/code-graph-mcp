use super::*;
use crate::domain::{REL_ROUTES_TO, REL_EXPORTS};

#[test]
fn test_extract_bash_call_relations() {
    let code = r#"#!/usr/bin/env bash

run_pipeline() {
    fetch_data "$1"
    transform_records
    /usr/bin/cat report.txt
    ./scripts/finalize.sh
    : noop
    [ -f /tmp/lock ] && exit 1
    echo "$RESULT"
    foo$VAR something
    $(dynamic_cmd)
}
"#;
    let relations = extract_relations(code, "bash").unwrap();
    let calls: Vec<&str> = relations.iter()
        .filter(|r| r.relation == REL_CALLS && r.source_name == "run_pipeline")
        .map(|r| r.target_name.as_str())
        .collect();
    // Static, identifier-shaped callees → emitted (path prefix stripped).
    assert!(calls.contains(&"fetch_data"), "missing fetch_data, got: {:?}", calls);
    assert!(calls.contains(&"transform_records"), "missing transform_records, got: {:?}", calls);
    assert!(calls.contains(&"cat"), "missing cat (path prefix stripped), got: {:?}", calls);
    assert!(calls.contains(&"finalize.sh"), "missing finalize.sh (./prefix stripped), got: {:?}", calls);
    assert!(calls.contains(&"echo"), "missing echo, got: {:?}", calls);
    // Non-static / non-identifier-shaped → skipped.
    assert!(!calls.contains(&":"), "':' should be skipped, got: {:?}", calls);
    assert!(!calls.contains(&"["), "'[' test command should be skipped, got: {:?}", calls);
    assert!(!calls.iter().any(|c| c.contains('$')),
        "variable expansions / substitutions should be skipped, got: {:?}", calls);
}

#[test]
fn test_extract_c_include_imports() {
    let code = "#include \"local/utils.h\"\n#include <stdio.h>\n#include \"helpers.hpp\"\n\nint main() { return 0; }\n";
    let relations = extract_relations(code, "c").unwrap();
    let imports: Vec<&str> = relations.iter()
        .filter(|r| r.relation == REL_IMPORTS)
        .map(|r| r.target_name.as_str())
        .collect();
    assert!(imports.contains(&"utils"),
        "C: missing utils (.h stripped, path stripped), got: {:?}", imports);
    assert!(imports.contains(&"stdio"),
        "C: missing stdio (system_lib_string), got: {:?}", imports);
    assert!(imports.contains(&"helpers"),
        "C: missing helpers (.hpp stripped), got: {:?}", imports);
    let import_sources: Vec<&str> = relations.iter()
        .filter(|r| r.relation == REL_IMPORTS)
        .map(|r| r.source_name.as_str())
        .collect();
    assert!(import_sources.iter().all(|s| *s == "<module>"),
        "all C #include sources should be <module>, got: {:?}", import_sources);
}

#[test]
fn test_extract_cpp_include_imports() {
    let code = "#include <vector>\n#include \"my/header.hpp\"\n\nint main() { return 0; }\n";
    let relations = extract_relations(code, "cpp").unwrap();
    let imports: Vec<&str> = relations.iter()
        .filter(|r| r.relation == REL_IMPORTS)
        .map(|r| r.target_name.as_str())
        .collect();
    assert!(imports.contains(&"vector"),
        "C++: missing vector (system header, no extension), got: {:?}", imports);
    assert!(imports.contains(&"header"),
        "C++: missing header (.hpp stripped + path stripped), got: {:?}", imports);
}

#[test]
fn test_extract_bash_source_imports() {
    let code = r#"#!/usr/bin/env bash
source ./lib/utils.sh
source "/etc/profile.d/lang.sh"
source 'helpers.bash'
. ~/.bashrc
. /usr/local/etc/init
source $HOME/dynamic.sh
source "${LIB_DIR}/runtime.sh"

bootstrap() {
    source ./conditional/feature.sh
    fetch_data
}
"#;
    let relations = extract_relations(code, "bash").unwrap();
    let imports: Vec<&str> = relations.iter()
        .filter(|r| r.relation == REL_IMPORTS)
        .map(|r| r.target_name.as_str())
        .collect();
    // Static, .sh-stripped, path-stripped targets.
    assert!(imports.contains(&"utils"), "missing utils, got: {:?}", imports);
    assert!(imports.contains(&"lang"), "missing lang (double-quoted), got: {:?}", imports);
    assert!(imports.contains(&"helpers"),
        "missing helpers (single-quoted, .bash stripped), got: {:?}", imports);
    assert!(imports.contains(&".bashrc"),
        "missing .bashrc (no extension to strip), got: {:?}", imports);
    assert!(imports.contains(&"init"), "missing init, got: {:?}", imports);
    assert!(imports.contains(&"feature"),
        "missing feature (inside function), got: {:?}", imports);
    // Dynamic paths skipped.
    assert!(!imports.iter().any(|i| i.contains('$') || i.contains('{')),
        "dynamic paths should be skipped, got: {:?}", imports);
    // All imports use <module> as source_name (mirrors JS require pattern).
    let import_sources: Vec<&str> = relations.iter()
        .filter(|r| r.relation == REL_IMPORTS)
        .map(|r| r.source_name.as_str())
        .collect();
    assert!(import_sources.iter().all(|s| *s == "<module>"),
        "all import sources should be <module>, got: {:?}", import_sources);
    // `source ./conditional/feature.sh` inside bootstrap() must NOT also
    // emit a CALLS edge for `source`.
    let calls_to_source: Vec<&str> = relations.iter()
        .filter(|r| r.relation == REL_CALLS && r.target_name == "source")
        .map(|r| r.source_name.as_str())
        .collect();
    assert!(calls_to_source.is_empty(),
        "`source` should not emit CALLS, got source_names: {:?}", calls_to_source);
}

#[test]
fn test_extract_call_relations() {
    let code = r#"
function handleLogin(req) {
    const user = validateToken(req.token);
    sendResponse(req, user);
}
"#;
    let relations = extract_relations(code, "typescript").unwrap();
    let calls: Vec<&str> = relations.iter()
        .filter(|r| r.relation == REL_CALLS)
        .map(|r| r.target_name.as_str())
        .collect();
    assert!(calls.contains(&"validateToken"), "got calls: {:?}", calls);
    assert!(calls.contains(&"sendResponse"), "got calls: {:?}", calls);
}

#[test]
fn test_extract_import_relations() {
    let code = r#"
import { UserService } from './services/user';
import jwt from 'jsonwebtoken';
"#;
    let relations = extract_relations(code, "typescript").unwrap();
    let imports: Vec<&str> = relations.iter()
        .filter(|r| r.relation == REL_IMPORTS)
        .map(|r| r.target_name.as_str())
        .collect();
    assert!(imports.contains(&"UserService"), "got imports: {:?}", imports);
}

#[test]
fn test_extract_js_commonjs_require() {
    let code = r#"
const fs = require('node:fs');
const path = require('path');
const lifecycle = require('./lifecycle');
const versionUtils = require('../utils/version-utils.js');
"#;
    let relations = extract_relations(code, "javascript").unwrap();
    let imports: Vec<&str> = relations.iter()
        .filter(|r| r.relation == REL_IMPORTS)
        .map(|r| r.target_name.as_str())
        .collect();
    assert!(imports.contains(&"fs"),        "expected fs import, got: {:?}", imports);
    assert!(imports.contains(&"path"),      "expected path import, got: {:?}", imports);
    assert!(imports.contains(&"lifecycle"), "expected lifecycle import, got: {:?}", imports);
    assert!(imports.contains(&"version-utils"),
        "expected stripped .js extension, got: {:?}", imports);
}

#[test]
fn test_extract_tsx_commonjs_require_and_route() {
    // TSX shares the JS/TS pipeline but went through a distinct config.name —
    // require() and Express route arms previously matched only "js"|"ts".
    let code = r#"
const React = require('react');
const { helpers } = require('./helpers');
app.get('/api/widgets', getWidgets);
"#;
    let relations = extract_relations(code, "tsx").unwrap();
    let imports: Vec<&str> = relations.iter()
        .filter(|r| r.relation == REL_IMPORTS)
        .map(|r| r.target_name.as_str()).collect();
    assert!(imports.contains(&"react"),   "tsx require('react'); got: {:?}", imports);
    assert!(imports.contains(&"helpers"), "tsx require('./helpers'); got: {:?}", imports);

    let routes: Vec<&str> = relations.iter()
        .filter(|r| r.relation == REL_ROUTES_TO)
        .map(|r| r.target_name.as_str()).collect();
    assert!(routes.contains(&"getWidgets"),
        "tsx Express route target; got: {:?}", routes);
}

#[test]
fn test_extract_inherits_relations() {
    let code = r#"
class AdminService extends UserService {
    getPermissions() { return []; }
}
"#;
    let relations = extract_relations(code, "typescript").unwrap();
    let inherits: Vec<&str> = relations.iter()
        .filter(|r| r.relation == REL_INHERITS)
        .map(|r| r.target_name.as_str())
        .collect();
    assert!(inherits.contains(&"UserService"), "got inherits: {:?}", inherits);
}

#[test]
fn test_extract_express_routes() {
    let code = r#"
app.post('/api/login', handleLogin);
app.get('/api/users/:id', getUser);
"#;
    let relations = extract_relations(code, "typescript").unwrap();
    let routes: Vec<(&str, &str)> = relations.iter()
        .filter(|r| r.relation == REL_ROUTES_TO)
        .map(|r| (r.metadata.as_deref().unwrap_or(""), r.target_name.as_str()))
        .collect();
    assert!(routes.iter().any(|(meta, target)| meta.contains("/api/login") && *target == "handleLogin"),
        "got routes: {:?}", routes);
}

#[test]
fn test_extract_express_inline_arrow_routes() {
    let code = r#"
router.post('/api/login', async (req, res) => {
    const valid = validateCredentials(req.body.email);
    res.json({ token: 'ok' });
});
router.get('/api/users/:id', authMiddleware, async (req, res) => {
    res.json(user);
});
"#;
    let relations = extract_relations(code, "typescript").unwrap();
    let routes: Vec<(&str, &str)> = relations.iter()
        .filter(|r| r.relation == REL_ROUTES_TO)
        .map(|r| (r.metadata.as_deref().unwrap_or(""), r.target_name.as_str()))
        .collect();
    assert!(routes.iter().any(|(meta, _target)| meta.contains("/api/login") && meta.contains("\"inline\":true")),
        "should detect inline arrow handler route, got: {:?}", routes);
    assert!(routes.iter().any(|(meta, _target)| meta.contains("/api/users/:id")),
        "should detect multi-arg inline route, got: {:?}", routes);
}

#[test]
fn test_extract_python_flask_routes() {
    let code = r#"
@app.route('/api/users', methods=['GET'])
def get_users():
    return jsonify(users)
"#;
    let relations = extract_relations(code, "python").unwrap();
    let routes: Vec<&str> = relations.iter()
        .filter(|r| r.relation == REL_ROUTES_TO)
        .map(|r| r.target_name.as_str())
        .collect();
    assert!(routes.contains(&"get_users"), "got routes: {:?}", routes);
}

// --- Task 2: Java inheritance ---

#[test]
fn test_extract_java_inheritance() {
    let code = "public class Dog extends Animal {\n    public void bark() {}\n}\n";
    let relations = extract_relations(code, "java").unwrap();
    let inherits: Vec<&str> = relations.iter()
        .filter(|r| r.relation == REL_INHERITS)
        .map(|r| r.target_name.as_str())
        .collect();
    assert!(inherits.contains(&"Animal"), "got: {:?}", inherits);
}

// --- Task 3: Python imports ---

#[test]
fn test_extract_python_import() {
    let code = "import os\n";
    let relations = extract_relations(code, "python").unwrap();
    let imports: Vec<&str> = relations.iter()
        .filter(|r| r.relation == REL_IMPORTS)
        .map(|r| r.target_name.as_str())
        .collect();
    assert!(imports.contains(&"os"), "got: {:?}", imports);
}

#[test]
fn test_extract_python_from_import() {
    let code = "from collections import OrderedDict, defaultdict\n";
    let relations = extract_relations(code, "python").unwrap();
    let imports: Vec<&str> = relations.iter()
        .filter(|r| r.relation == REL_IMPORTS)
        .map(|r| r.target_name.as_str())
        .collect();
    assert!(imports.contains(&"OrderedDict"), "got: {:?}", imports);
    assert!(imports.contains(&"defaultdict"), "got: {:?}", imports);
}

// --- Task 4: Python class inheritance ---

#[test]
fn test_extract_python_inheritance() {
    let code = "class Dog(Animal):\n    def bark(self):\n        pass\n";
    let relations = extract_relations(code, "python").unwrap();
    let inherits: Vec<&str> = relations.iter()
        .filter(|r| r.relation == REL_INHERITS)
        .map(|r| r.target_name.as_str())
        .collect();
    assert!(inherits.contains(&"Animal"), "got: {:?}", inherits);
}

#[test]
fn test_extract_rust_use_imports() {
    let source = r#"
use std::collections::HashMap;
use anyhow::Result;

fn main() {
    let m: HashMap<String, String> = HashMap::new();
}
"#;
    let tree = crate::parser::treesitter::parse_tree(source, "rust").unwrap();
    let relations = extract_relations_from_tree(&tree, source, "rust");
    let imports: Vec<&ParsedRelation> = relations.iter().filter(|r| r.relation == REL_IMPORTS).collect();
    assert!(imports.iter().any(|r| r.target_name == "HashMap"), "should import HashMap, got: {:?}", imports.iter().map(|r| &r.target_name).collect::<Vec<_>>());
    assert!(imports.iter().any(|r| r.target_name == "Result"), "should import Result, got: {:?}", imports.iter().map(|r| &r.target_name).collect::<Vec<_>>());
}

#[test]
fn test_extract_go_import_relations() {
    let source = r#"
package main

import (
    "fmt"
    "net/http"
)

func main() {
    fmt.Println("hello")
}
"#;
    let tree = crate::parser::treesitter::parse_tree(source, "go").unwrap();
    let relations = extract_relations_from_tree(&tree, source, "go");
    let imports: Vec<&ParsedRelation> = relations.iter().filter(|r| r.relation == REL_IMPORTS).collect();
    assert!(imports.iter().any(|r| r.target_name == "fmt"), "should import fmt, got: {:?}", imports.iter().map(|r| &r.target_name).collect::<Vec<_>>());
    assert!(imports.iter().any(|r| r.target_name == "http"), "should import http, got: {:?}", imports.iter().map(|r| &r.target_name).collect::<Vec<_>>());
}

#[test]
fn test_extract_rust_grouped_use_imports() {
    let source = r#"
use std::collections::{HashMap, HashSet, BTreeMap};
use std::io::Read as _;

fn main() {}
"#;
    let tree = crate::parser::treesitter::parse_tree(source, "rust").unwrap();
    let relations = extract_relations_from_tree(&tree, source, "rust");
    let imports: Vec<&str> = relations.iter()
        .filter(|r| r.relation == REL_IMPORTS)
        .map(|r| r.target_name.as_str())
        .collect();
    assert!(imports.contains(&"HashMap"), "should import HashMap, got: {:?}", imports);
    assert!(imports.contains(&"HashSet"), "should import HashSet, got: {:?}", imports);
    assert!(imports.contains(&"BTreeMap"), "should import BTreeMap, got: {:?}", imports);
    assert!(imports.contains(&"Read"), "should import Read (not 'Read as _'), got: {:?}", imports);
    // Should NOT contain braces or 'as _'
    assert!(!imports.iter().any(|i| i.contains('{')), "should not have brace in import names: {:?}", imports);
}

#[test]
fn test_python_route_no_false_positive_on_cache_get() {
    // @cache.get should NOT be detected as a route (cache is not a known framework receiver)
    let code = r#"
@cache.get('/dashboard')
def get_dashboard():
    return render_template('dashboard.html')
"#;
    let relations = extract_relations(code, "python").unwrap();
    let routes: Vec<&ParsedRelation> = relations.iter()
        .filter(|r| r.relation == REL_ROUTES_TO)
        .collect();
    assert!(routes.is_empty(), "should not detect route from @cache.get, got: {:?}",
        routes.iter().map(|r| (&r.source_name, &r.target_name)).collect::<Vec<_>>());
}

#[test]
fn test_python_route_no_false_positive_on_getter() {
    // A decorator containing "get" as substring (e.g., @target) should NOT be detected as a route
    let code = r#"
@cache_target('/dashboard')
def get_dashboard():
    return render_template('dashboard.html')
"#;
    let relations = extract_relations(code, "python").unwrap();
    let routes: Vec<&ParsedRelation> = relations.iter()
        .filter(|r| r.relation == REL_ROUTES_TO)
        .collect();
    assert!(routes.is_empty(), "should not detect route from @login_required, got: {:?}", routes.iter().map(|r| (&r.source_name, &r.target_name)).collect::<Vec<_>>());
}

#[test]
fn test_python_route_detects_dotted_pattern() {
    // @app.get('/path') should still be detected
    let code = r#"
@app.get('/api/items')
def list_items():
    return items
"#;
    let relations = extract_relations(code, "python").unwrap();
    let routes: Vec<&ParsedRelation> = relations.iter()
        .filter(|r| r.relation == REL_ROUTES_TO)
        .collect();
    assert!(!routes.is_empty(), "should detect route from @app.get, got no routes");
    assert!(routes[0].target_name == "list_items", "target should be list_items");
}

#[test]
fn test_extract_rust_impl_trait() {
    let source = r#"
struct MyStruct;
trait MyTrait { fn do_thing(&self); fn other(&self); }
impl MyTrait for MyStruct {
    fn do_thing(&self) {}
    fn other(&self) {}
}
"#;
    let relations = extract_relations(source, "rust").unwrap();
    let impls: Vec<(&str, &str)> = relations.iter()
        .filter(|r| r.relation == REL_IMPLEMENTS)
        .map(|r| (r.source_name.as_str(), r.target_name.as_str()))
        .collect();
    // Type-level: MyStruct implements MyTrait
    assert!(impls.contains(&("MyStruct", "MyTrait")), "got implements: {:?}", impls);
    // Method-level: MyStruct → do_thing, MyStruct → other
    assert!(impls.contains(&("MyStruct", "do_thing")), "method-level edge missing for do_thing: {:?}", impls);
    assert!(impls.contains(&("MyStruct", "other")), "method-level edge missing for other: {:?}", impls);
    assert_eq!(impls.len(), 3, "expected 3 implements edges (1 type + 2 methods), got: {:?}", impls);
}

#[test]
fn test_bare_impl_no_implements_relation() {
    // `impl Type { ... }` (no trait) should produce zero REL_IMPLEMENTS relations
    let source = r#"
struct MyStruct;
impl MyStruct {
    fn new() -> Self { MyStruct }
    fn do_thing(&self) {}
}
"#;
    let relations = extract_relations(source, "rust").unwrap();
    let impls: Vec<_> = relations.iter()
        .filter(|r| r.relation == REL_IMPLEMENTS)
        .collect();
    assert!(impls.is_empty(), "bare impl should produce no implements relations, got: {:?}",
        impls.iter().map(|r| (&r.source_name, &r.target_name)).collect::<Vec<_>>());
}

#[test]
fn test_rust_struct_instantiation_creates_calls_edge() {
    let source = r#"
struct Config { verbose: bool, path: String }

fn build_config() -> Config {
    Config { verbose: true, path: "/tmp".into() }
}
"#;
    let relations = extract_relations(source, "rust").unwrap();
    let calls: Vec<(&str, &str)> = relations.iter()
        .filter(|r| r.relation == REL_CALLS)
        .map(|r| (r.source_name.as_str(), r.target_name.as_str()))
        .collect();
    assert!(calls.contains(&("build_config", "Config")),
        "struct instantiation should create calls edge, got: {:?}", calls);
}

#[test]
fn test_rust_scoped_struct_instantiation() {
    let source = r#"
fn create() {
    let node = crate::parser::NodeRecord { name: "foo".into() };
}
"#;
    let relations = extract_relations(source, "rust").unwrap();
    let calls: Vec<(&str, &str)> = relations.iter()
        .filter(|r| r.relation == REL_CALLS)
        .map(|r| (r.source_name.as_str(), r.target_name.as_str()))
        .collect();
    // Should strip path prefix, keeping just "NodeRecord"
    assert!(calls.contains(&("create", "NodeRecord")),
        "scoped struct should strip path, got: {:?}", calls);
}

#[test]
fn test_extract_go_http_routes() {
    let code = r#"
package main

func main() {
    http.HandleFunc("/api/health", healthCheck)
}
"#;
    let relations = extract_relations(code, "go").unwrap();
    assert!(relations.iter().any(|r| r.relation == REL_ROUTES_TO && r.target_name == "healthCheck"),
        "got relations: {:?}", relations.iter().map(|r| (&r.relation, &r.target_name)).collect::<Vec<_>>());
}

#[test]
fn test_extract_ts_implements() {
    let code = "class UserService implements IUserService {\n    getUser() { return null; }\n}\n";
    let relations = extract_relations(code, "typescript").unwrap();
    let impls: Vec<&str> = relations.iter()
        .filter(|r| r.relation == REL_IMPLEMENTS)
        .map(|r| r.target_name.as_str())
        .collect();
    assert!(impls.contains(&"IUserService"), "got implements: {:?}", impls);
}

#[test]
fn test_extract_java_implements() {
    let code = "public class ArrayList implements List, Serializable {\n}\n";
    let relations = extract_relations(code, "java").unwrap();
    let impls: Vec<&str> = relations.iter()
        .filter(|r| r.relation == REL_IMPLEMENTS)
        .map(|r| r.target_name.as_str())
        .collect();
    assert!(impls.contains(&"List"), "got implements: {:?}", impls);
}

#[test]
fn test_extract_ts_exports() {
    let code = "export function handleLogin(req: Request) {}\nexport class AuthService {}\n";
    let relations = extract_relations(code, "typescript").unwrap();
    let exports: Vec<&str> = relations.iter()
        .filter(|r| r.relation == REL_EXPORTS)
        .map(|r| r.target_name.as_str())
        .collect();
    assert!(exports.contains(&"handleLogin"), "got exports: {:?}", exports);
    assert!(exports.contains(&"AuthService"), "got exports: {:?}", exports);
}

#[test]
fn test_go_selector_call_relations() {
    // Go receiver.Method() calls should be extracted
    let code = r#"
package main

import "fmt"

func main() {
    fmt.Println("hello")
    http.HandleFunc("/", handler)
}
"#;
    let relations = extract_relations(code, "go").unwrap();
    let calls: Vec<(&str, &str)> = relations.iter()
        .filter(|r| r.relation == REL_CALLS)
        .map(|r| (r.source_name.as_str(), r.target_name.as_str()))
        .collect();
    assert!(calls.contains(&("main", "Println")),
        "fmt.Println() should create call relation, got: {:?}", calls);
    assert!(calls.contains(&("main", "HandleFunc")),
        "http.HandleFunc() should create call relation, got: {:?}", calls);
}

#[test]
fn test_rust_scoped_call_relations() {
    // Self::method() and Path::func() should be extracted as call relations
    let code = r#"
impl Database {
    fn open() {
        Self::open_impl(false);
    }
    fn open_impl(flag: bool) {
        HashMap::new();
    }
}
"#;
    let relations = extract_relations(code, "rust").unwrap();
    let calls: Vec<(&str, &str)> = relations.iter()
        .filter(|r| r.relation == REL_CALLS)
        .map(|r| (r.source_name.as_str(), r.target_name.as_str()))
        .collect();
    assert!(calls.contains(&("open", "open_impl")),
        "Self::open_impl() should create call relation, got: {:?}", calls);
    assert!(calls.contains(&("open_impl", "new")),
        "HashMap::new() should create call relation, got: {:?}", calls);
}

#[test]
fn test_rust_method_call_on_object() {
    // obj.method() should also be extracted as a call relation
    let code = r#"
fn test_func() {
    let server = McpServer::from_project_root(path).unwrap();
    server.handle_message(init).unwrap();
    tool_call_json("search", args);
}
"#;
    let relations = extract_relations(code, "rust").unwrap();
    eprintln!("All relations:");
    for r in &relations {
        eprintln!("  {} --[{}]--> {}", r.source_name, r.relation, r.target_name);
    }
    let calls: Vec<(&str, &str)> = relations.iter()
        .filter(|r| r.relation == REL_CALLS)
        .map(|r| (r.source_name.as_str(), r.target_name.as_str()))
        .collect();
    eprintln!("Calls: {:?}", calls);
    assert!(calls.contains(&("test_func", "from_project_root")),
        "McpServer::from_project_root() should create call, got: {:?}", calls);
    assert!(calls.contains(&("test_func", "handle_message")),
        "server.handle_message() should create call, got: {:?}", calls);
    assert!(calls.contains(&("test_func", "tool_call_json")),
        "tool_call_json() should create call, got: {:?}", calls);
}

#[test]
fn test_rust_try_expr_and_match_calls() {
    // Reproduce actual patterns from main.rs run_serve: try expressions, match scrutinee, method calls
    let code = r#"
fn run_serve() {
    let project_root = std::env::current_dir().unwrap();
    let server = code_graph_mcp::mcp::server::McpServer::from_project_root(&project_root).unwrap();
    server.set_notify_writer(Box::new(io::stdout()));
    match server.handle_message(&buf) {
        Ok(Some(response)) => {
            writeln!(stdout, "{}", response).unwrap();
            stdout.flush().unwrap();
        }
        Ok(None) => {}
        Err(e) => {}
    }
    server.run_startup_tasks();
    server.flush_metrics();
}
"#;
    let relations = extract_relations(code, "rust").unwrap();
    let calls: Vec<(&str, &str)> = relations.iter()
        .filter(|r| r.relation == REL_CALLS)
        .map(|r| (r.source_name.as_str(), r.target_name.as_str()))
        .collect();
    assert!(calls.contains(&("run_serve", "from_project_root")),
        "McpServer::from_project_root() missing, got: {:?}", calls);
    assert!(calls.contains(&("run_serve", "set_notify_writer")),
        "server.set_notify_writer() missing, got: {:?}", calls);
    assert!(calls.contains(&("run_serve", "handle_message")),
        "server.handle_message() missing, got: {:?}", calls);
    assert!(calls.contains(&("run_serve", "run_startup_tasks")),
        "server.run_startup_tasks() missing, got: {:?}", calls);
    assert!(calls.contains(&("run_serve", "flush_metrics")),
        "server.flush_metrics() missing, got: {:?}", calls);
}

#[test]
fn test_scope_qualification_class_method() {
    // Methods inside a class should have scope qualified as ClassName.method_name
    let code = r#"
class UserService {
    getUser(id) {
        return this.db.findById(id);
    }
    deleteUser(id) {
        this.getUser(id);
        this.db.remove(id);
    }
}
"#;
    let relations = extract_relations(code, "typescript").unwrap();
    let calls: Vec<(&str, &str)> = relations.iter()
        .filter(|r| r.relation == REL_CALLS)
        .map(|r| (r.source_name.as_str(), r.target_name.as_str()))
        .collect();
    // The scope for getUser should be "UserService.getUser", not just "getUser"
    assert!(calls.iter().any(|(src, tgt)| *src == "UserService.getUser" && *tgt == "findById"),
        "getUser scope should be qualified as UserService.getUser, got calls: {:?}", calls);
    assert!(calls.iter().any(|(src, tgt)| *src == "UserService.deleteUser" && *tgt == "getUser"),
        "deleteUser scope should be qualified as UserService.deleteUser, got calls: {:?}", calls);
}

#[test]
fn test_scope_standalone_function_not_qualified() {
    // Standalone functions (not inside a class) should NOT be qualified with a class prefix
    let code = r#"
function doWork() {
    process();
}
"#;
    let relations = extract_relations(code, "typescript").unwrap();
    let calls: Vec<(&str, &str)> = relations.iter()
        .filter(|r| r.relation == REL_CALLS)
        .map(|r| (r.source_name.as_str(), r.target_name.as_str()))
        .collect();
    assert!(calls.iter().any(|(src, tgt)| *src == "doWork" && *tgt == "process"),
        "standalone function scope should remain unqualified, got calls: {:?}", calls);
}

#[test]
fn test_rust_deeply_nested_scoped_call() {
    // code_graph_mcp::cli::cmd_show() should extract "cmd_show" as the callee
    let code = r#"
fn main() {
    print_version();
    code_graph_mcp::cli::cmd_show(&project_root, &args);
    std::env::current_dir();
}
fn print_version() {}
"#;
    let relations = extract_relations(code, "rust").unwrap();
    let calls: Vec<(&str, &str)> = relations.iter()
        .filter(|r| r.relation == REL_CALLS)
        .map(|r| (r.source_name.as_str(), r.target_name.as_str()))
        .collect();
    eprintln!("All calls: {:?}", calls);
    assert!(calls.contains(&("main", "print_version")),
        "simple call should work, got: {:?}", calls);
    assert!(calls.contains(&("main", "cmd_show")),
        "deeply nested scoped call should extract rightmost name, got: {:?}", calls);
    assert!(calls.contains(&("main", "current_dir")),
        "std::env::current_dir() should extract current_dir, got: {:?}", calls);
}

#[test]
fn test_rust_match_arm_dispatch_calls() {
    // Calls inside match arms should be detected — this is the pattern used by
    // handle_tool (self.tool_*) and main (code_graph_mcp::cli::cmd_*)
    let code = r#"
impl Server {
    fn handle_tool(&self, name: &str) -> i32 {
        let result = match name {
            "search" => self.tool_search(),
            "map" => self.tool_map(),
            _ => 0,
        };
        self.log_result();
        result
    }
}
"#;
    let relations = extract_relations(code, "rust").unwrap();
    let calls: Vec<(&str, &str)> = relations.iter()
        .filter(|r| r.relation == REL_CALLS)
        .map(|r| (r.source_name.as_str(), r.target_name.as_str()))
        .collect();
    eprintln!("Match arm calls: {:?}", calls);
    // Note: Rust `impl` blocks don't set class context (unlike class {} in TS/JS),
    // so scope is just "handle_tool" not "Server.handle_tool"
    assert!(calls.contains(&("handle_tool", "tool_search")),
        "self.tool_search() in match arm should be detected, got: {:?}", calls);
    assert!(calls.contains(&("handle_tool", "tool_map")),
        "self.tool_map() in match arm should be detected, got: {:?}", calls);
    assert!(calls.contains(&("handle_tool", "log_result")),
        "self.log_result() outside match should be detected, got: {:?}", calls);
}

#[test]
fn test_real_handle_tool_dispatch_pattern() {
    // Reproduce the exact pattern from McpServer::handle_tool in mod.rs
    let code = r#"
impl McpServer {
    fn handle_tool(&self, name: &str, args: &serde_json::Value) -> Result<serde_json::Value> {
        let start = std::time::Instant::now();
        let result = match name {
            "semantic_code_search" => self.tool_semantic_search(args),
            "get_call_graph" => self.tool_get_call_graph(args),
            "find_http_route" | "trace_http_chain" => self.tool_trace_http_chain(args),
            "get_ast_node" | "read_snippet" => self.tool_get_ast_node(args),
            "start_watch" => self.tool_start_watch(),
            "stop_watch" => self.tool_stop_watch(),
            "get_index_status" => self.tool_get_index_status(),
            "rebuild_index" => self.tool_rebuild_index(args),
            "impact_analysis" => self.tool_impact_analysis(args),
            "module_overview" => self.tool_module_overview(args),
            "dependency_graph" => self.tool_dependency_graph(args),
            "find_similar_code" => self.tool_find_similar_code(args),
            "project_map" => self.tool_project_map(args),
            "ast_search" => self.tool_ast_search(args),
            "find_references" => self.tool_find_references(args),
            "find_dead_code" => self.tool_find_dead_code(args),
            _ => Err(anyhow!("Unknown tool")),
        };
        let elapsed = start.elapsed();
        lock_or_recover(&self.metrics, "metrics")
            .record_tool_call(name, elapsed.as_millis() as u64, false);
        result
    }
}
"#;
    let relations = extract_relations(code, "rust").unwrap();
    let calls: Vec<(&str, &str)> = relations.iter()
        .filter(|r| r.relation == REL_CALLS)
        .map(|r| (r.source_name.as_str(), r.target_name.as_str()))
        .collect();
    eprintln!("All calls from handle_tool ({}):", calls.len());
    for (src, tgt) in &calls {
        eprintln!("  {} -> {}", src, tgt);
    }
    assert!(calls.iter().any(|(_, t)| *t == "tool_semantic_search"),
        "tool_semantic_search not found in: {:?}", calls);
    assert!(calls.iter().any(|(_, t)| *t == "tool_find_dead_code"),
        "tool_find_dead_code not found in: {:?}", calls);
    assert!(calls.iter().any(|(_, t)| *t == "lock_or_recover"),
        "lock_or_recover not found in: {:?}", calls);
    assert!(calls.iter().any(|(_, t)| *t == "record_tool_call"),
        "record_tool_call not found in: {:?}", calls);
}

/// Every ParsedRelation returned by extract_relations must be stamped with
/// the source language of its originating file. This invariant underpins
/// the same-language edge resolution in pipeline.rs; a parser regression
/// that silently returned empty source_language would reintroduce the
/// cross-language false-positive calls edges we guarded against.
#[test]
fn test_source_language_stamped_on_all_relations() {
    // One minimal sample per supported language. We only assert:
    //   (a) every returned relation carries the right source_language stamp
    //   (b) across all cases combined, at least one relation was produced
    //       (guards against the parser regressing to "zero relations globally")
    let cases = &[
        ("rust", "fn a() { b(); } fn b() {}"),
        ("javascript", "function a() { b(); } function b() {}"),
        ("typescript", "function a() { b(); } function b() {}"),
        ("go", "package p\nfunc a() { b() }\nfunc b() {}\n"),
    ];
    let mut total_relations = 0usize;
    for (lang, src) in cases {
        let relations = extract_relations(src, lang).unwrap();
        total_relations += relations.len();
        for r in &relations {
            assert_eq!(
                r.source_language, *lang,
                "{}: relation {:?} → {:?} has wrong source_language {:?}",
                lang, r.source_name, r.target_name, r.source_language
            );
        }
    }
    assert!(
        total_relations > 0,
        "expected at least one relation across all language samples — parser regression?"
    );
}

// --- Tier 2 inheritance smoke tests (Phase A audit) ---
// Expected-behavior tests: a failure here = a real bug to fix in Phase B.

#[test]
fn test_extract_kotlin_inheritance() {
    // Kotlin: `class S : Base(), Cloneable` — Base is concrete (constructor
    // call), Cloneable is interface. Both should produce INHERITS edges
    // (Kotlin doesn't syntactically distinguish, the type system does).
    let code = "class UserService : BaseService(), Cloneable {\n    fun foo() {}\n}\n";
    let relations = extract_relations(code, "kotlin").unwrap();
    let inherits: Vec<&str> = relations.iter()
        .filter(|r| r.relation == REL_INHERITS || r.relation == REL_IMPLEMENTS)
        .map(|r| r.target_name.as_str())
        .collect();
    assert!(inherits.contains(&"BaseService"),
        "Kotlin: missing BaseService, got: {:?}", inherits);
    assert!(inherits.contains(&"Cloneable"),
        "Kotlin: missing Cloneable, got: {:?}", inherits);
}

#[test]
fn test_extract_swift_inheritance() {
    // Swift: `class S: BaseService, Codable` — comma-separated conformance.
    let code = "class UserService: BaseService, Codable, Hashable {\n    func foo() {}\n}\n";
    let relations = extract_relations(code, "swift").unwrap();
    let inherits: Vec<&str> = relations.iter()
        .filter(|r| r.relation == REL_INHERITS || r.relation == REL_IMPLEMENTS)
        .map(|r| r.target_name.as_str())
        .collect();
    assert!(inherits.contains(&"BaseService"),
        "Swift: missing BaseService, got: {:?}", inherits);
    assert!(inherits.contains(&"Codable"),
        "Swift: missing Codable, got: {:?}", inherits);
    assert!(inherits.contains(&"Hashable"),
        "Swift: missing Hashable, got: {:?}", inherits);
}

#[test]
fn test_extract_dart_inheritance() {
    // Dart has 3 inheritance keywords: extends (single), implements (multi),
    // with (mixin, multi). All conceptually contribute to type lineage.
    let code = "class UserService extends BaseService implements Loggable, Cacheable {\n  void foo() {}\n}\n";
    let relations = extract_relations(code, "dart").unwrap();
    let lineage: Vec<&str> = relations.iter()
        .filter(|r| r.relation == REL_INHERITS || r.relation == REL_IMPLEMENTS)
        .map(|r| r.target_name.as_str())
        .collect();
    assert!(lineage.contains(&"BaseService"),
        "Dart: missing BaseService (extends), got: {:?}", lineage);
    assert!(lineage.contains(&"Loggable"),
        "Dart: missing Loggable (implements), got: {:?}", lineage);
    assert!(lineage.contains(&"Cacheable"),
        "Dart: missing Cacheable (implements), got: {:?}", lineage);
}

#[test]
fn test_extract_php_inheritance() {
    // PHP: extends (single class) + implements (multiple interfaces).
    let code = "<?php\nclass UserService extends BaseService implements Loggable, Cacheable {\n    public function foo() {}\n}\n";
    let relations = extract_relations(code, "php").unwrap();
    let inherits: Vec<&str> = relations.iter()
        .filter(|r| r.relation == REL_INHERITS)
        .map(|r| r.target_name.as_str())
        .collect();
    let implements: Vec<&str> = relations.iter()
        .filter(|r| r.relation == REL_IMPLEMENTS)
        .map(|r| r.target_name.as_str())
        .collect();
    assert!(inherits.contains(&"BaseService"),
        "PHP: missing BaseService (extends), got INHERITS: {:?}", inherits);
    assert!(implements.contains(&"Loggable"),
        "PHP: missing Loggable (implements), got IMPLEMENTS: {:?}", implements);
    assert!(implements.contains(&"Cacheable"),
        "PHP: missing Cacheable (implements), got IMPLEMENTS: {:?}", implements);
}

#[test]
fn test_extract_ruby_inheritance() {
    let code = "class UserService < BaseService\n  def foo\n  end\nend\n";
    let relations = extract_relations(code, "ruby").unwrap();
    let inherits: Vec<&str> = relations.iter()
        .filter(|r| r.relation == REL_INHERITS)
        .map(|r| r.target_name.as_str())
        .collect();
    assert!(inherits.contains(&"BaseService"),
        "Ruby: missing BaseService, got: {:?}", inherits);
}

// --- Tier 2 calls + imports smoke tests (Phase C audit) ---

#[test]
fn test_extract_kotlin_calls() {
    let code = "fun process() {\n    fetch()\n    store()\n}\n";
    let relations = extract_relations(code, "kotlin").unwrap();
    let calls: Vec<&str> = relations.iter()
        .filter(|r| r.relation == REL_CALLS && r.source_name == "process")
        .map(|r| r.target_name.as_str())
        .collect();
    assert!(calls.contains(&"fetch"),
        "Kotlin: missing fetch call, got: {:?}", calls);
    assert!(calls.contains(&"store"),
        "Kotlin: missing store call, got: {:?}", calls);
}

#[test]
fn test_extract_kotlin_imports() {
    let code = "import com.example.UserService\nimport kotlinx.coroutines.flow.Flow\n\nfun process() {}\n";
    let relations = extract_relations(code, "kotlin").unwrap();
    let imports: Vec<&str> = relations.iter()
        .filter(|r| r.relation == REL_IMPORTS)
        .map(|r| r.target_name.as_str())
        .collect();
    assert!(!imports.is_empty(),
        "Kotlin: expected at least one IMPORTS edge, got 0 (relations: {:?})",
        relations.iter().map(|r| (&r.relation, &r.target_name)).collect::<Vec<_>>());
    assert!(imports.iter().any(|i| i == &"UserService" || i.contains("UserService")),
        "Kotlin: missing UserService import, got: {:?}", imports);
}

#[test]
fn test_extract_swift_calls() {
    let code = "func process() {\n    fetch()\n    store()\n}\n";
    let relations = extract_relations(code, "swift").unwrap();
    let calls: Vec<&str> = relations.iter()
        .filter(|r| r.relation == REL_CALLS && r.source_name == "process")
        .map(|r| r.target_name.as_str())
        .collect();
    assert!(calls.contains(&"fetch"),
        "Swift: missing fetch call, got: {:?}", calls);
    assert!(calls.contains(&"store"),
        "Swift: missing store call, got: {:?}", calls);
}

#[test]
fn test_extract_swift_imports() {
    let code = "import Foundation\nimport UIKit\n\nfunc process() {}\n";
    let relations = extract_relations(code, "swift").unwrap();
    let imports: Vec<&str> = relations.iter()
        .filter(|r| r.relation == REL_IMPORTS)
        .map(|r| r.target_name.as_str())
        .collect();
    assert!(imports.contains(&"Foundation"),
        "Swift: missing Foundation, got: {:?}", imports);
    assert!(imports.contains(&"UIKit"),
        "Swift: missing UIKit, got: {:?}", imports);
}

#[test]
fn test_extract_dart_calls() {
    let code = "void process() {\n  fetch();\n  store();\n}\n";
    let relations = extract_relations(code, "dart").unwrap();
    let calls: Vec<&str> = relations.iter()
        .filter(|r| r.relation == REL_CALLS && r.source_name == "process")
        .map(|r| r.target_name.as_str())
        .collect();
    assert!(calls.contains(&"fetch"),
        "Dart: missing fetch call, got: {:?}", calls);
    assert!(calls.contains(&"store"),
        "Dart: missing store call, got: {:?}", calls);
}

#[test]
fn test_extract_dart_imports() {
    let code = "import 'package:flutter/material.dart';\nimport 'dart:async';\n\nvoid process() {}\n";
    let relations = extract_relations(code, "dart").unwrap();
    let imports: Vec<&str> = relations.iter()
        .filter(|r| r.relation == REL_IMPORTS)
        .map(|r| r.target_name.as_str())
        .collect();
    assert!(!imports.is_empty(),
        "Dart: expected at least one IMPORTS edge, got 0 (relations: {:?})",
        relations.iter().map(|r| (&r.relation, &r.target_name)).collect::<Vec<_>>());
    assert!(imports.iter().any(|i| i.contains("material") || i.contains("flutter")),
        "Dart: missing material/flutter import, got: {:?}", imports);
    assert!(imports.iter().any(|i| i.contains("async") || i.contains("dart:async")),
        "Dart: missing async import, got: {:?}", imports);
}

#[test]
fn test_extract_php_calls() {
    let code = "<?php\nfunction process() {\n    fetch();\n    store();\n}\n";
    let relations = extract_relations(code, "php").unwrap();
    let calls: Vec<&str> = relations.iter()
        .filter(|r| r.relation == REL_CALLS && r.source_name == "process")
        .map(|r| r.target_name.as_str())
        .collect();
    assert!(calls.contains(&"fetch"),
        "PHP: missing fetch call, got: {:?}", calls);
    assert!(calls.contains(&"store"),
        "PHP: missing store call, got: {:?}", calls);
}

#[test]
fn test_extract_php_imports() {
    let code = "<?php\nuse App\\Services\\UserService;\nuse App\\Models\\Order;\n\nfunction process() {}\n";
    let relations = extract_relations(code, "php").unwrap();
    let imports: Vec<&str> = relations.iter()
        .filter(|r| r.relation == REL_IMPORTS)
        .map(|r| r.target_name.as_str())
        .collect();
    assert!(!imports.is_empty(),
        "PHP: expected at least one IMPORTS edge, got 0 (relations: {:?})",
        relations.iter().map(|r| (&r.relation, &r.target_name)).collect::<Vec<_>>());
    assert!(imports.iter().any(|i| i.contains("UserService")),
        "PHP: missing UserService, got: {:?}", imports);
    assert!(imports.iter().any(|i| i.contains("Order")),
        "PHP: missing Order, got: {:?}", imports);
}

#[test]
fn test_extract_ruby_calls() {
    // Bare names (`fetch`, `store`) in Ruby are statically ambiguous
    // (local var read vs. method call) — tree-sitter-ruby parses them
    // as `identifier`, not `call`. Use parens to force the call shape.
    let code = "def process\n  fetch()\n  store()\nend\n";
    let relations = extract_relations(code, "ruby").unwrap();
    let calls: Vec<&str> = relations.iter()
        .filter(|r| r.relation == REL_CALLS && r.source_name == "process")
        .map(|r| r.target_name.as_str())
        .collect();
    assert!(calls.contains(&"fetch"),
        "Ruby: missing fetch call, got: {:?}", calls);
    assert!(calls.contains(&"store"),
        "Ruby: missing store call, got: {:?}", calls);
}

#[test]
fn test_extract_ruby_imports() {
    let code = "require 'json'\nrequire_relative 'helper'\n\ndef process\nend\n";
    let relations = extract_relations(code, "ruby").unwrap();
    let imports: Vec<&str> = relations.iter()
        .filter(|r| r.relation == REL_IMPORTS)
        .map(|r| r.target_name.as_str())
        .collect();
    assert!(imports.contains(&"json"),
        "Ruby: missing json (require), got: {:?}", imports);
    assert!(imports.contains(&"helper"),
        "Ruby: missing helper (require_relative), got: {:?}", imports);
}

#[test]
fn test_extract_csharp_calls() {
    let code = "class App {\n    void Process() {\n        Fetch();\n        Store();\n    }\n}\n";
    let relations = extract_relations(code, "csharp").unwrap();
    let calls: Vec<&str> = relations.iter()
        .filter(|r| r.relation == REL_CALLS)
        .map(|r| r.target_name.as_str())
        .collect();
    assert!(calls.contains(&"Fetch"),
        "C#: missing Fetch call, got: {:?}", calls);
    assert!(calls.contains(&"Store"),
        "C#: missing Store call, got: {:?}", calls);
}

#[test]
fn test_extract_csharp_imports() {
    let code = "using System;\nusing System.Collections.Generic;\n\nclass App {}\n";
    let relations = extract_relations(code, "csharp").unwrap();
    let imports: Vec<&str> = relations.iter()
        .filter(|r| r.relation == REL_IMPORTS)
        .map(|r| r.target_name.as_str())
        .collect();
    assert!(!imports.is_empty(),
        "C#: expected at least one IMPORTS edge, got 0 (relations: {:?})",
        relations.iter().map(|r| (&r.relation, &r.target_name)).collect::<Vec<_>>());
    assert!(imports.iter().any(|i| i == &"System" || i.contains("System")),
        "C#: missing System import, got: {:?}", imports);
}

#[test]
fn test_extract_csharp_inheritance() {
    // C#: `class S : Base, IInterface` — current code uses IFoo prefix
    // heuristic to split into INHERITS (Base) vs IMPLEMENTS (IInterface).
    let code = "class UserService : BaseService, IDisposable, ICloneable {\n    public void Foo() {}\n}\n";
    let relations = extract_relations(code, "csharp").unwrap();
    let inherits: Vec<&str> = relations.iter()
        .filter(|r| r.relation == REL_INHERITS)
        .map(|r| r.target_name.as_str())
        .collect();
    let implements: Vec<&str> = relations.iter()
        .filter(|r| r.relation == REL_IMPLEMENTS)
        .map(|r| r.target_name.as_str())
        .collect();
    assert!(inherits.contains(&"BaseService"),
        "C#: missing BaseService (INHERITS), got: {:?}", inherits);
    assert!(implements.contains(&"IDisposable"),
        "C#: missing IDisposable (IMPLEMENTS), got: {:?}", implements);
    assert!(implements.contains(&"ICloneable"),
        "C#: missing ICloneable (IMPLEMENTS), got: {:?}", implements);
}
