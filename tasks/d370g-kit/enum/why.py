#!/usr/bin/env python3
"""why.py TRUTH_JSON HEAD_DIR: classify HEAD's WRONG and REFUSED files.

WRONG: by the patch the file applies (setattr / alias are outside the rule).
REFUSED: whether the class CPython refused is the call's class or one of its
ancestors in the file ("self"), or another class of the module ("other").
"""
import collections, json, os, re, sqlite3, subprocess, sys
sys.path.insert(0, os.path.dirname(__file__))
from check import edges, tag_of

truth = json.load(open(sys.argv[1])); head_dir = sys.argv[2]
db = sqlite3.connect(f"file:{head_dir}/.code-graph/index.db?mode=ro", uri=True)
ext_root = os.path.dirname(os.path.abspath(head_dir))
wrong, refused = collections.Counter(), collections.Counter()
examples = collections.defaultdict(list)
for name, t in sorted(truth.items()):
    path = os.path.join(head_dir, name); text = open(path).read(); lines = text.split("\n")
    he, _ = edges(db, name)
    own = [e for e in he if '"own"' in e[3]]
    if not own:
        continue
    ran = t["ran"][0] if t["ran"] else None
    tags = [tag_of(lines, e[1], e[2]) for e in own]
    if t["import_error"]:
        # The failing line, from a traceback.
        r = subprocess.run([sys.executable, "-I", "-c",
            "import runpy,sys,traceback\nsys.path.insert(0,sys.argv[2])\ntry: runpy.run_path(sys.argv[1])\n"
            "except BaseException:\n tb=traceback.extract_tb(sys.exc_info()[2])\n print([f.lineno for f in tb if f.filename==sys.argv[1]][-1])",
            path, ext_root], capture_output=True, text=True)
        line = int(r.stdout.strip() or 0)
        m = re.match(r"\s*class (\w+)", lines[line - 1]) if line else None
        failing = m.group(1) if m else "?"
        target = re.search(r"return (\w+)\.m\(None\)", text).group(1)
        # target's in-file ancestors by name (crude: transitive over class lines)
        # Ancestors through bases defined above the class naming them (a name
        # defined further down is still extstar's class when the statement runs).
        order = [c for c in re.findall(r"^\s*class (\w+)", text, re.M)]
        bases = {c: re.findall(r"\b(K\d|S\d+|G\d+)\b", b) for c, b in re.findall(r"^\s*class (\w+)\(([^)]*)\)", text, re.M)}
        anc, todo = set(), [target]
        while todo:
            c = todo.pop()
            if c in anc: continue
            anc.add(c)
            todo += [b for b in bases.get(c, []) if b in order and c in order and order.index(b) < order.index(c)]
        kind = "self" if failing in anc else "other"
        reason = re.sub(r"for bases .*|class \w+", "", t["import_error"])[:60]
        refused[(kind, reason)] += 1
        if kind == "self" and len(examples["self"]) < 40:
            examples["self"].append(f"{name}: failing={failing} target={target} {t['import_error'][:90]}")
    elif not (len(own) == 1 and tags[0] == ran):
        patch = ("setattr" if re.search(r"^setattr\(", text, re.M) else
                 "alias" if re.search(r"^_V\.m = ", text, re.M) else "?")
        wrong[patch] += 1
        if patch == "?" and len(examples["wrong?"]) < 6:
            examples["wrong?"].append(f"{name}: ran={t['ran'][:2]} exc={t['exc']} own={tags}")
print("WRONG by patch:", dict(wrong))
for (k, reason), n in sorted(refused.items()):
    print(f"REFUSED {k:5} {n:4}  {reason}")
for k, ex in examples.items():
    print("---", k)
    for e in ex: print("  ", e)
