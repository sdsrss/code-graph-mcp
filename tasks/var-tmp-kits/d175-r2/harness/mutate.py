#!/usr/bin/env python3
"""Apply one mutation at a time to the mutation copy, run `cargo test --lib`, restore.
usage: mutate.py [name ...]"""
import os, sys, subprocess, hashlib, re, shutil, time

SRC = "/var/tmp/d175-r2/mut/src/indexer/pipeline/index_files.rs"
PRISTINE = "/var/tmp/d175-r2/head/src/indexer/pipeline/index_files.rs"
LOGDIR = "/var/tmp/d175-r2/mutlogs"
ENV = dict(os.environ, CARGO_TARGET_DIR="/var/tmp/d175-r2/target-test")

ORIG_ML = '''                    let module_level = src_lang == "python"
                        && relation.as_str() == REL_IMPORTS
                        && found.iter().any(|(_, _, ty)| *ty != "method");'''
ORIG_FILTER = '''.filter(|(_, _, ty)| !module_level || *ty != "method")'''

def ml(lang, rel, quant="any"):
    return (f'                    let module_level = {lang}\n'
            f'                        && {rel}\n'
            f'                        && found.iter().{quant}(|(_, _, ty)| *ty != "method");')

MUTS = {
    "M1_revert_to_bcba08f9": (ORIG_ML, '''                    let module_level = src_lang == "python"
                        && found.iter().any(|(_, _, ty)| *ty != "method");'''),
    "M2_relation_inverted": (ORIG_ML, ml('src_lang == "python"', 'relation.as_str() != REL_IMPORTS')),
    "M3_filter_never_fires": (ORIG_ML, ml('src_lang == "no-such-language"', 'relation.as_str() == REL_IMPORTS')),
    "M4_any_language": (ORIG_ML, ml('!src_lang.is_empty()', 'relation.as_str() == REL_IMPORTS')),
    "M6_imports_and_references": (ORIG_ML, ml('src_lang == "python"',
        '(relation.as_str() == REL_IMPORTS || relation.as_str() == REL_REFERENCES)')),
    "M7_keep_only_methods": (ORIG_FILTER, '''.filter(|(_, _, ty)| !module_level || *ty == "method")'''),
    "M8_all_but_calls": (ORIG_ML, ml('src_lang == "python"', 'relation.as_str() != REL_CALLS')),
    "M9_calls_only": (ORIG_ML, ml('src_lang == "python"', 'relation.as_str() == REL_CALLS')),
}

def md5(p):
    return hashlib.md5(open(p, "rb").read()).hexdigest()

def main():
    os.makedirs(LOGDIR, exist_ok=True)
    names = sys.argv[1:] or list(MUTS)
    assert md5(SRC) == md5(PRISTINE), "mutation copy is not pristine"
    for name in names:
        old, new = MUTS[name]
        text = open(SRC).read()
        assert text.count(old) == 1, f"{name}: target found {text.count(old)} times"
        open(SRC, "w").write(text.replace(old, new))
        t0 = time.time()
        log = os.path.join(LOGDIR, name + ".log")
        with open(log, "w") as f:
            p = subprocess.run(["cargo", "test", "--lib"], cwd="/var/tmp/d175-r2/mut", env=ENV,
                               stdout=f, stderr=subprocess.STDOUT)
        out = open(log).read()
        shutil.copyfile(PRISTINE, SRC)
        assert md5(SRC) == md5(PRISTINE)
        res = re.findall(r"^test result: .*$", out, re.M)
        failed = sorted(set(re.findall(r"^    (\S+)$", out.split("failures:")[-1], re.M))) if "failures:" in out else []
        compile_err = "error[" in out or "could not compile" in out
        print(f"{name}: exit={p.returncode} {'COMPILE-ERROR ' if compile_err else ''}{res[-1] if res else ''} "
              f"({time.time()-t0:.0f}s) failed={failed}", flush=True)

if __name__ == "__main__":
    main()
