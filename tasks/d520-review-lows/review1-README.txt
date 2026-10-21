Pre-release review of code-graph-mcp 0.176.0 (f6e8c932..d7a18c5d), 2026-10-21. Everything else was deleted.
Builds (release, --features embed-model, own src copies, shared target, touched): cgm-base = f31bbf5e md5 e3c2482c9f28...,
cgm-head = d7a18c5d md5 f568d44cf82d...; cgm-v175 = copy of /var/tmp/d518r2/bin/cgm-v175 md5 e7a9b5c482a6....
Ran: fullcmp.py (10 corpora, base vs head: edges md5-identical; pending flask 921->923, hono 3005->3026, nx 8834->8848,
std 8556->8560, leveldb 1854->1911, cgm(0.174 tree) 8828->9917, cgm-now 12181->13320);
par.py on /var/tmp/d520-review/scn (994 scen / 1205 steps): v175 362, base 276, head 270, head-base = head-v175 = {};
par.py on /var/tmp/d518r2/scn/twofile2-4 (51 scen / 101 steps): v175 12, base 8, head 8, head-v175 = 6 (q03,r11,q20,q23,q24,r06);
scen.py 0.175.0 reviews: 1789 step lines = 863 step-0 + 926 compared; v175 335, base 262, head 262, same steps;
parity.py D#370(a)+big_run_attr: 65/66 equal in all three arms (p15);
fuzz.py flask seeds 1-3 x30: new-divergence ops v175 6, base 3, head 2; ops after which inc != fresh v175 33, base 32, head 20;
fuzz2.py EXT=.rs (this repo) / .cc (leveldb) / .ts (hono) x30: base == head per op (rs 16/30 differ in both, cc 0, ts 0);
compat.sh (v175 <-> head), race.sh (indexer + 6 readers x10), read-only and held-lock reader probes, c4.py (flask rename),
mutate.py (13 mutations of the fix, lib tests in a debug copy), cost.py (this repo edit: 326 vs 337 ms /15, 309 vs 312 ms /41).
Safe to delete.
