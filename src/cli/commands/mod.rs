use super::*;

pub mod affected;
pub mod ast_search;
pub mod benchmark;
pub mod callgraph;
pub mod centrality;
pub mod cycles;
pub mod dead_code;
pub mod deps;
pub mod impact;
pub mod map;
pub mod overview;
pub mod refs;
pub mod reindex;
pub mod report;
pub mod search;
pub mod show;
pub mod similar;
pub mod snapshot;
pub mod stats;
pub mod surprising;
pub mod tour;
pub mod trace;

/// The traversal's own depth ceiling, re-exported so the handlers that clamp a
/// `--depth` read the enforcing constant instead of restating its value. MCP
/// derives its `COUNT_RANGES` depth rows from the same place, for the same
/// reason: a restated bound is a second copy that goes stale unwatched.
pub(crate) use crate::graph::query::CALL_GRAPH_MAX_DEPTH;

/// Clamp a count-like CLI argument into `lo..=hi` and say so when the value moved.
///
/// The clamp and the disclosure are one call for the same reason `COUNT_RANGES`
/// exists on the MCP side: the bound that is ENFORCED and the bound that is
/// REPORTED cannot drift when a single expression produces both. Before this,
/// every CLI handler spelled `.clamp(lo, hi)` inline and only `callgraph` and
/// `affected` said anything, so `search --limit 500` returned exactly 100 rows
/// with nothing to distinguish that cut from a complete answer — and
/// `ast-search --limit 999` went one worse, clamping to 100 and then printing
/// "raise --limit to see the rest".
///
/// stderr, not stdout: a `--json` consumer's envelope stays parseable, and on a
/// terminal both streams land in the same place.
pub(crate) fn clamp_arg<T>(flag: &str, requested: T, lo: T, hi: T) -> T
where
    T: Ord + Copy + std::fmt::Display,
{
    let applied = requested.clamp(lo, hi);
    if applied != requested {
        eprintln!(
            "[code-graph] {flag} clamped to {applied} (requested {requested}) — valid range is {lo}..={hi}"
        );
    }
    applied
}

/// [`clamp_arg`] for an argument with a floor but no ceiling.
///
/// Separate rather than `clamp_arg(.., 1, u32::MAX)` because the message states
/// the range, and `valid range is 1..=4294967295` advertises a ceiling that is
/// an integer-width artefact, not a rule the tool has. `centrality` and
/// `surprising` rank as many rows as you ask for; `callgraph`'s ceiling is
/// disclosed downstream by the traversal's own `depth_capped` signal.
pub(crate) fn floor_arg<T>(flag: &str, requested: T, lo: T) -> T
where
    T: Ord + Copy + std::fmt::Display,
{
    if requested < lo {
        eprintln!(
            "[code-graph] {flag} raised to {lo} (requested {requested}) — the minimum is {lo}"
        );
        return lo;
    }
    requested
}

pub use affected::*;
pub use ast_search::*;
pub use benchmark::*;
pub use callgraph::*;
pub use centrality::*;
pub use cycles::*;
pub use dead_code::*;
pub use deps::*;
pub use impact::*;
pub use map::*;
pub use overview::*;
pub use refs::*;
pub use reindex::*;
pub use report::*;
pub use search::*;
pub use show::*;
pub use similar::*;
pub use snapshot::*;
pub use stats::*;
pub use surprising::*;
pub use tour::*;
pub use trace::*;
