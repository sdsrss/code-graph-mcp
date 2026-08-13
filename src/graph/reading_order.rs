//! Dependency-ordered "reading order" for the `tour` command.
//!
//! Pure graph logic over the project-map module dependency edges: a Kahn
//! topological sort that lists a module's prerequisites (the modules it imports)
//! before the modules that build on them, so reading top-to-bottom builds
//! understanding from the ground up. Deterministic for a fixed index (no
//! HashMap iteration order leaks into the output) and cycle-tolerant.

use std::cmp::Reverse;
use std::collections::{BTreeSet, BinaryHeap, HashMap};

use crate::storage::queries::{EntryPoint, ModuleDep, ModuleStats};

/// Heuristic role of a module in the dependency graph (precedence:
/// Entry > Foundational > Core > Mid).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    /// Contains a program/HTTP entry point — the "top" of the graph.
    Entry,
    /// Imports nothing in-scope — a leaf primitive, read first.
    Foundational,
    /// Imported by many modules (`depended_on_by >= CORE_THRESHOLD`).
    Core,
    /// Everything else.
    Mid,
}

impl Role {
    pub fn as_str(self) -> &'static str {
        match self {
            Role::Entry => "entry",
            Role::Foundational => "foundational",
            Role::Core => "core",
            Role::Mid => "mid",
        }
    }
}

/// `depended_on_by` count at or above which a non-entry, non-foundational module
/// is labelled `Core`.
const CORE_THRESHOLD: usize = 3;

/// One module in the computed reading order.
#[derive(Debug, Clone)]
pub struct ReadingOrderEntry {
    pub path: String,
    pub role: Role,
    /// Number of in-scope modules that import this one.
    pub depended_on_by: usize,
    /// In-scope modules this one imports (sorted, for annotation).
    pub depends_on: Vec<String>,
    pub key_symbols: Vec<String>,
    /// True when this module was emitted via cycle-breaking (its prerequisites
    /// were not all satisfiable because of an import cycle).
    pub in_cycle: bool,
}

/// Directory part of a file path (everything before the last '/'), matching
/// the `dir_of` convention used by the project-map module aggregation.
fn dir_of(path: &str) -> &str {
    match path.rfind('/') {
        Some(i) => &path[..i],
        None => "<root>",
    }
}

/// Compute the dependency-ordered reading order for the given modules.
///
/// `deps` are cross-module import edges (`from` imports `to`); only edges whose
/// *both* endpoints are in `modules` are considered (so a `PATH`-scoped subset
/// stays self-consistent). `entry_points` flag the modules that own a program
/// or HTTP entry point.
pub fn compute_reading_order(
    modules: &[ModuleStats],
    deps: &[ModuleDep],
    entry_points: &[EntryPoint],
) -> Vec<ReadingOrderEntry> {
    let n = modules.len();
    if n == 0 {
        return Vec::new();
    }

    // Deterministic indexing: order modules by path, key everything by position.
    let mut sorted: Vec<&ModuleStats> = modules.iter().collect();
    sorted.sort_by(|a, b| a.path.cmp(&b.path));
    let idx: HashMap<&str, usize> = sorted
        .iter()
        .enumerate()
        .map(|(i, m)| (m.path.as_str(), i))
        .collect();

    // Prerequisites each module imports (in-scope only) + reverse dependents.
    let mut prereqs: Vec<BTreeSet<usize>> = vec![BTreeSet::new(); n];
    let mut dependents: Vec<Vec<usize>> = vec![Vec::new(); n];
    for d in deps {
        if let (Some(&fi), Some(&ti)) = (idx.get(d.from.as_str()), idx.get(d.to.as_str())) {
            if fi == ti {
                continue; // self-import is not a prerequisite
            }
            if prereqs[fi].insert(ti) {
                dependents[ti].push(fi);
            }
        }
    }

    // Modules owning a program / HTTP entry point.
    let mut is_entry = vec![false; n];
    for ep in entry_points {
        if let Some(&i) = idx.get(dir_of(&ep.file)) {
            is_entry[i] = true;
        }
    }

    // Kahn topological sort — prerequisites first, ties broken by path position
    // (smallest first) for determinism.
    let mut indeg: Vec<usize> = (0..n).map(|i| prereqs[i].len()).collect();
    let mut emitted = vec![false; n];
    let mut in_cycle = vec![false; n];
    let mut heap: BinaryHeap<Reverse<usize>> = BinaryHeap::new();
    for (i, &d) in indeg.iter().enumerate() {
        if d == 0 {
            heap.push(Reverse(i));
        }
    }

    let mut emit_order: Vec<usize> = Vec::with_capacity(n);
    while emit_order.len() < n {
        let next = match heap.pop() {
            Some(Reverse(i)) => i,
            None => {
                // Import cycle: no prerequisite-free module remains. Break it
                // deterministically — smallest remaining indegree, ties by
                // position — and flag the cut.
                let cut = (0..n)
                    .filter(|&i| !emitted[i])
                    .min_by_key(|&i| (indeg[i], i))
                    .expect("loop guard guarantees a remaining module");
                in_cycle[cut] = true;
                cut
            }
        };
        if emitted[next] {
            continue;
        }
        emitted[next] = true;
        emit_order.push(next);
        for &dep_i in &dependents[next] {
            if !emitted[dep_i] && indeg[dep_i] > 0 {
                indeg[dep_i] -= 1;
                if indeg[dep_i] == 0 {
                    heap.push(Reverse(dep_i));
                }
            }
        }
    }

    emit_order
        .into_iter()
        .map(|i| {
            let depends_on: Vec<String> =
                prereqs[i].iter().map(|&p| sorted[p].path.clone()).collect();
            let depended_on_by = dependents[i].len();
            let role = if is_entry[i] {
                Role::Entry
            } else if depends_on.is_empty() {
                Role::Foundational
            } else if depended_on_by >= CORE_THRESHOLD {
                Role::Core
            } else {
                Role::Mid
            };
            ReadingOrderEntry {
                path: sorted[i].path.clone(),
                role,
                depended_on_by,
                depends_on,
                key_symbols: sorted[i].key_symbols.clone(),
                in_cycle: in_cycle[i],
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn module(path: &str) -> ModuleStats {
        ModuleStats {
            path: path.to_string(),
            files: 1,
            functions: 1,
            classes: 0,
            interfaces_traits: 0,
            constants: 0,
            languages: vec!["rust".to_string()],
            key_symbols: vec![],
        }
    }

    /// `from` imports `to`.
    fn dep(from: &str, to: &str) -> ModuleDep {
        ModuleDep {
            from: from.to_string(),
            to: to.to_string(),
            import_count: 1,
        }
    }

    fn main_entry(file: &str) -> EntryPoint {
        EntryPoint {
            route: "main".to_string(),
            handler: "main".to_string(),
            file: file.to_string(),
            kind: "main".to_string(),
        }
    }

    fn paths(order: &[ReadingOrderEntry]) -> Vec<&str> {
        order.iter().map(|e| e.path.as_str()).collect()
    }

    #[test]
    fn empty_input_yields_empty() {
        assert!(compute_reading_order(&[], &[], &[]).is_empty());
    }

    #[test]
    fn linear_chain_lists_prerequisites_first() {
        // a imports b, b imports c  ⇒  read c, then b, then a.
        let modules = [module("a"), module("b"), module("c")];
        let deps = [dep("a", "b"), dep("b", "c")];
        let order = compute_reading_order(&modules, &deps, &[]);
        assert_eq!(paths(&order), ["c", "b", "a"]);
    }

    #[test]
    fn diamond_orders_base_first_apex_last_deterministically() {
        // a→b, a→c, b→d, c→d  ⇒  d first, a last; b,c by path.
        let modules = [module("a"), module("b"), module("c"), module("d")];
        let deps = [dep("a", "b"), dep("a", "c"), dep("b", "d"), dep("c", "d")];
        let order1 = compute_reading_order(&modules, &deps, &[]);
        let order2 = compute_reading_order(&modules, &deps, &[]);
        assert_eq!(paths(&order1), ["d", "b", "c", "a"]);
        assert_eq!(paths(&order1), paths(&order2), "must be deterministic");
    }

    #[test]
    fn cycle_emits_every_module_once_with_deterministic_cut() {
        // a→b, b→a  ⇒  both emitted exactly once. The lex-smallest module is the
        // deterministic cut point (emitted first, flagged); the other then
        // resolves normally. (Cut-point flagging, not full SCC membership.)
        let modules = [module("a"), module("b")];
        let deps = [dep("a", "b"), dep("b", "a")];
        let order = compute_reading_order(&modules, &deps, &[]);
        assert_eq!(order.len(), 2, "every module emitted exactly once");
        assert_eq!(
            paths(&order),
            ["a", "b"],
            "deterministic cycle break (lex smallest first)"
        );
        assert!(order[0].in_cycle, "cut point flagged");
        assert_eq!(
            order.iter().filter(|e| e.in_cycle).count(),
            1,
            "exactly the deterministic cut point is flagged"
        );
    }

    #[test]
    fn counts_and_depends_on_are_correct() {
        let modules = [module("a"), module("b"), module("c")];
        let deps = [dep("a", "c"), dep("b", "c")];
        let order = compute_reading_order(&modules, &deps, &[]);
        let c = order.iter().find(|e| e.path == "c").unwrap();
        assert_eq!(c.depended_on_by, 2, "c is imported by a and b");
        let a = order.iter().find(|e| e.path == "a").unwrap();
        assert_eq!(a.depends_on, ["c"]);
        assert_eq!(a.depended_on_by, 0);
    }

    #[test]
    fn role_labels_cover_entry_foundational_core_mid() {
        // leaf: imports nothing            → Foundational (heavily-depended leaf, like domain.rs)
        // hub:  imports leaf, depended-on-by 3 (u1,u2,u3) → Core
        // u1:   imports hub, depended-on-by 0             → Mid
        // src:  owns src/main.rs                          → Entry (precedence over Foundational)
        let modules = [
            module("leaf"),
            module("hub"),
            module("u1"),
            module("u2"),
            module("u3"),
            module("src"),
        ];
        let deps = [
            dep("hub", "leaf"),
            dep("u1", "hub"),
            dep("u2", "hub"),
            dep("u3", "hub"),
        ];
        let eps = [main_entry("src/main.rs")];
        let order = compute_reading_order(&modules, &deps, &eps);
        let role = |p: &str| order.iter().find(|e| e.path == p).unwrap().role;
        assert_eq!(role("leaf"), Role::Foundational, "imports nothing in-scope");
        assert_eq!(role("hub"), Role::Core, "depended-on-by 3 AND imports leaf");
        assert_eq!(role("u1"), Role::Mid, "imports hub but depended-on-by 0");
        assert_eq!(role("src"), Role::Entry, "owns src/main.rs entry point");
    }

    #[test]
    fn out_of_scope_edges_are_ignored() {
        // Edge to a module not in the set must not create a phantom prerequisite.
        let modules = [module("a")];
        let deps = [dep("a", "external_not_in_set")];
        let order = compute_reading_order(&modules, &deps, &[]);
        assert_eq!(paths(&order), ["a"]);
        assert_eq!(order[0].depends_on.len(), 0, "out-of-scope dep dropped");
        assert_eq!(order[0].role, Role::Foundational);
    }
}
