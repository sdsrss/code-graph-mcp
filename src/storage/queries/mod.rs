//! SQL query layer split into per-domain submodules.
//!
//! All public items are re-exported here so external callers can keep using the
//! flat `crate::storage::queries::X` import path that predates the split.
//! Cross-submodule helpers live in `helpers` (placeholders, MAX_IN_PARAMS,
//! generic `first_row`, test_db harness) and `nodes` (NODE_SELECT*, map_node_row).

mod helpers;

mod dead_code;
mod edges;
mod files;
mod imports;
mod nodes;
mod project_map;
mod routes;
mod search;
mod vectors;

pub use dead_code::{find_dead_code, DeadCodeResult};
pub use edges::{
    count_pending_unresolved_calls, delete_pending_unresolved_call, get_edge_source_names,
    get_edge_sources_with_files, get_edge_target_names, get_edge_target_names_batch,
    get_edge_targets_with_files, get_edges_batch, get_edges_from, get_incoming_references,
    insert_edge, insert_edge_cached, insert_pending_unresolved_call, list_pending_unresolved_calls,
    EdgeInfo, EdgeRecord, IncomingReference, PendingCallRow,
};
pub use files::{
    delete_files_by_paths, get_all_file_hashes, get_file_language, get_file_path,
    get_index_status, upsert_file, FileRecord, IndexStatus,
};
pub use imports::{get_import_tree, FileDependency};
pub use nodes::{
    delete_nodes_by_file, get_all_node_names_with_ids, get_dirty_node_ids,
    get_first_node_id_by_name, get_inbound_calls_for_pending, get_inbound_cross_file_edges,
    get_node_by_id, get_node_ids_by_name, get_node_names_with_paths_excluding_files,
    get_node_with_file_by_id, get_nodes_by_file_path, get_nodes_by_name,
    get_nodes_missing_context, get_nodes_with_files_by_filters, get_nodes_with_files_by_ids,
    get_nodes_with_files_by_name, insert_node, insert_node_cached, update_context_strings_batch,
    NameEntry, NodeRecord, NodeResult, NodeWithFile,
};
#[cfg(test)]
pub use nodes::update_context_string;
pub use project_map::{get_project_map, EntryPoint, HotFunction, ModuleDep, ModuleStats};
pub use routes::{
    find_routes_by_path, get_callers_with_route_info, get_module_exports, CallerWithRouteInfo,
    ModuleExport, RouteMatch,
};
pub use search::{find_functions_by_fuzzy_name, fts5_search, FtsResult, NameCandidate};
#[cfg(test)]
pub use search::fts5_search_with_tests;
pub use vectors::{
    count_nodes_with_vectors, get_node_embedding, get_unembedded_nodes, insert_node_vector,
    insert_node_vectors_batch, vector_search,
};
