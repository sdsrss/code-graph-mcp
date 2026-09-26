//! Java-specific extraction: REFERENCES edges for edgeless type-position usages.
//!
//! Like Rust/TS/Go (and UNLIKE Python), tree-sitter-java represents a type name
//! in type position as a distinct `type_identifier` kind, so the extractor gates
//! on node kind. The kinds were probe-confirmed against the bundled
//! tree-sitter-java grammar:
//! - field type:     `class S { Foo f; }` → `field_declaration[field=type]` → `type_identifier Foo`
//! - param type:     `m(Foo x)` → `formal_parameter[field=type]` → `type_identifier Foo`
//! - return type:    `Foo m()` → `method_declaration[field=type]` → `type_identifier Foo`
//! - local var:      `Foo v = ...` → `local_variable_declaration[field=type]` → `type_identifier Foo`
//! - generic arg:    `List<Foo>` → `generic_type` → `type_arguments` → `type_identifier Foo`
//! - array element:  `Foo[]` → `array_type[field=element]` → `type_identifier Foo`
//! - `new` type:     `new Foo()` → `object_creation_expression[field=type]` → `type_identifier Foo`
//! - qualified type: `pkg.Sub.Deep` → nested `scoped_type_identifier`; see below.
//!
//! Naturally excluded (NOT `type_identifier`, never reach this fn):
//! - definition NAMES — `class Foo {}` / `interface Foo {}` / `enum Foo {}` /
//!   `record Foo()` all put the name in the `name` field as a plain `identifier`,
//!   so the type's own definition name never self-references (no parent-skip
//!   needed, unlike Go/Rust where the def name IS a `type_identifier`);
//! - PRIMITIVES — `int long short byte` → `integral_type`, `double float` →
//!   `floating_point_type`, `boolean` → `boolean_type`, `void` → `void_type`,
//!   `char` → its own kind; none are `type_identifier`;
//! - value field-access / method calls — `obj.field`, `Math.PI`, `obj.call()` use
//!   `identifier` / `field_access` / `method_invocation`, never `type_identifier`;
//! - annotation names — `@Override` is a `marker_annotation` whose `name` field is
//!   a plain `identifier`, not a `type_identifier` (and `Override` is in the noise
//!   set as belt-and-suspenders).
//!
//! Heritage (handled by parent-chain skip, since these ARE `type_identifier`):
//! - `class Foo extends Bar` → `superclass` clause → `type_identifier Bar`;
//! - `implements Baz` → `super_interfaces` → `type_list` → `type_identifier Baz`;
//! - `interface I extends J` → `extends_interfaces` → `type_list` → `type_identifier J`.
//!   These already yield inherits/implements edges, so a references edge would be
//!   a double edge (and would defeat dead-code detection for the supertype).
//!
//! Qualified types: `pkg.Sub.Deep` parses as nested `scoped_type_identifier`,
//! where EVERY segment (`pkg`, `Sub`, `Deep`, and `java`/`util` in
//! `java.util.List`) is a `type_identifier`. In this grammar version
//! `scoped_type_identifier` exposes no field names; each has exactly two named
//! children `[scope, tail]` where `scope` is the inner `scoped_type_identifier`
//! (or the head `type_identifier`) and `tail` is the rightmost `type_identifier`.
//! Only the rightmost segment of the WHOLE chain (`Deep` / `List`) is a real type
//! usage; the package-path segments are not. We emit a `type_identifier` under a
//! `scoped_type_identifier` only when it is the chain tail — verified by walking
//! up the STI chain and requiring the node (then each enclosing STI) to be the
//! tail (last named child) of its STI parent. A segment that is ever a `scope`
//! (`named_child(0)`) child is rejected.

use super::super::node_text;
use super::ParsedRelation;
use crate::domain::{JAVA_TYPE_REFERENCE_NOISE, REL_REFERENCES};

/// True if `node` (a `type_identifier`) is the tail of a `scoped_type_identifier`
/// chain — i.e. the rightmost segment of a qualified type like `pkg.Sub.Deep`
/// (`Deep`) — rather than a package-path segment (`pkg`, `Sub`).
///
/// Each `scoped_type_identifier` has two named children: `named_child(0)` = the
/// scope (head `type_identifier` or inner `scoped_type_identifier`) and
/// `named_child(1)` = the tail `type_identifier`. A `type_identifier` is the
/// whole-chain tail iff it is the tail child of its STI parent AND every
/// enclosing STI is itself the tail child of ITS parent, up to the first non-STI
/// ancestor. If at any level the current subtree is the scope child
/// (`named_child(0)`), it is a package-path segment, not the chain tail.
fn is_scoped_type_tail(node: &tree_sitter::Node) -> bool {
    let mut current = *node;
    loop {
        let parent = match current.parent() {
            Some(p) => p,
            None => return false,
        };
        if parent.kind() != "scoped_type_identifier" {
            // Reached the top of the STI chain without ever being a scope child:
            // `current` is the rightmost subtree → this is the chain tail.
            return true;
        }
        // Must be the tail (last named child) of this STI level, not the scope.
        let tail = parent.named_child(parent.named_child_count().saturating_sub(1));
        if tail.map(|t| t.id()) != Some(current.id()) {
            return false;
        }
        current = parent;
    }
}

/// True if `node` (a `type_identifier`) sits in a heritage clause
/// (`extends` / `implements`) and is therefore already covered by an
/// inherits/implements edge. Walks a bounded parent chain because the type can be
/// directly under `superclass` (`extends Bar`) or nested in a `type_list` under
/// `super_interfaces` / `extends_interfaces` (`implements Baz`, multiple
/// interfaces). Stops at the first declaration/body boundary so an ordinary type
/// usage elsewhere in the class is never mistaken for heritage.
fn is_in_heritage_clause(node: &tree_sitter::Node) -> bool {
    let mut current = match node.parent() {
        Some(p) => p,
        None => return false,
    };
    // Bounded climb: heritage is at most type_identifier → type_list →
    // super_interfaces, so a depth of a few is plenty.
    for _ in 0..4 {
        match current.kind() {
            "superclass" | "super_interfaces" | "extends_interfaces" => return true,
            // `type_list` wraps the interface list under super_interfaces /
            // extends_interfaces; `scoped_type_identifier` keeps the chain
            // together so a qualified supertype (`extends a.b.Base`) still resolves.
            "type_list" | "scoped_type_identifier" => {
                current = match current.parent() {
                    Some(p) => p,
                    None => return false,
                };
            }
            // Any other ancestor (class_body, generic_type, field_declaration,
            // formal_parameter, ...) means this is not a heritage type.
            _ => return false,
        }
    }
    false
}

/// Emit a `references` edge for a `type_identifier` used in type position. Skips
/// the cases already covered by another edge (or that aren't usages) so the
/// references edge stays a pure "edgeless usage" signal:
/// - heritage types (`extends Bar` / `implements Baz`) — already an
///   inherits/implements edge (`is_in_heritage_clause`);
/// - package-path segments of a qualified type (`pkg`/`Sub` in `pkg.Sub.Deep`) —
///   only the chain tail is a real type usage (`is_scoped_type_tail`);
/// - JDK common reference types (`String`, `List`, `Override`, ...) —
///   `JAVA_TYPE_REFERENCE_NOISE`, they resolve to the JDK, not a project symbol;
/// - empty.
///
/// The type's OWN definition name needs no skip here: Java class/interface/enum/
/// record names are plain `identifier`s, not `type_identifier`s, so they never
/// reach this fn.
pub(super) fn extract_java_type_reference(
    node: &tree_sitter::Node,
    source: &str,
    scope: Option<&str>,
) -> Option<ParsedRelation> {
    if is_in_heritage_clause(node) {
        return None;
    }
    // Qualified-type package-path segments are `type_identifier`s under a
    // `scoped_type_identifier`; only the chain tail is a real type usage.
    if node
        .parent()
        .map(|p| p.kind() == "scoped_type_identifier")
        .unwrap_or(false)
        && !is_scoped_type_tail(node)
    {
        return None;
    }
    let name = node_text(node, source);
    if name.is_empty() || JAVA_TYPE_REFERENCE_NOISE.contains(&name) {
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
