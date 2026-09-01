//! SQL query layer split into per-domain submodules.
//!
//! All public items are re-exported here so external callers can keep using the
//! flat `crate::storage::queries::X` import path that predates the split.
//! Cross-submodule helpers live in `helpers` (placeholders, MAX_IN_PARAMS,
//! generic `first_row`, test_db harness) and `nodes` (NODE_SELECT*, map_node_row).

pub(crate) mod helpers;

mod dead_code;
mod edges;
mod embedding_cache;
mod files;
mod imports;
mod meta;
mod nodes;
mod project_map;
pub(crate) mod routes;
mod search;
mod vectors;

pub use dead_code::{
    dead_code_report, find_dead_code, unindexed_path_prefix, validate_dead_code_type_filter,
    DeadCodeItem, DeadCodeReport, DeadCodeResult,
};
pub use edges::{
    age_and_evict_pending_unresolved_calls, count_pending_unresolved_calls,
    delete_pending_unresolved_call, get_edge_source_names, get_edge_sources_with_files,
    get_edge_target_names, get_edge_target_names_batch, get_edge_targets_with_files,
    get_edges_batch, get_edges_from, get_incoming_references, insert_edge, insert_edge_cached,
    insert_pending_unresolved_call, list_pending_unresolved_calls, resolution_stats, EdgeInfo,
    EdgeRecord, IncomingReference, PendingCallRow, ResolutionStats,
};
pub use embedding_cache::{
    cache_key, cache_put_embeddings, ensure_embedding_cache_valid, gc_embedding_cache,
    partition_by_cache, seed_embedding_cache_from_vectors,
};
pub use files::{
    delete_files_by_paths, get_all_file_hashes, get_file_language, get_file_path, get_index_status,
    upsert_file, FileRecord, IndexStatus,
};
pub use imports::{
    all_file_import_edges, file_is_indexed, get_import_tree, get_reverse_dependents, FileDependency,
};
pub use meta::{delete_meta, get_meta, set_meta};
#[cfg(test)]
pub use nodes::update_context_string;
pub use nodes::{
    delete_nodes_by_file, filter_method_ids, get_all_node_names_with_ids, get_dirty_node_ids,
    get_external_sentinel_importers, get_first_node_id_by_name, get_inbound_calls_for_pending,
    get_inbound_cross_file_edges, get_inbound_relations_for_requeue, get_node_by_id,
    get_node_ids_by_name, get_node_names_with_paths_excluding_files, get_node_paths_by_ids,
    get_node_qualified_names_by_ids, get_node_with_file_by_id, get_nodes_by_file_path,
    get_nodes_by_name, get_nodes_missing_context, get_nodes_with_files_by_filters,
    get_nodes_with_files_by_ids, get_nodes_with_files_by_name, get_structural_dependent_files,
    insert_node, insert_node_cached, reap_orphan_external_nodes, update_context_strings_batch,
    NameEntry, NodeRecord, NodeResult, NodeWithFile,
};
pub use project_map::{get_project_map, EntryPoint, HotFunction, ModuleDep, ModuleStats};
pub use routes::{
    fetch_route_metadata_map, find_routes_by_path, get_module_exports, CallerWithRouteInfo,
    ModuleExport, RouteMatch,
};
#[cfg(test)]
pub use search::fts5_search_with_tests;
pub use search::{find_functions_by_fuzzy_name, fts5_search, FtsResult, NameCandidate};
pub use vectors::{
    compact_node_vectors, compact_node_vectors_if_wasteful, count_nodes_with_vectors,
    count_unembedded_nodes, delete_node_vectors_batch, get_node_embedding, get_unembedded_nodes,
    get_unembedded_nodes_excluding, insert_node_vector, insert_node_vectors_batch,
    reap_orphan_vectors, vec_slot_occupancy, vector_search,
};
