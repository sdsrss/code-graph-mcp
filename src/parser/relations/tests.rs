use super::*;
use crate::domain::{REL_EXPORTS, REL_REFERENCES, REL_ROUTES_TO};

#[test]
fn test_extract_php_include_imports() {
    // PHP file includes (require / require_once / include / include_once) must
    // produce REL_IMPORTS edges to the bare file stem (directory + `.php`
    // stripped), mirroring C `#include` and JS `require`. Pre-fix these were
    // silently dropped — PHP files got symbols/calls/`use` imports but no
    // file-include dependency edges, so deps/cycles/affected/project_map
    // under-reported PHP cross-file dependencies. INDEX_VERSION 23→24.
    let code = "<?php\n\
        require_once 'lib.php';\n\
        require 'src/User.php';\n\
        include 'helpers.php';\n\
        include_once __DIR__ . '/config.php';\n\
        use App\\Models\\Account;\n\
        function handle($r) { return process($r); }\n";
    let rels = extract_relations(code, "php").unwrap();
    let imports: Vec<&str> = rels
        .iter()
        .filter(|r| r.relation == REL_IMPORTS)
        .map(|r| r.target_name.as_str())
        .collect();
    assert!(
        imports.contains(&"lib"),
        "require_once 'lib.php' → import 'lib'; got: {:?}",
        imports
    );
    assert!(
        imports.contains(&"User"),
        "require 'src/User.php' → 'User' (dir stripped); got: {:?}",
        imports
    );
    assert!(
        imports.contains(&"helpers"),
        "include 'helpers.php' → 'helpers'; got: {:?}",
        imports
    );
    assert!(
        imports.contains(&"config"),
        "include_once __DIR__.'/config.php' → 'config'; got: {:?}",
        imports
    );
    // The existing `use` namespace import (last segment) must still work.
    assert!(
        imports.contains(&"Account"),
        "use App\\Models\\Account → 'Account'; got: {:?}",
        imports
    );
}

#[test]
fn test_php_top_level_call_attributes_to_module() {
    // A PHP call outside any function/method (top-level script) must attribute to
    // <module>, mirroring the python/ruby/bash arms — otherwise the callee has no
    // incoming edge and is false-reported as dead-code. Before this fix the PHP
    // call arm required Some(active_scope), silently dropping every top-level
    // call. INDEX_VERSION 44→45.
    let code = "<?php\n\
        function greetPhp() { return 1; }\n\
        greetPhp();\n";
    let rels = extract_relations(code, "php").unwrap();
    let has_edge = rels.iter().any(|r| {
        r.relation == crate::domain::REL_CALLS
            && r.target_name == "greetPhp"
            && r.source_name == "<module>"
    });
    assert!(
        has_edge,
        "top-level greetPhp() must produce a <module> → greetPhp call edge; got calls: {:?}",
        rels.iter()
            .filter(|r| r.relation == crate::domain::REL_CALLS)
            .map(|r| (r.source_name.as_str(), r.target_name.as_str()))
            .collect::<Vec<_>>()
    );
}

#[test]
fn test_extract_java_imports() {
    // Java `import p.B;` / `import java.util.List;` must produce REL_IMPORTS edges
    // to the last (imported-type) segment. Before this fix Java matched no import
    // arm — `import_declaration` was gated to swift — so Java import edges were 0:
    // `<external>` nodes were missing, import-aware call resolution was dead for
    // Java, and imported classes were false-reported as dead-code. This violated
    // the CLAUDE.md "Java Full-tier includes imports" claim. INDEX_VERSION 42→43.
    let code = "\
package com.example.app;\n\
import p.B;\n\
import java.util.List;\n\
import java.util.*;\n\
import static org.junit.Assert.assertEquals;\n\
class App { }\n";
    let rels = extract_relations(code, "java").unwrap();
    let imports: Vec<&str> = rels
        .iter()
        .filter(|r| r.relation == REL_IMPORTS)
        .map(|r| r.target_name.as_str())
        .collect();
    assert!(
        imports.contains(&"B"),
        "import p.B → 'B'; got: {:?}",
        imports
    );
    assert!(
        imports.contains(&"List"),
        "import java.util.List → 'List'; got: {:?}",
        imports
    );
    assert!(
        imports.contains(&"assertEquals"),
        "static import → last segment 'assertEquals'; got: {:?}",
        imports
    );
    // A wildcard on-demand import names no single symbol → emit nothing (in
    // particular never the package segment `util` or a bare `*`).
    assert!(
        !imports.contains(&"util"),
        "wildcard import must not emit the package segment; got: {:?}",
        imports
    );
    assert!(
        !imports.contains(&"*"),
        "wildcard '*' must not be emitted; got: {:?}",
        imports
    );
}

#[test]
fn test_extract_ts_reexport_from_barrel() {
    // A barrel/index re-export `export { X, Y } from './mod'` is a DEPENDENCY on
    // './mod'. Emit a REL_IMPORTS edge per re-exported name, stamped with the same
    // js_module metadata a regular named import carries, so Phase-2 resolves each to
    // the source file. Before INDEX_VERSION 41 these produced ZERO edges — barrel
    // files were invisible to deps/affected/impact/cycles/tour and find-references.
    let code = "\
        export { API_URL, host } from './constants';\n\
        export { callApi as call } from './consumer';\n\
        export const LOCAL = 1;\n\
        export function localFn() { return 1; }\n";
    let rels = extract_relations(code, "typescript").unwrap();

    let reexports: Vec<&str> = rels
        .iter()
        .filter(|r| r.relation == REL_IMPORTS)
        .map(|r| r.target_name.as_str())
        .collect();
    assert!(
        reexports.contains(&"API_URL"),
        "re-export → import 'API_URL'; got: {:?}",
        reexports
    );
    assert!(
        reexports.contains(&"host"),
        "re-export → import 'host'; got: {:?}",
        reexports
    );
    // A renamed re-export resolves on the SOURCE name (callApi), not the alias (call).
    assert!(
        reexports.contains(&"callApi"),
        "renamed re-export uses source name 'callApi'; got: {:?}",
        reexports
    );
    assert!(
        !reexports.contains(&"call"),
        "the alias must not be the dependency target; got: {:?}",
        reexports
    );

    // The js_module specifier is stamped so Phase-2 resolves to the concrete file.
    let api = rels
        .iter()
        .find(|r| r.relation == REL_IMPORTS && r.target_name == "API_URL")
        .expect("API_URL re-export edge");
    assert!(
        api.metadata
            .as_deref()
            .unwrap_or("")
            .contains("./constants"),
        "re-export import carries js_module metadata; got: {:?}",
        api.metadata
    );

    // Declaration exports in the same file still emit REL_EXPORTS (path unchanged).
    let exports: Vec<&str> = rels
        .iter()
        .filter(|r| r.relation == REL_EXPORTS)
        .map(|r| r.target_name.as_str())
        .collect();
    assert!(
        exports.contains(&"LOCAL"),
        "declaration const export still works; got: {:?}",
        exports
    );
    assert!(
        exports.contains(&"localFn"),
        "function export still works; got: {:?}",
        exports
    );
}

#[test]
fn test_extract_flask_route_methods_kwarg() {
    // Flask `@app.route('/x', methods=['POST'])` must derive the HTTP method from
    // the `methods=` kwarg, not default to "ANY" (which breaks `trace 'POST /x'`
    // since trace filters routes by exact method). FastAPI `.get()`/`.post()`
    // decorators already work via the decorator name.
    let code = "from flask import Flask\n\
        app = Flask(__name__)\n\
        @app.route('/users', methods=['GET'])\n\
        def list_users():\n    return []\n\
        @app.route('/users/<id>', methods=['DELETE'])\n\
        def remove_user(id):\n    return None\n\
        @app.route('/multi', methods=['POST', 'PUT'])\n\
        def multi():\n    return None\n\
        @app.route('/noverb')\n\
        def noverb():\n    return None\n";
    let rels = extract_relations(code, "python").unwrap();
    let route_method = |handler: &str| -> Option<String> {
        rels.iter()
            .find(|r| r.relation == REL_ROUTES_TO && r.source_name == handler)
            .and_then(|r| r.metadata.as_ref())
            .and_then(|m| serde_json::from_str::<serde_json::Value>(m).ok())
            .and_then(|v| v.get("method").and_then(|x| x.as_str()).map(String::from))
    };
    assert_eq!(
        route_method("list_users").as_deref(),
        Some("GET"),
        "methods=['GET'] → GET"
    );
    assert_eq!(
        route_method("remove_user").as_deref(),
        Some("DELETE"),
        "methods=['DELETE'] → DELETE"
    );
    // Multi-method: the single-method metadata schema stores the first listed.
    assert_eq!(
        route_method("multi").as_deref(),
        Some("POST"),
        "methods=['POST','PUT'] → first (POST)"
    );
    // No methods= kwarg → GET, Flask's own default (`methods` defaults to
    // `["GET"]`, with HEAD/OPTIONS auto-derived). Storing the wildcard "ANY"
    // here traded a false negative for a false positive: `trace 'DELETE /noverb'`
    // matched a route that answers 405 at runtime.
    assert_eq!(
        route_method("noverb").as_deref(),
        Some("GET"),
        "no methods= → GET (Flask default), not the ANY wildcard"
    );
    // The precision claim, stated as behaviour rather than storage: a verb Flask
    // would reject must NOT match the stored route.
    assert!(
        !crate::domain::route_method_matches(&route_method("noverb").unwrap(), "DELETE"),
        "no-methods Flask route must not match DELETE"
    );
    assert!(
        crate::domain::route_method_matches(&route_method("noverb").unwrap(), "get"),
        "no-methods Flask route still matches GET (case-insensitively)"
    );
}

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
    let calls: Vec<&str> = relations
        .iter()
        .filter(|r| r.relation == REL_CALLS && r.source_name == "run_pipeline")
        .map(|r| r.target_name.as_str())
        .collect();
    // Static, identifier-shaped callees → emitted (path prefix stripped).
    assert!(
        calls.contains(&"fetch_data"),
        "missing fetch_data, got: {:?}",
        calls
    );
    assert!(
        calls.contains(&"transform_records"),
        "missing transform_records, got: {:?}",
        calls
    );
    assert!(
        calls.contains(&"cat"),
        "missing cat (path prefix stripped), got: {:?}",
        calls
    );
    assert!(
        calls.contains(&"finalize.sh"),
        "missing finalize.sh (./prefix stripped), got: {:?}",
        calls
    );
    assert!(calls.contains(&"echo"), "missing echo, got: {:?}", calls);
    // Non-static / non-identifier-shaped → skipped.
    assert!(
        !calls.contains(&":"),
        "':' should be skipped, got: {:?}",
        calls
    );
    assert!(
        !calls.contains(&"["),
        "'[' test command should be skipped, got: {:?}",
        calls
    );
    assert!(
        !calls.iter().any(|c| c.contains('$')),
        "variable expansions / substitutions should be skipped, got: {:?}",
        calls
    );
}

#[test]
fn test_extract_bash_top_level_call_attributes_to_module() {
    // Top-level commands are bash's imperative execution flow. An entry-point
    // function invoked only at the script top level must emit a
    // `<module> calls run_app` edge, or dead-code would flag the entry point.
    // (INDEX_VERSION 25→26.)
    let code = r#"#!/usr/bin/env bash
run_app() {
    echo hi
}
cd /tmp
run_app "$@"
"#;
    let relations = extract_relations(code, "bash").unwrap();
    let module_calls: Vec<&str> = relations
        .iter()
        .filter(|r| r.relation == REL_CALLS && r.source_name == "<module>")
        .map(|r| r.target_name.as_str())
        .collect();
    assert!(
        module_calls.contains(&"run_app"),
        "top-level `run_app` invocation must attribute to <module>, got: {:?}",
        module_calls
    );
    // A call INSIDE a function still attributes to that function, not <module>.
    assert!(
        !relations.iter().any(|r| r.relation == REL_CALLS
            && r.source_name == "<module>"
            && r.target_name == "echo"),
        "`echo` is inside run_app's body, must attribute to run_app not <module>"
    );
}

#[test]
fn test_extract_python_top_level_call_attributes_to_module() {
    // A function invoked only at module top level must produce a
    // `<module> calls main_entry` edge, else dead-code flags the entry point.
    // (Same fix as bash, INDEX_VERSION 26→27.)
    let code =
        "def main_entry():\n    return helper()\n\ndef helper():\n    return 1\n\nmain_entry()\n";
    let relations = extract_relations(code, "python").unwrap();
    let module_calls: Vec<&str> = relations
        .iter()
        .filter(|r| r.relation == REL_CALLS && r.source_name == "<module>")
        .map(|r| r.target_name.as_str())
        .collect();
    assert!(
        module_calls.contains(&"main_entry"),
        "top-level main_entry() must attribute to <module>, got: {:?}",
        module_calls
    );
    // The call INSIDE main_entry still attributes to main_entry, not <module>.
    assert!(
        !module_calls.contains(&"helper"),
        "`helper` is inside main_entry's body, must attribute to main_entry not <module>"
    );
}

#[test]
fn test_extract_ruby_top_level_call_attributes_to_module() {
    // Ruby method calls need parens/a receiver to parse as `call` nodes (bare
    // `entry` parses as an identifier — a separate, pre-existing gap). A
    // top-level `entry()` must attribute to <module> so the entry isn't dead.
    let code = "def entry\n  1\nend\n\nentry()\n";
    let relations = extract_relations(code, "ruby").unwrap();
    let module_calls: Vec<&str> = relations
        .iter()
        .filter(|r| r.relation == REL_CALLS && r.source_name == "<module>")
        .map(|r| r.target_name.as_str())
        .collect();
    assert!(
        module_calls.contains(&"entry"),
        "top-level entry() must attribute to <module>, got: {:?}",
        module_calls
    );
}

#[test]
fn test_extract_c_include_imports() {
    let code = "#include \"local/utils.h\"\n#include <stdio.h>\n#include \"helpers.hpp\"\n\nint main() { return 0; }\n";
    let relations = extract_relations(code, "c").unwrap();
    let imports: Vec<&str> = relations
        .iter()
        .filter(|r| r.relation == REL_IMPORTS)
        .map(|r| r.target_name.as_str())
        .collect();
    assert!(
        imports.contains(&"utils"),
        "C: missing utils (.h stripped, path stripped), got: {:?}",
        imports
    );
    assert!(
        imports.contains(&"stdio"),
        "C: missing stdio (system_lib_string), got: {:?}",
        imports
    );
    assert!(
        imports.contains(&"helpers"),
        "C: missing helpers (.hpp stripped), got: {:?}",
        imports
    );
    let import_sources: Vec<&str> = relations
        .iter()
        .filter(|r| r.relation == REL_IMPORTS)
        .map(|r| r.source_name.as_str())
        .collect();
    assert!(
        import_sources.iter().all(|s| *s == "<module>"),
        "all C #include sources should be <module>, got: {:?}",
        import_sources
    );
}

#[test]
fn test_extract_cpp_include_imports() {
    let code = "#include <vector>\n#include \"my/header.hpp\"\n\nint main() { return 0; }\n";
    let relations = extract_relations(code, "cpp").unwrap();
    let imports: Vec<&str> = relations
        .iter()
        .filter(|r| r.relation == REL_IMPORTS)
        .map(|r| r.target_name.as_str())
        .collect();
    assert!(
        imports.contains(&"vector"),
        "C++: missing vector (system header, no extension), got: {:?}",
        imports
    );
    assert!(
        imports.contains(&"header"),
        "C++: missing header (.hpp stripped + path stripped), got: {:?}",
        imports
    );
}

#[test]
fn test_cpp_qualified_call_and_method_scope() {
    // C++ Class::method scope: qualified calls must produce edges (previously
    // dropped at extract_callee_name's `_ => None`), and method call sources
    // must be scoped as Class.method (previously `<module>` — scope_name has no
    // `name` field for C/C++ function_definition).
    let code = r#"
class Engine {
    void start() { ignite(); }
};
void Engine::ignite() { }
void run() { Engine::ignite(); }
"#;
    let relations = extract_relations(code, "cpp").unwrap();
    let calls: Vec<(&str, &str)> = relations
        .iter()
        .filter(|r| r.relation == REL_CALLS)
        .map(|r| (r.source_name.as_str(), r.target_name.as_str()))
        .collect();
    // (1) qualified call `Engine::ignite()` inside run() must be an edge to `ignite`
    assert!(
        calls.iter().any(|(s, t)| *s == "run" && *t == "ignite"),
        "Engine::ignite() qualified call should yield run→ignite; got: {:?}",
        calls
    );
    // (2) in-class method scope: start() calls ignite() → source Engine.start
    assert!(
        calls
            .iter()
            .any(|(s, t)| *s == "Engine.start" && *t == "ignite"),
        "in-class method call source should be Engine.start; got: {:?}",
        calls
    );
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
    let imports: Vec<&str> = relations
        .iter()
        .filter(|r| r.relation == REL_IMPORTS)
        .map(|r| r.target_name.as_str())
        .collect();
    // Static, .sh-stripped, path-stripped targets.
    assert!(
        imports.contains(&"utils"),
        "missing utils, got: {:?}",
        imports
    );
    assert!(
        imports.contains(&"lang"),
        "missing lang (double-quoted), got: {:?}",
        imports
    );
    assert!(
        imports.contains(&"helpers"),
        "missing helpers (single-quoted, .bash stripped), got: {:?}",
        imports
    );
    assert!(
        imports.contains(&".bashrc"),
        "missing .bashrc (no extension to strip), got: {:?}",
        imports
    );
    assert!(
        imports.contains(&"init"),
        "missing init, got: {:?}",
        imports
    );
    assert!(
        imports.contains(&"feature"),
        "missing feature (inside function), got: {:?}",
        imports
    );
    // Dynamic paths skipped.
    assert!(
        !imports.iter().any(|i| i.contains('$') || i.contains('{')),
        "dynamic paths should be skipped, got: {:?}",
        imports
    );
    // All imports use <module> as source_name (mirrors JS require pattern).
    let import_sources: Vec<&str> = relations
        .iter()
        .filter(|r| r.relation == REL_IMPORTS)
        .map(|r| r.source_name.as_str())
        .collect();
    assert!(
        import_sources.iter().all(|s| *s == "<module>"),
        "all import sources should be <module>, got: {:?}",
        import_sources
    );
    // `source ./conditional/feature.sh` inside bootstrap() must NOT also
    // emit a CALLS edge for `source`.
    let calls_to_source: Vec<&str> = relations
        .iter()
        .filter(|r| r.relation == REL_CALLS && r.target_name == "source")
        .map(|r| r.source_name.as_str())
        .collect();
    assert!(
        calls_to_source.is_empty(),
        "`source` should not emit CALLS, got source_names: {:?}",
        calls_to_source
    );
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
    let calls: Vec<&str> = relations
        .iter()
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
    let imports: Vec<&str> = relations
        .iter()
        .filter(|r| r.relation == REL_IMPORTS)
        .map(|r| r.target_name.as_str())
        .collect();
    assert!(
        imports.contains(&"UserService"),
        "got imports: {:?}",
        imports
    );
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
    let imports: Vec<&str> = relations
        .iter()
        .filter(|r| r.relation == REL_IMPORTS)
        .map(|r| r.target_name.as_str())
        .collect();
    assert!(
        imports.contains(&"fs"),
        "expected fs import, got: {:?}",
        imports
    );
    assert!(
        imports.contains(&"path"),
        "expected path import, got: {:?}",
        imports
    );
    assert!(
        imports.contains(&"lifecycle"),
        "expected lifecycle import, got: {:?}",
        imports
    );
    assert!(
        imports.contains(&"version-utils"),
        "expected stripped .js extension, got: {:?}",
        imports
    );
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
    let imports: Vec<&str> = relations
        .iter()
        .filter(|r| r.relation == REL_IMPORTS)
        .map(|r| r.target_name.as_str())
        .collect();
    assert!(
        imports.contains(&"react"),
        "tsx require('react'); got: {:?}",
        imports
    );
    assert!(
        imports.contains(&"helpers"),
        "tsx require('./helpers'); got: {:?}",
        imports
    );

    let routes: Vec<&str> = relations
        .iter()
        .filter(|r| r.relation == REL_ROUTES_TO)
        .map(|r| r.target_name.as_str())
        .collect();
    assert!(
        routes.contains(&"getWidgets"),
        "tsx Express route target; got: {:?}",
        routes
    );
}

#[test]
fn test_extract_inherits_relations() {
    let code = r#"
class AdminService extends UserService {
    getPermissions() { return []; }
}
"#;
    let relations = extract_relations(code, "typescript").unwrap();
    let inherits: Vec<&str> = relations
        .iter()
        .filter(|r| r.relation == REL_INHERITS)
        .map(|r| r.target_name.as_str())
        .collect();
    assert!(
        inherits.contains(&"UserService"),
        "got inherits: {:?}",
        inherits
    );
}

#[test]
fn test_extract_express_routes() {
    let code = r#"
app.post('/api/login', handleLogin);
app.get('/api/users/:id', getUser);
"#;
    let relations = extract_relations(code, "typescript").unwrap();
    let routes: Vec<(&str, &str)> = relations
        .iter()
        .filter(|r| r.relation == REL_ROUTES_TO)
        .map(|r| (r.metadata.as_deref().unwrap_or(""), r.target_name.as_str()))
        .collect();
    assert!(
        routes
            .iter()
            .any(|(meta, target)| meta.contains("/api/login") && *target == "handleLogin"),
        "got routes: {:?}",
        routes
    );
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
    let routes: Vec<(&str, &str)> = relations
        .iter()
        .filter(|r| r.relation == REL_ROUTES_TO)
        .map(|r| (r.metadata.as_deref().unwrap_or(""), r.target_name.as_str()))
        .collect();
    assert!(
        routes
            .iter()
            .any(|(meta, _target)| meta.contains("/api/login") && meta.contains("\"inline\":true")),
        "should detect inline arrow handler route, got: {:?}",
        routes
    );
    assert!(
        routes
            .iter()
            .any(|(meta, _target)| meta.contains("/api/users/:id")),
        "should detect multi-arg inline route, got: {:?}",
        routes
    );
}

#[test]
fn test_inline_route_handler_scopes_calls_and_materializes_node() {
    let code = r#"
app.get('/users', async (req, res) => {
    const u = fetchUser(req.params.id);
    res.json(u);
});
"#;
    let relations = extract_relations(code, "typescript").unwrap();
    // routes_to edge now targets the synthetic handler node, not <module>.
    let route = relations
        .iter()
        .find(|r| r.relation == REL_ROUTES_TO)
        .expect("route edge");
    // Synthetic handler name now carries a per-occurrence `#Lstart` suffix so
    // duplicate same-route handlers stay distinct; assert the base + that all four
    // derivation points (route source/target, scoped call, materialized node) agree.
    assert!(
        route.source_name.starts_with("GET /users#L"),
        "route edge source = synthetic handler name, got {}",
        route.source_name
    );
    assert_eq!(
        route.target_name, route.source_name,
        "routes_to is a self-edge on the handler node"
    );
    assert!(route
        .metadata
        .as_deref()
        .unwrap_or("")
        .contains("\"inline\":true"));
    // The call inside the handler attributes to the handler, not the file <module>.
    let call = relations
        .iter()
        .find(|r| r.relation == crate::domain::REL_CALLS && r.target_name == "fetchUser")
        .expect("fetchUser call edge");
    assert_eq!(call.source_name, route.source_name,
        "inline-handler call must scope to the SAME synthetic handler node as the route edge, got source={}", call.source_name);
    // Node materialization: extract_nodes produces a function node with the same name.
    let tree = crate::parser::treesitter::parse_tree(code, "typescript").unwrap();
    let nodes = crate::parser::treesitter::extract_nodes_from_tree(&tree, code, "typescript");
    assert!(
        nodes
            .iter()
            .any(|n| n.name == route.source_name && n.node_type == "function"),
        "inline handler must be materialized as a function node matching the edge name; got: {:?}",
        nodes
            .iter()
            .map(|n| (n.name.clone(), n.node_type.clone()))
            .collect::<Vec<_>>()
    );
}

#[test]
fn test_fastify_inline_route_handler_scopes_calls() {
    let code = r#"
fastify.post('/login', async (req, reply) => {
    const ok = checkAuth(req.body);
    reply.send({ ok });
});
"#;
    let relations = extract_relations(code, "javascript").unwrap();
    let route = relations
        .iter()
        .find(|r| r.relation == REL_ROUTES_TO)
        .expect("fastify route edge");
    assert!(
        route.source_name.starts_with("POST /login#L"),
        "fastify route recognized + synthetic name, got {}",
        route.source_name
    );
    let call = relations
        .iter()
        .find(|r| r.relation == crate::domain::REL_CALLS && r.target_name == "checkAuth")
        .expect("checkAuth call edge");
    assert_eq!(
        call.source_name, route.source_name,
        "fastify inline handler must scope its calls to the same handler node, got source={}",
        call.source_name
    );
}

#[test]
fn test_extract_python_flask_routes() {
    let code = r#"
@app.route('/api/users', methods=['GET'])
def get_users():
    return jsonify(users)
"#;
    let relations = extract_relations(code, "python").unwrap();
    let routes: Vec<&str> = relations
        .iter()
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
    let inherits: Vec<&str> = relations
        .iter()
        .filter(|r| r.relation == REL_INHERITS)
        .map(|r| r.target_name.as_str())
        .collect();
    assert!(inherits.contains(&"Animal"), "got: {:?}", inherits);
}

// Regression: tree-sitter-java uses `method_invocation` (NOT `call_expression`),
// which had no dispatch arm in walk_for_relations — so every Java call edge was
// silently dropped even though Java is documented Full tier. Verify bare `foo()`,
// `this.foo()`, and receiver `obj.foo()` all emit calls under the qualified
// method scope `Svc.run`.
#[test]
fn test_extract_java_method_calls() {
    let code = "class Svc {\n  void run() { helper(); this.other(); dep.work(); }\n  void helper() {}\n  void other() {}\n}\n";
    let relations = extract_relations(code, "java").unwrap();
    let calls: Vec<&str> = relations
        .iter()
        .filter(|r| r.relation == REL_CALLS && r.source_name == "Svc.run")
        .map(|r| r.target_name.as_str())
        .collect();
    assert!(
        calls.contains(&"helper"),
        "bare call helper() missing; got: {:?}",
        calls
    );
    assert!(
        calls.contains(&"other"),
        "this.other() call missing; got: {:?}",
        calls
    );
    assert!(
        calls.contains(&"work"),
        "receiver dep.work() call missing; got: {:?}",
        calls
    );
}

// --- Task 3: Python imports ---

#[test]
fn test_extract_python_import() {
    let code = "import os\n";
    let relations = extract_relations(code, "python").unwrap();
    let imports: Vec<&str> = relations
        .iter()
        .filter(|r| r.relation == REL_IMPORTS)
        .map(|r| r.target_name.as_str())
        .collect();
    assert!(imports.contains(&"os"), "got: {:?}", imports);
}

#[test]
fn test_extract_python_from_import() {
    let code = "from collections import OrderedDict, defaultdict\n";
    let relations = extract_relations(code, "python").unwrap();
    let imports: Vec<&str> = relations
        .iter()
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
    let inherits: Vec<&str> = relations
        .iter()
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
    let imports: Vec<&ParsedRelation> = relations
        .iter()
        .filter(|r| r.relation == REL_IMPORTS)
        .collect();
    assert!(
        imports.iter().any(|r| r.target_name == "Result"),
        "should import Result, got: {:?}",
        imports.iter().map(|r| &r.target_name).collect::<Vec<_>>()
    );
    // IDX v53: std-root imports emit an EXTERNAL-marked relation — the bare
    // "HashMap" must never enter global bare-name resolution (that produced the
    // phantom `use std::fs` → `fn fs` test-helper edges), but the marked
    // relation still binds to the `<external>` sentinel so the call-side prune
    // has an import to contradict against. Non-std externals (anyhow above) keep
    // the ordinary bare-name path with no marker.
    let hashmap = imports
        .iter()
        .find(|r| r.target_name == "HashMap")
        .expect("std::collections::HashMap must emit an external-marked import");
    assert!(
        crate::domain::is_external_import_meta(hashmap.metadata.as_deref()),
        "std-rooted import must carry the external marker, got {:?}",
        hashmap.metadata
    );
    let anyhow_result = imports.iter().find(|r| r.target_name == "Result").unwrap();
    assert!(
        !crate::domain::is_external_import_meta(anyhow_result.metadata.as_deref()),
        "a non-std crate is indistinguishable from a workspace sibling — it must \
         NOT be marked external, got {:?}",
        anyhow_result.metadata
    );
}

#[test]
fn test_rust_std_root_use_binds_external_not_project() {
    // Audit 2026-07-24 (map phantom edges): every `use std::fs;` in the repo
    // emitted a bare `imports → fs` relation that global bare-name resolution
    // bound to the single project symbol named `fs` — a #[cfg(test)] helper in
    // an unrelated module — fabricating 13 cross-module import edges. Roots
    // statically known external (`std`/`core`/`alloc`/`proc_macro`) can never
    // resolve inside the project, in any shape (simple / grouped / aliased /
    // leading `::` / root-level use-list). v52 dropped them; v53 marks them so
    // they bind `<external>` instead — see `domain::IMPORT_EXTERNAL_META`.
    // `crate::`-rooted and unknown-crate roots keep the bare-name path.
    let source = r#"
use std::fs;
use std::collections::{HashMap, HashSet};
use std::io::Read as _;
use core::fmt::Debug;
use alloc::vec::Vec;
use ::std::mem::swap;
use {std::io::Write, crate::a::cb};
use crate::domain::normalize;
use somecrate::helpers::assist;

fn main() {}
"#;
    let tree = crate::parser::treesitter::parse_tree(source, "rust").unwrap();
    let relations = extract_relations_from_tree(&tree, source, "rust");
    let imports: Vec<&ParsedRelation> = relations
        .iter()
        .filter(|r| r.relation == REL_IMPORTS)
        .collect();
    let named = |n: &str| imports.iter().find(|r| r.target_name == n).copied();
    let names = || {
        imports
            .iter()
            .map(|r| r.target_name.as_str())
            .collect::<Vec<_>>()
    };

    // `swap` covers the leading-`::` form (P2-1) and is the exact name whose
    // call-side phantom the sentinel edge lets the prune pass remove.
    // `Write` covers the root-level use-list form (P2-2), whose members used to
    // be classified against the whole braced text and so never matched a root.
    for external in [
        "fs", "HashMap", "HashSet", "Read", "Debug", "Vec", "swap", "Write",
    ] {
        let rel = named(external)
            .unwrap_or_else(|| panic!("`{external}` must emit an import, got: {:?}", names()));
        assert!(
            crate::domain::is_external_import_meta(rel.metadata.as_deref()),
            "std/core/alloc-rooted `{external}` must be marked external so it \
             binds the sentinel and not a same-named project symbol; got {:?}",
            rel.metadata
        );
    }
    // A mixed root-level list must get a MIXED verdict, not one all-or-nothing
    // decision for the declaration.
    for internal in ["cb", "normalize", "assist"] {
        let rel = named(internal).unwrap_or_else(|| {
            panic!(
                "project/unknown-crate import `{internal}` must still extract, got: {:?}",
                names()
            )
        });
        assert!(
            !crate::domain::is_external_import_meta(rel.metadata.as_deref()),
            "`{internal}` is not statically external; got {:?}",
            rel.metadata
        );
    }
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
    let imports: Vec<&ParsedRelation> = relations
        .iter()
        .filter(|r| r.relation == REL_IMPORTS)
        .collect();
    assert!(
        imports.iter().any(|r| r.target_name == "fmt"),
        "should import fmt, got: {:?}",
        imports.iter().map(|r| &r.target_name).collect::<Vec<_>>()
    );
    assert!(
        imports.iter().any(|r| r.target_name == "http"),
        "should import http, got: {:?}",
        imports.iter().map(|r| &r.target_name).collect::<Vec<_>>()
    );
}

#[test]
fn test_extract_rust_grouped_use_imports() {
    // Non-std crate root: std-root declarations are skipped whole as of IDX
    // v52, so grouped/nested/aliased coverage must ride a resolvable root.
    let source = r#"
use mylib::collections::{HashMap, HashSet, BTreeMap};
use mylib::io::Read as _;

fn main() {}
"#;
    let tree = crate::parser::treesitter::parse_tree(source, "rust").unwrap();
    let relations = extract_relations_from_tree(&tree, source, "rust");
    let imports: Vec<&str> = relations
        .iter()
        .filter(|r| r.relation == REL_IMPORTS)
        .map(|r| r.target_name.as_str())
        .collect();
    assert!(
        imports.contains(&"HashMap"),
        "should import HashMap, got: {:?}",
        imports
    );
    assert!(
        imports.contains(&"HashSet"),
        "should import HashSet, got: {:?}",
        imports
    );
    assert!(
        imports.contains(&"BTreeMap"),
        "should import BTreeMap, got: {:?}",
        imports
    );
    assert!(
        imports.contains(&"Read"),
        "should import Read (not 'Read as _'), got: {:?}",
        imports
    );
    // Should NOT contain braces or 'as _'
    assert!(
        !imports.iter().any(|i| i.contains('{')),
        "should not have brace in import names: {:?}",
        imports
    );
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
    let routes: Vec<&ParsedRelation> = relations
        .iter()
        .filter(|r| r.relation == REL_ROUTES_TO)
        .collect();
    assert!(
        routes.is_empty(),
        "should not detect route from @cache.get, got: {:?}",
        routes
            .iter()
            .map(|r| (&r.source_name, &r.target_name))
            .collect::<Vec<_>>()
    );
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
    let routes: Vec<&ParsedRelation> = relations
        .iter()
        .filter(|r| r.relation == REL_ROUTES_TO)
        .collect();
    assert!(
        routes.is_empty(),
        "should not detect route from @login_required, got: {:?}",
        routes
            .iter()
            .map(|r| (&r.source_name, &r.target_name))
            .collect::<Vec<_>>()
    );
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
    let routes: Vec<&ParsedRelation> = relations
        .iter()
        .filter(|r| r.relation == REL_ROUTES_TO)
        .collect();
    assert!(
        !routes.is_empty(),
        "should detect route from @app.get, got no routes"
    );
    assert!(
        routes[0].target_name == "list_items",
        "target should be list_items"
    );
}

/// Regression: Python `call` nodes (tree-sitter uses `call`, not `call_expression`)
/// must produce REL_CALLS edges. Previously dropped because the extractor only
/// handled `"call_expression"` and `"call" if config.name == "ruby"`, leaving
/// Python in the default-no-match path. README documents Python as "Full" tier
/// (calls + imports + inheritance + routes + test markers) — without this,
/// every Python project shows caller_count=0 and false-positive dead code.
#[test]
fn test_extract_python_bare_call() {
    let code = "def caller():\n    return used_fn()\n";
    let relations = extract_relations(code, "python").unwrap();
    let calls: Vec<(&str, &str)> = relations
        .iter()
        .filter(|r| r.relation == REL_CALLS)
        .map(|r| (r.source_name.as_str(), r.target_name.as_str()))
        .collect();
    assert!(
        calls.iter().any(|(s, t)| *s == "caller" && *t == "used_fn"),
        "Python bare call `used_fn()` inside `caller` should emit REL_CALLS edge; got: {:?}",
        calls
    );
}

#[test]
fn test_extract_python_method_call() {
    // obj.method() — Python `attribute` node inside `call.function`.
    let code = "def caller(obj):\n    return obj.method()\n";
    let relations = extract_relations(code, "python").unwrap();
    let calls: Vec<(&str, &str)> = relations
        .iter()
        .filter(|r| r.relation == REL_CALLS)
        .map(|r| (r.source_name.as_str(), r.target_name.as_str()))
        .collect();
    assert!(calls.iter().any(|(s, t)| *s == "caller" && *t == "method"),
        "Python method call `obj.method()` inside `caller` should emit REL_CALLS edge with target=method; got: {:?}", calls);
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
    let impls: Vec<(&str, &str)> = relations
        .iter()
        .filter(|r| r.relation == REL_IMPLEMENTS)
        .map(|r| (r.source_name.as_str(), r.target_name.as_str()))
        .collect();
    // Type-level: MyStruct implements MyTrait
    assert!(
        impls.contains(&("MyStruct", "MyTrait")),
        "got implements: {:?}",
        impls
    );
    // Method-level: MyStruct → do_thing, MyStruct → other
    assert!(
        impls.contains(&("MyStruct", "do_thing")),
        "method-level edge missing for do_thing: {:?}",
        impls
    );
    assert!(
        impls.contains(&("MyStruct", "other")),
        "method-level edge missing for other: {:?}",
        impls
    );
    assert_eq!(
        impls.len(),
        3,
        "expected 3 implements edges (1 type + 2 methods), got: {:?}",
        impls
    );
}

#[test]
fn test_extract_rust_impl_trait_generic_type_strips_params() {
    // Regression: `impl<'a, W: Write> Write for CapWriter<'a, W>` stored
    // source_name as the verbatim "CapWriter<'a, W>" text from the tree-sitter
    // type field. Phase 2 source resolution does exact name match against
    // local node names ("CapWriter"), so source_ids ended up empty and no
    // implements edges emitted — every method on a generic trait impl
    // appeared as dead code. Strip generics so source_name is the bare type.
    let source = r#"
trait MyTrait { fn do_thing(&self); }
struct Generic<'a, W: std::io::Write>(&'a W);
impl<'a, W: std::io::Write> MyTrait for Generic<'a, W> {
    fn do_thing(&self) {}
}
"#;
    let relations = extract_relations(source, "rust").unwrap();
    let impls: Vec<(&str, &str)> = relations
        .iter()
        .filter(|r| r.relation == REL_IMPLEMENTS)
        .map(|r| (r.source_name.as_str(), r.target_name.as_str()))
        .collect();
    assert!(
        impls.contains(&("Generic", "MyTrait")),
        "type-level edge must use bare struct name (no generics); got: {:?}",
        impls
    );
    assert!(
        impls.contains(&("Generic", "do_thing")),
        "method-level edge must use bare struct name; got: {:?}",
        impls
    );
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
    let impls: Vec<_> = relations
        .iter()
        .filter(|r| r.relation == REL_IMPLEMENTS)
        .collect();
    assert!(
        impls.is_empty(),
        "bare impl should produce no implements relations, got: {:?}",
        impls
            .iter()
            .map(|r| (&r.source_name, &r.target_name))
            .collect::<Vec<_>>()
    );
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
    let calls: Vec<(&str, &str)> = relations
        .iter()
        .filter(|r| r.relation == REL_CALLS)
        .map(|r| (r.source_name.as_str(), r.target_name.as_str()))
        .collect();
    assert!(
        calls.contains(&("build_config", "Config")),
        "struct instantiation should create calls edge, got: {:?}",
        calls
    );
}

#[test]
fn test_rust_scoped_struct_instantiation() {
    let source = r#"
fn create() {
    let node = crate::parser::NodeRecord { name: "foo".into() };
}
"#;
    let relations = extract_relations(source, "rust").unwrap();
    let calls: Vec<(&str, &str)> = relations
        .iter()
        .filter(|r| r.relation == REL_CALLS)
        .map(|r| (r.source_name.as_str(), r.target_name.as_str()))
        .collect();
    // Should strip path prefix, keeping just "NodeRecord"
    assert!(
        calls.contains(&("create", "NodeRecord")),
        "scoped struct should strip path, got: {:?}",
        calls
    );
}

#[test]
fn test_rust_path_reference_to_const_emits_references_edge() {
    let src = r#"
fn build() -> String {
    let w = crate::domain::SHARED;
    w.to_string()
}
"#;
    let rels = extract_relations(src, "rust").unwrap();
    let has_ref = rels
        .iter()
        .any(|r| r.relation == REL_REFERENCES && r.target_name == "SHARED");
    assert!(
        has_ref,
        "expected a references edge to SHARED; got: {:?}",
        rels.iter()
            .map(|r| (r.relation.as_str(), r.target_name.as_str()))
            .collect::<Vec<_>>()
    );
}

#[test]
fn test_rust_path_reference_as_call_argument_emits_references_edge() {
    // A path-qualified value passed as a call ARGUMENT (not the callee) is a
    // real value usage and MUST still emit a references edge — distinct from
    // the callee case, where the `function`-field path is already a calls edge.
    let src = r#"fn f() { do_thing(crate::domain::CB); }"#;
    let rels = extract_relations(src, "rust").unwrap();
    assert!(
        rels.iter()
            .any(|r| r.relation == REL_REFERENCES && r.target_name == "CB"),
        "arg-position path must emit a references edge to CB; got: {:?}",
        rels.iter()
            .map(|r| (r.relation.as_str(), r.target_name.as_str()))
            .collect::<Vec<_>>()
    );
}

#[test]
fn test_rust_path_reference_call_callee_does_not_emit_references_edge() {
    let src = r#"fn build() { crate::domain::compute(); }"#;
    let rels = extract_relations(src, "rust").unwrap();
    assert!(
        rels.iter()
            .any(|r| r.relation == REL_CALLS && r.target_name == "compute"),
        "call must be a calls edge; got: {:?}",
        rels.iter()
            .map(|r| (r.relation.as_str(), r.target_name.as_str()))
            .collect::<Vec<_>>()
    );
    assert!(
        !rels
            .iter()
            .any(|r| r.relation == REL_REFERENCES && r.target_name == "compute"),
        "a called fn must NOT also be a references edge; got: {:?}",
        rels.iter()
            .map(|r| (r.relation.as_str(), r.target_name.as_str()))
            .collect::<Vec<_>>()
    );
}

#[test]
fn test_rust_macro_arg_call_emits_calls_edge() {
    // tree-sitter parses macro arguments as opaque token_trees — `normalize(...)`
    // inside assert_eq! has no call_expression node, so calls made only through
    // macros were invisible: their targets showed as dead code and impact/
    // callgraph missed the calling fn (field failure 2026-07-24).
    let src = r#"
fn check() {
    assert_eq!(normalize(1, 2), 3);
}
"#;
    let rels = extract_relations(src, "rust").unwrap();
    assert!(
        rels.iter().any(|r| r.relation == REL_CALLS
            && r.source_name == "check"
            && r.target_name == "normalize"),
        "call inside macro args must emit a calls edge, got: {:?}",
        rels.iter()
            .map(|r| (
                r.relation.as_str(),
                r.source_name.as_str(),
                r.target_name.as_str()
            ))
            .collect::<Vec<_>>()
    );
}

#[test]
fn test_rust_macro_rules_body_call_emits_calls_edge() {
    // The cmd_stats shape: a fn-local macro_rules! whose rule body calls a fn.
    // The body is a token_tree, so `bail_out(0)` had no edge and `impact
    // bail_out` missed `run` as a caller.
    let src = r#"
fn run() {
    macro_rules! sout {
        ($($a:tt)*) => {
            if let Err(e) = writeln!(out, $($a)*) {
                bail_out(0);
            }
        };
    }
    sout!("x");
}
"#;
    let rels = extract_relations(src, "rust").unwrap();
    assert!(
        rels.iter().any(|r| r.relation == REL_CALLS
            && r.source_name == "run"
            && r.target_name == "bail_out"),
        "call inside macro_rules body must emit a calls edge, got: {:?}",
        rels.iter()
            .map(|r| (
                r.relation.as_str(),
                r.source_name.as_str(),
                r.target_name.as_str()
            ))
            .collect::<Vec<_>>()
    );
    // The macro's own name (`writeln!`, `sout!`) is not a fn call.
    assert!(
        !rels
            .iter()
            .any(|r| r.relation == REL_CALLS && r.target_name == "writeln"),
        "macro names must not become calls edges"
    );
}

#[test]
fn test_rust_macro_token_call_exclusions() {
    // Token-soup shapes that LOOK call-adjacent but must not emit calls edges:
    // method tails (unknown receiver aliases same-named fns), `$fragment(...)`,
    // path tails after `::` (v1 skips them — std paths dominate), and
    // macro-generated `fn` definitions.
    let src = r#"
fn run() {
    assert!(x.compute());
    assert!(crate::util::helper(1));
    macro_rules! gen {
        ($f:ident) => { $f(1) };
        () => { fn generated() {} };
    }
}
"#;
    let rels = extract_relations(src, "rust").unwrap();
    for banned in ["compute", "helper", "f", "generated"] {
        assert!(
            !rels
                .iter()
                .any(|r| r.relation == REL_CALLS && r.target_name == banned),
            "{banned} must not get a calls edge from macro tokens, got: {:?}",
            rels.iter()
                .filter(|r| r.relation == REL_CALLS)
                .map(|r| (r.source_name.as_str(), r.target_name.as_str()))
                .collect::<Vec<_>>()
        );
    }
}

#[test]
fn test_rust_macro_pattern_match_not_a_call() {
    // matches!/assert!(matches!(…)) put PATTERNS in macro args: `Some(y)`
    // parses identically to a call inside the opaque token_tree (audit
    // 2026-07-24 — this fabricated a calls→Some edge and could mark a
    // pattern-only variant as "live"). Uppercase-initial names are
    // variant/type constructors-or-patterns, never the snake_case fn calls
    // this pass recovers, so they must not emit calls edges.
    let src = r#"
fn check(x: Option<u32>) -> bool {
    matches!(x, Some(y) if y > 0)
}
fn guard(e: &MyEnum) -> bool {
    assert!(matches!(e, MyEnum::Variant(_) | Wrapped(_)));
    debug_assert!(matches!(e, Boxed(inner) if inner.ready()));
    true
}
"#;
    let rels = extract_relations(src, "rust").unwrap();
    for banned in ["Some", "Variant", "Wrapped", "Boxed"] {
        assert!(
            !rels
                .iter()
                .any(|r| r.relation == REL_CALLS && r.target_name == banned),
            "pattern {banned} must not get a calls edge from macro tokens, got: {:?}",
            rels.iter()
                .filter(|r| r.relation == REL_CALLS)
                .map(|r| (r.source_name.as_str(), r.target_name.as_str()))
                .collect::<Vec<_>>()
        );
    }
}

#[test]
fn test_rust_lowercase_pattern_in_matches_is_not_a_call() {
    // The CamelCase skip is a CONVENTION check, not a structural one: a crate
    // that carries `#[allow(non_camel_case_types)]` (bindgen output, C-ABI enum
    // mirrors) has lowercase tuple variants, and `matches!(x, ok(v))` walks
    // straight past the uppercase guard into a fabricated `calls → ok` edge —
    // pointing at whichever same-language `fn ok` the resolver likes.
    //
    // Pattern position IS structural here: everything after the first top-level
    // `,` of a matches!-family macro is a pattern. The guard is the counterweight
    // — `if is_ready(v)` after the pattern is an EXPRESSION, and swallowing it
    // would repeat the over-collection that cost real edges in the
    // value-reference pass (audit 2026-07-28).
    let src = r#"
fn check(x: Thing) -> bool {
    matches!(x, ok(v) if is_ready(v))
}
fn wrapped(x: Thing) -> bool {
    assert!(matches!(x, err(code) if reportable(code)));
    true
}
fn scrutinee(x: Thing) -> bool {
    matches!(compute(x), ok(_))
}
"#;
    let rels = extract_relations(src, "rust").unwrap();
    let calls: Vec<(&str, &str)> = rels
        .iter()
        .filter(|r| r.relation == REL_CALLS)
        .map(|r| (r.source_name.as_str(), r.target_name.as_str()))
        .collect();
    for banned in ["ok", "err"] {
        assert!(
            !calls.iter().any(|(_, t)| *t == banned),
            "lowercase pattern {banned} must not get a calls edge, got: {calls:?}"
        );
    }
    // Guard expressions and the scrutinee are code and keep their edges.
    for kept in ["is_ready", "reportable", "compute"] {
        assert!(
            calls.iter().any(|(_, t)| *t == kept),
            "{kept} is an expression, not a pattern — its edge must survive, got: {calls:?}"
        );
    }
}

#[test]
fn test_rust_top_level_macro_call_not_indexed() {
    // Parity with the call_expression arm: Rust calls with no enclosing named
    // scope are deliberately not indexed (no bare top-level statements in Rust;
    // a <module> edge here would be noise the graph commands never resolve).
    let src = r#"
lazy_static! {
    static ref X: u32 = build_x();
}
"#;
    let rels = extract_relations(src, "rust").unwrap();
    assert!(
        !rels
            .iter()
            .any(|r| r.relation == REL_CALLS && r.target_name == "build_x"),
        "top-level macro-body call must not emit a calls edge"
    );
}

#[test]
fn test_rust_path_reference_struct_expr_path_does_not_emit_references_edge() {
    // Path-qualified struct instantiation uses `scoped_type_identifier`, whose
    // inner segments are `scoped_identifier` nodes. They must NOT leak a
    // `references` edge to an intermediate path segment ("parser") — that path
    // is already covered by the `calls` edge to the struct ("NodeRecord").
    let src = r#"fn create() { let node = crate::parser::NodeRecord { name: 1 }; }"#;
    let rels = extract_relations(src, "rust").unwrap();
    assert!(
        !rels.iter().any(|r| r.relation == REL_REFERENCES),
        "struct-expr type path must not emit references edges; got: {:?}",
        rels.iter()
            .map(|r| (r.relation.as_str(), r.target_name.as_str()))
            .collect::<Vec<_>>()
    );
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
    assert!(
        relations
            .iter()
            .any(|r| r.relation == REL_ROUTES_TO && r.target_name == "healthCheck"),
        "got relations: {:?}",
        relations
            .iter()
            .map(|r| (&r.relation, &r.target_name))
            .collect::<Vec<_>>()
    );
}

/// axum builder-chain route extraction (roadmap 2026-07-18 §2.1): every
/// `.route(path, get(h).post(h2))` link emits one routes_to per (method, handler).
#[test]
fn test_extract_axum_routes() {
    let code = r#"
use axum::{routing::get, Router};

async fn list_users() {}
async fn create_user() {}
async fn health() {}

fn app() -> Router {
    Router::new()
        .route("/users", get(list_users).post(create_user))
        .route("/health", get(health))
}
"#;
    let relations = extract_relations(code, "rust").unwrap();
    let routes: Vec<(&str, &str)> = relations
        .iter()
        .filter(|r| r.relation == REL_ROUTES_TO)
        .map(|r| (r.metadata.as_deref().unwrap_or(""), r.target_name.as_str()))
        .collect();
    assert!(
        routes.iter().any(|(m, t)| m.contains(r#""method":"GET"#)
            && m.contains("/users")
            && *t == "list_users"),
        "GET /users -> list_users missing; got routes: {:?}",
        routes
    );
    assert!(
        routes.iter().any(|(m, t)| m.contains(r#""method":"POST"#)
            && m.contains("/users")
            && *t == "create_user"),
        "POST /users -> create_user (chained method router) missing; got routes: {:?}",
        routes
    );
    assert!(
        routes
            .iter()
            .any(|(m, t)| m.contains("/health") && *t == "health"),
        "GET /health -> health missing; got routes: {:?}",
        routes
    );
}

/// Inline `.nest("/prefix", Router::new().route(...))` composes the prefix onto
/// the nested paths. Cross-variable nest (a router built elsewhere) is a
/// documented non-goal — no dataflow resolution at parse time.
#[test]
fn test_extract_axum_nested_route_prefix() {
    let code = r#"
use axum::{routing::get, Router};

async fn list_users() {}

fn app() -> Router {
    Router::new().nest("/api", Router::new().route("/users", get(list_users)))
}
"#;
    let relations = extract_relations(code, "rust").unwrap();
    let routes: Vec<&str> = relations
        .iter()
        .filter(|r| r.relation == REL_ROUTES_TO)
        .map(|r| r.metadata.as_deref().unwrap_or(""))
        .collect();
    assert!(
        routes.iter().any(|m| m.contains(r#""path":"/api/users""#)),
        "nested prefix must compose to /api/users; got: {:?}",
        routes
    );
}

/// Path-qualified handlers and method fns resolve to their last segment:
/// `.route("/u", axum::routing::get(handlers::list_users))` → list_users.
#[test]
fn test_extract_axum_scoped_handler_and_method() {
    let code = r#"
fn app() -> axum::Router {
    axum::Router::new().route("/u", axum::routing::get(handlers::list_users))
}
"#;
    let relations = extract_relations(code, "rust").unwrap();
    let routes: Vec<(&str, &str)> = relations
        .iter()
        .filter(|r| r.relation == REL_ROUTES_TO)
        .map(|r| (r.metadata.as_deref().unwrap_or(""), r.target_name.as_str()))
        .collect();
    assert!(
        routes
            .iter()
            .any(|(m, t)| m.contains(r#""method":"GET"#) && *t == "list_users"),
        "scoped method fn + scoped handler must resolve; got: {:?}",
        routes
    );
}

/// ESM namespace import `import * as ns from './m'` (roadmap 2026-07-18 §2.3):
/// must emit a q:"ns_import" REL_IMPORTS marker carrying the alias + specifier
/// (was silently dropped — the import_clause walk only knew named specifiers).
#[test]
fn test_extract_ts_namespace_import_marker() {
    let code =
        "import * as helpers from './helpers';\nexport function run() { return helpers.fmt(); }\n";
    let relations = extract_relations(code, "typescript").unwrap();
    let marker = relations.iter().find(|r| {
        r.relation == REL_IMPORTS
            && r.metadata
                .as_deref()
                .is_some_and(|m| m.contains("ns_import"))
    });
    let marker = marker.unwrap_or_else(|| {
        panic!(
            "namespace import must emit a ns_import marker; got: {:?}",
            relations
                .iter()
                .map(|r| (&r.relation, &r.target_name, &r.metadata))
                .collect::<Vec<_>>()
        )
    });
    assert_eq!(
        marker.target_name, "helpers",
        "marker must carry the ALIAS (ns_module_map key)"
    );
    assert!(
        marker.metadata.as_deref().unwrap().contains("./helpers"),
        "specifier must ride along"
    );
    // The old path must NOT also emit a garbage `* as helpers` name.
    assert!(
        !relations.iter().any(|r| r.target_name.contains('*')),
        "no star-shaped garbage names; got: {:?}",
        relations.iter().map(|r| &r.target_name).collect::<Vec<_>>()
    );
}

/// Star re-export `export * from './m'` (barrel wildcard): must emit a
/// q:"star_reexport" module-level dependency marker (was ZERO edges — the
/// barrel was invisible to deps/affected/cycles).
#[test]
fn test_extract_ts_star_reexport_marker() {
    let code = "export * from './widgets';\nexport * as shapes from './shapes';\n";
    let relations = extract_relations(code, "typescript").unwrap();
    let stars: Vec<&ParsedRelation> = relations
        .iter()
        .filter(|r| {
            r.relation == REL_IMPORTS
                && r.metadata
                    .as_deref()
                    .is_some_and(|m| m.contains("star_reexport"))
        })
        .collect();
    assert!(
        stars
            .iter()
            .any(|r| r.metadata.as_deref().unwrap().contains("./widgets")),
        "export * from must emit a star_reexport marker; got: {:?}",
        relations
            .iter()
            .map(|r| (&r.relation, &r.target_name, &r.metadata))
            .collect::<Vec<_>>()
    );
    assert!(
        stars
            .iter()
            .any(|r| r.metadata.as_deref().unwrap().contains("./shapes")),
        "export * as ns from must too; got: {:?}",
        stars
            .iter()
            .map(|r| (&r.target_name, &r.metadata))
            .collect::<Vec<_>>()
    );
}

/// Negative: a bare `.get(...)` call outside `.route(...)` args (reqwest-style
/// clients, HashMap::get) must not fabricate routes_to edges.
#[test]
fn test_axum_no_false_positive_on_client_get() {
    let code = r#"
fn fetch(client: &Client, m: &std::collections::HashMap<String, String>) {
    let _r = client.get("/users");
    let _v = m.get("key");
}
"#;
    let relations = extract_relations(code, "rust").unwrap();
    assert!(
        !relations.iter().any(|r| r.relation == REL_ROUTES_TO),
        "bare .get calls must not create routes; got: {:?}",
        relations
            .iter()
            .filter(|r| r.relation == REL_ROUTES_TO)
            .map(|r| (&r.target_name, &r.metadata))
            .collect::<Vec<_>>()
    );
}

#[test]
fn test_extract_ts_implements() {
    let code = "class UserService implements IUserService {\n    getUser() { return null; }\n}\n";
    let relations = extract_relations(code, "typescript").unwrap();
    let impls: Vec<&str> = relations
        .iter()
        .filter(|r| r.relation == REL_IMPLEMENTS)
        .map(|r| r.target_name.as_str())
        .collect();
    assert!(
        impls.contains(&"IUserService"),
        "got implements: {:?}",
        impls
    );
}

#[test]
fn test_extract_java_implements() {
    let code = "public class ArrayList implements List, Serializable {\n}\n";
    let relations = extract_relations(code, "java").unwrap();
    let impls: Vec<&str> = relations
        .iter()
        .filter(|r| r.relation == REL_IMPLEMENTS)
        .map(|r| r.target_name.as_str())
        .collect();
    assert!(impls.contains(&"List"), "got implements: {:?}", impls);
}

#[test]
fn test_extract_ts_exports() {
    let code = "export function handleLogin(req: Request) {}\nexport class AuthService {}\n";
    let relations = extract_relations(code, "typescript").unwrap();
    let exports: Vec<&str> = relations
        .iter()
        .filter(|r| r.relation == REL_EXPORTS)
        .map(|r| r.target_name.as_str())
        .collect();
    assert!(
        exports.contains(&"handleLogin"),
        "got exports: {:?}",
        exports
    );
    assert!(
        exports.contains(&"AuthService"),
        "got exports: {:?}",
        exports
    );
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
    let calls: Vec<(&str, &str)> = relations
        .iter()
        .filter(|r| r.relation == REL_CALLS)
        .map(|r| (r.source_name.as_str(), r.target_name.as_str()))
        .collect();
    assert!(
        calls.contains(&("main", "Println")),
        "fmt.Println() should create call relation, got: {:?}",
        calls
    );
    assert!(
        calls.contains(&("main", "HandleFunc")),
        "http.HandleFunc() should create call relation, got: {:?}",
        calls
    );
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
    let calls: Vec<(&str, &str)> = relations
        .iter()
        .filter(|r| r.relation == REL_CALLS)
        .map(|r| (r.source_name.as_str(), r.target_name.as_str()))
        .collect();
    assert!(
        calls.contains(&("open", "open_impl")),
        "Self::open_impl() should create call relation, got: {:?}",
        calls
    );
    assert!(
        calls.contains(&("open_impl", "new")),
        "HashMap::new() should create call relation, got: {:?}",
        calls
    );
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
        eprintln!(
            "  {} --[{}]--> {}",
            r.source_name, r.relation, r.target_name
        );
    }
    let calls: Vec<(&str, &str)> = relations
        .iter()
        .filter(|r| r.relation == REL_CALLS)
        .map(|r| (r.source_name.as_str(), r.target_name.as_str()))
        .collect();
    eprintln!("Calls: {:?}", calls);
    assert!(
        calls.contains(&("test_func", "from_project_root")),
        "McpServer::from_project_root() should create call, got: {:?}",
        calls
    );
    assert!(
        calls.contains(&("test_func", "handle_message")),
        "server.handle_message() should create call, got: {:?}",
        calls
    );
    assert!(
        calls.contains(&("test_func", "tool_call_json")),
        "tool_call_json() should create call, got: {:?}",
        calls
    );
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
    let calls: Vec<(&str, &str)> = relations
        .iter()
        .filter(|r| r.relation == REL_CALLS)
        .map(|r| (r.source_name.as_str(), r.target_name.as_str()))
        .collect();
    assert!(
        calls.contains(&("run_serve", "from_project_root")),
        "McpServer::from_project_root() missing, got: {:?}",
        calls
    );
    assert!(
        calls.contains(&("run_serve", "set_notify_writer")),
        "server.set_notify_writer() missing, got: {:?}",
        calls
    );
    assert!(
        calls.contains(&("run_serve", "handle_message")),
        "server.handle_message() missing, got: {:?}",
        calls
    );
    assert!(
        calls.contains(&("run_serve", "run_startup_tasks")),
        "server.run_startup_tasks() missing, got: {:?}",
        calls
    );
    assert!(
        calls.contains(&("run_serve", "flush_metrics")),
        "server.flush_metrics() missing, got: {:?}",
        calls
    );
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
    let calls: Vec<(&str, &str)> = relations
        .iter()
        .filter(|r| r.relation == REL_CALLS)
        .map(|r| (r.source_name.as_str(), r.target_name.as_str()))
        .collect();
    // The scope for getUser should be "UserService.getUser", not just "getUser"
    assert!(
        calls
            .iter()
            .any(|(src, tgt)| *src == "UserService.getUser" && *tgt == "findById"),
        "getUser scope should be qualified as UserService.getUser, got calls: {:?}",
        calls
    );
    assert!(
        calls
            .iter()
            .any(|(src, tgt)| *src == "UserService.deleteUser" && *tgt == "getUser"),
        "deleteUser scope should be qualified as UserService.deleteUser, got calls: {:?}",
        calls
    );
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
    let calls: Vec<(&str, &str)> = relations
        .iter()
        .filter(|r| r.relation == REL_CALLS)
        .map(|r| (r.source_name.as_str(), r.target_name.as_str()))
        .collect();
    assert!(
        calls
            .iter()
            .any(|(src, tgt)| *src == "doWork" && *tgt == "process"),
        "standalone function scope should remain unqualified, got calls: {:?}",
        calls
    );
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
    let calls: Vec<(&str, &str)> = relations
        .iter()
        .filter(|r| r.relation == REL_CALLS)
        .map(|r| (r.source_name.as_str(), r.target_name.as_str()))
        .collect();
    eprintln!("All calls: {:?}", calls);
    assert!(
        calls.contains(&("main", "print_version")),
        "simple call should work, got: {:?}",
        calls
    );
    assert!(
        calls.contains(&("main", "cmd_show")),
        "deeply nested scoped call should extract rightmost name, got: {:?}",
        calls
    );
    assert!(
        calls.contains(&("main", "current_dir")),
        "std::env::current_dir() should extract current_dir, got: {:?}",
        calls
    );
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
    let calls: Vec<(&str, &str)> = relations
        .iter()
        .filter(|r| r.relation == REL_CALLS)
        .map(|r| (r.source_name.as_str(), r.target_name.as_str()))
        .collect();
    eprintln!("Match arm calls: {:?}", calls);
    // Note: Rust `impl` blocks don't set class context (unlike class {} in TS/JS),
    // so scope is just "handle_tool" not "Server.handle_tool"
    assert!(
        calls.contains(&("handle_tool", "tool_search")),
        "self.tool_search() in match arm should be detected, got: {:?}",
        calls
    );
    assert!(
        calls.contains(&("handle_tool", "tool_map")),
        "self.tool_map() in match arm should be detected, got: {:?}",
        calls
    );
    assert!(
        calls.contains(&("handle_tool", "log_result")),
        "self.log_result() outside match should be detected, got: {:?}",
        calls
    );
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
    let calls: Vec<(&str, &str)> = relations
        .iter()
        .filter(|r| r.relation == REL_CALLS)
        .map(|r| (r.source_name.as_str(), r.target_name.as_str()))
        .collect();
    eprintln!("All calls from handle_tool ({}):", calls.len());
    for (src, tgt) in &calls {
        eprintln!("  {} -> {}", src, tgt);
    }
    assert!(
        calls.iter().any(|(_, t)| *t == "tool_semantic_search"),
        "tool_semantic_search not found in: {:?}",
        calls
    );
    assert!(
        calls.iter().any(|(_, t)| *t == "tool_find_dead_code"),
        "tool_find_dead_code not found in: {:?}",
        calls
    );
    assert!(
        calls.iter().any(|(_, t)| *t == "lock_or_recover"),
        "lock_or_recover not found in: {:?}",
        calls
    );
    assert!(
        calls.iter().any(|(_, t)| *t == "record_tool_call"),
        "record_tool_call not found in: {:?}",
        calls
    );
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
    let inherits: Vec<&str> = relations
        .iter()
        .filter(|r| r.relation == REL_INHERITS || r.relation == REL_IMPLEMENTS)
        .map(|r| r.target_name.as_str())
        .collect();
    assert!(
        inherits.contains(&"BaseService"),
        "Kotlin: missing BaseService, got: {:?}",
        inherits
    );
    assert!(
        inherits.contains(&"Cloneable"),
        "Kotlin: missing Cloneable, got: {:?}",
        inherits
    );
}

#[test]
fn test_extract_swift_inheritance() {
    // Swift: `class S: BaseService, Codable` — comma-separated conformance.
    let code = "class UserService: BaseService, Codable, Hashable {\n    func foo() {}\n}\n";
    let relations = extract_relations(code, "swift").unwrap();
    let inherits: Vec<&str> = relations
        .iter()
        .filter(|r| r.relation == REL_INHERITS || r.relation == REL_IMPLEMENTS)
        .map(|r| r.target_name.as_str())
        .collect();
    assert!(
        inherits.contains(&"BaseService"),
        "Swift: missing BaseService, got: {:?}",
        inherits
    );
    assert!(
        inherits.contains(&"Codable"),
        "Swift: missing Codable, got: {:?}",
        inherits
    );
    assert!(
        inherits.contains(&"Hashable"),
        "Swift: missing Hashable, got: {:?}",
        inherits
    );
}

#[test]
fn test_extract_dart_inheritance() {
    // Dart has 3 inheritance keywords: extends (single), implements (multi),
    // with (mixin, multi). All conceptually contribute to type lineage.
    let code = "class UserService extends BaseService implements Loggable, Cacheable {\n  void foo() {}\n}\n";
    let relations = extract_relations(code, "dart").unwrap();
    let lineage: Vec<&str> = relations
        .iter()
        .filter(|r| r.relation == REL_INHERITS || r.relation == REL_IMPLEMENTS)
        .map(|r| r.target_name.as_str())
        .collect();
    assert!(
        lineage.contains(&"BaseService"),
        "Dart: missing BaseService (extends), got: {:?}",
        lineage
    );
    assert!(
        lineage.contains(&"Loggable"),
        "Dart: missing Loggable (implements), got: {:?}",
        lineage
    );
    assert!(
        lineage.contains(&"Cacheable"),
        "Dart: missing Cacheable (implements), got: {:?}",
        lineage
    );
}

#[test]
fn test_extract_php_inheritance() {
    // PHP: extends (single class) + implements (multiple interfaces).
    let code = "<?php\nclass UserService extends BaseService implements Loggable, Cacheable {\n    public function foo() {}\n}\n";
    let relations = extract_relations(code, "php").unwrap();
    let inherits: Vec<&str> = relations
        .iter()
        .filter(|r| r.relation == REL_INHERITS)
        .map(|r| r.target_name.as_str())
        .collect();
    let implements: Vec<&str> = relations
        .iter()
        .filter(|r| r.relation == REL_IMPLEMENTS)
        .map(|r| r.target_name.as_str())
        .collect();
    assert!(
        inherits.contains(&"BaseService"),
        "PHP: missing BaseService (extends), got INHERITS: {:?}",
        inherits
    );
    assert!(
        implements.contains(&"Loggable"),
        "PHP: missing Loggable (implements), got IMPLEMENTS: {:?}",
        implements
    );
    assert!(
        implements.contains(&"Cacheable"),
        "PHP: missing Cacheable (implements), got IMPLEMENTS: {:?}",
        implements
    );
}

#[test]
fn test_extract_ruby_inheritance() {
    let code = "class UserService < BaseService\n  def foo\n  end\nend\n";
    let relations = extract_relations(code, "ruby").unwrap();
    let inherits: Vec<&str> = relations
        .iter()
        .filter(|r| r.relation == REL_INHERITS)
        .map(|r| r.target_name.as_str())
        .collect();
    assert!(
        inherits.contains(&"BaseService"),
        "Ruby: missing BaseService, got: {:?}",
        inherits
    );
}

// --- Go struct/interface embedding → inherits (method promotion / iface composition) ---

fn go_inherits(code: &str) -> Vec<(String, String)> {
    extract_relations(code, "go")
        .unwrap()
        .iter()
        .filter(|r| r.relation == REL_INHERITS)
        .map(|r| (r.source_name.clone(), r.target_name.clone()))
        .collect()
}

#[test]
fn test_extract_go_struct_embedding() {
    // Embedded field (no field name) is Go's idiomatic "inheritance" (method
    // promotion); a normal named field is NOT.
    let code = "package p\ntype Animal struct{}\ntype Dog struct {\n\tAnimal\n\tName string\n}\n";
    let inh = go_inherits(code);
    assert!(
        inh.contains(&("Dog".into(), "Animal".into())),
        "Go: struct embedding should emit inherits Dog->Animal, got: {:?}",
        inh
    );
    assert!(
        !inh.iter().any(|(_, t)| t == "Name" || t == "string"),
        "Go: normal named field must not be inheritance, got: {:?}",
        inh
    );
}

#[test]
fn test_extract_go_interface_embedding() {
    // Embedded interfaces (type_elem) compose; methods (method_elem) do not.
    let code = "package p\ntype Reader interface{ Read() }\ntype Writer interface{ Write() }\ntype RW interface {\n\tReader\n\tWriter\n\tClose() error\n}\n";
    let inh = go_inherits(code);
    assert!(
        inh.contains(&("RW".into(), "Reader".into())),
        "Go: interface embedding should emit inherits RW->Reader, got: {:?}",
        inh
    );
    assert!(
        inh.contains(&("RW".into(), "Writer".into())),
        "Go: interface embedding should emit inherits RW->Writer, got: {:?}",
        inh
    );
    assert!(
        !inh.iter().any(|(_, t)| t == "Close" || t == "error"),
        "Go: interface method must not be inheritance, got: {:?}",
        inh
    );
}

#[test]
fn test_extract_go_pointer_and_qualified_embedding() {
    // Pointer embedding (*Base) and qualified embedding (pkg.Type) both promote
    // methods; bind on the simple type name (Base / Mutex).
    let code = "package p\ntype Base struct{}\ntype Sub struct {\n\t*Base\n\tsync.Mutex\n}\n";
    let inh = go_inherits(code);
    assert!(
        inh.contains(&("Sub".into(), "Base".into())),
        "Go: pointer embedding should emit inherits Sub->Base, got: {:?}",
        inh
    );
    assert!(
        inh.contains(&("Sub".into(), "Mutex".into())),
        "Go: qualified embedding should emit inherits Sub->Mutex, got: {:?}",
        inh
    );
}

#[test]
fn test_go_normal_field_not_inheritance() {
    let code = "package p\ntype Foo struct{}\ntype S struct {\n\tf Foo\n}\n";
    let inh = go_inherits(code);
    assert!(
        inh.is_empty(),
        "Go: a normal named field (f Foo) must produce no inherits, got: {:?}",
        inh
    );
}

#[test]
fn test_go_interface_typeset_not_inheritance() {
    // Go 1.18 type-set constraint: `interface { Signed | Unsigned }` is a UNION
    // (one type_elem with >1 child), NOT embedding — must emit no inherits edge.
    // A genuine embedded interface is one type_elem per parent (1 child each).
    let inh = go_inherits("package p\ntype Number interface {\n\tSigned | Unsigned\n}\n");
    assert!(
        inh.is_empty(),
        "Go: a type-set union constraint must not be inheritance, got: {:?}",
        inh
    );
    // ~int approximation element is also a constraint, not embedding.
    let inh2 = go_inherits("package p\ntype I interface {\n\t~int\n}\n");
    assert!(
        inh2.is_empty(),
        "Go: ~int approximation must not be inheritance, got: {:?}",
        inh2
    );
    // Sanity: genuine multi-parent embedding still works (regression guard).
    let inh3 = go_inherits("package p\ntype RW interface {\n\tReader\n\tWriter\n}\n");
    assert!(
        inh3.contains(&("RW".into(), "Reader".into()))
            && inh3.contains(&("RW".into(), "Writer".into())),
        "Go: genuine interface embedding must still emit inherits, got: {:?}",
        inh3
    );
}

#[test]
fn test_go_generic_embedding() {
    // Embedded generic types bind on the generic's base name.
    let s = go_inherits("package p\ntype Sub struct {\n\tBase[int]\n}\n");
    assert!(
        s.contains(&("Sub".into(), "Base".into())),
        "Go: embedded generic struct field Base[int] should emit inherits Sub->Base, got: {:?}",
        s
    );
    let i = go_inherits("package p\ntype X interface {\n\tContainer[int]\n}\n");
    assert!(i.contains(&("X".into(), "Container".into())),
        "Go: embedded generic interface Container[int] should emit inherits X->Container, got: {:?}", i);
}

// --- Dart mixins (`with M`) → inherits (mixin application injects methods) ---

#[test]
fn test_extract_dart_mixin() {
    let code = "class Base {}\nmixin M {}\nmixin N {}\nclass C extends Base with M, N {}\n";
    let inh: Vec<(String, String)> = extract_relations(code, "dart")
        .unwrap()
        .iter()
        .filter(|r| r.relation == REL_INHERITS)
        .map(|r| (r.source_name.clone(), r.target_name.clone()))
        .collect();
    assert!(
        inh.contains(&("C".into(), "Base".into())),
        "Dart: extends should emit inherits C->Base, got: {:?}",
        inh
    );
    assert!(
        inh.contains(&("C".into(), "M".into())),
        "Dart: mixin M should emit inherits C->M, got: {:?}",
        inh
    );
    assert!(
        inh.contains(&("C".into(), "N".into())),
        "Dart: mixin N should emit inherits C->N, got: {:?}",
        inh
    );
}

#[test]
fn test_extract_dart_mixin_only() {
    // No `extends`: the mixin must still bind to the bare mixin name, never the
    // malformed `"with M"` the text-clean fallback used to produce.
    let code = "mixin M {}\nclass C with M {}\n";
    let inh: Vec<String> = extract_relations(code, "dart")
        .unwrap()
        .iter()
        .filter(|r| r.relation == REL_INHERITS)
        .map(|r| r.target_name.clone())
        .collect();
    assert!(
        inh.contains(&"M".to_string()),
        "Dart: `with M` should emit inherits C->M, got: {:?}",
        inh
    );
    assert!(
        !inh.iter().any(|t| t.contains("with")),
        "Dart: mixin target must be the bare name, not `with M`, got: {:?}",
        inh
    );
}

// --- C++ base classes → inherits (C++ has no separate interface concept) ---

fn cpp_inherits(code: &str) -> Vec<(String, String)> {
    extract_relations(code, "cpp")
        .unwrap()
        .iter()
        .filter(|r| r.relation == REL_INHERITS)
        .map(|r| (r.source_name.clone(), r.target_name.clone()))
        .collect()
}

#[test]
fn test_extract_cpp_single_inheritance() {
    let inh = cpp_inherits("class Animal {};\nclass Dog : public Animal {};\n");
    assert!(
        inh.contains(&("Dog".into(), "Animal".into())),
        "C++: `class Dog : public Animal` should emit inherits Dog->Animal, got: {:?}",
        inh
    );
}

#[test]
fn test_extract_cpp_multiple_inheritance() {
    // Access specifiers (public/private/protected) are skipped; every base is inherits.
    let inh = cpp_inherits("class D : public A, private B, protected C {};\n");
    for base in ["A", "B", "C"] {
        assert!(
            inh.contains(&("D".into(), base.into())),
            "C++: multiple inheritance missing D->{}, got: {:?}",
            base,
            inh
        );
    }
}

#[test]
fn test_extract_cpp_struct_inheritance() {
    // struct default inheritance has no access_specifier node.
    let inh = cpp_inherits("struct Base {};\nstruct S : Base {};\n");
    assert!(
        inh.contains(&("S".into(), "Base".into())),
        "C++: `struct S : Base` should emit inherits S->Base, got: {:?}",
        inh
    );
}

#[test]
fn test_extract_cpp_qualified_and_template_base() {
    // Qualified (ns::Base) binds on the name tail; template base (Tmpl<int>) on the template name.
    let inh = cpp_inherits("class T : public ns::Base {};\nclass U : public Tmpl<int> {};\n");
    assert!(
        inh.contains(&("T".into(), "Base".into())),
        "C++: qualified base should bind T->Base, got: {:?}",
        inh
    );
    assert!(
        inh.contains(&("U".into(), "Tmpl".into())),
        "C++: template base should bind U->Tmpl, got: {:?}",
        inh
    );
}

#[test]
fn test_cpp_no_base_no_inheritance() {
    let inh = cpp_inherits("struct Point { int x; int y; };\nclass Empty {};\n");
    assert!(
        inh.is_empty(),
        "C++: a class/struct with no base clause must produce no inherits, got: {:?}",
        inh
    );
}

#[test]
fn test_c_struct_no_inheritance() {
    // Pure C has no inheritance concept; a C struct never carries a base clause.
    let inh: Vec<(String, String)> = extract_relations("struct Point { int x; int y; };\n", "c")
        .unwrap()
        .iter()
        .filter(|r| r.relation == REL_INHERITS)
        .map(|r| (r.source_name.clone(), r.target_name.clone()))
        .collect();
    assert!(inh.is_empty(), "C: no inheritance possible, got: {:?}", inh);
}

// --- Tier 2 calls + imports smoke tests (Phase C audit) ---

#[test]
fn test_extract_kotlin_calls() {
    let code = "fun process() {\n    fetch()\n    store()\n}\n";
    let relations = extract_relations(code, "kotlin").unwrap();
    let calls: Vec<&str> = relations
        .iter()
        .filter(|r| r.relation == REL_CALLS && r.source_name == "process")
        .map(|r| r.target_name.as_str())
        .collect();
    assert!(
        calls.contains(&"fetch"),
        "Kotlin: missing fetch call, got: {:?}",
        calls
    );
    assert!(
        calls.contains(&"store"),
        "Kotlin: missing store call, got: {:?}",
        calls
    );
}

#[test]
fn test_extract_kotlin_imports() {
    let code =
        "import com.example.UserService\nimport kotlinx.coroutines.flow.Flow\n\nfun process() {}\n";
    let relations = extract_relations(code, "kotlin").unwrap();
    let imports: Vec<&str> = relations
        .iter()
        .filter(|r| r.relation == REL_IMPORTS)
        .map(|r| r.target_name.as_str())
        .collect();
    assert!(
        !imports.is_empty(),
        "Kotlin: expected at least one IMPORTS edge, got 0 (relations: {:?})",
        relations
            .iter()
            .map(|r| (&r.relation, &r.target_name))
            .collect::<Vec<_>>()
    );
    assert!(
        imports
            .iter()
            .any(|i| i == &"UserService" || i.contains("UserService")),
        "Kotlin: missing UserService import, got: {:?}",
        imports
    );
}

#[test]
fn test_extract_swift_calls() {
    let code = "func process() {\n    fetch()\n    store()\n}\n";
    let relations = extract_relations(code, "swift").unwrap();
    let calls: Vec<&str> = relations
        .iter()
        .filter(|r| r.relation == REL_CALLS && r.source_name == "process")
        .map(|r| r.target_name.as_str())
        .collect();
    assert!(
        calls.contains(&"fetch"),
        "Swift: missing fetch call, got: {:?}",
        calls
    );
    assert!(
        calls.contains(&"store"),
        "Swift: missing store call, got: {:?}",
        calls
    );
}

#[test]
fn test_extract_swift_imports() {
    let code = "import Foundation\nimport UIKit\n\nfunc process() {}\n";
    let relations = extract_relations(code, "swift").unwrap();
    let imports: Vec<&str> = relations
        .iter()
        .filter(|r| r.relation == REL_IMPORTS)
        .map(|r| r.target_name.as_str())
        .collect();
    assert!(
        imports.contains(&"Foundation"),
        "Swift: missing Foundation, got: {:?}",
        imports
    );
    assert!(
        imports.contains(&"UIKit"),
        "Swift: missing UIKit, got: {:?}",
        imports
    );
}

#[test]
fn test_extract_dart_calls() {
    let code = "void process() {\n  fetch();\n  store();\n}\n";
    let relations = extract_relations(code, "dart").unwrap();
    let calls: Vec<&str> = relations
        .iter()
        .filter(|r| r.relation == REL_CALLS && r.source_name == "process")
        .map(|r| r.target_name.as_str())
        .collect();
    assert!(
        calls.contains(&"fetch"),
        "Dart: missing fetch call, got: {:?}",
        calls
    );
    assert!(
        calls.contains(&"store"),
        "Dart: missing store call, got: {:?}",
        calls
    );
}

#[test]
fn test_extract_dart_calls_in_non_statement_positions() {
    // Regression: Dart call extraction only fired on `expression_statement`
    // (`foo();`), so calls in return / assignment / argument / binary-expr
    // positions — the majority — were silently dropped. Now dispatched on the
    // `selector(argument_part)` node (callee = preceding sibling).
    let code = "\
String describe() {
  return \"x \" + sound();
}
int compute(int y) {
  return helper(y);
}
void build() {
  var d = make();
  obj.run();
  wrap(inner());
}
String arrow() => render();
";
    let relations = extract_relations(code, "dart").unwrap();
    let calls = |scope: &str| -> Vec<String> {
        relations
            .iter()
            .filter(|r| r.relation == REL_CALLS && r.source_name == scope)
            .map(|r| r.target_name.clone())
            .collect()
    };
    assert!(
        calls("describe").contains(&"sound".to_string()),
        "call inside `return \"x\" + sound()` (binary expr) must resolve; got: {:?}",
        calls("describe")
    );
    assert!(
        calls("compute").contains(&"helper".to_string()),
        "call inside `return helper(y)` must resolve; got: {:?}",
        calls("compute")
    );
    let b = calls("build");
    assert!(
        b.contains(&"make".to_string()),
        "call inside `var d = make()` must resolve; got: {:?}",
        b
    );
    assert!(
        b.contains(&"run".to_string()),
        "method call `obj.run()` must resolve to `run`; got: {:?}",
        b
    );
    assert!(
        b.contains(&"wrap".to_string()) && b.contains(&"inner".to_string()),
        "both outer `wrap(...)` and nested `inner()` must resolve; got: {:?}",
        b
    );
    assert!(
        calls("arrow").contains(&"render".to_string()),
        "call in arrow body `=> render()` must resolve; got: {:?}",
        calls("arrow")
    );
}

#[test]
fn test_extract_dart_top_level_function_symbol() {
    // Regression: top-level Dart functions parse as a bare function_signature
    // sibling under `program` (no `declaration` wrapper), so they were never
    // extracted as symbols — callgraph/impact/dead-code couldn't see them.
    let code = "int helper(int x) {\n  return x + 1;\n}\n\nclass C {\n  int m() => 1;\n}\n";
    let nodes = crate::parser::treesitter::parse_code(code, "dart").unwrap();
    let helper = nodes.iter().find(|n| n.name == "helper");
    assert!(
        helper.is_some(),
        "top-level Dart function `helper` must be a symbol node; got: {:?}",
        nodes
            .iter()
            .map(|n| (&n.node_type, &n.name))
            .collect::<Vec<_>>()
    );
    assert_eq!(
        helper.unwrap().node_type,
        "function",
        "top-level function should be type `function`"
    );
    // The class method must NOT be double-extracted as a top-level function.
    let m_count = nodes.iter().filter(|n| n.name == "m").count();
    assert_eq!(
        m_count, 1,
        "class method `m` must be extracted exactly once (no double-extract)"
    );
}

#[test]
fn test_extract_dart_imports() {
    let code =
        "import 'package:flutter/material.dart';\nimport 'dart:async';\n\nvoid process() {}\n";
    let relations = extract_relations(code, "dart").unwrap();
    let imports: Vec<&str> = relations
        .iter()
        .filter(|r| r.relation == REL_IMPORTS)
        .map(|r| r.target_name.as_str())
        .collect();
    assert!(
        !imports.is_empty(),
        "Dart: expected at least one IMPORTS edge, got 0 (relations: {:?})",
        relations
            .iter()
            .map(|r| (&r.relation, &r.target_name))
            .collect::<Vec<_>>()
    );
    assert!(
        imports
            .iter()
            .any(|i| i.contains("material") || i.contains("flutter")),
        "Dart: missing material/flutter import, got: {:?}",
        imports
    );
    assert!(
        imports
            .iter()
            .any(|i| i.contains("async") || i.contains("dart:async")),
        "Dart: missing async import, got: {:?}",
        imports
    );
}

#[test]
fn test_extract_php_calls() {
    let code = "<?php\nfunction process() {\n    fetch();\n    store();\n}\n";
    let relations = extract_relations(code, "php").unwrap();
    let calls: Vec<&str> = relations
        .iter()
        .filter(|r| r.relation == REL_CALLS && r.source_name == "process")
        .map(|r| r.target_name.as_str())
        .collect();
    assert!(
        calls.contains(&"fetch"),
        "PHP: missing fetch call, got: {:?}",
        calls
    );
    assert!(
        calls.contains(&"store"),
        "PHP: missing store call, got: {:?}",
        calls
    );
}

#[test]
fn test_extract_php_imports() {
    let code = "<?php\nuse App\\Services\\UserService;\nuse App\\Models\\Order;\n\nfunction process() {}\n";
    let relations = extract_relations(code, "php").unwrap();
    let imports: Vec<&str> = relations
        .iter()
        .filter(|r| r.relation == REL_IMPORTS)
        .map(|r| r.target_name.as_str())
        .collect();
    assert!(
        !imports.is_empty(),
        "PHP: expected at least one IMPORTS edge, got 0 (relations: {:?})",
        relations
            .iter()
            .map(|r| (&r.relation, &r.target_name))
            .collect::<Vec<_>>()
    );
    assert!(
        imports.iter().any(|i| i.contains("UserService")),
        "PHP: missing UserService, got: {:?}",
        imports
    );
    assert!(
        imports.iter().any(|i| i.contains("Order")),
        "PHP: missing Order, got: {:?}",
        imports
    );
}

#[test]
fn test_extract_ruby_calls() {
    // Parens force the `call` shape (handled by the `"call"` arm). Bare
    // statement-position names are ALSO extracted now (the ruby_bare_calls
    // pass — see test_extract_ruby_bare_calls_statement_position); bare names
    // in RHS/argument positions stay ambiguous and are intentionally skipped.
    let code = "def process\n  fetch()\n  store()\nend\n";
    let relations = extract_relations(code, "ruby").unwrap();
    let calls: Vec<&str> = relations
        .iter()
        .filter(|r| r.relation == REL_CALLS && r.source_name == "process")
        .map(|r| r.target_name.as_str())
        .collect();
    assert!(
        calls.contains(&"fetch"),
        "Ruby: missing fetch call, got: {:?}",
        calls
    );
    assert!(
        calls.contains(&"store"),
        "Ruby: missing store call, got: {:?}",
        calls
    );
}

#[test]
fn test_extract_ruby_bare_calls_statement_position() {
    // Parens-less calls in statement position now produce edges via the
    // dedicated ruby_bare_calls pass (INDEX_VERSION 27→28). `setup`/`helper`
    // are method calls; `x` (assigned) is a local and must NOT produce an edge.
    let code = "def entry\n  setup\n  helper\n  x = 5\n  x\nend\n\ndef setup\n  1\nend\n\ndef helper\n  2\nend\n";
    let relations = extract_relations(code, "ruby").unwrap();
    let calls: Vec<&str> = relations
        .iter()
        .filter(|r| r.relation == REL_CALLS && r.source_name == "entry")
        .map(|r| r.target_name.as_str())
        .collect();
    assert!(
        calls.contains(&"setup"),
        "bare `setup` call must be captured, got: {:?}",
        calls
    );
    assert!(
        calls.contains(&"helper"),
        "bare `helper` call must be captured, got: {:?}",
        calls
    );
    assert!(
        !calls.contains(&"x"),
        "local var `x` must NOT be a call, got: {:?}",
        calls
    );
}

#[test]
fn test_ruby_bare_calls_exclude_locals_params_blockparams() {
    // Flood-avoidance safety net (the whole reason bare-call extraction was
    // historically deferred): a local var whose name MATCHES a method name, a
    // method param, multiple-assignment targets, and a block param must NEVER
    // produce a bare-call edge — Ruby's own assigned-vs-call disambiguation.
    let code = "def config\n  9\nend\n\ndef process(data)\n  config = load()\n  config\n  data\n  a, b = pair()\n  a\n  b\n  items.each do |item|\n    item\n  end\n  go\nend\n\ndef go\n  1\nend\n";
    let relations = extract_relations(code, "ruby").unwrap();
    let bare: Vec<&str> = relations
        .iter()
        .filter(|r| r.relation == REL_CALLS && r.source_name == "process")
        .map(|r| r.target_name.as_str())
        .collect();
    assert!(
        bare.contains(&"go"),
        "bare `go` call must be captured, got: {:?}",
        bare
    );
    for local in ["config", "data", "a", "b", "item"] {
        assert!(
            !bare.contains(&local),
            "`{local}` is a local/param/block-param and must NOT be a bare call, got: {:?}",
            bare
        );
    }
}

#[test]
fn test_ruby_bare_call_nested_method_is_own_scope() {
    // A local in the OUTER method must not suppress a same-named bare call in a
    // NESTED method (nested defs are separate scopes with their own locals).
    let code = "def outer\n  helper = 1\n  helper\n  def inner\n    helper\n  end\nend\n\ndef helper\n  2\nend\n";
    let relations = extract_relations(code, "ruby").unwrap();
    // outer: `helper` is a local (assigned) → no edge.
    assert!(
        !relations.iter().any(|r| r.relation == REL_CALLS
            && r.source_name == "outer"
            && r.target_name == "helper"),
        "outer's `helper` is a local var, must not be a call"
    );
    // inner: `helper` is NOT bound in inner → a call.
    assert!(
        relations.iter().any(|r| r.relation == REL_CALLS
            && r.source_name == "inner"
            && r.target_name == "helper"),
        "inner's `helper` (unbound in inner's scope) must be a call"
    );
}

#[test]
fn test_extract_ruby_imports() {
    let code = "require 'json'\nrequire_relative 'helper'\n\ndef process\nend\n";
    let relations = extract_relations(code, "ruby").unwrap();
    let imports: Vec<&str> = relations
        .iter()
        .filter(|r| r.relation == REL_IMPORTS)
        .map(|r| r.target_name.as_str())
        .collect();
    assert!(
        imports.contains(&"json"),
        "Ruby: missing json (require), got: {:?}",
        imports
    );
    assert!(
        imports.contains(&"helper"),
        "Ruby: missing helper (require_relative), got: {:?}",
        imports
    );
}

#[test]
fn test_extract_csharp_calls() {
    let code = "class App {\n    void Process() {\n        Fetch();\n        Store();\n    }\n}\n";
    let relations = extract_relations(code, "csharp").unwrap();
    let calls: Vec<&str> = relations
        .iter()
        .filter(|r| r.relation == REL_CALLS)
        .map(|r| r.target_name.as_str())
        .collect();
    assert!(
        calls.contains(&"Fetch"),
        "C#: missing Fetch call, got: {:?}",
        calls
    );
    assert!(
        calls.contains(&"Store"),
        "C#: missing Store call, got: {:?}",
        calls
    );
}

#[test]
fn test_extract_csharp_imports() {
    let code = "using System;\nusing System.Collections.Generic;\n\nclass App {}\n";
    let relations = extract_relations(code, "csharp").unwrap();
    let imports: Vec<&str> = relations
        .iter()
        .filter(|r| r.relation == REL_IMPORTS)
        .map(|r| r.target_name.as_str())
        .collect();
    assert!(
        !imports.is_empty(),
        "C#: expected at least one IMPORTS edge, got 0 (relations: {:?})",
        relations
            .iter()
            .map(|r| (&r.relation, &r.target_name))
            .collect::<Vec<_>>()
    );
    assert!(
        imports
            .iter()
            .any(|i| i == &"System" || i.contains("System")),
        "C#: missing System import, got: {:?}",
        imports
    );
}

#[test]
fn test_extract_csharp_inheritance() {
    // C#: `class S : Base, IInterface` — current code uses IFoo prefix
    // heuristic to split into INHERITS (Base) vs IMPLEMENTS (IInterface).
    let code =
        "class UserService : BaseService, IDisposable, ICloneable {\n    public void Foo() {}\n}\n";
    let relations = extract_relations(code, "csharp").unwrap();
    let inherits: Vec<&str> = relations
        .iter()
        .filter(|r| r.relation == REL_INHERITS)
        .map(|r| r.target_name.as_str())
        .collect();
    let implements: Vec<&str> = relations
        .iter()
        .filter(|r| r.relation == REL_IMPLEMENTS)
        .map(|r| r.target_name.as_str())
        .collect();
    assert!(
        inherits.contains(&"BaseService"),
        "C#: missing BaseService (INHERITS), got: {:?}",
        inherits
    );
    assert!(
        implements.contains(&"IDisposable"),
        "C#: missing IDisposable (IMPLEMENTS), got: {:?}",
        implements
    );
    assert!(
        implements.contains(&"ICloneable"),
        "C#: missing ICloneable (IMPLEMENTS), got: {:?}",
        implements
    );
}

#[test]
fn test_rust_callee_path_qualifier_strips_crate() {
    let code = "fn caller() { crate::snapshot::create(); }";
    let relations = extract_relations(code, "rust").unwrap();
    let call = relations
        .iter()
        .find(|r| r.relation == REL_CALLS && r.target_name == "create")
        .expect("missing call to create");
    assert_eq!(
        call.metadata.as_deref(),
        Some(r#"{"q":"path","v":"snapshot"}"#),
        "metadata should encode Path qualifier with crate stripped"
    );
}

// T3: single-segment Type::method path
#[test]
fn test_rust_callee_type_method_call_path() {
    let code = r#"fn caller() { File::create("/tmp/x"); }"#;
    let relations = extract_relations(code, "rust").unwrap();
    let call = relations
        .iter()
        .find(|r| r.relation == REL_CALLS && r.target_name == "create")
        .expect("missing call to create");
    assert_eq!(
        call.metadata.as_deref(),
        Some(r#"{"q":"path","v":"File"}"#),
        "single-segment Path with non-reserved name should be preserved"
    );
}

// T4: reserved-only path collapses to bare
#[test]
fn test_rust_callee_crate_only_path_collapses_to_bare() {
    let code = "fn caller() { crate::foo(); }";
    let relations = extract_relations(code, "rust").unwrap();
    let call = relations
        .iter()
        .find(|r| r.relation == REL_CALLS && r.target_name == "foo")
        .expect("missing call to foo");
    assert_eq!(
        call.metadata, None,
        "crate::foo() qualifier collapses to Bare after stripping reserved prefix"
    );
}

// T5: super:: strip, multi-segment, chained reserved prefixes
#[test]
fn test_rust_callee_super_prefix_stripped() {
    // super:: must be stripped per reserved-prefix rule.
    let code = "fn caller() { super::sibling::foo(); }";
    let relations = extract_relations(code, "rust").unwrap();
    let call = relations
        .iter()
        .find(|r| r.relation == REL_CALLS && r.target_name == "foo")
        .expect("missing call to foo");
    assert_eq!(
        call.metadata.as_deref(),
        Some(r#"{"q":"path","v":"sibling"}"#),
    );
}

#[test]
fn test_rust_callee_multi_segment_path_preserved() {
    let code = "fn caller() { crate::a::b::c::deep(); }";
    let relations = extract_relations(code, "rust").unwrap();
    let call = relations
        .iter()
        .find(|r| r.relation == REL_CALLS && r.target_name == "deep")
        .expect("missing call to deep");
    assert_eq!(
        call.metadata.as_deref(),
        Some(r#"{"q":"path","v":"a::b::c"}"#),
    );
}

#[test]
fn test_rust_callee_chained_reserved_prefixes_stripped() {
    // Multiple consecutive reserved prefixes: ensure drain(..skip) consumes
    // ALL leading reserved segments, not just the first.
    let code = "fn caller() { super::super::foo(); }";
    let relations = extract_relations(code, "rust").unwrap();
    let call = relations
        .iter()
        .find(|r| r.relation == REL_CALLS && r.target_name == "foo")
        .expect("missing call to foo");
    assert_eq!(
        call.metadata, None,
        "two consecutive `super::` segments + bare name → fully stripped → Bare"
    );
}

#[test]
fn test_rust_callee_obj_method_receiver_qualifier() {
    let code = "fn caller(p: &std::path::Path) { p.exists(); }";
    let relations = extract_relations(code, "rust").unwrap();
    let call = relations
        .iter()
        .find(|r| r.relation == REL_CALLS && r.target_name == "exists")
        .expect("missing call to exists");
    assert_eq!(
        call.metadata.as_deref(),
        Some(r#"{"q":"recv","v":"p"}"#),
        "obj.method() where obj is a plain identifier emits Receiver qualifier"
    );
}

#[test]
fn test_rust_callee_builder_chain_qualifier() {
    let code = r#"fn caller() {
        OpenOptions::new().create(true).open("/tmp/x");
    }"#;
    let relations = extract_relations(code, "rust").unwrap();

    // OpenOptions::new() → Path
    let new_call = relations
        .iter()
        .find(|r| r.relation == REL_CALLS && r.target_name == "new")
        .expect("missing call to new");
    assert_eq!(
        new_call.metadata.as_deref(),
        Some(r#"{"q":"path","v":"OpenOptions"}"#),
    );

    // .create(true) — receiver is call_expression → Chain
    let create_call = relations
        .iter()
        .find(|r| r.relation == REL_CALLS && r.target_name == "create")
        .expect("missing call to create");
    assert_eq!(create_call.metadata.as_deref(), Some(r#"{"q":"chain"}"#),);

    // .open(...) — receiver is also call_expression → Chain
    let open_call = relations
        .iter()
        .find(|r| r.relation == REL_CALLS && r.target_name == "open")
        .expect("missing call to open");
    assert_eq!(open_call.metadata.as_deref(), Some(r#"{"q":"chain"}"#),);
}

#[test]
fn test_rust_callee_self_recv_within_impl() {
    let code = r#"
        struct Db;
        impl Db {
            fn caller(&self) { self.helper(); }
            fn helper(&self) {}
        }
    "#;
    let relations = extract_relations(code, "rust").unwrap();
    let call = relations
        .iter()
        .find(|r| r.relation == REL_CALLS && r.target_name == "helper")
        .expect("missing call to helper");
    assert_eq!(
        call.metadata.as_deref(),
        Some(r#"{"q":"self","v":"Db"}"#),
        "self.method() inside impl Db emits SelfRecv with type name"
    );
}

#[test]
fn test_rust_callee_self_type_within_impl() {
    let code = r#"
        struct Db;
        impl Db {
            fn make() -> Self { Self::default() }
        }
        impl Default for Db { fn default() -> Self { Db } }
    "#;
    let relations = extract_relations(code, "rust").unwrap();
    let call = relations
        .iter()
        .find(|r| {
            r.relation == REL_CALLS && r.target_name == "default" && r.source_name.contains("make")
        })
        .expect("missing call to default from make");
    assert_eq!(
        call.metadata.as_deref(),
        Some(r#"{"q":"stype","v":"Db"}"#),
        "Self::method() inside impl Db emits SelfType with type name"
    );
}

#[test]
fn test_js_simple_receiver_call_emits_recv_metadata() {
    // JS simple-identifier member calls (`foo.bar()`) carry a Receiver qualifier
    // so the indexer can bind them to a require-namespace module
    // (`const foo = require('./x')`); see Cycle 4. Bare calls (`baz()`) keep
    // metadata=None — the guard the previous test_non_rust_callee_metadata_
    // unchanged enforced, preserved here for the non-receiver shapes.
    let code = "function caller() { foo.bar(); baz(); }";
    let relations = extract_relations(code, "javascript").unwrap();
    let bar = relations
        .iter()
        .find(|r| r.relation == REL_CALLS && r.target_name == "bar")
        .expect("missing call bar");
    assert_eq!(
        bar.metadata.as_deref(),
        Some(r#"{"q":"recv","v":"foo"}"#),
        "foo.bar() must emit a Receiver qualifier for require-namespace resolution"
    );
    let baz = relations
        .iter()
        .find(|r| r.relation == REL_CALLS && r.target_name == "baz")
        .expect("missing call baz");
    assert_eq!(baz.metadata, None, "bare baz() must keep metadata=None");
}

#[test]
fn test_python_receiver_type_propagation_from_ctor_assignment() {
    // Issue #32 cause 2: `recv.method()` whose receiver is fixed by a single
    // local `recv = ClassName(...)` constructor assignment carries an rtype
    // qualifier so Phase-2 resolution binds it to ClassName.method instead of
    // dropping the ambiguous by-name fan-out (write defined on N classes).
    let code = r#"
class DataWriter:
    def write(self, x):
        return x

def save(x):
    writer = DataWriter()
    writer.write(x)
"#;
    let relations = extract_relations(code, "python").unwrap();
    let call = relations
        .iter()
        .find(|r| r.relation == REL_CALLS && r.target_name == "write")
        .expect("missing call to write");
    assert_eq!(
        call.metadata.as_deref(),
        Some(r#"{"q":"rtype","v":"DataWriter"}"#),
        "writer.write() with `writer = DataWriter()` must carry rtype=DataWriter"
    );
    // The constructor call itself is a bare identifier call — no rtype.
    let ctor = relations
        .iter()
        .find(|r| r.relation == REL_CALLS && r.target_name == "DataWriter")
        .expect("missing constructor call DataWriter");
    assert_eq!(
        ctor.metadata, None,
        "DataWriter() constructor call stays bare"
    );
}

#[test]
fn test_python_receiver_type_not_inferred_when_ambiguous_or_unknown() {
    // Reassignment to a different type, a parameter receiver, self.method(), and
    // a lower-case factory RHS must ALL stay metadata=None (bare) so a wrong-type
    // edge is never emitted — the inference only fires on a provably-single
    // constructor assignment.
    let code = r#"
class A:
    def run(self):
        return 1

class B:
    def run(self):
        return 2

def reassigned(flag):
    w = A()
    w = B()
    w.run()

def from_param(w):
    w.run()

def lower_factory():
    w = make_thing()
    w.run()

class Holder:
    def caller(self):
        self.run()
    def run(self):
        return 0
"#;
    let relations = extract_relations(code, "python").unwrap();
    let runs: Vec<_> = relations
        .iter()
        .filter(|r| r.relation == REL_CALLS && r.target_name == "run")
        .collect();
    assert!(
        !runs.is_empty(),
        "expected some run() calls to be extracted"
    );
    for r in &runs {
        assert_eq!(
            r.metadata, None,
            "ambiguous/unknown receiver must stay bare (source={}); got {:?}",
            r.source_name, r.metadata
        );
    }
}

#[test]
fn test_python_receiver_type_from_parameter_annotation() {
    // Issue #32 cause 2 extension: a receiver that is a parameter with an
    // explicit class annotation (`def f(w: DataWriter)`) carries the rtype
    // qualifier just like a local constructor assignment — the annotation is an
    // explicit, reliable type. Default-valued annotated params work too.
    let code = r#"
class DataWriter:
    def write(self, x):
        return x

def save(writer: DataWriter, x):
    writer.write(x)

def save_default(writer: DataWriter = None):
    writer.write(1)
"#;
    let relations = extract_relations(code, "python").unwrap();
    let writes: Vec<_> = relations
        .iter()
        .filter(|r| r.relation == REL_CALLS && r.target_name == "write")
        .collect();
    assert_eq!(
        writes.len(),
        2,
        "expected two write() calls; got {:?}",
        writes.iter().map(|r| &r.source_name).collect::<Vec<_>>()
    );
    for w in &writes {
        assert_eq!(
            w.metadata.as_deref(),
            Some(r#"{"q":"rtype","v":"DataWriter"}"#),
            "param-annotated receiver `writer: DataWriter` must carry rtype=DataWriter (source={})",
            w.source_name
        );
    }
}

#[test]
fn test_python_receiver_type_param_annotation_negatives() {
    // Un-annotated param, a builtin/lower-case annotation, and a param shadowed by
    // a later local reassignment must all stay bare (no wrong-type edge).
    let code = r#"
class A:
    def run(self):
        return 1

class B:
    def run(self):
        return 2

def no_annotation(w):
    w.run()

def builtin_annotation(w: str):
    w.run()

def reassigned(w: A):
    w = B()
    w.run()
"#;
    let relations = extract_relations(code, "python").unwrap();
    let runs: Vec<_> = relations
        .iter()
        .filter(|r| r.relation == REL_CALLS && r.target_name == "run")
        .collect();
    assert!(!runs.is_empty(), "expected some run() calls");
    for r in &runs {
        // `reassigned` has one local `w = B()` → that's the cause-2 ctor path, so
        // it legitimately carries rtype=B (the local reassignment wins, not the
        // stale param annotation A). The other two must be bare.
        if r.source_name.contains("reassigned") {
            assert_eq!(
                r.metadata.as_deref(),
                Some(r#"{"q":"rtype","v":"B"}"#),
                "local reassignment `w = B()` overrides the param annotation"
            );
        } else {
            assert_eq!(
                r.metadata, None,
                "un-annotated / builtin-annotated receiver must stay bare (source={}); got {:?}",
                r.source_name, r.metadata
            );
        }
    }
}

#[test]
fn test_rust_type_usage_emits_references_edge() {
    let src = r#"
struct AppConfig {
    widget: WidgetConfig,
}
fn make() -> WidgetConfig { WidgetConfig {} }
"#;
    let rels = extract_relations(src, "rust").unwrap();
    let ref_count = rels
        .iter()
        .filter(|r| r.relation == REL_REFERENCES && r.target_name == "WidgetConfig")
        .count();
    // Exactly 2: the field type + the return type. The `WidgetConfig {}`
    // struct-expr name is skipped (already a `calls` edge), and the struct's
    // own `name` (AppConfig) is not WidgetConfig.
    assert_eq!(
        ref_count,
        2,
        "expected exactly 2 references edges to WidgetConfig (field type + return type); got: {:?}",
        rels.iter()
            .map(|r| (r.relation.as_str(), r.target_name.as_str()))
            .collect::<Vec<_>>()
    );
}

#[test]
fn test_rust_type_definition_name_does_not_self_reference() {
    let src = r#"struct Foo { x: u32 }"#;
    let rels = extract_relations(src, "rust").unwrap();
    assert!(
        !rels
            .iter()
            .any(|r| r.relation == REL_REFERENCES && r.target_name == "Foo"),
        "a struct's own name must not be a references edge; got: {:?}",
        rels.iter()
            .map(|r| (r.relation.as_str(), r.target_name.as_str()))
            .collect::<Vec<_>>()
    );
}

#[test]
fn test_rust_inherent_impl_header_does_not_emit_references_edge() {
    // `impl Widget` — the impl-header type name is already covered by the impl
    // machinery; a references edge would defeat dead-code detection (Widget
    // would always look used).
    let src = r#"impl Widget { fn f() {} }"#;
    let rels = extract_relations(src, "rust").unwrap();
    assert!(
        !rels
            .iter()
            .any(|r| r.relation == REL_REFERENCES && r.target_name == "Widget"),
        "an inherent impl header type must not be a references edge; got: {:?}",
        rels.iter()
            .map(|r| (r.relation.as_str(), r.target_name.as_str()))
            .collect::<Vec<_>>()
    );
}

#[test]
fn test_rust_trait_impl_header_does_not_emit_references_edge() {
    // `impl MyTrait for Widget` — neither the trait name nor the type name
    // should yield a references edge (both are IMPLEMENTS-edge territory).
    let src = r#"impl MyTrait for Widget {}"#;
    let rels = extract_relations(src, "rust").unwrap();
    assert!(
        !rels.iter().any(|r| r.relation == REL_REFERENCES
            && (r.target_name == "Widget" || r.target_name == "MyTrait")),
        "a trait impl header (type or trait) must not be a references edge; got: {:?}",
        rels.iter()
            .map(|r| (r.relation.as_str(), r.target_name.as_str()))
            .collect::<Vec<_>>()
    );
}

#[test]
fn test_rust_return_type_still_emits_references_edge_alongside_impl() {
    // Guard against the impl-header skip over-reaching: a real type usage in a
    // return position must still emit a references edge even when the same type
    // also appears in an impl header.
    let src = r#"
impl Widget { fn f() {} }
fn make() -> Widget { todo!() }
"#;
    let rels = extract_relations(src, "rust").unwrap();
    assert!(
        rels.iter()
            .any(|r| r.relation == REL_REFERENCES && r.target_name == "Widget"),
        "a return-type usage of Widget must still emit a references edge; got: {:?}",
        rels.iter()
            .map(|r| (r.relation.as_str(), r.target_name.as_str()))
            .collect::<Vec<_>>()
    );
}

#[test]
fn test_rust_inferred_type_placeholder_does_not_emit_references_edge() {
    let src = r#"fn f() { let v: Vec<_> = items.collect::<Vec<_>>(); }"#;
    let rels = extract_relations(src, "rust").unwrap();
    assert!(
        !rels
            .iter()
            .any(|r| r.relation == REL_REFERENCES && r.target_name == "_"),
        "inferred-type placeholder `_` must not emit a references edge; got: {:?}",
        rels.iter()
            .map(|r| (r.relation.as_str(), r.target_name.as_str()))
            .collect::<Vec<_>>()
    );
}

#[test]
fn test_ts_type_usage_emits_references_edge() {
    // A type used in type position (interface field type, return type, var
    // annotation) must emit a `references` edge. The interface's OWN name
    // `Widget` must NOT self-reference.
    let src = r#"interface Widget { size: number } function make(): Widget { return null as any; } const w: Widget = make();"#;
    let rels = extract_relations(src, "typescript").unwrap();
    let has_ref = rels
        .iter()
        .any(|r| r.relation == REL_REFERENCES && r.target_name == "Widget");
    assert!(
        has_ref,
        "expected a references edge to Widget (return type / annotation); got: {:?}",
        rels.iter()
            .map(|r| (r.relation.as_str(), r.target_name.as_str()))
            .collect::<Vec<_>>()
    );
    // The interface's own name must not self-reference.
    let widget_self_ref = rels.iter().any(|r| {
        r.relation == REL_REFERENCES && r.target_name == "Widget" && r.source_name == "Widget"
    });
    assert!(
        !widget_self_ref,
        "the interface's own name `Widget` must not self-reference; got: {:?}",
        rels.iter()
            .map(|r| (
                r.relation.as_str(),
                r.source_name.as_str(),
                r.target_name.as_str()
            ))
            .collect::<Vec<_>>()
    );
}

#[test]
fn test_ts_generic_arg_emits_references_edge() {
    // Type used as a generic argument (`Array<Foo>`) is a real type-position
    // usage and must emit a references edge.
    let src = r#"function g(): Array<Foo> { return []; }"#;
    let rels = extract_relations(src, "typescript").unwrap();
    assert!(
        rels.iter()
            .any(|r| r.relation == REL_REFERENCES && r.target_name == "Foo"),
        "a generic-arg type usage of Foo must emit a references edge; got: {:?}",
        rels.iter()
            .map(|r| (r.relation.as_str(), r.target_name.as_str()))
            .collect::<Vec<_>>()
    );
}

#[test]
fn test_ts_extends_clause_does_not_emit_references_edge() {
    // `class Foo extends Bar {}` — Bar is an inherits/extends edge, NOT a
    // references edge (avoid double-emit).
    let src = r#"class Foo extends Bar {}"#;
    let rels = extract_relations(src, "typescript").unwrap();
    assert!(
        !rels
            .iter()
            .any(|r| r.relation == REL_REFERENCES && r.target_name == "Bar"),
        "an extends-clause superclass must not be a references edge; got: {:?}",
        rels.iter()
            .map(|r| (r.relation.as_str(), r.target_name.as_str()))
            .collect::<Vec<_>>()
    );
}

#[test]
fn test_ts_implements_clause_does_not_emit_references_edge() {
    // `class Foo implements Iface {}` — Iface is an implements edge, NOT a
    // references edge.
    let src = r#"class Foo implements Iface {}"#;
    let rels = extract_relations(src, "typescript").unwrap();
    assert!(
        !rels
            .iter()
            .any(|r| r.relation == REL_REFERENCES && r.target_name == "Iface"),
        "an implements-clause interface must not be a references edge; got: {:?}",
        rels.iter()
            .map(|r| (r.relation.as_str(), r.target_name.as_str()))
            .collect::<Vec<_>>()
    );
}

#[test]
fn test_ts_predefined_types_do_not_emit_references_edge() {
    // Primitives (string/number/boolean) parse as `predefined_type`, not
    // `type_identifier`, so they are naturally excluded.
    let src = r#"function f(x: number, y: string): boolean { return true; }"#;
    let rels = extract_relations(src, "typescript").unwrap();
    assert!(
        !rels.iter().any(|r| r.relation == REL_REFERENCES
            && matches!(r.target_name.as_str(), "number" | "string" | "boolean")),
        "predefined primitive types must not emit references edges; got: {:?}",
        rels.iter()
            .map(|r| (r.relation.as_str(), r.target_name.as_str()))
            .collect::<Vec<_>>()
    );
}

#[test]
fn test_ts_type_alias_rhs_emits_but_name_does_not() {
    // `type Alias = Widget;` — the RHS `Widget` is a usage (emit), the alias
    // name `Alias` is a declaration (skip).
    let src = r#"type Alias = Widget;"#;
    let rels = extract_relations(src, "typescript").unwrap();
    assert!(
        rels.iter()
            .any(|r| r.relation == REL_REFERENCES && r.target_name == "Widget"),
        "type-alias RHS Widget must emit a references edge; got: {:?}",
        rels.iter()
            .map(|r| (r.relation.as_str(), r.target_name.as_str()))
            .collect::<Vec<_>>()
    );
    assert!(
        !rels
            .iter()
            .any(|r| r.relation == REL_REFERENCES && r.target_name == "Alias"),
        "type-alias own name Alias must not self-reference; got: {:?}",
        rels.iter()
            .map(|r| (r.relation.as_str(), r.target_name.as_str()))
            .collect::<Vec<_>>()
    );
}

// --- Python type-annotation REFERENCES edges ---
// tree-sitter-python wraps annotation types in a `type` node; the type NAME is a
// plain `identifier` (same kind as value identifiers), so the gate is the
// annotation context (`type`-node ancestor), not the node kind. Base classes
// live in `argument_list [field=superclasses]` (not a `type` node) → naturally
// excluded; value identifiers (`u`, `account`, `compute`) never sit under a
// `type` node.

#[test]
fn test_python_param_and_return_annotation_emit_references() {
    // `def make(u: User) -> Account:` — param type `User` + return type `Account`
    // must emit references edges; the value identifier `u` and the attribute read
    // `account` (`u.account`) must NOT.
    let src = "def make(u: User) -> Account:\n    return u.account\n";
    let rels = extract_relations(src, "python").unwrap();
    let refs: Vec<&str> = rels
        .iter()
        .filter(|r| r.relation == REL_REFERENCES)
        .map(|r| r.target_name.as_str())
        .collect();
    assert!(
        refs.contains(&"User"),
        "param annotation `User` must emit a references edge; got: {:?}",
        refs
    );
    assert!(
        refs.contains(&"Account"),
        "return annotation `Account` must emit a references edge; got: {:?}",
        refs
    );
    assert!(
        !refs.contains(&"u"),
        "value identifier `u` must NOT be a references edge; got: {:?}",
        refs
    );
    assert!(
        !refs.contains(&"account"),
        "attribute read `account` must NOT be a references edge; got: {:?}",
        refs
    );
}

#[test]
fn test_python_annotated_class_attr_emits_references() {
    // `class Service:\n    cache: Cache` — annotated class attribute → reference
    // to `Cache`.
    let src = "class Service:\n    cache: Cache\n";
    let rels = extract_relations(src, "python").unwrap();
    let refs: Vec<&str> = rels
        .iter()
        .filter(|r| r.relation == REL_REFERENCES)
        .map(|r| r.target_name.as_str())
        .collect();
    assert!(
        refs.contains(&"Cache"),
        "annotated class attr `Cache` must emit a references edge; got: {:?}",
        refs
    );
    assert!(
        !refs.contains(&"cache"),
        "the annotated name `cache` (LHS) must NOT be a references edge; got: {:?}",
        refs
    );
}

#[test]
fn test_python_base_class_does_not_emit_references() {
    // `class Foo(Base):` — Base is an inherits edge (extracted by inheritance),
    // NOT a references edge (avoid double-emit).
    let src = "class Foo(Base):\n    pass\n";
    let rels = extract_relations(src, "python").unwrap();
    assert!(
        !rels
            .iter()
            .any(|r| r.relation == REL_REFERENCES && r.target_name == "Base"),
        "base class `Base` must NOT be a references edge; got: {:?}",
        rels.iter()
            .map(|r| (r.relation.as_str(), r.target_name.as_str()))
            .collect::<Vec<_>>()
    );
}

#[test]
fn test_python_builtin_annotation_does_not_emit_references() {
    // `x: int = 3` — `int` is a builtin, must NOT emit a references edge (noise).
    let src = "x: int = 3\n";
    let rels = extract_relations(src, "python").unwrap();
    assert!(
        !rels
            .iter()
            .any(|r| r.relation == REL_REFERENCES && r.target_name == "int"),
        "builtin `int` must NOT be a references edge; got: {:?}",
        rels.iter()
            .map(|r| (r.relation.as_str(), r.target_name.as_str()))
            .collect::<Vec<_>>()
    );
}

#[test]
fn test_python_generic_arg_annotation_emits_references_skips_typing_generic() {
    // `def g(items: List[User]) -> Dict[str, User]:` — `User` (generic arg) is a
    // project type → reference; the `typing` generics `List`/`Dict` and builtin
    // `str` are stdlib → skipped.
    let src = "def g(items: List[User]) -> Dict[str, User]:\n    return {}\n";
    let rels = extract_relations(src, "python").unwrap();
    let refs: Vec<&str> = rels
        .iter()
        .filter(|r| r.relation == REL_REFERENCES)
        .map(|r| r.target_name.as_str())
        .collect();
    assert!(
        refs.contains(&"User"),
        "generic arg `User` must emit a references edge; got: {:?}",
        refs
    );
    assert!(
        !refs.contains(&"List") && !refs.contains(&"Dict"),
        "typing generics `List`/`Dict` must be skipped; got: {:?}",
        refs
    );
    assert!(
        !refs.contains(&"str"),
        "builtin `str` must be skipped; got: {:?}",
        refs
    );
}

#[test]
fn test_python_dotted_annotation_emits_tail_only_and_no_value_attr_noise() {
    // `meta: mod.Meta = None` (dotted annotation) → reference to the tail `Meta`
    // only, NOT the module path head `mod`. And a value attribute read
    // `obj.method` in non-annotation position must NOT emit any reference.
    let src = "def f(self):\n    meta: mod.Meta = None\n    v = obj.attr\n    return v\n";
    let rels = extract_relations(src, "python").unwrap();
    let refs: Vec<&str> = rels
        .iter()
        .filter(|r| r.relation == REL_REFERENCES)
        .map(|r| r.target_name.as_str())
        .collect();
    assert!(
        refs.contains(&"Meta"),
        "dotted annotation tail `Meta` must emit a references edge; got: {:?}",
        refs
    );
    assert!(
        !refs.contains(&"mod"),
        "dotted annotation head `mod` (module path) must NOT be a references edge; got: {:?}",
        refs
    );
    assert!(
        !refs.contains(&"obj") && !refs.contains(&"attr"),
        "value attribute read `obj.attr` must NOT emit any references edge; got: {:?}",
        refs
    );
}

// --- Go type-position REFERENCES edges ---
// tree-sitter-go represents a type name in type position as a `type_identifier`
// (like Rust/TS, a distinct kind from value identifiers). UNLIKE TS, Go builtins
// (`int`, `string`, ...) are ALSO `type_identifier`, so a builtin skip-set
// (GO_TYPE_REFERENCE_NOISE) is required. Value selectors (`pkg.Func()`,
// `obj.field`) use `field_identifier`/`identifier`, never `type_identifier`, so
// they are naturally excluded. The qualified-type head (`pkg` in `pkg.Type`) is a
// `package_identifier` (naturally excluded); only the tail `Type` is a
// `type_identifier`.

#[test]
fn test_go_param_and_return_type_emit_references() {
    // `func make(u User) Account { return Account{} }` — param type `User` +
    // return type `Account` must emit references; the struct's OWN definition
    // name `Account` (from `type Account struct {}`) must NOT self-reference, and
    // builtins must not emit.
    let src = "type Account struct {}\nfunc make(u User) Account { return Account{} }\n";
    let rels = extract_relations(src, "go").unwrap();
    let refs: Vec<&str> = rels
        .iter()
        .filter(|r| r.relation == REL_REFERENCES)
        .map(|r| r.target_name.as_str())
        .collect();
    assert!(
        refs.contains(&"User"),
        "param type `User` must emit a references edge; got: {:?}",
        refs
    );
    assert!(
        refs.contains(&"Account"),
        "return type `Account` must emit a references edge; got: {:?}",
        refs
    );
    // The struct's own definition name must not self-reference.
    let account_self_ref = rels.iter().any(|r| {
        r.relation == REL_REFERENCES && r.target_name == "Account" && r.source_name == "Account"
    });
    assert!(
        !account_self_ref,
        "the struct's own name `Account` must not self-reference; got: {:?}",
        rels.iter()
            .map(|r| (
                r.relation.as_str(),
                r.source_name.as_str(),
                r.target_name.as_str()
            ))
            .collect::<Vec<_>>()
    );
}

#[test]
fn test_go_struct_field_type_emits_references() {
    // `type S struct { conn Conn }` — the field type `Conn` is a usage → reference.
    // The field NAME `conn` (`field_identifier`) and the struct name `S` must not.
    let src = "type S struct {\n\tconn Conn\n}\n";
    let rels = extract_relations(src, "go").unwrap();
    let refs: Vec<&str> = rels
        .iter()
        .filter(|r| r.relation == REL_REFERENCES)
        .map(|r| r.target_name.as_str())
        .collect();
    assert!(
        refs.contains(&"Conn"),
        "struct field type `Conn` must emit a references edge; got: {:?}",
        refs
    );
    assert!(
        !refs.contains(&"conn"),
        "the field name `conn` must NOT emit a references edge; got: {:?}",
        refs
    );
    assert!(
        !refs.contains(&"S"),
        "the struct's own name `S` must NOT self-reference; got: {:?}",
        refs
    );
}

#[test]
fn test_go_value_selector_does_not_emit_references() {
    // `pkg.DoThing()` — `DoThing` is a `field_identifier` on a value selector, NOT
    // a `type_identifier`, so it must not emit a references edge.
    let src = "func run() {\n\tpkg.DoThing()\n}\n";
    let rels = extract_relations(src, "go").unwrap();
    assert!(
        !rels
            .iter()
            .any(|r| r.relation == REL_REFERENCES && r.target_name == "DoThing"),
        "a value selector call `pkg.DoThing()` must NOT emit a references edge; got: {:?}",
        rels.iter()
            .map(|r| (r.relation.as_str(), r.target_name.as_str()))
            .collect::<Vec<_>>()
    );
    assert!(
        !rels
            .iter()
            .any(|r| r.relation == REL_REFERENCES && r.target_name == "pkg"),
        "the selector operand `pkg` must NOT emit a references edge; got: {:?}",
        rels.iter()
            .map(|r| (r.relation.as_str(), r.target_name.as_str()))
            .collect::<Vec<_>>()
    );
}

#[test]
fn test_go_builtin_type_does_not_emit_references() {
    // `var x int` — `int` is a `type_identifier` in tree-sitter-go but a builtin,
    // so it must be filtered out by GO_TYPE_REFERENCE_NOISE.
    let src = "var x int\n";
    let rels = extract_relations(src, "go").unwrap();
    assert!(
        !rels
            .iter()
            .any(|r| r.relation == REL_REFERENCES && r.target_name == "int"),
        "builtin `int` must NOT emit a references edge; got: {:?}",
        rels.iter()
            .map(|r| (r.relation.as_str(), r.target_name.as_str()))
            .collect::<Vec<_>>()
    );
}

#[test]
fn test_java_param_and_return_and_new_type_emit_references() {
    // `class Svc { Account make(User u) { return new Account(); } }` — param type
    // `User`, return type `Account`, and `new Account()` type all emit references.
    // The class's OWN definition names `Account`/`Svc` (the `name` field of a
    // class_declaration, which is an `identifier`, NOT a `type_identifier`) must
    // NOT self-reference. Primitives must not emit.
    let src = "class Account {}\nclass Svc { Account make(User u) { return new Account(); } }\n";
    let rels = extract_relations(src, "java").unwrap();
    let refs: Vec<&str> = rels
        .iter()
        .filter(|r| r.relation == REL_REFERENCES)
        .map(|r| r.target_name.as_str())
        .collect();
    assert!(
        refs.contains(&"User"),
        "param type `User` must emit a references edge; got: {:?}",
        refs
    );
    assert!(
        refs.contains(&"Account"),
        "return type / `new Account()` type `Account` must emit a references edge; got: {:?}",
        refs
    );
    // Class definition names must never self-reference.
    let account_self = rels.iter().any(|r| {
        r.relation == REL_REFERENCES && r.target_name == "Account" && r.source_name == "Account"
    });
    assert!(
        !account_self,
        "the class's own name `Account` must not self-reference; got: {:?}",
        rels.iter()
            .map(|r| (
                r.relation.as_str(),
                r.source_name.as_str(),
                r.target_name.as_str()
            ))
            .collect::<Vec<_>>()
    );
    assert!(
        !refs.contains(&"Svc"),
        "the class's own name `Svc` must NOT emit a references edge; got: {:?}",
        refs
    );
}

#[test]
fn test_java_field_type_emits_references() {
    // `class S { Conn conn; }` — field type `Conn` is a usage → reference; the
    // field NAME `conn` (an `identifier`, not a `type_identifier`) and the class
    // name `S` must not.
    let src = "class S { Conn conn; }\n";
    let rels = extract_relations(src, "java").unwrap();
    let refs: Vec<&str> = rels
        .iter()
        .filter(|r| r.relation == REL_REFERENCES)
        .map(|r| r.target_name.as_str())
        .collect();
    assert!(
        refs.contains(&"Conn"),
        "field type `Conn` must emit a references edge; got: {:?}",
        refs
    );
    assert!(
        !refs.contains(&"conn"),
        "the field name `conn` must NOT emit a references edge; got: {:?}",
        refs
    );
    assert!(
        !refs.contains(&"S"),
        "the class's own name `S` must NOT self-reference; got: {:?}",
        refs
    );
}

#[test]
fn test_java_heritage_types_do_not_emit_references() {
    // `class Foo extends Bar implements Baz {}` — `Bar` (superclass clause) and
    // `Baz` (super_interfaces clause) already yield inherits/implements edges, so
    // they must NOT also emit references edges.
    let src = "class Foo extends Bar implements Baz {}\n";
    let rels = extract_relations(src, "java").unwrap();
    assert!(
        !rels
            .iter()
            .any(|r| r.relation == REL_REFERENCES && r.target_name == "Bar"),
        "superclass `Bar` must NOT emit a references edge (heritage); got: {:?}",
        rels.iter()
            .map(|r| (r.relation.as_str(), r.target_name.as_str()))
            .collect::<Vec<_>>()
    );
    assert!(
        !rels
            .iter()
            .any(|r| r.relation == REL_REFERENCES && r.target_name == "Baz"),
        "interface `Baz` must NOT emit a references edge (heritage); got: {:?}",
        rels.iter()
            .map(|r| (r.relation.as_str(), r.target_name.as_str()))
            .collect::<Vec<_>>()
    );
}

#[test]
fn test_java_jdk_noise_type_does_not_emit_references() {
    // `class S { String name; }` — `String` is a JDK common type (noise); it must
    // be filtered out by JAVA_TYPE_REFERENCE_NOISE.
    let src = "class S { String name; }\n";
    let rels = extract_relations(src, "java").unwrap();
    assert!(
        !rels
            .iter()
            .any(|r| r.relation == REL_REFERENCES && r.target_name == "String"),
        "JDK type `String` must NOT emit a references edge (noise); got: {:?}",
        rels.iter()
            .map(|r| (r.relation.as_str(), r.target_name.as_str()))
            .collect::<Vec<_>>()
    );
}

#[test]
fn test_java_primitive_type_does_not_emit_references() {
    // `class S { int x; }` — `int` is `integral_type`, a SEPARATE kind from
    // `type_identifier`, so it is naturally excluded (never reaches the extractor).
    let src = "class S { int x; }\n";
    let rels = extract_relations(src, "java").unwrap();
    assert!(
        !rels
            .iter()
            .any(|r| r.relation == REL_REFERENCES && r.target_name == "int"),
        "primitive `int` must NOT emit a references edge (separate kind); got: {:?}",
        rels.iter()
            .map(|r| (r.relation.as_str(), r.target_name.as_str()))
            .collect::<Vec<_>>()
    );
}

#[test]
fn test_java_generic_arg_and_qualified_tail_emit_only_tail() {
    // Generic arg `List<Foo>` → `Foo` emits. Qualified type `pkg.Sub.Deep field;`
    // → only the chain TAIL `Deep` emits; the package-path segments `pkg`/`Sub`
    // (also `type_identifier`s under nested `scoped_type_identifier`) must NOT.
    let src = "class A { java.util.List<Foo> g() { return null; } pkg.Sub.Deep d; }\n";
    let rels = extract_relations(src, "java").unwrap();
    let refs: Vec<&str> = rels
        .iter()
        .filter(|r| r.relation == REL_REFERENCES)
        .map(|r| r.target_name.as_str())
        .collect();
    assert!(
        refs.contains(&"Foo"),
        "generic arg `Foo` in `List<Foo>` must emit a references edge; got: {:?}",
        refs
    );
    assert!(
        refs.contains(&"Deep"),
        "qualified-type tail `Deep` in `pkg.Sub.Deep` must emit a references edge; got: {:?}",
        refs
    );
    assert!(
        !refs.contains(&"pkg"),
        "qualified-type package segment `pkg` must NOT emit a references edge; got: {:?}",
        refs
    );
    assert!(
        !refs.contains(&"Sub"),
        "qualified-type package segment `Sub` must NOT emit a references edge; got: {:?}",
        refs
    );
    // `java.util.List` is JDK noise on the tail (List) and path segments java/util.
    assert!(
        !refs.contains(&"java") && !refs.contains(&"util"),
        "qualified-type package segments `java`/`util` must NOT emit a references edge; got: {:?}",
        refs
    );
}

// ─────────────────────────────────────────────────────────────────────────
// Phase 1: bare-identifier function-VALUE references (callbacks / fn pointers)
//
// RED tests (Inc 0). Existing `references` extraction covers type-position and
// path-qualified value usages; the gap is a BARE `identifier` used as a function
// value (passed as a callback / fn pointer). Positive cases (R1–R3, R8–R9) are
// EXPECTED TO FAIL until candidate generation lands. Negative/guard cases
// (R4–R7, R10–R11) lock the precision boundary and may already pass.
// ─────────────────────────────────────────────────────────────────────────

#[test]
fn test_r1_rust_bare_fn_call_arg_emits_references_edge() {
    let src = r#"fn caller() { install(handler); } fn handler() {}"#;
    let rels = extract_relations(src, "rust").unwrap();
    assert!(
        rels.iter().any(|r| r.relation == REL_REFERENCES
            && r.target_name == "handler"
            && r.source_name == "caller"),
        "bare fn passed as a call argument must emit references caller->handler; got: {:?}",
        rels.iter()
            .map(|r| (
                r.relation.as_str(),
                r.source_name.as_str(),
                r.target_name.as_str()
            ))
            .collect::<Vec<_>>()
    );
}

#[test]
fn test_r2_rust_bare_fn_hof_arg_emits_references_edge() {
    let src = r#"fn caller() { let _ = xs.iter().map(double); } fn double() {}"#;
    let rels = extract_relations(src, "rust").unwrap();
    assert!(
        rels.iter().any(|r| r.relation == REL_REFERENCES
            && r.target_name == "double"
            && r.source_name == "caller"),
        "bare fn passed to a HOF (.map) must emit references caller->double; got: {:?}",
        rels.iter()
            .map(|r| (
                r.relation.as_str(),
                r.source_name.as_str(),
                r.target_name.as_str()
            ))
            .collect::<Vec<_>>()
    );
}

#[test]
fn test_r3_rust_address_of_fn_arg_emits_references_edge() {
    let src = r#"fn caller() { signal(&shutdown); } fn shutdown() {}"#;
    let rels = extract_relations(src, "rust").unwrap();
    assert!(
        rels.iter().any(|r| r.relation == REL_REFERENCES
            && r.target_name == "shutdown"
            && r.source_name == "caller"),
        "address-of fn passed as arg (&shutdown) must emit references caller->shutdown; got: {:?}",
        rels.iter()
            .map(|r| (
                r.relation.as_str(),
                r.source_name.as_str(),
                r.target_name.as_str()
            ))
            .collect::<Vec<_>>()
    );
}

#[test]
fn test_r4_rust_param_passed_as_arg_does_not_emit_references_edge() {
    // `handler` is a PARAMETER of `run`, not the global fn — passing it through
    // must NOT emit a references edge (M2 param exclusion).
    let src = r#"fn run<F>(handler: F) { spawn(handler); } fn handler() {}"#;
    let rels = extract_relations(src, "rust").unwrap();
    assert!(
        !rels.iter().any(|r| r.relation == REL_REFERENCES
            && r.target_name == "handler"
            && r.source_name == "run"),
        "a parameter passed through must NOT emit a references edge (M2); got: {:?}",
        rels.iter()
            .map(|r| (
                r.relation.as_str(),
                r.source_name.as_str(),
                r.target_name.as_str()
            ))
            .collect::<Vec<_>>()
    );
}

#[test]
fn test_r5_rust_call_in_arg_position_does_not_emit_references_edge() {
    let src = r#"fn caller() { foo(bar()); } fn bar() {}"#;
    let rels = extract_relations(src, "rust").unwrap();
    assert!(
        !rels
            .iter()
            .any(|r| r.relation == REL_REFERENCES && r.target_name == "bar"),
        "a called fn in arg position (bar()) is a calls edge, not references; got: {:?}",
        rels.iter()
            .map(|r| (r.relation.as_str(), r.target_name.as_str()))
            .collect::<Vec<_>>()
    );
}

#[test]
fn test_r6_rust_field_access_arg_does_not_emit_references_edge() {
    let src = r#"fn caller() { foo(x.field); }"#;
    let rels = extract_relations(src, "rust").unwrap();
    assert!(
        !rels
            .iter()
            .any(|r| r.relation == REL_REFERENCES
                && (r.target_name == "field" || r.target_name == "x")),
        "member access in arg position must not emit a references edge; got: {:?}",
        rels.iter()
            .map(|r| (r.relation.as_str(), r.target_name.as_str()))
            .collect::<Vec<_>>()
    );
}

#[test]
fn test_r7_rust_call_callee_does_not_also_emit_value_reference() {
    let src = r#"fn caller() { register(handler); } fn handler() {} fn register<F>(_f: F) {}"#;
    let rels = extract_relations(src, "rust").unwrap();
    assert!(
        rels.iter()
            .any(|r| r.relation == REL_CALLS && r.target_name == "register"),
        "the callee must still be a calls edge; got: {:?}",
        rels.iter()
            .map(|r| (r.relation.as_str(), r.target_name.as_str()))
            .collect::<Vec<_>>()
    );
    assert!(
        !rels
            .iter()
            .any(|r| r.relation == REL_REFERENCES && r.target_name == "register"),
        "the callee must NOT also be a references edge; got: {:?}",
        rels.iter()
            .map(|r| (r.relation.as_str(), r.target_name.as_str()))
            .collect::<Vec<_>>()
    );
}

#[test]
fn test_r8_js_bare_fn_call_arg_emits_references_edge() {
    let src = r#"function caller() { arr.map(myFunc); } function myFunc() {}"#;
    let rels = extract_relations(src, "javascript").unwrap();
    assert!(
        rels.iter().any(|r| r.relation == REL_REFERENCES
            && r.target_name == "myFunc"
            && r.source_name == "caller"),
        "bare fn passed to .map must emit references caller->myFunc; got: {:?}",
        rels.iter()
            .map(|r| (
                r.relation.as_str(),
                r.source_name.as_str(),
                r.target_name.as_str()
            ))
            .collect::<Vec<_>>()
    );
}

#[test]
fn test_r9_js_callback_arg_emits_references_edge() {
    let src = r#"function caller() { on('click', handler); } function handler() {}"#;
    let rels = extract_relations(src, "javascript").unwrap();
    assert!(
        rels.iter().any(|r| r.relation == REL_REFERENCES
            && r.target_name == "handler"
            && r.source_name == "caller"),
        "bare fn passed as a callback arg must emit references caller->handler; got: {:?}",
        rels.iter()
            .map(|r| (
                r.relation.as_str(),
                r.source_name.as_str(),
                r.target_name.as_str()
            ))
            .collect::<Vec<_>>()
    );
}

#[test]
fn test_r10_ts_param_passed_as_arg_does_not_emit_references_edge() {
    let src = r#"function run(cb: Fn) { q(cb); }"#;
    let rels = extract_relations(src, "typescript").unwrap();
    assert!(
        !rels.iter().any(|r| r.relation == REL_REFERENCES
            && r.target_name == "cb"
            && r.source_name == "run"),
        "a TS parameter passed through must NOT emit a references edge (M2); got: {:?}",
        rels.iter()
            .map(|r| (
                r.relation.as_str(),
                r.source_name.as_str(),
                r.target_name.as_str()
            ))
            .collect::<Vec<_>>()
    );
}

#[test]
fn test_r11_js_call_in_arg_position_does_not_emit_references_edge() {
    let src = r#"function caller() { foo(bar()); } function bar() {}"#;
    let rels = extract_relations(src, "javascript").unwrap();
    assert!(
        !rels
            .iter()
            .any(|r| r.relation == REL_REFERENCES && r.target_name == "bar"),
        "a called fn in arg position (bar()) is a calls edge, not references; got: {:?}",
        rels.iter()
            .map(|r| (r.relation.as_str(), r.target_name.as_str()))
            .collect::<Vec<_>>()
    );
}

#[test]
fn test_r16_rust_local_let_binding_does_not_emit_references_edge() {
    // `db` is a LOCAL `let` binding (holds a value), not the global fn `db` — passing
    // it by `&` or as an arg must NOT emit a reference to the same-named fn (M2.5).
    // This is the dominant Phase-1 false positive: idiomatic `let db = open();
    // run(&db)` where an accessor fn/method `db` also exists.
    let src = r#"fn caller() { let db = open(); run(&db); use_it(db); } fn db() {}"#;
    let rels = extract_relations(src, "rust").unwrap();
    assert!(
        !rels
            .iter()
            .any(|r| r.relation == REL_REFERENCES && r.target_name == "db"),
        "a local `let` binding passed as arg must NOT emit a references edge (M2.5); got: {:?}",
        rels.iter()
            .map(|r| (
                r.relation.as_str(),
                r.source_name.as_str(),
                r.target_name.as_str()
            ))
            .collect::<Vec<_>>()
    );
}

#[test]
fn test_r17_js_local_const_binding_does_not_emit_references_edge() {
    let src = r#"function caller() { const cb = make(); run(cb); } function cb() {}"#;
    let rels = extract_relations(src, "javascript").unwrap();
    assert!(
        !rels
            .iter()
            .any(|r| r.relation == REL_REFERENCES && r.target_name == "cb"),
        "a local const binding passed as arg must NOT emit a references edge (M2.5); got: {:?}",
        rels.iter()
            .map(|r| (
                r.relation.as_str(),
                r.source_name.as_str(),
                r.target_name.as_str()
            ))
            .collect::<Vec<_>>()
    );
}

#[test]
fn test_r18_rust_genuine_fn_pointer_still_emits_after_m2_5() {
    // M2.5 must NOT over-suppress: a module-level fn passed as a callback (no local
    // binding shadows it) still emits. Guards against the local-binding exclusion
    // accidentally killing real callbacks.
    let src = r#"fn caller() { query_map(params, map_row); } fn map_row() {}"#;
    let rels = extract_relations(src, "rust").unwrap();
    assert!(
        rels.iter().any(|r| r.relation == REL_REFERENCES
            && r.target_name == "map_row"
            && r.source_name == "caller"),
        "a genuine fn-pointer callback (no local shadow) must still emit references; got: {:?}",
        rels.iter()
            .map(|r| (
                r.relation.as_str(),
                r.source_name.as_str(),
                r.target_name.as_str()
            ))
            .collect::<Vec<_>>()
    );
}

#[test]
fn test_n1_rust_macro_path_does_not_emit_references_edge() {
    // `tracing::error!(...)` is a macro_invocation whose `macro` field is the scoped
    // path `tracing::error`. The path-reference extractor must NOT treat the macro
    // name tail (`error`) as a value reference — it collides with same-named fns.
    let src = r#"fn f() { tracing::error!("boom {}", x); } fn error() {}"#;
    let rels = extract_relations(src, "rust").unwrap();
    assert!(
        !rels
            .iter()
            .any(|r| r.relation == REL_REFERENCES && r.target_name == "error"),
        "a macro path tail must NOT emit a references edge; got: {:?}",
        rels.iter()
            .map(|r| (r.relation.as_str(), r.target_name.as_str()))
            .collect::<Vec<_>>()
    );
}

#[test]
fn test_n2_rust_type_associated_path_does_not_emit_references_edge() {
    // `String::as_str` passed as a fn pointer is a Type::method associated path. We
    // cannot resolve the associated item (std method, not in the index), and binding
    // the bare tail `as_str` to an unrelated local fn is a false positive. Suppress
    // PascalCase-head (type-associated) value paths.
    let src = r#"fn f() { let _ = xs.iter().map(String::as_str); } fn as_str() {}"#;
    let rels = extract_relations(src, "rust").unwrap();
    assert!(!rels.iter().any(|r| r.relation == REL_REFERENCES && r.target_name == "as_str"),
        "a Type::method associated path must NOT emit a references edge to the bare tail; got: {:?}",
        rels.iter().map(|r| (r.relation.as_str(), r.target_name.as_str())).collect::<Vec<_>>());
}

#[test]
fn test_n3_rust_module_path_value_still_emits_after_noise_fix() {
    // Guard: the noise fix (macro + type-associated suppression) must NOT touch
    // legitimate lowercase module-path value references (`crate::domain::SHARED`).
    let src = r#"fn build() { let w = crate::domain::SHARED; }"#;
    let rels = extract_relations(src, "rust").unwrap();
    assert!(
        rels.iter()
            .any(|r| r.relation == REL_REFERENCES && r.target_name == "SHARED"),
        "a lowercase module-path value reference must still emit; got: {:?}",
        rels.iter()
            .map(|r| (r.relation.as_str(), r.target_name.as_str()))
            .collect::<Vec<_>>()
    );
}

#[test]
fn test_r19_rust_if_let_binding_does_not_emit_references_edge() {
    // `node` bound by `if let Some(node) = ...` is a local, not the global fn `node`.
    // if-let/while-let bindings are `let_condition` patterns, NOT `let_declaration`.
    let src = r#"fn caller() { if let Some(node) = g() { use_it(node); } } fn node() {}"#;
    let rels = extract_relations(src, "rust").unwrap();
    assert!(
        !rels
            .iter()
            .any(|r| r.relation == REL_REFERENCES && r.target_name == "node"),
        "an if-let pattern binding must NOT emit a references edge; got: {:?}",
        rels.iter()
            .map(|r| (
                r.relation.as_str(),
                r.source_name.as_str(),
                r.target_name.as_str()
            ))
            .collect::<Vec<_>>()
    );
}

#[test]
fn test_r20_rust_for_loop_binding_does_not_emit_references_edge() {
    let src = r#"fn caller() { for item in xs { take(item); } } fn item() {}"#;
    let rels = extract_relations(src, "rust").unwrap();
    assert!(
        !rels
            .iter()
            .any(|r| r.relation == REL_REFERENCES && r.target_name == "item"),
        "a for-loop pattern binding must NOT emit a references edge; got: {:?}",
        rels.iter()
            .map(|r| (
                r.relation.as_str(),
                r.source_name.as_str(),
                r.target_name.as_str()
            ))
            .collect::<Vec<_>>()
    );
}

#[test]
fn test_r21_rust_match_arm_binding_does_not_emit_references_edge() {
    let src = r#"fn caller() { match g() { Ok(val) => keep(val), Err(error) => log(error) } } fn val() {} fn error() {}"#;
    let rels = extract_relations(src, "rust").unwrap();
    assert!(
        !rels.iter().any(|r| r.relation == REL_REFERENCES
            && (r.target_name == "val" || r.target_name == "error")),
        "match-arm pattern bindings must NOT emit references edges; got: {:?}",
        rels.iter()
            .map(|r| (
                r.relation.as_str(),
                r.source_name.as_str(),
                r.target_name.as_str()
            ))
            .collect::<Vec<_>>()
    );
}

// ─────────────────────────────────────────────────────────────────────────
// Phase 2: value references in BINDING-RHS and RETURN positions (callbacks /
// fn pointers stored or returned by name). Same gates as Phase 1 (M2/M2.5
// local-binding exclusion, same-language resolution, self/Self/_ skip).
// ─────────────────────────────────────────────────────────────────────────

#[test]
fn test_p1_rust_let_rhs_fn_emits_references_edge() {
    // `let cb = handler;` stores a fn by name — RHS value reference.
    let src = r#"fn caller() { let cb = handler; use_it(cb); } fn handler() {}"#;
    let rels = extract_relations(src, "rust").unwrap();
    assert!(
        rels.iter().any(|r| r.relation == REL_REFERENCES
            && r.target_name == "handler"
            && r.source_name == "caller"),
        "let-binding RHS fn must emit references caller->handler; got: {:?}",
        rels.iter()
            .map(|r| (
                r.relation.as_str(),
                r.source_name.as_str(),
                r.target_name.as_str()
            ))
            .collect::<Vec<_>>()
    );
}

#[test]
fn test_p2_js_const_rhs_fn_emits_references_edge() {
    let src = r#"function caller() { const cb = handler; } function handler() {}"#;
    let rels = extract_relations(src, "javascript").unwrap();
    assert!(
        rels.iter().any(|r| r.relation == REL_REFERENCES
            && r.target_name == "handler"
            && r.source_name == "caller"),
        "const-binding RHS fn must emit references caller->handler; got: {:?}",
        rels.iter()
            .map(|r| (
                r.relation.as_str(),
                r.source_name.as_str(),
                r.target_name.as_str()
            ))
            .collect::<Vec<_>>()
    );
}

#[test]
fn test_p3_rust_let_rhs_param_does_not_emit_references_edge() {
    // RHS is a parameter, not a global fn — M2 excludes.
    let src = r#"fn caller(p: i32) { let x = p; } fn p() {}"#;
    let rels = extract_relations(src, "rust").unwrap();
    assert!(
        !rels
            .iter()
            .any(|r| r.relation == REL_REFERENCES && r.target_name == "p"),
        "let RHS that is a parameter must NOT emit a references edge; got: {:?}",
        rels.iter()
            .map(|r| (
                r.relation.as_str(),
                r.source_name.as_str(),
                r.target_name.as_str()
            ))
            .collect::<Vec<_>>()
    );
}

#[test]
fn test_p4_rust_explicit_return_fn_emits_references_edge() {
    let src = r#"fn caller() -> F { return handler; } fn handler() {}"#;
    let rels = extract_relations(src, "rust").unwrap();
    assert!(
        rels.iter().any(|r| r.relation == REL_REFERENCES
            && r.target_name == "handler"
            && r.source_name == "caller"),
        "explicit `return fn` must emit references caller->handler; got: {:?}",
        rels.iter()
            .map(|r| (
                r.relation.as_str(),
                r.source_name.as_str(),
                r.target_name.as_str()
            ))
            .collect::<Vec<_>>()
    );
}

#[test]
fn test_p5_rust_tail_expr_fn_emits_references_edge() {
    // Rust tail expression (no `return`, no trailing `;`) returns the fn by name.
    let src = r#"fn caller() -> F { handler } fn handler() {}"#;
    let rels = extract_relations(src, "rust").unwrap();
    assert!(
        rels.iter().any(|r| r.relation == REL_REFERENCES
            && r.target_name == "handler"
            && r.source_name == "caller"),
        "Rust tail-expr fn must emit references caller->handler; got: {:?}",
        rels.iter()
            .map(|r| (
                r.relation.as_str(),
                r.source_name.as_str(),
                r.target_name.as_str()
            ))
            .collect::<Vec<_>>()
    );
}

#[test]
fn test_p6_js_return_fn_emits_references_edge() {
    let src = r#"function caller() { return handler; } function handler() {}"#;
    let rels = extract_relations(src, "javascript").unwrap();
    assert!(
        rels.iter().any(|r| r.relation == REL_REFERENCES
            && r.target_name == "handler"
            && r.source_name == "caller"),
        "JS `return fn` must emit references caller->handler; got: {:?}",
        rels.iter()
            .map(|r| (
                r.relation.as_str(),
                r.source_name.as_str(),
                r.target_name.as_str()
            ))
            .collect::<Vec<_>>()
    );
}

#[test]
fn test_p7_js_arrow_body_fn_emits_references_edge() {
    // `const f = () => handler` is an implicit-return of the fn by name.
    let src = r#"const f = () => handler; function handler() {}"#;
    let rels = extract_relations(src, "javascript").unwrap();
    assert!(
        rels.iter().any(|r| r.relation == REL_REFERENCES
            && r.target_name == "handler"
            && r.source_name == "f"),
        "JS arrow implicit-return fn must emit references f->handler; got: {:?}",
        rels.iter()
            .map(|r| (
                r.relation.as_str(),
                r.source_name.as_str(),
                r.target_name.as_str()
            ))
            .collect::<Vec<_>>()
    );
}

#[test]
fn test_p8_rust_tail_expr_local_does_not_emit_references_edge() {
    // Returning a LOCAL by tail expression must NOT reference a same-named fn (M2.5).
    let src = r#"fn caller() -> X { let r = compute(); r } fn r() {}"#;
    let rels = extract_relations(src, "rust").unwrap();
    assert!(
        !rels
            .iter()
            .any(|r| r.relation == REL_REFERENCES && r.target_name == "r"),
        "tail-expr returning a local must NOT emit a references edge; got: {:?}",
        rels.iter()
            .map(|r| (
                r.relation.as_str(),
                r.source_name.as_str(),
                r.target_name.as_str()
            ))
            .collect::<Vec<_>>()
    );
}

#[test]
fn test_p9_rust_return_call_does_not_emit_references_edge() {
    // `return helper();` is a CALL, not a value reference.
    let src = r#"fn caller() -> X { return helper(); } fn helper() {}"#;
    let rels = extract_relations(src, "rust").unwrap();
    assert!(
        !rels
            .iter()
            .any(|r| r.relation == REL_REFERENCES && r.target_name == "helper"),
        "returning a call result must NOT emit a references edge; got: {:?}",
        rels.iter()
            .map(|r| (r.relation.as_str(), r.target_name.as_str()))
            .collect::<Vec<_>>()
    );
}

// ── Phase 2 Inc A: struct / object field VALUES (`Config { cb: handler }`) ──

#[test]
fn test_q1_rust_struct_field_value_fn_emits_references_edge() {
    let src = r#"fn caller() { let _c = Config { cb: handler }; } fn handler() {}"#;
    let rels = extract_relations(src, "rust").unwrap();
    assert!(
        rels.iter().any(|r| r.relation == REL_REFERENCES
            && r.target_name == "handler"
            && r.source_name == "caller"),
        "struct field value fn must emit references caller->handler; got: {:?}",
        rels.iter()
            .map(|r| (
                r.relation.as_str(),
                r.source_name.as_str(),
                r.target_name.as_str()
            ))
            .collect::<Vec<_>>()
    );
}

#[test]
fn test_q2_js_object_property_value_fn_emits_references_edge() {
    let src = r#"function caller() { const o = { onClick: handler }; } function handler() {}"#;
    let rels = extract_relations(src, "javascript").unwrap();
    assert!(
        rels.iter().any(|r| r.relation == REL_REFERENCES
            && r.target_name == "handler"
            && r.source_name == "caller"),
        "object property value fn must emit references caller->handler; got: {:?}",
        rels.iter()
            .map(|r| (
                r.relation.as_str(),
                r.source_name.as_str(),
                r.target_name.as_str()
            ))
            .collect::<Vec<_>>()
    );
}

#[test]
fn test_q3_js_object_property_key_does_not_emit_references_edge() {
    // The property KEY (`handler:`) is not a value reference — only the value is.
    let src = r#"function caller() { const o = { handler: compute() }; } function handler() {}"#;
    let rels = extract_relations(src, "javascript").unwrap();
    assert!(
        !rels
            .iter()
            .any(|r| r.relation == REL_REFERENCES && r.target_name == "handler"),
        "an object property key must NOT emit a references edge; got: {:?}",
        rels.iter()
            .map(|r| (r.relation.as_str(), r.target_name.as_str()))
            .collect::<Vec<_>>()
    );
}

// ── Phase 2 Inc B: Python value references (call-arg / keyword / RHS / return) ──

#[test]
fn test_b1_python_call_arg_fn_emits_references_edge() {
    let src = "def caller():\n    install(handler)\n\ndef handler():\n    pass\n";
    let rels = extract_relations(src, "python").unwrap();
    assert!(
        rels.iter().any(|r| r.relation == REL_REFERENCES
            && r.target_name == "handler"
            && r.source_name == "caller"),
        "python call-arg fn must emit references caller->handler; got: {:?}",
        rels.iter()
            .map(|r| (
                r.relation.as_str(),
                r.source_name.as_str(),
                r.target_name.as_str()
            ))
            .collect::<Vec<_>>()
    );
}

#[test]
fn test_b2_python_keyword_arg_fn_emits_references_edge() {
    let src = "def caller():\n    sorted(xs, key=my_key)\n\ndef my_key():\n    pass\n";
    let rels = extract_relations(src, "python").unwrap();
    assert!(
        rels.iter().any(|r| r.relation == REL_REFERENCES
            && r.target_name == "my_key"
            && r.source_name == "caller"),
        "python keyword-arg fn value must emit references caller->my_key; got: {:?}",
        rels.iter()
            .map(|r| (
                r.relation.as_str(),
                r.source_name.as_str(),
                r.target_name.as_str()
            ))
            .collect::<Vec<_>>()
    );
}

#[test]
fn test_b3_python_assignment_rhs_fn_emits_references_edge() {
    let src = "def caller():\n    cb = handler\n\ndef handler():\n    pass\n";
    let rels = extract_relations(src, "python").unwrap();
    assert!(
        rels.iter().any(|r| r.relation == REL_REFERENCES
            && r.target_name == "handler"
            && r.source_name == "caller"),
        "python assignment RHS fn must emit references caller->handler; got: {:?}",
        rels.iter()
            .map(|r| (
                r.relation.as_str(),
                r.source_name.as_str(),
                r.target_name.as_str()
            ))
            .collect::<Vec<_>>()
    );
}

#[test]
fn test_b4_python_return_fn_emits_references_edge() {
    let src = "def caller():\n    return handler\n\ndef handler():\n    pass\n";
    let rels = extract_relations(src, "python").unwrap();
    assert!(
        rels.iter().any(|r| r.relation == REL_REFERENCES
            && r.target_name == "handler"
            && r.source_name == "caller"),
        "python return fn must emit references caller->handler; got: {:?}",
        rels.iter()
            .map(|r| (
                r.relation.as_str(),
                r.source_name.as_str(),
                r.target_name.as_str()
            ))
            .collect::<Vec<_>>()
    );
}

#[test]
fn test_b5_python_param_does_not_emit_references_edge() {
    let src = "def caller(handler):\n    install(handler)\n";
    let rels = extract_relations(src, "python").unwrap();
    assert!(
        !rels
            .iter()
            .any(|r| r.relation == REL_REFERENCES && r.target_name == "handler"),
        "python parameter passed through must NOT emit a references edge; got: {:?}",
        rels.iter()
            .map(|r| (
                r.relation.as_str(),
                r.source_name.as_str(),
                r.target_name.as_str()
            ))
            .collect::<Vec<_>>()
    );
}

#[test]
fn test_b6_python_local_assignment_target_does_not_emit_references_edge() {
    let src = "def caller():\n    db = get()\n    use(db)\n\ndef db():\n    pass\n";
    let rels = extract_relations(src, "python").unwrap();
    assert!(
        !rels
            .iter()
            .any(|r| r.relation == REL_REFERENCES && r.target_name == "db"),
        "a python local assignment target must NOT emit a references edge; got: {:?}",
        rels.iter()
            .map(|r| (
                r.relation.as_str(),
                r.source_name.as_str(),
                r.target_name.as_str()
            ))
            .collect::<Vec<_>>()
    );
}

#[test]
fn test_b7_python_call_in_arg_does_not_emit_references_edge() {
    let src = "def caller():\n    foo(bar())\n\ndef bar():\n    pass\n";
    let rels = extract_relations(src, "python").unwrap();
    assert!(
        !rels
            .iter()
            .any(|r| r.relation == REL_REFERENCES && r.target_name == "bar"),
        "a called fn in arg position must NOT emit a references edge; got: {:?}",
        rels.iter()
            .map(|r| (r.relation.as_str(), r.target_name.as_str()))
            .collect::<Vec<_>>()
    );
}

// ── Phase 2 Inc C: Go value references (call-arg / := RHS / return) ──

#[test]
fn test_c1_go_call_arg_fn_emits_references_edge() {
    let src = "package main\nfunc caller() { install(handler) }\nfunc handler() {}\n";
    let rels = extract_relations(src, "go").unwrap();
    assert!(
        rels.iter().any(|r| r.relation == REL_REFERENCES
            && r.target_name == "handler"
            && r.source_name == "caller"),
        "go call-arg fn must emit references caller->handler; got: {:?}",
        rels.iter()
            .map(|r| (
                r.relation.as_str(),
                r.source_name.as_str(),
                r.target_name.as_str()
            ))
            .collect::<Vec<_>>()
    );
}

#[test]
fn test_c2_go_short_var_rhs_fn_emits_references_edge() {
    let src = "package main\nfunc caller() { cb := handler; _ = cb }\nfunc handler() {}\n";
    let rels = extract_relations(src, "go").unwrap();
    assert!(
        rels.iter().any(|r| r.relation == REL_REFERENCES
            && r.target_name == "handler"
            && r.source_name == "caller"),
        "go := RHS fn must emit references caller->handler; got: {:?}",
        rels.iter()
            .map(|r| (
                r.relation.as_str(),
                r.source_name.as_str(),
                r.target_name.as_str()
            ))
            .collect::<Vec<_>>()
    );
}

#[test]
fn test_c3_go_return_fn_emits_references_edge() {
    let src = "package main\nfunc caller() func() { return handler }\nfunc handler() {}\n";
    let rels = extract_relations(src, "go").unwrap();
    assert!(
        rels.iter().any(|r| r.relation == REL_REFERENCES
            && r.target_name == "handler"
            && r.source_name == "caller"),
        "go return fn must emit references caller->handler; got: {:?}",
        rels.iter()
            .map(|r| (
                r.relation.as_str(),
                r.source_name.as_str(),
                r.target_name.as_str()
            ))
            .collect::<Vec<_>>()
    );
}

#[test]
fn test_c4_go_param_does_not_emit_references_edge() {
    let src = "package main\nfunc caller(handler func()) { install(handler) }\n";
    let rels = extract_relations(src, "go").unwrap();
    assert!(
        !rels
            .iter()
            .any(|r| r.relation == REL_REFERENCES && r.target_name == "handler"),
        "go parameter passed through must NOT emit a references edge; got: {:?}",
        rels.iter()
            .map(|r| (
                r.relation.as_str(),
                r.source_name.as_str(),
                r.target_name.as_str()
            ))
            .collect::<Vec<_>>()
    );
}

#[test]
fn test_c5_go_short_var_local_does_not_emit_references_edge() {
    let src = "package main\nfunc caller() { db := get(); use(db) }\nfunc db() {}\n";
    let rels = extract_relations(src, "go").unwrap();
    assert!(
        !rels
            .iter()
            .any(|r| r.relation == REL_REFERENCES && r.target_name == "db"),
        "a go := local must NOT emit a references edge; got: {:?}",
        rels.iter()
            .map(|r| (
                r.relation.as_str(),
                r.source_name.as_str(),
                r.target_name.as_str()
            ))
            .collect::<Vec<_>>()
    );
}

#[test]
fn test_c6_go_call_in_arg_does_not_emit_references_edge() {
    let src = "package main\nfunc caller() { foo(bar()) }\nfunc bar() {}\n";
    let rels = extract_relations(src, "go").unwrap();
    assert!(
        !rels
            .iter()
            .any(|r| r.relation == REL_REFERENCES && r.target_name == "bar"),
        "a called fn in arg position must NOT emit a references edge; got: {:?}",
        rels.iter()
            .map(|r| (r.relation.as_str(), r.target_name.as_str()))
            .collect::<Vec<_>>()
    );
}

// ── Phase 3a: C/C++ value references (call-arg / &fn / designated init / RHS / return) ──

#[test]
fn test_d1_c_call_arg_fn_emits_references_edge() {
    let src = "void handler(void) {}\nvoid caller(void) { install(handler); }\n";
    let rels = extract_relations(src, "c").unwrap();
    assert!(
        rels.iter().any(|r| r.relation == REL_REFERENCES
            && r.target_name == "handler"
            && r.source_name == "caller"),
        "c call-arg fn must emit references caller->handler; got: {:?}",
        rels.iter()
            .map(|r| (
                r.relation.as_str(),
                r.source_name.as_str(),
                r.target_name.as_str()
            ))
            .collect::<Vec<_>>()
    );
}

#[test]
fn test_d2_c_address_of_fn_emits_references_edge() {
    let src = "void handler(int s) {}\nvoid caller(void) { signal(2, &handler); }\n";
    let rels = extract_relations(src, "c").unwrap();
    assert!(
        rels.iter().any(|r| r.relation == REL_REFERENCES
            && r.target_name == "handler"
            && r.source_name == "caller"),
        "c &fn must emit references caller->handler; got: {:?}",
        rels.iter()
            .map(|r| (
                r.relation.as_str(),
                r.source_name.as_str(),
                r.target_name.as_str()
            ))
            .collect::<Vec<_>>()
    );
}

#[test]
fn test_d3_c_designated_initializer_vtable_emits_references_edge() {
    let src = "int my_read(void) { return 0; }\nvoid caller(void) { struct ops o = { .read = my_read }; }\n";
    let rels = extract_relations(src, "c").unwrap();
    assert!(
        rels.iter().any(|r| r.relation == REL_REFERENCES
            && r.target_name == "my_read"
            && r.source_name == "caller"),
        "c designated-init vtable field must emit references caller->my_read; got: {:?}",
        rels.iter()
            .map(|r| (
                r.relation.as_str(),
                r.source_name.as_str(),
                r.target_name.as_str()
            ))
            .collect::<Vec<_>>()
    );
}

#[test]
fn test_d4_c_init_declarator_rhs_fn_emits_references_edge() {
    let src = "void handler(void) {}\nvoid caller(void) { fn_t cb = handler; (void)cb; }\n";
    let rels = extract_relations(src, "c").unwrap();
    assert!(
        rels.iter().any(|r| r.relation == REL_REFERENCES
            && r.target_name == "handler"
            && r.source_name == "caller"),
        "c init-declarator RHS fn must emit references caller->handler; got: {:?}",
        rels.iter()
            .map(|r| (
                r.relation.as_str(),
                r.source_name.as_str(),
                r.target_name.as_str()
            ))
            .collect::<Vec<_>>()
    );
}

#[test]
fn test_d5_c_return_fn_emits_references_edge() {
    let src = "void handler(void) {}\nfn_t caller(void) { return handler; }\n";
    let rels = extract_relations(src, "c").unwrap();
    assert!(
        rels.iter().any(|r| r.relation == REL_REFERENCES
            && r.target_name == "handler"
            && r.source_name == "caller"),
        "c return fn must emit references caller->handler; got: {:?}",
        rels.iter()
            .map(|r| (
                r.relation.as_str(),
                r.source_name.as_str(),
                r.target_name.as_str()
            ))
            .collect::<Vec<_>>()
    );
}

#[test]
fn test_d6_c_param_does_not_emit_references_edge() {
    let src = "void caller(fn_t handler) { install(handler); }\n";
    let rels = extract_relations(src, "c").unwrap();
    assert!(
        !rels
            .iter()
            .any(|r| r.relation == REL_REFERENCES && r.target_name == "handler"),
        "c parameter passed through must NOT emit a references edge; got: {:?}",
        rels.iter()
            .map(|r| (
                r.relation.as_str(),
                r.source_name.as_str(),
                r.target_name.as_str()
            ))
            .collect::<Vec<_>>()
    );
}

#[test]
fn test_d7_c_local_declaration_does_not_emit_references_edge() {
    let src = "void use(void* p) {}\nvoid db(void) {}\nvoid caller(void) { void* db = get(); use(db); }\n";
    let rels = extract_relations(src, "c").unwrap();
    assert!(
        !rels
            .iter()
            .any(|r| r.relation == REL_REFERENCES && r.target_name == "db"),
        "a c local declaration must NOT emit a references edge; got: {:?}",
        rels.iter()
            .map(|r| (
                r.relation.as_str(),
                r.source_name.as_str(),
                r.target_name.as_str()
            ))
            .collect::<Vec<_>>()
    );
}

#[test]
fn test_d8_c_call_in_arg_does_not_emit_references_edge() {
    let src = "int bar(void) { return 0; }\nvoid caller(void) { foo(bar()); }\n";
    let rels = extract_relations(src, "c").unwrap();
    assert!(
        !rels
            .iter()
            .any(|r| r.relation == REL_REFERENCES && r.target_name == "bar"),
        "a called fn in arg position must NOT emit a references edge; got: {:?}",
        rels.iter()
            .map(|r| (r.relation.as_str(), r.target_name.as_str()))
            .collect::<Vec<_>>()
    );
}

#[test]
fn test_d9_cpp_call_arg_fn_emits_references_edge() {
    // Confirm the cpp config dispatches the value-reference pass too.
    let src = "void handler() {}\nvoid caller() { install(handler); }\n";
    let rels = extract_relations(src, "cpp").unwrap();
    assert!(
        rels.iter().any(|r| r.relation == REL_REFERENCES
            && r.target_name == "handler"
            && r.source_name == "caller"),
        "cpp call-arg fn must emit references caller->handler; got: {:?}",
        rels.iter()
            .map(|r| (
                r.relation.as_str(),
                r.source_name.as_str(),
                r.target_name.as_str()
            ))
            .collect::<Vec<_>>()
    );
}

// ── Phase 3b Inc E: JSX attribute callbacks (`onClick={handleClick}`) ──

#[test]
fn test_e1_tsx_jsx_attr_callback_emits_references_edge() {
    let src = r#"function caller() { return <Button onClick={handleClick} />; } function handleClick() {}"#;
    let rels = extract_relations(src, "tsx").unwrap();
    assert!(
        rels.iter().any(|r| r.relation == REL_REFERENCES
            && r.target_name == "handleClick"
            && r.source_name == "caller"),
        "JSX attr callback must emit references caller->handleClick; got: {:?}",
        rels.iter()
            .map(|r| (
                r.relation.as_str(),
                r.source_name.as_str(),
                r.target_name.as_str()
            ))
            .collect::<Vec<_>>()
    );
}

#[test]
fn test_e2_tsx_jsx_attr_arrow_does_not_emit_bare_reference() {
    // An inline arrow attr (`onClick={() => h()}`) is not a bare-id value reference.
    let src =
        r#"function caller() { return <Button onClick={() => other()} />; } function other() {}"#;
    let rels = extract_relations(src, "tsx").unwrap();
    assert!(
        !rels
            .iter()
            .any(|r| r.relation == REL_REFERENCES && r.target_name == "other"),
        "an inline-arrow JSX attr must NOT emit a bare references edge to its call; got: {:?}",
        rels.iter()
            .map(|r| (r.relation.as_str(), r.target_name.as_str()))
            .collect::<Vec<_>>()
    );
}

// ── Phase 3b Inc F: Go composite-literal field values (`T{cb: fn}`) ──

#[test]
fn test_f1_go_composite_keyed_field_fn_emits_references_edge() {
    let src = "package main\nfunc caller() { _ = Handler{OnEvent: handler} }\nfunc handler() {}\n";
    let rels = extract_relations(src, "go").unwrap();
    assert!(
        rels.iter().any(|r| r.relation == REL_REFERENCES
            && r.target_name == "handler"
            && r.source_name == "caller"),
        "go composite keyed field fn must emit references caller->handler; got: {:?}",
        rels.iter()
            .map(|r| (
                r.relation.as_str(),
                r.source_name.as_str(),
                r.target_name.as_str()
            ))
            .collect::<Vec<_>>()
    );
}

// ── Phase 3b Inc G: tuple return / RHS (Python `return f, g`) ──

#[test]
fn test_g1_python_tuple_return_emits_references_edges() {
    let src = "def caller():\n    return f, g\n\ndef f():\n    pass\n\ndef g():\n    pass\n";
    let rels = extract_relations(src, "python").unwrap();
    let got: Vec<&str> = rels
        .iter()
        .filter(|r| r.relation == REL_REFERENCES && r.source_name == "caller")
        .map(|r| r.target_name.as_str())
        .collect();
    assert!(
        got.contains(&"f") && got.contains(&"g"),
        "python tuple return must emit references to both f and g; got: {:?}",
        got
    );
}

// ── Phase 3b Inc H: primitive-type-head path residual (`str::trim`) ──

#[test]
fn test_h1_rust_primitive_head_path_does_not_emit_references_edge() {
    let src = r#"fn caller() { let _ = xs.iter().map(str::trim); } fn trim() {}"#;
    let rels = extract_relations(src, "rust").unwrap();
    assert!(
        !rels
            .iter()
            .any(|r| r.relation == REL_REFERENCES && r.target_name == "trim"),
        "a primitive-type-head path (`str::trim`) must NOT emit a references edge; got: {:?}",
        rels.iter()
            .map(|r| (r.relation.as_str(), r.target_name.as_str()))
            .collect::<Vec<_>>()
    );
}

// --- v49 audit fixes: top-level call `<module>` fallback parity (C#/Kotlin/Swift/Dart) ---

#[test]
fn test_csharp_top_level_call_attributes_to_module() {
    // C# 9+ top-level statement calls (outside any method/type) must attribute to
    // <module>, mirroring the php/python/ruby arms — otherwise a function invoked
    // only from a top-level statement has no incoming edge and is false-reported
    // as dead-code. Before this fix the C# invocation_expression arm required
    // Some(active_scope), dropping every top-level call. INDEX_VERSION 48→49.
    let code = "int Helper() { return 1; }\nHelper();\n";
    let rels = extract_relations(code, "csharp").unwrap();
    let has_edge = rels.iter().any(|r| {
        r.relation == REL_CALLS && r.target_name == "Helper" && r.source_name == "<module>"
    });
    assert!(
        has_edge,
        "top-level Helper() must produce a <module> → Helper call edge; got calls: {:?}",
        rels.iter()
            .filter(|r| r.relation == REL_CALLS)
            .map(|r| (r.source_name.as_str(), r.target_name.as_str()))
            .collect::<Vec<_>>()
    );
}

#[test]
fn test_kotlin_top_level_call_attributes_to_module() {
    // Kotlin allows executable statements at file top level (script-style `main`-less
    // init). A top-level call must attribute to <module>. Before v49 the generic
    // call_expression fallback granted <module> only to js/ts/tsx.
    let code = "fun helper(): Int { return 1 }\nval x = helper()\n";
    let rels = extract_relations(code, "kotlin").unwrap();
    let has_edge = rels.iter().any(|r| {
        r.relation == REL_CALLS && r.target_name == "helper" && r.source_name == "<module>"
    });
    assert!(
        has_edge,
        "top-level helper() must produce a <module> → helper call edge; got calls: {:?}",
        rels.iter()
            .filter(|r| r.relation == REL_CALLS)
            .map(|r| (r.source_name.as_str(), r.target_name.as_str()))
            .collect::<Vec<_>>()
    );
}

#[test]
fn test_swift_top_level_call_attributes_to_module() {
    // Swift top-level code (main.swift / scripts) allows executable statements.
    // A top-level call must attribute to <module>. Before v49 only js/ts/tsx got
    // the generic call_expression <module> fallback.
    let code = "func helper() -> Int { return 1 }\nlet x = helper()\n";
    let rels = extract_relations(code, "swift").unwrap();
    let has_edge = rels.iter().any(|r| {
        r.relation == REL_CALLS && r.target_name == "helper" && r.source_name == "<module>"
    });
    assert!(
        has_edge,
        "top-level helper() must produce a <module> → helper call edge; got calls: {:?}",
        rels.iter()
            .filter(|r| r.relation == REL_CALLS)
            .map(|r| (r.source_name.as_str(), r.target_name.as_str()))
            .collect::<Vec<_>>()
    );
}

#[test]
fn test_rust_go_top_level_calls_still_excluded_from_module_fallback() {
    // Negative control: Rust/Go route through the same generic call_expression arm
    // but their top-level call omission is INTENTIONAL — extending the <module>
    // fallback to Kotlin/Swift must not leak into Rust/Go. (Rust has no bare
    // top-level call statement; a const initializer call sits at item level with no
    // enclosing fn — it must stay dropped, not attribute to <module>.)
    let rust = "const X: i32 = compute();\nfn compute() -> i32 { 1 }\n";
    let rels = extract_relations(rust, "rust").unwrap();
    assert!(
        !rels
            .iter()
            .any(|r| r.relation == REL_CALLS && r.source_name == "<module>"),
        "Rust top-level init calls must NOT attribute to <module>; got: {:?}",
        rels.iter()
            .filter(|r| r.relation == REL_CALLS)
            .map(|r| (r.source_name.as_str(), r.target_name.as_str()))
            .collect::<Vec<_>>()
    );

    let go = "package main\nvar x = compute()\nfunc compute() int { return 1 }\n";
    let grels = extract_relations(go, "go").unwrap();
    assert!(
        !grels
            .iter()
            .any(|r| r.relation == REL_CALLS && r.source_name == "<module>"),
        "Go package-level init calls must NOT attribute to <module>; got: {:?}",
        grels
            .iter()
            .filter(|r| r.relation == REL_CALLS)
            .map(|r| (r.source_name.as_str(), r.target_name.as_str()))
            .collect::<Vec<_>>()
    );
}

#[test]
fn test_dart_top_level_call_attributes_to_module() {
    // A Dart library-level (top-level) call must attribute to <module>, mirroring
    // the php/python/ruby/C# arms. Before v49 the Dart `selector` arm required
    // Some(active_scope), dropping library-level calls.
    let code = "int helper() => 1;\nfinal x = helper();\n";
    let rels = extract_relations(code, "dart").unwrap();
    let has_edge = rels.iter().any(|r| {
        r.relation == REL_CALLS && r.target_name == "helper" && r.source_name == "<module>"
    });
    assert!(
        has_edge,
        "top-level helper() must produce a <module> → helper call edge; got calls: {:?}",
        rels.iter()
            .filter(|r| r.relation == REL_CALLS)
            .map(|r| (r.source_name.as_str(), r.target_name.as_str()))
            .collect::<Vec<_>>()
    );
}

// --- v49 audit fix: Dart `mixin M {}` extracted as a symbol node ---

#[test]
fn test_dart_mixin_declaration_extracted_as_node() {
    // `mixin M {}` parses as (mixin_declaration (mixin) (identifier) (class_body))
    // — the name is a POSITIONAL identifier child, not a `name:` field, so the
    // class arm missed it. Without a node for the mixin, the `with M` inherits edge
    // (emitted by relations/inherits.rs) drops at Phase-2 same-language resolution.
    let code = "class Derived extends Base with MixinA {}\nmixin MixinA {}\nclass Base {}\n";
    let nodes = crate::parser::treesitter::parse_code(code, "dart").unwrap();
    let mixin = nodes.iter().find(|n| n.name == "MixinA");
    assert!(
        mixin.is_some(),
        "Dart `mixin MixinA` must be extracted as a symbol node named `MixinA`; got: {:?}",
        nodes
            .iter()
            .map(|n| (n.node_type.as_str(), n.name.as_str()))
            .collect::<Vec<_>>()
    );
    // The name must be the bare identifier, never `mixin MixinA`.
    assert_eq!(mixin.unwrap().name, "MixinA");
    // And the inherits edge Derived→MixinA is still emitted (target now resolvable).
    let rels = extract_relations(code, "dart").unwrap();
    assert!(
        rels.iter().any(|r| r.relation == REL_INHERITS
            && r.source_name == "Derived"
            && r.target_name == "MixinA"),
        "inherits edge Derived→MixinA must be emitted; got inherits: {:?}",
        rels.iter()
            .filter(|r| r.relation == REL_INHERITS)
            .map(|r| (r.source_name.as_str(), r.target_name.as_str()))
            .collect::<Vec<_>>()
    );
}

// --- v49 audit fix: callee-qualifier metadata JSON built with serde_json (escaping) ---

#[test]
fn test_serialize_callee_qualifier_escapes_special_chars() {
    use super::helpers::CalleeQualifier;
    // A payload containing `"` and `\` must produce VALID JSON (escaped), not a
    // malformed blob that parse_callee_metadata / json_extract silently reject.
    let q = CalleeQualifier::Receiver("a\"b\\c".to_string());
    let s = serialize_callee_qualifier(&q).unwrap();
    let v: serde_json::Value =
        serde_json::from_str(&s).unwrap_or_else(|e| panic!("must be valid JSON; got {s:?}: {e}"));
    assert_eq!(v.get("q").and_then(|x| x.as_str()), Some("recv"));
    assert_eq!(
        v.get("v").and_then(|x| x.as_str()),
        Some("a\"b\\c"),
        "the `v` payload must round-trip through JSON unescaping"
    );
    // Byte-identical to the old format! output for the common identifier-only case.
    let plain = serialize_callee_qualifier(&CalleeQualifier::Receiver("foo".to_string())).unwrap();
    assert_eq!(
        plain, r#"{"q":"recv","v":"foo"}"#,
        "identifier-only payload must stay byte-identical to the pre-fix format"
    );
    let stype = serialize_callee_qualifier(&CalleeQualifier::SelfType("Db".to_string())).unwrap();
    assert_eq!(stype, r#"{"q":"stype","v":"Db"}"#);
    let path =
        serialize_callee_qualifier(&CalleeQualifier::Path(vec!["a".into(), "b".into()])).unwrap();
    assert_eq!(path, r#"{"q":"path","v":"a::b"}"#);
    assert_eq!(
        serialize_callee_qualifier(&CalleeQualifier::Chain).unwrap(),
        r#"{"q":"chain"}"#
    );
}

// --- v49 audit fix: doc_comment NUL bytes stripped (FTS5 C-string truncation) ---

#[test]
fn test_doc_comment_strips_nul_bytes() {
    // doc_comment is stored as SQLite TEXT and fed to FTS5, which stops at the first
    // NUL — so a NUL inside a preceding comment would make everything after it
    // unsearchable. Strip NUL→space (same policy as code_content since v48).
    // Use a Rust block comment: its lexer scans to `*/` and keeps an embedded NUL
    // inside the comment token (a line comment or a JS `/*` at stmt-start would be
    // re-tokenized around the NUL and never reach get_preceding_comment intact).
    let code = "/* a\0b */\nfn foo() -> i32 { 1 }\n";
    let nodes = crate::parser::treesitter::parse_code(code, "rust").unwrap();
    let foo = nodes
        .iter()
        .find(|n| n.name == "foo")
        .expect("fn foo must be extracted");
    let doc = foo
        .doc_comment
        .clone()
        .expect("foo must carry the preceding block comment");
    assert!(
        !doc.contains('\0'),
        "doc_comment must not contain NUL bytes; got: {doc:?}"
    );
    // The bytes around the NUL must remain (NUL→space, not truncated at the NUL).
    assert_eq!(
        doc, "/* a b */",
        "NUL must become a space with all other bytes unchanged; got: {doc:?}"
    );
}

// --- v50: constructor-instantiation call edges (Task 1) ---
// RED baseline (probe, pre-fix): JS/TS/TSX `new Foo()` produced ZERO edges;
// C#/PHP `new Foo()` produced ZERO edges; only Java `new Foo()` emitted a
// `references` edge (the type-reference pass). Adding a `calls` edge to the
// constructor name makes an only-instantiated class visible to callgraph/impact
// and non-dead.

fn calls_of(rels: &[ParsedRelation]) -> Vec<(String, String)> {
    rels.iter()
        .filter(|r| r.relation == REL_CALLS)
        .map(|r| (r.source_name.clone(), r.target_name.clone()))
        .collect()
}

#[test]
fn test_js_new_expression_creates_calls_edge() {
    // Bare, member, generic, and top-level (<module>) forms.
    let code = "function build() { const a = new Widget(); const b = new ns.Panel(); }\n\
                const top = new Root();\n";
    let rels = extract_relations(code, "javascript").unwrap();
    let calls = calls_of(&rels);
    assert!(
        calls.contains(&("build".into(), "Widget".into())),
        "new Widget() → build→Widget calls edge; got {calls:?}"
    );
    assert!(
        calls.contains(&("build".into(), "Panel".into())),
        "new ns.Panel() → build→Panel (member form, bare class name); got {calls:?}"
    );
    assert!(
        calls.contains(&("<module>".into(), "Root".into())),
        "top-level new Root() → <module>→Root; got {calls:?}"
    );
    // Member form carries a Receiver qualifier consistent with member call_expressions.
    let panel = rels
        .iter()
        .find(|r| r.relation == REL_CALLS && r.target_name == "Panel")
        .unwrap();
    assert_eq!(
        panel.metadata.as_deref(),
        Some(r#"{"q":"recv","v":"ns"}"#),
        "member new ns.Panel() carries recv=ns; got {:?}",
        panel.metadata
    );
}

#[test]
fn test_ts_new_expression_creates_calls_edge_strips_generics() {
    let code = "function build() { const a = new Store<State>(); const b = new mod.Cache(); }\n";
    let rels = extract_relations(code, "typescript").unwrap();
    let calls = calls_of(&rels);
    assert!(
        calls.contains(&("build".into(), "Store".into())),
        "new Store<State>() → build→Store (generic arg stripped); got {calls:?}"
    );
    assert!(
        calls.contains(&("build".into(), "Cache".into())),
        "new mod.Cache() → build→Cache; got {calls:?}"
    );
}

#[test]
fn test_tsx_new_expression_creates_calls_edge() {
    let code = "function build() { const a = new Model(); }\n";
    let rels = extract_relations(code, "tsx").unwrap();
    assert!(
        calls_of(&rels).contains(&("build".into(), "Model".into())),
        "TSX new Model() → build→Model; got {:?}",
        calls_of(&rels)
    );
}

#[test]
fn test_csharp_object_creation_creates_calls_edge() {
    // Bare and qualified forms; top-level new → <module>.
    let code = "class App { void M() { var a = new Widget(); var b = new Ns.Panel(); } }\n";
    let rels = extract_relations(code, "csharp").unwrap();
    let calls = calls_of(&rels);
    assert!(
        calls.contains(&("App.M".into(), "Widget".into())),
        "C# new Widget() → App.M→Widget; got {calls:?}"
    );
    assert!(
        calls.contains(&("App.M".into(), "Panel".into())),
        "C# new Ns.Panel() → App.M→Panel (qualified tail); got {calls:?}"
    );
}

#[test]
fn test_php_object_creation_creates_calls_edge() {
    // Bare, namespaced, and the relative `self`/`static`/`parent` skips.
    let code =
        "<?php\nfunction build() { $a = new Widget(); $b = new Ns\\Panel(); $c = new self(); }\n";
    let rels = extract_relations(code, "php").unwrap();
    let calls = calls_of(&rels);
    assert!(
        calls.contains(&("build".into(), "Widget".into())),
        "PHP new Widget() → build→Widget; got {calls:?}"
    );
    assert!(
        calls.contains(&("build".into(), "Panel".into())),
        "PHP new Ns\\Panel() → build→Panel (last segment); got {calls:?}"
    );
    assert!(
        !calls.iter().any(|(_, t)| t == "self"),
        "PHP new self() must NOT emit a `self` calls edge; got {calls:?}"
    );
}

#[test]
fn test_java_object_creation_covered_by_type_reference() {
    // Matrix verdict: Java `new Foo()` is COVERED today via extract_java_type_reference
    // (a `references` edge on the `new` type), so it is deliberately NOT extended to a
    // calls edge in v50. This guards that the covering edge still exists.
    let code = "class A { void m() { Foo x = new Foo(); } }\n";
    let rels = extract_relations(code, "java").unwrap();
    let refs: Vec<&str> = rels
        .iter()
        .filter(|r| r.relation == REL_REFERENCES)
        .map(|r| r.target_name.as_str())
        .collect();
    assert!(
        refs.contains(&"Foo"),
        "Java new Foo() must keep its references edge (dead-code-safe); got refs {refs:?}"
    );
}

// --- v50: C# top-level local function extraction (Task 2) ---
#[test]
fn test_csharp_local_function_extracted_as_symbol() {
    // RED baseline (probe, pre-fix): parse_code produced ZERO symbols for a
    // top-level `void Greet(){}`, so the v49 <module>→Greet call edge dangled.
    let code = "void Greet() { Log(); }\nGreet();\n";
    let nodes = crate::parser::treesitter::parse_code(code, "csharp").unwrap();
    let greet = nodes.iter().find(|n| n.name == "Greet").unwrap_or_else(|| {
        panic!(
            "C# top-level local fn Greet must be extracted; got {:?}",
            nodes
                .iter()
                .map(|n| (n.node_type.as_str(), n.name.as_str()))
                .collect::<Vec<_>>()
        )
    });
    assert_eq!(
        greet.node_type, "function",
        "top-level local fn is function-kind"
    );
    // The <module>→Greet call edge (v49) now has a resolvable target node.
    let rels = extract_relations(code, "csharp").unwrap();
    assert!(
        calls_of(&rels).contains(&("<module>".into(), "Greet".into())),
        "top-level Greet() → <module>→Greet edge present; got {:?}",
        calls_of(&rels)
    );
}

// --- v50: edge-metadata serde_json migration (Task 3) ---
#[test]
fn test_rtype_and_impl_method_metadata_escape_hostile_names() {
    // The two migrated metadata builders must produce valid JSON even for a name
    // carrying `"` and `\` (serde_json escapes; the old format! form did not for
    // rtype). Parse back and confirm the value round-trips exactly.
    let hostile = r#"Ev"il\Type"#;
    for built in [
        serialize_rtype_metadata(hostile),
        serialize_impl_method_metadata(hostile),
    ] {
        let v: serde_json::Value = serde_json::from_str(&built)
            .unwrap_or_else(|e| panic!("metadata must be valid JSON, got {built:?}: {e}"));
        assert_eq!(
            v["v"].as_str(),
            Some(hostile),
            "hostile name must round-trip through the escaped JSON; got {built:?}"
        );
    }
    // Byte-identical to the historic form for the identifier-only common case.
    assert_eq!(
        serialize_rtype_metadata("DataWriter"),
        r#"{"q":"rtype","v":"DataWriter"}"#
    );
    assert_eq!(
        serialize_impl_method_metadata("Db"),
        r#"{"q":"impl_method","v":"Db"}"#
    );
}

// --- v50: NUL strip on the signature triplet (Task 4) ---
#[test]
fn test_signature_fields_strip_nul_bytes() {
    // A NUL inside a default-value string in the signature region must be
    // stripped from param_types + signature (node_text is a raw byte slice, so it
    // would otherwise carry the NUL → SQLite LIKE truncates at it). The
    // context_string built downstream derives from these now-clean fields.
    let code = "function f(x: string = \"a\0b\"): number { return 1; }\n";
    let nodes = crate::parser::treesitter::parse_code(code, "typescript").unwrap();
    let f = nodes
        .iter()
        .find(|n| n.name == "f")
        .expect("fn f extracted");
    let params = f.param_types.clone().expect("f has params");
    assert!(
        !params.contains('\0'),
        "param_types must be NUL-free; got {params:?}"
    );
    assert!(
        params.contains('b'),
        "bytes after the NUL must survive; got {params:?}"
    );
    if let Some(sig) = &f.signature {
        assert!(
            !sig.contains('\0'),
            "signature must be NUL-free; got {sig:?}"
        );
    }
}

#[test]
fn test_rust_cfg_predicates_are_not_calls() {
    // `#[cfg(not(windows))]` and `cfg!(any(unix))` put `not(…)` / `any(…)` in a
    // token_tree that is byte-identical to a call. Every cfg predicate name is
    // lowercase, so the CamelCase pattern guard (which catches `Some(y)`) waves
    // them straight through: the production index carried `any` ×3 and `not` ×4
    // in pending_unresolved_calls, each traced to a real `#[cfg(not(windows))]`
    // inside a function body. A project defining `fn any` / `fn not` — ordinary
    // in predicate and iterator utility modules — promotes those to real edges
    // pointing at the wrong symbol.
    let source = r#"
fn run() {
    #[cfg(not(windows))]
    let a = 1;
    #[cfg(any(unix, target_os = "macos"))]
    let b = 2;
    if cfg!(any(unix)) { compute(); }
    if cfg!(not(test)) { compute(); }
    assert_eq!(recovered(1), 2);
    // Scoped spellings: the `macro` field is a scoped_identifier whose full text
    // is `core::cfg`, which never equalled `cfg`.
    if core::cfg!(any(unix)) { compute(); }
    if std::cfg!(all(unix, target_pointer_width = "64")) { compute(); }
}
"#;
    let tree = crate::parser::treesitter::parse_tree(source, "rust").unwrap();
    let relations = extract_relations_from_tree(&tree, source, "rust");
    let calls: Vec<&str> = relations
        .iter()
        .filter(|r| r.relation == REL_CALLS)
        .map(|r| r.target_name.as_str())
        .collect();
    for predicate in ["any", "all", "not", "cfg"] {
        assert!(
            !calls.contains(&predicate),
            "cfg predicate `{predicate}` must not become a calls edge, got: {calls:?}"
        );
    }
    // Negative control. `compute()` in the `if cfg!(..) { compute(); }` bodies
    // above does NOT serve: it is a plain `call_expression` that never reaches
    // `extract_rust_macro_token_call`, so it survives even if that pass is
    // stubbed to `return None` — an earlier version of this test used it and was
    // asserting nothing. `recovered()` sits inside macro ARGUMENTS, which is the
    // only place this pass operates.
    assert!(
        calls.contains(&"recovered"),
        "a genuine call inside macro arguments must still emit an edge, got: {calls:?}"
    );
}

#[test]
fn test_rust_attribute_arguments_are_not_calls() {
    // The same shape outside `cfg!`: an attribute's argument list is metadata,
    // never code. The attribute must be INSIDE a function — the token-tree pass
    // requires an enclosing scope, so module-level attributes were never at risk
    // and testing one there would assert nothing.
    let source = r#"
fn run() {
    #[cfg_attr(test, serde(rename_all = "camelCase"))]
    struct Inner;
    helper();
}
"#;
    let tree = crate::parser::treesitter::parse_tree(source, "rust").unwrap();
    let relations = extract_relations_from_tree(&tree, source, "rust");
    let calls: Vec<&str> = relations
        .iter()
        .filter(|r| r.relation == REL_CALLS)
        .map(|r| r.target_name.as_str())
        .collect();
    for attr_token in ["cfg_attr", "serde", "feature"] {
        assert!(
            !calls.contains(&attr_token),
            "attribute argument `{attr_token}` must not become a calls edge, got: {calls:?}"
        );
    }
}

#[test]
fn test_rust_cfg_predicates_inside_another_macro_are_not_calls() {
    // The ancestor walk in `in_cfg_predicate` cannot see this shape: a
    // `#[cfg(...)]` written INSIDE another macro's token_tree is raw tokens —
    // there is no `attribute` node in the tree at all, and the enclosing
    // macro_invocation is `cfg_if!`/`quote!`, not `cfg!`. `cfg_if!` is the
    // idiomatic home of conditional compilation in a macro (libc, rand, ring),
    // so this is where the remaining volume lives; it additionally leaked `cfg`
    // itself as a callee name, which the attribute path never produced.
    let cases = [
        "fn run() { cfg_if! { if #[cfg(not(windows))] { helper_a(); } else if #[cfg(any(unix))] { helper_b(); } } }",
        "fn run() { quote! { #[cfg(all(feature = \"x\"))] fn z() {} }; helper_a(); }",
    ];
    for src in cases {
        let rels = extract_relations(src, "rust").unwrap();
        let calls: Vec<&str> = rels
            .iter()
            .filter(|r| r.relation == REL_CALLS)
            .map(|r| r.target_name.as_str())
            .collect();
        for predicate in ["cfg", "any", "all", "not"] {
            assert!(
                !calls.contains(&predicate),
                "cfg predicate `{predicate}` must not become a calls edge inside another macro, got: {calls:?}"
            );
        }
        // Negative control: the genuine calls in the same token soup survive.
        // Without this, returning None for everything would pass the above.
        assert!(
            calls.contains(&"helper_a"),
            "real calls inside the same macro must still be recovered, got: {calls:?}"
        );
    }
}

#[test]
fn test_raw_attribute_tokens_in_macro_are_not_calls() {
    // A name blacklist (`cfg`/`any`/`all`/`not`) was the first attempt here and
    // patched one instance of a wider class: ANY attribute written as raw tokens
    // inside a macro's token_tree. `allow` / `deny` / `doc` / `serde` all took
    // the same path and produced call edges, and a project defining `fn allow`
    // promoted them to edges pointing at the wrong symbol.
    let src = r#"
pub fn host() {
    wrap! {
        #[cfg(any(unix, windows))]
        #[allow(unused)]
        #[deny(warnings)]
        #[doc(hidden)]
        #[serde(rename_all = "camelCase")]
        fn inner() { real(true); }
    }
}
"#;
    let rels = extract_relations(src, "rust").unwrap();
    let calls: Vec<&str> = rels
        .iter()
        .filter(|r| r.relation == REL_CALLS)
        .map(|r| r.target_name.as_str())
        .collect();
    for attr in ["cfg", "any", "allow", "deny", "doc", "serde"] {
        assert!(
            !calls.contains(&attr),
            "attribute token `{attr}` inside a macro must not become a calls edge, got: {calls:?}"
        );
    }
    assert!(
        calls.contains(&"real"),
        "the genuine call in the attributed item must survive, got: {calls:?}"
    );
}

#[test]
fn test_project_fns_named_like_cfg_predicates_keep_their_edges() {
    // The structural rule must NOT cost what the name blacklist did. A project
    // defining `fn any` / `fn all` / `fn not` and calling them bare from macro
    // arguments keeps those edges — nothing here is inside a `#[...]` span.
    let src = r#"
pub fn any(x: bool) -> bool { x }
pub fn all(x: bool) -> bool { x }
pub fn not(x: bool) -> bool { !x }
pub fn caller() {
    assert!(any(true));
    assert_eq!(all(true), not(false));
}
"#;
    let rels = extract_relations(src, "rust").unwrap();
    let calls: Vec<&str> = rels
        .iter()
        .filter(|r| r.relation == REL_CALLS && r.source_name == "caller")
        .map(|r| r.target_name.as_str())
        .collect();
    for name in ["any", "all", "not"] {
        assert!(
            calls.contains(&name),
            "a project fn named `{name}` called from macro args must keep its edge, got: {calls:?}"
        );
    }
}

#[test]
fn test_index_expression_in_macro_args_is_not_treated_as_an_attribute() {
    // The bracket walk keys on `[` — an INDEX expression `a[f(x)]` opens one too.
    // Only a `#`-preceded bracket is an attribute; without that check this would
    // silently drop a real call.
    let src = r#"
pub fn caller(a: &[u8]) {
    assert_eq!(a[idx(1)], 0);
}
"#;
    let rels = extract_relations(src, "rust").unwrap();
    let calls: Vec<&str> = rels
        .iter()
        .filter(|r| r.relation == REL_CALLS)
        .map(|r| r.target_name.as_str())
        .collect();
    assert!(
        calls.contains(&"idx"),
        "a call inside an index expression is not an attribute argument, got: {calls:?}"
    );
}

#[test]
fn test_rust_local_closure_call_is_not_a_global_edge() {
    // Rust's VALUE namespace lets a `let` shadow an item, so `let cb = …; cb()`
    // unambiguously invokes the local closure — never a same-named global fn.
    // Both call channels reached this shape with no local-binding exclusion:
    // the ordinary `call_expression` arm and the macro token_tree pass. The
    // repo's own `refine_ambiguous_targets` (indexer/pipeline/resolve.rs) has a
    // deliberately-divergent local `is_test_path` closure, and the production
    // index recorded it as a caller of `domain::is_test_path` — a fn it never
    // calls.
    let cases = [
        // Ordinary call_expression channel.
        (
            "fn run(p: &str) { let is_test_path = |s: &str| s.contains(\"t\"); if is_test_path(p) { work(); } }",
            "is_test_path",
        ),
        // Macro token_tree channel (parent is `token_tree`, so the
        // value-reference pass never fires and its M2.5 exclusion never applied).
        (
            "fn run() { let cb = |v: i32| v + 1; assert_eq!(cb(1), 2); }",
            "cb",
        ),
        // Closure parameter, not a `let`.
        (
            "fn run() { helper(|fmt: fn(i32) -> i32| { let _ = fmt(1); }); }",
            "fmt",
        ),
        // `if let` / `match` / `for` bindings feed the same collector.
        (
            "fn run(o: Option<fn()>) { if let Some(handler) = o { handler(); } }",
            "handler",
        ),
    ];
    for (src, local) in cases {
        let rels = extract_relations(src, "rust").unwrap();
        let calls: Vec<&str> = rels
            .iter()
            .filter(|r| r.relation == REL_CALLS)
            .map(|r| r.target_name.as_str())
            .collect();
        assert!(
            !calls.contains(&local),
            "calling local binding `{local}` must not emit a global calls edge, got: {calls:?}"
        );
    }
}

#[test]
fn test_rust_local_exclusion_respects_scope_and_position() {
    // The exclusion's name set is a whole-body over-approximation: every binder
    // anywhere in the function, with no scope and no ordering. That is
    // precision-safe on the `references` axis and NOT on the calls axis, where
    // it silently drops real edges — and a missing calls edge is the dangerous
    // direction, because dead-code reads exactly that edge and a live function
    // becomes a deletion candidate.
    //
    // Each case binds `helper` somewhere that CANNOT shadow the call, so the
    // call is a genuine call of the global `helper` and must still emit.
    let cases: &[(&str, &str)] = &[
        (
            "binder comes after the call",
            "fn helper() -> i32 { 1 }\nfn run() -> i32 { let a = helper(); let helper = 9; a + helper }",
        ),
        (
            "binder in a sibling block",
            "fn helper() -> i32 { 1 }\nfn run() -> i32 { { let helper = 2; let _ = helper; } helper() }",
        ),
        (
            "for-loop binder, call after the loop",
            "fn helper() -> i32 { 1 }\nfn run(v: Vec<i32>) -> i32 { for helper in v { let _ = helper; } helper() }",
        ),
        (
            "match-arm binder, call after the match",
            "fn helper() -> i32 { 1 }\nfn run(x: Result<i32, i32>) -> i32 { match x { Ok(helper) => { let _ = helper; } Err(_) => {} } helper() }",
        ),
        (
            "if-let binder, call after",
            "fn helper() -> i32 { 1 }\nfn run(o: Option<i32>) -> i32 { if let Some(helper) = o { let _ = helper; } helper() }",
        ),
        (
            "closure param, call outside the closure",
            "fn helper() -> i32 { 1 }\nfn run() -> i32 { let f = |helper: i32| helper + 1; let _ = f(1); helper() }",
        ),
    ];

    let mut lost = Vec::new();
    for (label, src) in cases {
        let rels = extract_relations(src, "rust").unwrap();
        let calls: Vec<&str> = rels
            .iter()
            .filter(|r| r.relation == REL_CALLS)
            .map(|r| r.target_name.as_str())
            .collect();
        if !calls.contains(&"helper") {
            lost.push(*label);
        }
    }
    assert!(
        lost.is_empty(),
        "these shapes bind `helper` where it cannot shadow the call, so the call is genuine \
         and must emit — the exclusion is over-firing on them: {lost:?}"
    );

    // The other direction, unchanged: a binder that GENUINELY shadows still
    // suppresses. Without this the fix above could be "never exclude anything".
    let shadowed = [
        "fn helper() -> i32 { 1 }\nfn run() -> i32 { let helper = || 2; helper() }",
        "fn helper() -> i32 { 1 }\nfn run(v: Vec<fn() -> i32>) -> i32 { for helper in v { let _ = helper(); } 0 }",
        "fn helper() -> i32 { 1 }\nfn run(o: Option<fn() -> i32>) -> i32 { if let Some(helper) = o { helper() } else { 0 } }",
        "fn helper() -> i32 { 1 }\nfn run(f: fn() -> i32) -> i32 { let g = |helper: fn() -> i32| helper(); g(f) }",
    ];
    for src in shadowed {
        let rels = extract_relations(src, "rust").unwrap();
        let calls: Vec<&str> = rels
            .iter()
            .filter(|r| r.relation == REL_CALLS)
            .map(|r| r.target_name.as_str())
            .collect();
        assert!(
            !calls.contains(&"helper"),
            "a binder that really does shadow the call must still suppress it, got: {calls:?}\nsrc: {src}"
        );
    }
}

#[test]
fn test_rust_local_exclusion_keeps_genuine_calls() {
    // Negative control for the exclusion above: suppressing everything would
    // pass the previous test. A bare call whose name is NOT a local binding
    // still emits, including when a same-named local exists for a DIFFERENT
    // name in the same body.
    let src = r#"
fn run(p: &str) {
    let is_test_path = |s: &str| s.contains("t");
    if is_test_path(p) { work(); }
    helper(1);
}
"#;
    let rels = extract_relations(src, "rust").unwrap();
    let calls: Vec<&str> = rels
        .iter()
        .filter(|r| r.relation == REL_CALLS)
        .map(|r| r.target_name.as_str())
        .collect();
    for kept in ["work", "helper"] {
        assert!(
            calls.contains(&kept),
            "genuine call `{kept}` must survive local-binding exclusion, got: {calls:?}"
        );
    }
}

#[test]
fn test_rust_local_exclusion_spares_camelcase_constructors() {
    // The local collector deliberately OVER-collects from pattern fields: a
    // `match` arm `Ok(v)` contributes both `Ok` and `v`. That is precision-safe
    // for value references but would cost real instantiation edges on the calls
    // axis, where a tuple-variant constructor call IS the edge dead-code
    // detection relies on. Variant/type names are CamelCase by convention (the
    // same rule the macro pass already uses), so the exclusion skips them.
    let src = r#"
fn run(r: Result<i32, i32>) -> MyVariant {
    match r {
        Ok(v) => { let _ = v; }
        Err(e) => { let _ = e; }
    }
    MyVariant(3)
}
"#;
    let rels = extract_relations(src, "rust").unwrap();
    let calls: Vec<&str> = rels
        .iter()
        .filter(|r| r.relation == REL_CALLS)
        .map(|r| r.target_name.as_str())
        .collect();
    assert!(
        calls.contains(&"MyVariant"),
        "CamelCase constructor call must survive over-collected pattern names, got: {calls:?}"
    );
}

#[test]
fn test_method_call_survives_same_named_local() {
    // `CalleeQualifier::Bare` is not the same predicate as "the callee is a bare
    // identifier": it is also `extract_rust_field`'s FALLBACK arm for a method
    // call whose receiver is neither `self`, a plain identifier, nor a call. A
    // nested-field receiver (`ctx.db.conn()`) lands there. Gating the
    // local-binding exclusion on the enum instead of on the node shape dropped
    // 14 real `Database::conn` edges in this repo — every `cmd_*` that opens
    // with `let conn = ctx.db.conn();`, because the METHOD name equals the local
    // it is bound to. The method call must survive; only the bare call goes.
    let src = r#"
fn cmd_show(ctx: &Ctx) {
    let conn = ctx.db.conn();
    let helper = |x: i32| x + 1;
    let _ = helper(1);
    render(conn);
}
"#;
    let rels = extract_relations(src, "rust").unwrap();
    let calls: Vec<&str> = rels
        .iter()
        .filter(|r| r.relation == REL_CALLS)
        .map(|r| r.target_name.as_str())
        .collect();
    assert!(
        calls.contains(&"conn"),
        "a method call must not be suppressed by a same-named local, got: {calls:?}"
    );
    assert!(
        !calls.contains(&"helper"),
        "the bare local-closure call must still be suppressed, got: {calls:?}"
    );
    assert!(
        calls.contains(&"render"),
        "unrelated bare call must survive, got: {calls:?}"
    );
}

#[test]
fn test_match_guard_callee_is_not_a_local_binding() {
    // tree-sitter wraps a match arm's pattern AND its optional `if` guard in one
    // `match_pattern` node under the `pattern` field. The local-name collector
    // over-collects from pattern fields deliberately (a variant name costs
    // nothing), but a GUARD is an ordinary expression — sweeping it in makes
    // every function it calls look like a local. Real instance: `cmd_grep`'s
    // `Ok(c) if …!is_cwd_anchored(path) =>` suppressed the genuine call edge to
    // `fn is_cwd_anchored` (cli.rs:580).
    let src = r#"
fn cmd_grep(path: &str, r: Result<i32, i32>) {
    let resolved = match r {
        Ok(c) if is_cwd_anchored(path) => c,
        _ => 0,
    };
    let _ = resolved;
}
"#;
    let rels = extract_relations(src, "rust").unwrap();
    let calls: Vec<&str> = rels
        .iter()
        .filter(|r| r.relation == REL_CALLS)
        .map(|r| r.target_name.as_str())
        .collect();
    assert!(
        calls.contains(&"is_cwd_anchored"),
        "a fn called in a match guard is not a local binding, got: {calls:?}"
    );
    // Negative control: the arm's actual BINDER is still collected, so a bare
    // call to a name bound by the pattern is still suppressed.
    let shadowed = r#"
fn run(r: Result<fn(), i32>) {
    match r {
        Ok(handler) => { handler(); }
        Err(_) => {}
    }
}
"#;
    let rels2 = extract_relations(shadowed, "rust").unwrap();
    let calls2: Vec<&str> = rels2
        .iter()
        .filter(|r| r.relation == REL_CALLS)
        .map(|r| r.target_name.as_str())
        .collect();
    assert!(
        !calls2.contains(&"handler"),
        "a name bound by the arm pattern must still be excluded, got: {calls2:?}"
    );
}

// ---------------------------------------------------------------------------
// P1-3 heritage axis: the (language, declaration-kind) parity table.
//
// The heritage arm matched exactly three node kinds —
// `class_declaration | class_definition | class` — so every grammar that spells
// a heritage-carrying declaration differently emitted ZERO inheritance edges,
// silently. That is the "unguarded axis" shape: nothing failed, the graph was
// simply incomplete, and `find_dead_code` then reported implementers of an
// interface as unused because the edge that proves otherwise was never built.
//
// A table is the point: rows are (language, declaration kind), and a new row is
// one line. Written RED-first against the real grammars — every node kind and
// heritage-child kind below came from parsing these exact snippets, not from
// reading a grammar README.
// ---------------------------------------------------------------------------

/// `(language, source, expected (source_name, target_name, relation) edges)`.
#[allow(clippy::type_complexity)]
fn heritage_cases() -> Vec<(
    &'static str,
    &'static str,
    Vec<(&'static str, &'static str, &'static str)>,
)> {
    vec![
        // Java: interface extends (inherits), enum/record implements.
        (
            "java",
            "interface Shape extends Drawable, Sized { }",
            vec![
                ("Shape", "Drawable", REL_INHERITS),
                ("Shape", "Sized", REL_INHERITS),
            ],
        ),
        (
            "java",
            "enum Suit implements Comparable { HEARTS }",
            vec![("Suit", "Comparable", REL_IMPLEMENTS)],
        ),
        (
            "java",
            "record Point(int x, int y) implements Serializable { }",
            vec![("Point", "Serializable", REL_IMPLEMENTS)],
        ),
        // TypeScript: interface extends interface.
        (
            "typescript",
            "interface Admin extends User, Auditable { }",
            vec![
                ("Admin", "User", REL_INHERITS),
                ("Admin", "Auditable", REL_INHERITS),
            ],
        ),
        // C# already had a `base_list` arm keyed on the node kind rather than on
        // the declaration, so every C# declaration form was covered. Kept in the
        // table as the WORKING row: it pins the `interface_by_prefix` split
        // (`IRepo` → implements, `BaseRepo` → inherits), which is a convention,
        // not a grammar fact, and would otherwise be free to drift.
        (
            "csharp",
            "class Repo : BaseRepo, IRepo { }",
            vec![
                ("Repo", "BaseRepo", REL_INHERITS),
                ("Repo", "IRepo", REL_IMPLEMENTS),
            ],
        ),
        (
            "csharp",
            "interface IReadRepo : IRepo { }",
            vec![("IReadRepo", "IRepo", REL_IMPLEMENTS)],
        ),
        // PHP: interface extends.
        (
            "php",
            "<?php\ninterface Writer extends Stream { }",
            vec![("Writer", "Stream", REL_INHERITS)],
        ),
        // Kotlin: `object X : Base()`.
        (
            "kotlin",
            "object Registry : BaseRegistry { }",
            vec![("Registry", "BaseRegistry", REL_INHERITS)],
        ),
        // Swift: `protocol P: Q`.
        (
            "swift",
            "protocol Cache: Store { }",
            vec![("Cache", "Store", REL_INHERITS)],
        ),
        // Dart: `implements` on a class parses to an `interfaces` child that
        // neither heritage function read — so even the ALREADY-matched
        // `class_definition` produced nothing for this spelling.
        (
            "dart",
            "class FileStore implements Store { }",
            vec![("FileStore", "Store", REL_IMPLEMENTS)],
        ),
        (
            "dart",
            "enum Level implements Comparable { low }",
            vec![("Level", "Comparable", REL_IMPLEMENTS)],
        ),
    ]
}

#[test]
fn heritage_parity_across_declaration_kinds() {
    let mut missing = Vec::new();
    for (lang, src, expected) in heritage_cases() {
        let rels = match extract_relations(src, lang) {
            Ok(r) => r,
            Err(e) => {
                missing.push(format!("{lang}: parse failed: {e}"));
                continue;
            }
        };
        for (from, to, rel) in expected {
            let found = rels
                .iter()
                .any(|r| r.source_name == from && r.target_name == to && r.relation == rel);
            if !found {
                let got: Vec<String> = rels
                    .iter()
                    .filter(|r| r.relation == REL_INHERITS || r.relation == REL_IMPLEMENTS)
                    .map(|r| format!("{} -{}-> {}", r.source_name, r.relation, r.target_name))
                    .collect();
                missing.push(format!(
                    "{lang}: {from} -{rel}-> {to}  (got: {got:?})  src: {src:?}"
                ));
            }
        }
    }
    assert!(
        missing.is_empty(),
        "heritage edges missing for {} case(s):\n  {}",
        missing.len(),
        missing.join("\n  ")
    );
}

/// Negative control for the table above: a heritage-carrying declaration KIND
/// with no heritage clause must stay silent. Without this, "widen the match" is
/// indistinguishable from "emit an edge for every declaration".
#[test]
fn heritage_declarations_without_a_clause_emit_nothing() {
    for (lang, src) in [
        ("java", "interface Bare { }"),
        ("java", "enum Bare { A }"),
        ("typescript", "interface Bare { }"),
        ("csharp", "class Bare { }"),
        ("csharp", "interface IBare { }"),
        ("php", "<?php\ninterface Bare { }"),
        ("kotlin", "object Bare { }"),
        ("swift", "protocol Bare { }"),
        ("dart", "class Bare { }"),
        ("dart", "enum Bare { a }"),
    ] {
        let rels = extract_relations(src, lang).unwrap();
        let heritage: Vec<String> = rels
            .iter()
            .filter(|r| r.relation == REL_INHERITS || r.relation == REL_IMPLEMENTS)
            .map(|r| format!("{} -{}-> {}", r.source_name, r.relation, r.target_name))
            .collect();
        assert!(
            heritage.is_empty(),
            "{lang} {src:?} has no heritage clause but emitted {heritage:?}"
        );
    }
}

/// A C# `enum` names its UNDERLYING INTEGRAL TYPE with the same `base_list`
/// syntax a class uses for its base type. Reading that as inheritance would
/// invent `Level inherits byte` — a phantom edge bound to a real node, which
/// this repo has already learned is worse than a missing one.
#[test]
fn csharp_enum_underlying_type_is_not_inheritance() {
    let rels = extract_relations("enum Level : byte { Low }", "csharp").unwrap();
    let heritage: Vec<String> = rels
        .iter()
        .filter(|r| r.relation == REL_INHERITS || r.relation == REL_IMPLEMENTS)
        .map(|r| format!("{} -{}-> {}", r.source_name, r.relation, r.target_name))
        .collect();
    assert!(
        heritage.is_empty(),
        "a C# enum's underlying type is not a parent; got {heritage:?}"
    );
}
