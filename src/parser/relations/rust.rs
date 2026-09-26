//! Rust-specific extraction: `use` declarations (simple/grouped/nested/aliased)
//! and `impl Trait for Type` blocks (emits both type-level and method-level
//! IMPLEMENTS edges so the dead-code pass sees incoming edges on trait methods).
//! Also extracts REFERENCES edges for edgeless usages: path-qualified value
//! paths (`crate::domain::FOO`) and type-position usages (`field: MyType`,
//! `-> MyType`, `Vec<MyType>`).

use super::super::node_text;
use super::helpers::MAX_SUBTREE_DEPTH;
use super::ParsedRelation;
use crate::domain::{REL_CALLS, REL_IMPLEMENTS, REL_IMPORTS, REL_REFERENCES};

/// Leftmost path segment of a `use` argument: `std::fs` → "std",
/// `std::{fs, io}` → "std", `crate::x::Y` → "crate", `fs` → "fs".
/// Descends `path` fields (and through `use_as_clause`) to the root token.
fn use_path_root<'a>(node: &tree_sitter::Node, source: &'a str) -> &'a str {
    let mut cur = *node;
    for _ in 0..MAX_SUBTREE_DEPTH {
        if cur.kind() == "use_as_clause" {
            match cur.named_child(0) {
                Some(c) => {
                    cur = c;
                    continue;
                }
                None => break,
            }
        }
        match cur.child_by_field_name("path") {
            Some(p) => cur = p,
            None => break,
        }
    }
    node_text(&cur, source)
}

/// Extract import names from Rust `use` declarations by walking the tree-sitter AST.
/// Handles simple (`use foo::Bar`), grouped (`use foo::{Bar, Baz}`),
/// nested (`use foo::{bar::{A, B}}`), aliased (`use foo::Bar as B`), and glob imports.
///
/// Statically-known EXTERNAL roots (`std`/`core`/`alloc`/`proc_macro`) bind to
/// the `<external>` sentinel instead of the global bare-name pool (IDX v53;
/// v52 dropped them entirely). Their bare trailing segment used to enter the
/// global lookup with no qualifier metadata, where it bound to whatever single
/// same-family project symbol shared the name — every `use std::fs;` in the repo
/// produced a phantom `imports → fn fs` edge onto a `#[cfg(test)]` helper in
/// `js_modules.rs`, polluting 4 module_dependencies pairs in `map`.
///
/// v52 fixed that by emitting nothing, which is correct but leaves value on the
/// table: an explicit `<external>` binding *also* lets the existing
/// `prune_import_contradicted_call_edges` kill the sibling CALLS phantom
/// (`use std::mem::swap; swap(&mut a, &mut b)` → `calls → some_project::swap`).
/// See [`crate::domain::IMPORT_EXTERNAL_META`].
///
/// Non-std external crates (`anyhow`, …) can't be told apart from
/// workspace-sibling crates without Cargo.toml knowledge, so they still take the
/// bare-name path.
pub(super) fn extract_rust_use_imports(
    node: &tree_sitter::Node,
    source: &str,
    scope: Option<&str>,
    results: &mut Vec<ParsedRelation>,
) {
    const EXTERNAL_ROOTS: &[&str] = &["std", "core", "alloc", "proc_macro"];
    fn collect_use_names(node: &tree_sitter::Node, source: &str, names: &mut Vec<String>) {
        collect_use_names_inner(node, source, names, 0);
    }
    fn collect_use_names_inner(
        node: &tree_sitter::Node,
        source: &str,
        names: &mut Vec<String>,
        depth: usize,
    ) {
        if depth > MAX_SUBTREE_DEPTH {
            return;
        }
        match node.kind() {
            "use_as_clause" => {
                if let Some(child) = node.named_child(0) {
                    collect_use_names_inner(&child, source, names, depth + 1);
                }
            }
            "use_wildcard" => {}
            "use_list" => {
                for i in 0..node.named_child_count() {
                    if let Some(child) = node.named_child(i) {
                        collect_use_names_inner(&child, source, names, depth + 1);
                    }
                }
            }
            "scoped_use_list" => {
                for i in 0..node.named_child_count() {
                    if let Some(child) = node.named_child(i) {
                        if child.kind() != "scoped_identifier" && child.kind() != "identifier" {
                            collect_use_names_inner(&child, source, names, depth + 1);
                        }
                    }
                }
            }
            "scoped_identifier" => {
                if let Some(name_node) = node.child_by_field_name("name") {
                    let name = node_text(&name_node, source);
                    if !name.is_empty() && name != "*" && name != "self" {
                        names.push(name.to_string());
                    }
                }
            }
            "identifier" | "type_identifier" => {
                let name = node_text(node, source);
                if !name.is_empty() && name != "self" {
                    names.push(name.to_string());
                }
            }
            _ => {
                for i in 0..node.named_child_count() {
                    if let Some(child) = node.named_child(i) {
                        collect_use_names_inner(&child, source, names, depth + 1);
                    }
                }
            }
        }
    }

    // The use_declaration's named children are the argument (scoped_identifier,
    // use_list, …) plus an optional visibility_modifier.
    //
    // A ROOT-LEVEL use-list (`use {std::io::Read, crate::a::cb}`) is one child
    // with no `path` field, so `use_path_root` returned the whole braced text and
    // the external check never matched any member. Flatten one level so every
    // member is classified on its OWN root — and so a mixed list gets a mixed
    // verdict instead of one all-or-nothing decision for the declaration.
    let mut members: Vec<tree_sitter::Node> = Vec::new();
    for i in 0..node.named_child_count() {
        if let Some(child) = node.named_child(i) {
            if child.kind() == "use_list" {
                for j in 0..child.named_child_count() {
                    if let Some(m) = child.named_child(j) {
                        members.push(m);
                    }
                }
            } else {
                members.push(child);
            }
        }
    }

    let scope_name = scope.unwrap_or("<module>");
    for member in members {
        // A leading `::` (`use ::std::mem::swap`) is part of the root token's
        // text but not of the crate name, so the raw comparison missed it and the
        // path fell straight back into the phantom-producing bare-name pool.
        let root = use_path_root(&member, source).trim_start_matches("::");
        let external = EXTERNAL_ROOTS.contains(&root);
        let mut names = Vec::new();
        collect_use_names(&member, source, &mut names);
        for name in names {
            results.push(ParsedRelation {
                source_name: scope_name.to_string(),
                target_name: name,
                relation: REL_IMPORTS.into(),
                metadata: external.then(|| crate::domain::IMPORT_EXTERNAL_META.to_string()),
                source_language: String::new(),
                source_line: None,
            });
        }
    }
}

/// Extract `impl Trait for Type` → Type implements Trait
pub(super) fn extract_rust_impl_trait(
    node: &tree_sitter::Node,
    source: &str,
) -> Option<ParsedRelation> {
    // impl_item has "trait" and "type" fields when it's `impl Trait for Type`
    let trait_node = node.child_by_field_name("trait")?;
    let type_node = node.child_by_field_name("type")?;
    let trait_name = node_text(&trait_node, source).to_string();
    let type_text = node_text(&type_node, source).to_string();
    // Strip generics so source resolution can match the bare struct name.
    // The `type` field on a generic impl block returns the full `Type<'a, W>`
    // text; Phase 2 source resolution (index_files.rs) does exact-name match
    // against local node names ("Type"), so without stripping, no edge would
    // emit for any generic trait impl — every method appears dead.
    let type_name = type_text
        .split('<')
        .next()
        .unwrap_or(&type_text)
        .trim()
        .to_string();
    if trait_name.is_empty() || type_name.is_empty() {
        return None;
    }
    Some(ParsedRelation {
        source_name: type_name,
        target_name: trait_name,
        relation: REL_IMPLEMENTS.into(),
        metadata: None,
        source_language: String::new(),
        source_line: None,
    })
}

/// Emit a `references` edge for a path-qualified usage (`a::b::FOO`) that is
/// neither a call (its parent is a `call_expression` `function` field) nor part
/// of a `use` declaration, and is the outermost `scoped_identifier`.
///
/// Outermost-only is enforced by rejecting a `scoped_identifier` whose parent is
/// itself a `scoped_identifier` (intermediate path segments). Type-position
/// paths (`scoped_type_identifier`, e.g. the inner segments of a struct-expr
/// path `crate::parser::NodeRecord { .. }`) are excluded too — that usage is
/// already covered by the `calls` edge to the struct/type name, so re-emitting a
/// `references` edge to an intermediate path segment ("parser") would be a false
/// positive. `self`/`Self`/glob `*`/inferred-placeholder `_` names are skipped.
pub(super) fn extract_rust_path_reference(
    node: &tree_sitter::Node,
    source: &str,
    scope: Option<&str>,
) -> Option<ParsedRelation> {
    let parent = node.parent()?;
    match parent.kind() {
        // Callee of a call (`crate::foo::bar()`) — already a `calls` edge.
        // A `call_expression` parent where this node is NOT the `function`
        // field (e.g. a path passed as an argument) falls through to `_`.
        "call_expression"
            if parent.child_by_field_name("function").map(|f| f.id()) == Some(node.id()) =>
        {
            return None;
        }
        "use_declaration" | "scoped_use_list" | "use_list" | "use_as_clause" => return None,
        // Macro path (`tracing::error!`, `serde_json::json!`): the `macro` field of
        // a `macro_invocation`. The macro name tail is not a value reference — it
        // collides with same-named fns (`error`, `warn`).
        "macro_invocation" => return None,
        // Intermediate path segment of a longer `a::b::c` chain.
        "scoped_identifier" => return None,
        // Type-position path (struct-expr type, generic bounds, etc.). The
        // type name is already a `calls` edge; intermediate segments here are
        // module path, not a value reference.
        "scoped_type_identifier" => return None,
        _ => {}
    }
    // Type-associated path used as a value (`String::as_str`, `MyType::method` passed
    // as a fn pointer): the bare tail cannot resolve to the correct associated item
    // (std methods aren't indexed; a same-named local fn is the wrong target), so
    // suppress. Heuristic: a PascalCase head segment is a type; lowercase heads
    // (`crate`, `self`, `super`, snake_case module names like `domain`) are genuine
    // module-path values and still emit. Lowercase PRIMITIVE-type heads (`str::trim`,
    // `u32::MAX`) are also type-associated and are caught by the primitive list below.
    let path_text = node_text(node, source);
    if path_text
        .chars()
        .next()
        .map(|c| c.is_uppercase())
        .unwrap_or(false)
    {
        return None;
    }
    let head = path_text.split("::").next().unwrap_or(path_text);
    if matches!(
        head,
        "str"
            | "bool"
            | "char"
            | "u8"
            | "u16"
            | "u32"
            | "u64"
            | "u128"
            | "usize"
            | "i8"
            | "i16"
            | "i32"
            | "i64"
            | "i128"
            | "isize"
            | "f32"
            | "f64"
    ) {
        return None;
    }
    let name_node = node.child_by_field_name("name")?;
    let name = node_text(&name_node, source);
    // `_` added defensively (inferred-type placeholder) alongside self/Self/glob.
    if name.is_empty() || name == "self" || name == "Self" || name == "*" || name == "_" {
        return None;
    }
    Some(ParsedRelation {
        source_name: scope.unwrap_or("<module>").to_string(),
        target_name: name.to_string(),
        relation: REL_REFERENCES.into(),
        metadata: None,
        source_language: String::new(),
        source_line: None,
    })
}

/// Emit a `references` edge for a `type_identifier` used in type position
/// (field type, return type, generic arg). Skips these non-usage / already-edged
/// cases so the references edge stays a pure "edgeless usage" signal:
/// - the type's own definition name (the `name` field of
///   struct/enum/type/trait/union items) — declaration, not a usage;
/// - the `name` of a bare `struct_expression` (`Foo { .. }`) — already a `calls`
///   edge, avoids a double edge;
/// - the inner `type_identifier` of a `scoped_type_identifier` that is a
///   `struct_expression` name (`mod::Foo { .. }`) — same `calls` double-edge;
/// - the `type` / `trait` field of an `impl_item` (`impl Foo` /
///   `impl Trait for Foo`) — already an IMPLEMENTS edge, and a references edge
///   there would defeat dead-code detection (direct-parent `impl_item` only;
///   generic / path-qualified impl headers are a documented residual);
/// - `Self`;
/// - `_`, the inferred-type placeholder (`Vec<_>`, `collect::<Vec<_>>()`), which
///   tree-sitter parses as a `type_identifier` but is not a real type usage.
pub(super) fn extract_rust_type_reference(
    node: &tree_sitter::Node,
    source: &str,
    scope: Option<&str>,
) -> Option<ParsedRelation> {
    if let Some(parent) = node.parent() {
        // The `name` field of a definition is the declaration, not a usage.
        if matches!(
            parent.kind(),
            "struct_item" | "enum_item" | "type_item" | "trait_item" | "union_item"
        ) && parent.child_by_field_name("name").map(|n| n.id()) == Some(node.id())
        {
            return None;
        }
        // Impl-header type/trait names (`impl Widget` / `impl Trait for Widget`).
        // The impl relationship is already an IMPLEMENTS edge (see
        // extract_rust_impl_trait); a references edge here is redundant noise
        // AND defeats dead-code detection — every impl'd type would get an
        // incoming reference and could never be flagged dead. Only the
        // direct-parent `impl_item` case is covered: generic (`impl Container<T>`,
        // type field = generic_type) and path-qualified (`impl mod::Trait for
        // mod::Type`, fields = scoped_type_identifier) impl headers put the
        // type_identifier one level deeper and are a documented residual.
        if parent.kind() == "impl_item"
            && (parent.child_by_field_name("type").map(|n| n.id()) == Some(node.id())
                || parent.child_by_field_name("trait").map(|n| n.id()) == Some(node.id()))
        {
            return None;
        }
        // The `name` of `Foo { .. }` already yields a `calls` edge — don't double-emit.
        if parent.kind() == "struct_expression"
            && parent.child_by_field_name("name").map(|n| n.id()) == Some(node.id())
        {
            return None;
        }
        // Path-qualified struct expr `mod::Foo { .. }`: the `name` field is a
        // `scoped_type_identifier` whose inner `name` is this `type_identifier`
        // ("Foo"). The `struct_expression` arm already strips the path and emits
        // a `calls` edge to "Foo", so this inner type_identifier must not also
        // emit a `references` edge (would be a double edge — same target).
        if parent.kind() == "scoped_type_identifier"
            && parent.child_by_field_name("name").map(|n| n.id()) == Some(node.id())
        {
            if let Some(grandparent) = parent.parent() {
                if grandparent.kind() == "struct_expression"
                    && grandparent.child_by_field_name("name").map(|n| n.id()) == Some(parent.id())
                {
                    return None;
                }
            }
        }
    }
    let name = node_text(node, source);
    // `_` is the inferred-type placeholder (`Vec<_>`, `collect::<Vec<_>>()`),
    // which tree-sitter parses as a `type_identifier`. It is not a real type
    // usage; emitting a `references` edge to "_" piles every occurrence onto
    // whatever node happens to be named "_" (e.g. the `const _: () = assert!(..)`
    // guard), inflating its caller/impact counts with phantom edges.
    if name.is_empty() || name == "Self" || name == "_" {
        return None;
    }
    Some(ParsedRelation {
        source_name: scope.unwrap_or("<module>").to_string(),
        target_name: name.to_string(),
        relation: REL_REFERENCES.into(),
        metadata: None,
        source_language: String::new(),
        source_line: None,
    })
}

/// Emit a `references` edge for a BARE `identifier` used as a function VALUE —
/// a callback / function pointer passed in a call-argument position. Two shapes:
///   - direct argument:  `register(handler)` → identifier parent is `arguments`;
///   - address-of arg:   `signal(&shutdown)` → identifier under `reference_expression`
///     whose parent is `arguments`.
///
/// Self-exclusion is structural: a call's *callee* identifier has parent
/// `call_expression` (the `function` field), never `arguments`, so it never fires
/// here (and remains a `calls` edge). Path-qualified values (`crate::foo::bar`) are
/// `scoped_identifier`, handled by `extract_rust_path_reference`. `self/Self/_` skip.
///
/// M2 (param exclusion): a bare id equal to a parameter of the enclosing function
/// (or an enclosing closure) is a pass-through of a LOCAL binding, not a reference to
/// a same-named global fn — skip it. Without M2, `fn run(handler: F){ spawn(handler) }`
/// would fabricate an edge to whatever global `handler` happens to exist (FP-a).
/// Calls made inside macro token trees (`assert_eq!(foo(x), y)`, `macro_rules!`
/// rule bodies): tree-sitter parses macro arguments/bodies as opaque
/// `token_tree`s — no `call_expression` exists — so every such call was
/// invisible: the target showed as dead code and impact/callgraph missed the
/// calling fn (field failure 2026-07-24: `impact grep_exit` missed cmd_stats'
/// `sout!` body; tests calling a fn only inside `assert_eq!` were absent).
/// Heuristic: an `identifier` token directly followed by a `(…)` token_tree is
/// a call. Exclusions:
///   - previous token `.` → method tail; the receiver is unrecoverable in a
///     token soup, and a bare-name edge would alias every same-named fn
///   - previous token `::` → path tail (v1 skips: std/type-associated paths
///     dominate and the bare tail aliases same-named local fns)
///   - previous token `$` → macro fragment variable, not a name
///   - previous token is a definition keyword → macro-generated item, not a call
///   - no enclosing named scope → skip, parity with the call_expression arm's
///     deliberate Rust top-level omission
///
/// A macro name itself never matches: `foo!(…)` puts a `!` between the
/// identifier and the token_tree.
pub(super) fn extract_rust_macro_token_call(
    node: &tree_sitter::Node,
    source: &str,
    scope: Option<&str>,
) -> Option<ParsedRelation> {
    let scope = scope?;
    let parent = node.parent()?;
    if parent.kind() != "token_tree" {
        return None;
    }
    let next = node.next_sibling()?;
    if next.kind() != "token_tree" || source.as_bytes().get(next.start_byte()) != Some(&b'(') {
        return None;
    }
    if let Some(prev) = node.prev_sibling() {
        if matches!(
            prev.kind(),
            "." | "::"
                | "$"
                | "fn"
                | "struct"
                | "enum"
                | "union"
                | "trait"
                | "mod"
                | "type"
                | "impl"
        ) {
            return None;
        }
    }
    // Configuration predicates are not code. `#[cfg(not(windows))]` and
    // `cfg!(any(unix))` put `not(…)` / `any(…)` in a token_tree byte-identical to
    // a call, and every predicate name is lowercase — so the CamelCase guard
    // below waves them straight through. The production index carried `any` ×3
    // and `not` ×4 in `pending_unresolved_calls`; in a project that defines
    // `fn any` or `fn not` (predicate / iterator utility modules do) they promote
    // to real edges pointing at the wrong symbol.
    if in_cfg_predicate(node, source) {
        return None;
    }
    let name = node_text(node, source);
    // ...and structurally for attributes spelled as RAW TOKENS, which the
    // ancestor walk cannot see: `#[cfg(...)]` written inside another macro's
    // token_tree produces no `attribute` node anywhere in the tree — just the
    // token run `#` `[` `cfg` `(…)` `]` as siblings of the macro body.
    //
    //   cfg_if! { if #[cfg(not(windows))] { a(); } else { b(); } }
    //   quote!   { #[cfg(all(feature = "x"))] fn z() {} }
    //   wrap!    { #[allow(unused)] #[doc(hidden)] fn inner() { real(); } }
    //
    // `cfg_if!` is THE idiomatic home of conditional compilation inside a macro
    // (libc, rand, ring). A name blacklist was tried first and was the wrong
    // shape twice over: it missed every non-cfg attribute (`allow` / `deny` /
    // `doc` all still produced call edges), and it cost real edges for any
    // project that happens to define `fn any` / `fn all` / `fn not`. Matching
    // the bracket span instead is exact — it loses nothing and needs no list.
    if in_raw_attribute_tokens(node) {
        return None;
    }
    if name.is_empty() || name == "self" || name == "Self" || name == "_" {
        return None;
    }
    // Tuple-variant/tuple-struct PATTERNS (`matches!(x, Some(y))`) parse
    // identically to calls in a token soup — token_tree carries no
    // pattern-vs-expression split (audit 2026-07-24, empirically reproduced:
    // `matches!(x, Some(y))` fabricated a calls→Some edge).
    //
    // First line of defence is convention, because it is one comparison: variant
    // and type names are CamelCase (non_camel_case_types lint), the fn calls this
    // pass exists to recover are snake_case — so skip uppercase-initial names.
    //
    // The cost is two-sided, and only the first half is obvious:
    //   1. Constructor USES like `vec![Some(1)]` stay invisible, exactly as they
    //      were before this pass existed.
    //   2. Because they are invisible, a type CONSTRUCTED ONLY inside macros has
    //      no inbound edge at all — `find_dead_code` then reports it as dead.
    //      That is a recall loss on the type, not just on the call: the same
    //      exclusion that protects `Some` from a fake edge denies a
    //      macro-only-constructed struct its real one. Widening the skip (e.g.
    //      to every uppercase name anywhere) makes that worse, not better.
    if name.chars().next().is_some_and(|c| c.is_uppercase()) {
        return None;
    }
    // The reverse gap the convention cannot see: a LOWERCASE tuple variant, which
    // `#[allow(non_camel_case_types)]` code (bindgen output, C-ABI enum mirrors)
    // is full of. For the matches!-family shape — which is where pattern-shaped
    // token soup overwhelmingly comes from — argument position settles it with no
    // convention at all. Runs second because it walks ancestors and the check
    // above is one comparison.
    if in_matches_pattern_position(node, source) {
        return None;
    }
    // `let cb = |v| v + 1; assert_eq!(cb(1), 2)` calls the LOCAL closure. The
    // value-reference pass cannot suppress it here — inside a token_tree that
    // pass never fires at all — so this channel applies the exclusion itself.
    if shadowed_by_enclosing_local(node, source, name) {
        return None;
    }
    Some(ParsedRelation {
        source_name: scope.to_string(),
        target_name: name.to_string(),
        relation: REL_CALLS.into(),
        metadata: None,
        source_language: String::new(),
        source_line: None,
    })
}

/// Macros whose second and later top-level arguments are PATTERNS, not
/// expressions. `matches!` is std; the `assert_matches` pair is the
/// `assert_matches` crate / unstable std spelling of the same shape.
const PATTERN_ARG_MACROS: &[&str] = &["matches", "assert_matches", "debug_assert_matches"];

/// True when `node` sits in PATTERN position of a `matches!`-family macro.
///
/// Case conventions cannot decide this: `#[allow(non_camel_case_types)]` code
/// (bindgen output, C-ABI enum mirrors) has lowercase tuple variants, so
/// `matches!(x, ok(v))` walks past the CamelCase guard in
/// [`extract_rust_macro_token_call`] and fabricates a `calls → ok` edge aimed at
/// whichever same-language `fn ok` the resolver picks. Argument position is
/// structural and needs no convention.
///
/// Two shapes reach here. `matches!(…)` written directly is a `macro_invocation`
/// with a `token_tree` argument list; written INSIDE another macro
/// (`assert!(matches!(…))`) it is raw tokens — `matches` `!` `(…)` as siblings —
/// with no `macro_invocation` node anywhere, so the argument list is recognized
/// by its two preceding tokens instead.
///
/// The `if` guard is the counterweight: `matches!(x, ok(v) if is_ready(v))` puts
/// a real expression after the pattern, and `is_ready` must keep its edge.
/// Collecting the guard along with the pattern is precisely the over-collection
/// that silently deleted true edges from the value-reference axis (memory
/// `feedback_edge_exclusion_verify_by_index_diff`), so the scan returns to
/// expression state at a top-level `if`.
fn in_matches_pattern_position(node: &tree_sitter::Node, source: &str) -> bool {
    let mut child = *node;
    while let Some(parent) = child.parent() {
        if parent.kind() == "token_tree" && is_pattern_arg_list(&parent, source) {
            // Innermost enclosing pattern-macro argument list decides: a nested
            // `matches!` inside a guard is judged on its OWN argument positions.
            return child_is_after_pattern_comma(&parent, &child);
        }
        child = parent;
    }
    false
}

/// Is this `token_tree` the argument list of a [`PATTERN_ARG_MACROS`] macro?
fn is_pattern_arg_list(tt: &tree_sitter::Node, source: &str) -> bool {
    if source.as_bytes().get(tt.start_byte()) != Some(&b'(') {
        return false;
    }
    // Shape 1: a real `macro_invocation` node (macro written at expression level).
    if let Some(parent) = tt.parent() {
        if parent.kind() == "macro_invocation" {
            return parent
                .child_by_field_name("macro")
                .map(|m| node_text(&m, source))
                .is_some_and(|name| PATTERN_ARG_MACROS.contains(&name));
        }
    }
    // Shape 2: raw tokens inside an outer macro's token_tree — `matches` `!` `(…)`.
    let Some(bang) = tt.prev_sibling() else {
        return false;
    };
    if bang.kind() != "!" {
        return false;
    }
    bang.prev_sibling()
        .filter(|n| n.kind() == "identifier")
        .map(|n| node_text(&n, source))
        .is_some_and(|name| PATTERN_ARG_MACROS.contains(&name))
}

/// Walk the argument list left to right and report whether `child` (a direct
/// child of `arg_list` — the ancestor of the identifier under test) lands in
/// pattern state. State starts as expression (the scrutinee), flips at the first
/// top-level `,`, and flips back at a top-level `if` (the guard).
fn child_is_after_pattern_comma(arg_list: &tree_sitter::Node, child: &tree_sitter::Node) -> bool {
    let mut in_pattern = false;
    let mut cursor = arg_list.walk();
    for c in arg_list.children(&mut cursor) {
        if c.id() == child.id() {
            return in_pattern;
        }
        match c.kind() {
            "," => in_pattern = true,
            "if" => in_pattern = false,
            _ => {}
        }
    }
    false
}

/// True when `node` lies inside an attribute written as RAW TOKENS — the
/// `#` `[` … `]` run that appears when an attribute is nested in a macro's
/// token_tree and therefore never becomes an `attribute` node.
///
/// tree-sitter groups each bracket run into its own `token_tree`, so this walks
/// UP through enclosing `token_tree`s looking for one that opens with `[` and is
/// immediately preceded by `#`. That reaches both the attribute path itself
/// (`cfg` in `#[cfg(..)]`, a direct child of the `[…]` group) and anything
/// nested in its arguments (`any` in `#[cfg(any(unix))]`, several groups down).
/// Requiring the `#` is what keeps an INDEX expression — `a[f(x)]`, also a
/// `[`-opened token_tree — from losing its call edge.
fn in_raw_attribute_tokens(node: &tree_sitter::Node) -> bool {
    let mut cur = *node;
    for _ in 0..MAX_SUBTREE_DEPTH {
        let Some(parent) = cur.parent() else {
            return false;
        };
        if parent.kind() != "token_tree" {
            return false;
        }
        // tree-sitter groups each bracket run into its own `token_tree`, so the
        // attribute is the node `[ … ]` whose immediately-preceding sibling is
        // `#`. Requiring the `#` is what keeps an INDEX expression — `a[f(x)]`,
        // also a `[`-opened token_tree — from losing its call edge.
        let opens_with_bracket = parent.child(0).is_some_and(|c| c.kind() == "[");
        if opens_with_bracket && parent.prev_sibling().is_some_and(|h| h.kind() == "#") {
            return true;
        }
        cur = parent;
    }
    false
}

/// True when `node` sits inside a PARSED attribute (`#[cfg(…)]`, `#[derive(…)]`,
/// `#![feature(…)]`) or inside a `cfg!(…)` invocation — configuration
/// predicates, where nothing is ever a function call.
///
/// The nearest enclosing `macro_invocation` decides: any other macro
/// (`assert!(f())`, `println!("{}", g())`) may legitimately contain calls, and
/// recovering those is precisely why the token-tree pass exists. Attributes
/// written as raw tokens inside a macro body never become `attribute` nodes at
/// all and are handled by [`in_raw_attribute_tokens`] instead.
fn in_cfg_predicate(node: &tree_sitter::Node, source: &str) -> bool {
    let mut cur = *node;
    for _ in 0..MAX_SUBTREE_DEPTH {
        let Some(parent) = cur.parent() else {
            return false;
        };
        match parent.kind() {
            "attribute" | "attribute_item" | "inner_attribute_item" => return true,
            "macro_invocation" => {
                // Compare the LAST path segment: the `macro` field is a
                // `scoped_identifier` for `core::cfg!(…)` / `std::cfg!(…)`, whose
                // full text is `core::cfg` and never equalled `cfg`, so those
                // spellings leaked `any` / `all` / `not` as call edges. Both are
                // ordinary in code that avoids relying on the prelude.
                return parent.child_by_field_name("macro").is_some_and(|m| {
                    node_text(&m, source)
                        .rsplit("::")
                        .next()
                        .is_some_and(|seg| seg.trim() == "cfg")
                });
            }
            _ => cur = parent,
        }
    }
    false
}

pub(super) fn extract_rust_value_reference(
    node: &tree_sitter::Node,
    source: &str,
    scope: Option<&str>,
) -> Option<ParsedRelation> {
    let parent = node.parent()?;
    let in_value_position = match parent.kind() {
        // Phase 1: call argument, or `&fn` argument.
        "arguments" => true,
        "reference_expression" => parent
            .parent()
            .map(|gp| gp.kind() == "arguments")
            .unwrap_or(false),
        // Phase 2: binding RHS (`let cb = handler`) — only the `value` field, never
        // the `pattern` (which is a local binding, handled by M2.5).
        "let_declaration" => parent.child_by_field_name("value").map(|v| v.id()) == Some(node.id()),
        // Phase 2: struct field value (`Config { cb: handler }`) — `value` field
        // only, never the `field` name.
        "field_initializer" => {
            parent.child_by_field_name("value").map(|v| v.id()) == Some(node.id())
        }
        // Phase 2: explicit `return handler;`.
        "return_expression" => true,
        // Phase 2: tail expression of a block (`fn f() -> F { handler }`) — the last
        // named child with no trailing `;` (a `;` would reparent to
        // expression_statement). M2.5 still excludes a tail that is a local.
        "block" => {
            let n = parent.named_child_count();
            n > 0 && parent.named_child(n - 1).map(|c| c.id()) == Some(node.id())
        }
        _ => false,
    };
    if !in_value_position {
        return None;
    }
    let name = node_text(node, source);
    if name.is_empty() || name == "self" || name == "Self" || name == "_" {
        return None;
    }
    if with_enclosing_fn_local_names(node, source, |names| names.contains(name)) {
        return None;
    }
    Some(ParsedRelation {
        source_name: scope.unwrap_or("<module>").to_string(),
        target_name: name.to_string(),
        relation: REL_REFERENCES.into(),
        metadata: None,
        source_language: String::new(),
        source_line: None,
    })
}

/// True when a BARE callee name (`cb()`) is a local binding of the enclosing
/// function — a closure or fn-pointer call, not a call of the same-named global
/// fn. Rust's VALUE namespace lets a `let` shadow an item, so `let cb = …; cb()`
/// unambiguously invokes the local; emitting a `calls` edge for it points at
/// whichever project fn happens to share the name.
///
/// Both call channels needed this and neither had it: the ordinary
/// `call_expression` arm, and the macro token_tree pass (inside a token_tree the
/// value-reference pass never fires, so its M2.5 exclusion never applied). This
/// repo's own `refine_ambiguous_targets` keeps a deliberately-divergent local
/// `is_test_path` closure and the production index recorded it as a caller of
/// `domain::is_test_path`.
///
/// CamelCase names are exempt. [`collect_rust_binding_names`] over-collects from
/// pattern fields on purpose — a `match` arm `Ok(v)` contributes `Ok` as well as
/// `v` — which is precision-safe for value references but would cost real edges
/// here, where a tuple-variant/tuple-struct constructor call is exactly the edge
/// dead-code detection reads. Variant and type names are CamelCase by convention
/// (`non_camel_case_types`), the same rule [`extract_rust_macro_token_call`] uses
/// to tell patterns from calls in a token soup.
pub(super) fn shadowed_by_enclosing_local(
    node: &tree_sitter::Node,
    source: &str,
    name: &str,
) -> bool {
    if name.chars().next().is_some_and(|c| c.is_uppercase()) {
        return false;
    }
    // Two stages, and the split is what makes this both correct and cheap.
    //
    // The memoized set is a whole-function OVER-approximation: every binder
    // anywhere in the body, no scope, no ordering. On the `references` axis
    // that is precision-safe. On the CALLS axis it is not — it suppresses real
    // edges whenever the binder cannot actually shadow the call:
    //
    //   let a = helper(); let helper = 9;        // binder comes AFTER
    //   { let helper = 2; }  helper()            // binder in a sibling block
    //   for helper in v {}   helper()            // loop binder, call after
    //   match x { Ok(helper) => {} }  helper()   // arm binder, call after
    //   |helper| ...;        helper()            // closure param, call outside
    //
    // and a dropped `calls` edge is the dangerous direction: a live function
    // reported dead. So a hit on the over-approximation is not the answer, only
    // permission to ask the precise question — which walks the call's own
    // ancestor chain and costs nothing for the overwhelming majority of names,
    // because they never appear in the set at all.
    if !with_enclosing_fn_local_names(node, source, |names| names.contains(name)) {
        return false;
    }
    binder_shadows_call(node, source, name)
}

/// Is `name` bound by a binder that is BOTH in scope at `node` AND positioned
/// before it? Rust's real shadowing rule, as opposed to the whole-body
/// approximation [`with_enclosing_fn_local_names`] provides.
///
/// Walks outward from the call. At each enclosing block only the statements
/// that PRECEDE the call can shadow it; a construct that owns the call through
/// its body (a `for` loop, an `if let`, a `match` arm, a closure) shadows with
/// its own pattern regardless of byte order, since the binding is live for the
/// whole body. Stops at the enclosing `function_item` — Rust `let` scope does
/// not cross a nested-fn boundary.
fn binder_shadows_call(node: &tree_sitter::Node, source: &str, name: &str) -> bool {
    let mut child = *node;
    let mut cur = node.parent();
    while let Some(n) = cur {
        match n.kind() {
            // Only preceding statements are in scope. `child` is the subtree the
            // call sits in, so anything starting at or after it cannot shadow.
            "block" => {
                for i in 0..n.named_child_count() {
                    let Some(stmt) = n.named_child(i) else {
                        continue;
                    };
                    if stmt.start_byte() >= child.start_byte() {
                        break;
                    }
                    if stmt.kind() == "let_declaration" {
                        if let Some(pat) = stmt.child_by_field_name("pattern") {
                            if pattern_binds(&pat, source, name) {
                                return true;
                            }
                        }
                    }
                }
            }
            // Binding constructs: their pattern is live throughout the body, so
            // position does not apply — but only when the call is INSIDE that
            // body, which is exactly what walking up from the call establishes.
            "for_expression" | "let_condition" | "match_arm" => {
                if let Some(pat) = n.child_by_field_name("pattern") {
                    // A `match_arm`'s pattern wraps an optional `if` guard; only
                    // the binder half counts (the guard is an expression, and
                    // sweeping it in is how a real call became a "local").
                    let pat = if pat.kind() == "match_pattern" {
                        pat.named_child(0).unwrap_or(pat)
                    } else {
                        pat
                    };
                    if pattern_binds(&pat, source, name) {
                        return true;
                    }
                }
            }
            // `if let Some(x) = o { x() }` / `while let`: the binder lives in
            // the CONDITION, which is a SIBLING of the body we walked up from,
            // not an ancestor of the call. Reached only when the call is inside
            // the guarded branch, which is what makes the binding live for it.
            "if_expression" | "while_expression" => {
                let guarded = n
                    .child_by_field_name("consequence")
                    .or_else(|| n.child_by_field_name("body"))
                    .is_some_and(|b| b.id() == child.id());
                if guarded {
                    if let Some(cond) = n.child_by_field_name("condition") {
                        if cond.kind() == "let_condition" {
                            if let Some(pat) = cond.child_by_field_name("pattern") {
                                if pattern_binds(&pat, source, name) {
                                    return true;
                                }
                            }
                        }
                    }
                }
            }
            "closure_expression" => {
                if let Some(p) = n.child_by_field_name("parameters") {
                    let mut names = std::collections::HashSet::new();
                    collect_param_idents(&p, source, &mut names, 0);
                    if names.contains(name) {
                        return true;
                    }
                }
            }
            "function_item" => {
                if let Some(p) = n.child_by_field_name("parameters") {
                    let mut names = std::collections::HashSet::new();
                    collect_param_idents(&p, source, &mut names, 0);
                    return names.contains(name);
                }
                return false;
            }
            _ => {}
        }
        child = n;
        cur = n.parent();
    }
    false
}

/// Does this pattern introduce `name` as a binding? Identifier-only: a
/// CamelCase variant or const in the pattern is not a binder, and the caller
/// has already excluded CamelCase anyway.
fn pattern_binds(pat: &tree_sitter::Node, source: &str, name: &str) -> bool {
    let mut names = std::collections::HashSet::new();
    collect_idents_in(pat, source, &mut names, 0);
    names.contains(name)
}

fn collect_idents_in(
    node: &tree_sitter::Node,
    source: &str,
    out: &mut std::collections::HashSet<String>,
    depth: usize,
) {
    if depth > MAX_SUBTREE_DEPTH {
        return;
    }
    if node.kind() == "identifier" {
        out.insert(node_text(node, source).to_string());
    }
    for i in 0..node.named_child_count() {
        if let Some(c) = node.named_child(i) {
            collect_idents_in(&c, source, out, depth + 1);
        }
    }
}

thread_local! {
    /// Memo for the per-`function_item` half of the local-name set, keyed by the
    /// node id of the enclosing `function_item`.
    ///
    /// This runs on EVERY bare Rust call, and without it each call re-walks its
    /// whole enclosing function body: measured +16% on a full index of this
    /// repo's 109 Rust files (1.29s → 1.50s, median of 3) when the exclusion
    /// landed uncached.
    ///
    /// Node ids are unique WITHIN a tree and recycled ACROSS trees, so the cache
    /// is cleared at the top of every file's walk by
    /// [`reset_fn_local_names_cache`] — a stale cross-file hit would silently
    /// suppress real edges. `thread_local` (not a global) because file parsing
    /// fans out over rayon; each worker keeps its own cache and clears it per
    /// file it owns.
    static FN_LOCAL_NAMES: std::cell::RefCell<
        std::collections::HashMap<usize, std::rc::Rc<std::collections::HashSet<String>>>,
    > = std::cell::RefCell::new(std::collections::HashMap::new());
}

/// Drop the memo. MUST be called once per file before walking its tree — see
/// [`FN_LOCAL_NAMES`] for why a leaked entry is a correctness bug, not just
/// stale data.
pub(super) fn reset_fn_local_names_cache() {
    FN_LOCAL_NAMES.with(|c| c.borrow_mut().clear());
}

/// Run `f` over the LOCAL binding names visible to `node` (M2/M2.5), without
/// materializing a fresh set per call. A bare id equal to a local is a
/// pass-through of that local, not a reference to a same-named global fn.
/// Collects:
///   - parameters of the nearest enclosing `function_item` + any enclosing
///     closures (M2 — `type_identifier` types are excluded since only
///     `identifier` nodes are gathered; generic `<F>` params live in
///     `type_parameters`, not collected);
///   - `let` binding names in the nearest function body (M2.5) — gathered from
///     the `let_declaration` `pattern` field ONLY (never the RHS `value`), so
///     `let db = open()` contributes `db` but not `open`. This kills the
///     dominant Phase-1 false positive: `let db = open(); run(&db)` where an
///     accessor fn/method `db` also exists (`conn`, `db`, `picks` … measured in
///     dogfooding).
///
/// Rust `let` scope is function-local (nested fns don't capture), so the nearest
/// `function_item` is the correct boundary.
///
/// Split in two because only one half is memoizable:
///   - enclosing CLOSURE parameters are position-dependent (two sibling closures
///     in one fn have different params), so they are collected live — cheap, a
///     parameter list is tiny and there is rarely more than one closure deep;
///   - the enclosing `function_item`'s own params plus every binding in its body
///     depend only on the fn, so they come from [`FN_LOCAL_NAMES`]. That body
///     walk is the expensive half.
///
/// Memoizing the closure half too would be wrong in the recall-losing direction:
/// a call to a global `fmt` would be suppressed because an unrelated closure
/// elsewhere in the same fn happens to bind `fmt`.
fn with_enclosing_fn_local_names<R>(
    node: &tree_sitter::Node,
    source: &str,
    f: impl FnOnce(&std::collections::HashSet<String>) -> R,
) -> R {
    let mut closure_names = std::collections::HashSet::new();
    let mut fn_item = None;
    let mut cur = node.parent();
    while let Some(n) = cur {
        match n.kind() {
            "closure_expression" => {
                if let Some(p) = n.child_by_field_name("parameters") {
                    collect_param_idents(&p, source, &mut closure_names, 0);
                }
            }
            "function_item" => {
                fn_item = Some(n);
                break;
            }
            _ => {}
        }
        cur = n.parent();
    }

    let Some(item) = fn_item else {
        // No enclosing fn: closure params (if any) are all there is.
        return f(&closure_names);
    };

    let fn_names = FN_LOCAL_NAMES.with(|cache| {
        if let Some(hit) = cache.borrow().get(&item.id()) {
            return std::rc::Rc::clone(hit);
        }
        let mut names = std::collections::HashSet::new();
        if let Some(p) = item.child_by_field_name("parameters") {
            collect_param_idents(&p, source, &mut names, 0);
        }
        if let Some(body) = item.child_by_field_name("body") {
            collect_rust_binding_names(&body, source, &mut names, 0);
        }
        let rc = std::rc::Rc::new(names);
        cache
            .borrow_mut()
            .insert(item.id(), std::rc::Rc::clone(&rc));
        rc
    });

    if closure_names.is_empty() {
        f(&fn_names)
    } else {
        closure_names.extend(fn_names.iter().cloned());
        f(&closure_names)
    }
}

/// Walk a function body collecting binding names from every pattern-introducing
/// node's `pattern` field (not RHS values / scrutinees), so M2.5 suppresses bare
/// ids that are locals rather than global-fn references:
///   - `let_declaration`  (`let db = …`)
///   - `let_condition`    (`if let Some(node) = …` / `while let`)
///   - `match_arm`        (`Ok(val) => …`, `Err(error) => …`)
///   - `for_expression`   (`for item in …`)
///
/// Collecting from the `pattern` field gathers binding idents (and harmlessly any
/// enum-variant/const names in the pattern — over-collection is precision-safe).
/// Recurses to reach bindings in nested blocks. Used by `enclosing_fn_local_names`.
fn collect_rust_binding_names(
    node: &tree_sitter::Node,
    source: &str,
    out: &mut std::collections::HashSet<String>,
    depth: usize,
) {
    if depth > MAX_SUBTREE_DEPTH {
        return;
    }
    if matches!(
        node.kind(),
        "let_declaration" | "let_condition" | "match_arm" | "for_expression"
    ) {
        if let Some(pat) = node.child_by_field_name("pattern") {
            // A `match_arm`'s `pattern` field is a `match_pattern`, which wraps
            // BOTH the pattern and an optional `if` GUARD. The guard is an
            // ordinary expression, not a binder, so sweeping it in collected
            // every identifier it mentions — including called function names.
            // `Ok(c) if Path::new(p).is_relative() && !is_cwd_anchored(p) =>`
            // made `is_cwd_anchored` look like a local, suppressing the real
            // call edge from `cmd_grep` to the fn at cli.rs:580. Over-collection
            // is precision-safe for BINDERS (a variant name costs nothing) but
            // not for a guard, which is where callees live.
            let binder = if pat.kind() == "match_pattern" {
                pat.named_child(0)
            } else {
                Some(pat)
            };
            if let Some(b) = binder {
                collect_param_idents(&b, source, out, 0);
            }
        }
    }
    for i in 0..node.named_child_count() {
        if let Some(child) = node.named_child(i) {
            collect_rust_binding_names(&child, source, out, depth + 1);
        }
    }
}

fn collect_param_idents(
    node: &tree_sitter::Node,
    source: &str,
    out: &mut std::collections::HashSet<String>,
    depth: usize,
) {
    if depth > MAX_SUBTREE_DEPTH {
        return;
    }
    if node.kind() == "identifier" {
        out.insert(node_text(node, source).to_string());
    }
    for i in 0..node.named_child_count() {
        if let Some(child) = node.named_child(i) {
            collect_param_idents(&child, source, out, depth + 1);
        }
    }
}
