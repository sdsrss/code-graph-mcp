#!/usr/bin/env python3
"""judgecases.py <cases.py> <work> [filter...]: per case, fresh index both arms,
judge every base-only and every head calls/references edge with symjudge."""
import importlib.util, os, shutil, sqlite3, subprocess, sys, io, contextlib
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import symjudge  # noqa

BIN = {"base": "/var/tmp/d909-review/bin/base", "head": "/var/tmp/d909-review/bin/head"}
ENV = dict(os.environ, CODE_GRAPH_NO_AUTO_UPDATE="1", CODE_GRAPH_DISABLE_MODEL_DOWNLOAD="1")
ROWS = """SELECT e.relation, sf.path, sn.type, COALESCE(sn.qualified_name, sn.name), sn.start_line,
 tf.path, tn.type, COALESCE(tn.qualified_name, tn.name), tn.start_line, e.confidence, COALESCE(e.metadata,''), sf.language
 FROM edges e JOIN nodes sn ON sn.id=e.source_id JOIN files sf ON sf.id=sn.file_id
 JOIN nodes tn ON tn.id=e.target_id JOIN files tf ON tf.id=tn.file_id WHERE e.relation IN ('calls','references')"""


def index(arm, files, d):
    assert d.startswith("/var/tmp/d909-review/")
    shutil.rmtree(d, ignore_errors=True)
    os.makedirs(d)
    subprocess.run(["git", "init", "-q"], cwd=d, check=True)
    for p, body in files.items():
        f = os.path.join(d, p)
        os.makedirs(os.path.dirname(f), exist_ok=True)
        open(f, "w").write(body)
    r = subprocess.run([BIN[arm], "incremental-index", "--no-embed", "--quiet"], cwd=d, env=ENV, capture_output=True, text=True)
    assert r.returncode == 0, r.stderr
    con = sqlite3.connect(f"file:{d}/.code-graph/index.db?mode=ro", uri=True)
    rows = ["\t".join(map(str, x)) for x in con.execute(ROWS)]
    con.close()
    return rows


def judge(d, rows):
    tsv = os.path.join(d, "..", "j.tsv")
    open(tsv, "w").write("".join(r + "\n" for r in rows))
    sys.argv = ["symjudge", d, tsv]
    buf = io.StringIO()
    with contextlib.redirect_stdout(buf):
        out = symjudge.main()
    return out


spec = importlib.util.spec_from_file_location("cases", sys.argv[1])
mod = importlib.util.module_from_spec(spec)
spec.loader.exec_module(mod)
work = os.path.abspath(sys.argv[2])
pick = sys.argv[3:]
for name, files in mod.CASES.items():
    if pick and not any(p in name for p in pick):
        continue
    b = index("base", files, os.path.join(work, "b"))
    h = index("head", files, os.path.join(work, "h"))
    bo = [r for r in b if r not in h]
    print("===", name)
    for o in judge(os.path.join(work, "b"), bo):
        print("   base-only", o)
    for o in judge(os.path.join(work, "h"), h):
        print("   head     ", o)
