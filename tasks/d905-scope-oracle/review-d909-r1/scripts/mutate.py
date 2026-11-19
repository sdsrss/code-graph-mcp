#!/usr/bin/env python3
"""mutate.py [ids...]: apply each mutant to /var/tmp/d909-review/mut-src, build
the lib test binary, run the full lib suite, record compile status and failing
tests, restore the file from the copy's own git."""
import os, re, subprocess, sys, json

SRC = "/var/tmp/d909-review/mut-src"
ENV = dict(os.environ, CARGO_TARGET_DIR="/var/tmp/d909-review/target-mut",
           CODE_GRAPH_NO_AUTO_UPDATE="1", CODE_GRAPH_DISABLE_MODEL_DOWNLOAD="1")
PY = "src/parser/relations/python.rs"
TS = "src/parser/treesitter.rs"
MOD = "src/parser/relations/mod.rs"
MUTANTS = {
    "M01-attr-tail-gets-position": (PY, "call_at: bare.then(|| crate::parser::treesitter::BareCall {",
                                    "call_at: Some(()).map(|_| crate::parser::treesitter::BareCall {"),
    "M02-value-ref-no-position": (PY, "call_at: crate::parser::treesitter::bare_call_at(node, \"python\"),",
                                  "call_at: None,"),
    "M03-class-scope-all-languages": (TS, "lexical_scope: (language == \"python\")\n                        .then(",
                                      "lexical_scope: (language == \"python\" || true)\n                        .then("),
    "M04-header-spans-whole-body": (PY, "headers.push((n.start_byte(), body.start_byte()));",
                                    "headers.push((n.start_byte(), body.end_byte()));"),
    "M05-marking-skips-references": (MOD, "relations.iter_mut().filter_map(|r| r.call_at.as_mut()),",
                                     "relations.iter_mut().filter(|r| r.relation == crate::domain::REL_CALLS).filter_map(|r| r.call_at.as_mut()),"),
    "M06-every-python-class-file-scoped": (TS, ".then(|| lexical_scope(&node, language))\n                        .flatten(),",
                                           ".then_some(LexicalScope::File),"),
    "M07-partition-no-minus-one": (PY, ".checked_sub(1) {", ".checked_sub(0) {"),
    "M08-headers-accumulate-across-files": (PY, "PY_DEF_HEADERS.with(|h| *h.borrow_mut() = headers);",
                                            "PY_DEF_HEADERS.with(|h| h.borrow_mut().extend(headers));"),
    "M09-headers-for-classes-not-defs": (PY, "if kind == \"function_definition\" {\n                        headers.push",
                                         "if kind == \"class_definition\" {\n                        headers.push"),
    "M10-noise-check-ignores-bare": (PY, "|| (bare && py_read_names_no_definition(*node, name))",
                                     "|| py_read_names_no_definition(*node, name)"),
    "M11-annotation-at-def-start": (PY, "Some(i) if at < h[i].1 => h[i].0.checked_sub(1).unwrap_or(usize::MAX),",
                                    "Some(i) if at < h[i].1 => h[i].0,"),
    "M12-annotation-at-header-end": (PY, "Some(i) if at < h[i].1 => h[i].0.checked_sub(1).unwrap_or(usize::MAX),",
                                     "Some(i) if at < h[i].1 => h[i].1,"),
    "M13-annotation-reaches-members": (PY, "            at: py_annotation_at(node),\n            reaches_members: false,",
                                       "            at: py_annotation_at(node),\n            reaches_members: true,"),
    "M14-decorator-position-inside-def": (PY, "call_at: crate::parser::treesitter::bare_call_at(node, \"python\"),",
                                          "call_at: if parent.kind() == \"decorator\" { parent.parent().and_then(|w| w.child_by_field_name(\"definition\")).and_then(|d| d.child_by_field_name(\"body\")).and_then(|b| crate::parser::treesitter::bare_call_at(&b, \"python\")) } else { crate::parser::treesitter::bare_call_at(node, \"python\") },"),
}


def sh(cmd, **kw):
    return subprocess.run(cmd, cwd=SRC, env=ENV, capture_output=True, text=True, **kw)


def main():
    ids = sys.argv[1:] or list(MUTANTS)
    results = {}
    for mid in ids:
        f, old, new = MUTANTS[mid]
        p = os.path.join(SRC, f)
        s = open(p).read()
        n = s.count(old)
        if n != 1:
            results[mid] = f"anchor count {n}"
            print(mid, results[mid], flush=True)
            continue
        open(p, "w").write(s.replace(old, new))
        b = sh(["cargo", "test", "--locked", "--lib", "--no-run", "--message-format=short"])
        if b.returncode:
            err = [l for l in b.stderr.splitlines() if "error" in l][:3]
            results[mid] = f"COMPILE-FAIL {err}"
        else:
            exe = re.search(r"Executable unittests src/lib.rs \((.*?)\)", b.stderr).group(1)
            t = subprocess.run([exe, "-q"], cwd=SRC, env=ENV, capture_output=True, text=True)
            failed = re.findall(r"^    (\S+)$", t.stdout, re.M)
            summary = [l for l in t.stdout.splitlines() if l.startswith("test result")]
            results[mid] = f"{'KILLED' if t.returncode else 'SURVIVED'} {summary} failed={failed[:8]}"
        print(mid, results[mid], flush=True)
        r = sh(["git", "checkout", "--", f])
        assert r.returncode == 0, r.stderr
        os.utime(p, None)
        assert sh(["git", "status", "--porcelain"]).stdout.strip() == "", "restore failed"
    json.dump(results, open("/var/tmp/d909-review/mutants-" + "-".join(ids)[:40] + ".json", "w"), indent=1)


if __name__ == "__main__":
    main()
