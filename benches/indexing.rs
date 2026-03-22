use criterion::{criterion_group, criterion_main, Criterion};
use std::path::Path;
use tempfile::TempDir;

use code_graph_mcp::storage::db::Database;
use code_graph_mcp::indexer::pipeline;

fn generate_test_files(dir: &Path, count: usize) {
    for i in 0..count {
        let content = format!(
            "export function func{}(x: number): string {{\n  return x.toString();\n}}\n\
             export class Class{} {{\n  method{}() {{ return func{}(42); }}\n}}\n",
            i, i, i, i
        );
        std::fs::write(dir.join(format!("file_{}.ts", i)), content).unwrap();
    }
}

fn bench_full_index(c: &mut Criterion) {
    let mut group = c.benchmark_group("indexing");
    group.sample_size(10);

    for file_count in [50, 200] {
        group.bench_function(format!("full_index_{}_files", file_count), |b| {
            b.iter_with_setup(
                || {
                    let tmp = TempDir::new().unwrap();
                    generate_test_files(tmp.path(), file_count);
                    let db_dir = tmp.path().join(".code-graph");
                    std::fs::create_dir_all(&db_dir).unwrap();
                    let db = Database::open(&db_dir.join("index.db")).unwrap();
                    (tmp, db)
                },
                |(_tmp, db)| {
                    pipeline::run_full_index(&db, _tmp.path(), None, None).unwrap();
                },
            );
        });
    }
    group.finish();
}

fn bench_fts5_search(c: &mut Criterion) {
    let tmp = TempDir::new().unwrap();
    generate_test_files(tmp.path(), 200);
    let db_dir = tmp.path().join(".code-graph");
    std::fs::create_dir_all(&db_dir).unwrap();
    let db = Database::open(&db_dir.join("index.db")).unwrap();
    pipeline::run_full_index(&db, tmp.path(), None, None).unwrap();

    let mut group = c.benchmark_group("search");
    group.bench_function("fts5_search", |b| {
        b.iter(|| {
            code_graph_mcp::storage::queries::fts5_search(db.conn(), "func method", 10).unwrap();
        });
    });
    group.finish();
}

fn bench_call_graph(c: &mut Criterion) {
    let tmp = TempDir::new().unwrap();
    generate_test_files(tmp.path(), 100);
    let db_dir = tmp.path().join(".code-graph");
    std::fs::create_dir_all(&db_dir).unwrap();
    let db = Database::open(&db_dir.join("index.db")).unwrap();
    pipeline::run_full_index(&db, tmp.path(), None, None).unwrap();

    let mut group = c.benchmark_group("graph");
    group.bench_function("call_graph_depth_5", |b| {
        b.iter(|| {
            code_graph_mcp::graph::query::get_call_graph(
                db.conn(), "func0", "both", 5, None,
            ).unwrap();
        });
    });
    group.finish();
}

criterion_group!(benches, bench_full_index, bench_fts5_search, bench_call_graph);
criterion_main!(benches);
