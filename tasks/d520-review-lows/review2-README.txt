Review of d7a18c5d..aed40873 (code-graph-mcp 0.176.0 repair delta), 2026-10-21. Everything else here was deleted after the run.

Arms (release, --features embed-model, CARGO_TARGET_DIR here, md5):
  head = 03b01c4f 36e0f6c2 (same md5 as /var/tmp/d521/bin/cgm-fix2), pre = d7a18c5d f568d44c,
  low7 = 8f3f1670 b17a79c3, f31 = f31bbf5e e3c2482c, v175 = /var/tmp/d518r2/bin/cgm-v175 (e7a9b5c4),
  v175rel = ~/.cache/code-graph/bin/code-graph-mcp (f42613e2, copied).

What ran and what it showed:
- R1 (flask index built by v175): head health-check/callgraph/show rc 0 on 444 index and under a
  10 s BEGIN IMMEDIATE (5-21 ms each); pre rc 1 (readonly: "attempt to write a readonly database";
  locked: "database is locked" after 5013-5015 ms). Readers left schema_version 37 and the narrow key;
  head incremental-index -> key with COALESCE(metadata,''), schema_version 40, 2nd run 40, pending 923;
  v175 and v175rel health-check/callgraph rc 0; v175 rebuild-index --confirm rc 0 (narrow key again).
- Q1: head `show X`/`callgraph X` after deleting q.py writes 1 of 2 pending rows under the narrow key.
  v175-built DB (app_id 123): next head indexer wipes -> inc == fresh. f31-built DB (app_id 124,
  narrow): inc lacks `app.py f calls r.py X {"q":"member"}` vs fresh. Without the reader: equal.
- Full index (f31/low7/head), edges identical on flask hono express gin gson leveldb cgm nx std,
  repo@8f3f1670, repo@aed40873. Pending f31->head: cgm 8828->9917, express 1257, flask 921->923,
  gin 1955, gson 3933, hono 3005->3026, leveldb 1854->1911, nx 8834->8848, std 8556->8560,
  repo@8f3f1670 12181->13320, repo@aed40873 12183->13322.
- Suites: 1,205 steps: v175 362, f31 276, low7 270, head 270 (head subset of both, low7 == head).
  101 steps (twofile2-4): v175 12, f31=low7=head 8; head-not-v175 = q03 q20 q23 q24 r06 r11.
  926 steps (s1-s8 + rename + cases1-6; 863 scenarios, 1,789 lines): v175 335, f31=low7=head 262.
  66 parity scenarios: f31/low7/head 65 equal, p15 differs.
- flask fuzz seeds 1-3 x 30: introduced/differed-after: v175 6/33, f31 3/32, low7 2/20, head 2/20.
- Edit cost (append comment to src/storage/db.rs, repo@aed40873, load avg 5-14 from other sessions):
  f31/low7/head n=15 566/561/566, n=41 393/417/396; rerun f31/low7 n=15 425/404, n=41 384/360 ms.
- Mutations (mut tree = 03b01c4f, cargo test --lib): killed M2 M3 M4 M5 M6 M7 M8 M9 M11 M12;
  survived M1 (narrow key in create_tables_sql; full lib 1511/1511) and M10 (widen error swallowed;
  full lib 1511/1511); M13 (old rename leg alone, narrow key): pipeline test passes (pinned nothing).
- q20: inc keeps go.py <module> -imports{"attr":"run"}-> tools/utils.py <module>; 32 CLI invocations,
  none prints it; deps/affected/refs/callgraph/impact/map/dead-code text output identical.
- Context strings: an edge bound by the pending sweep is missing from both ends' context_string after
  an incremental run (head and v175, 3 small scenarios) -> FTS `search` order differs from fresh.
