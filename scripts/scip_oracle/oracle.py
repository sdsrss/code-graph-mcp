#!/usr/bin/env python3
"""Score code-graph's `calls` edges against a SCIP index: rust-analyzer for Rust,
scip-typescript for JavaScript/TypeScript, scip-python for Python, scip-clang for C++.

The oracle's gold set is every *call-shaped* reference in the SCIP index whose
callee is a function defined in this repo, attributed to the innermost function
definition that encloses it. Both ends are then aligned to index.db nodes by
(file, name, definition line inside the node's line span). Our edges are judged
only when both endpoints align, so every loss is counted in `funnel` instead of
silently shrinking the denominators.

    precision(tier)  = judged edges of that tier that are in gold / judged edges of that tier
    recall(floor)    = gold pairs we have at that tier or better / gold pairs

Usage: see README.md (run.sh builds the SCIP index and calls this).
"""

import argparse
import json
import os
import re
import sqlite3
import sys
from collections import Counter, defaultdict

import scip_decode

TIERS = ("extracted", "inferred", "ambiguous")
# --language -> the index's files.language values it scores.
LANGUAGES = {
    "rust": ("rust",),
    "javascript": ("javascript", "typescript"),
    "python": ("python",),
    "cpp": ("cpp", "c"),  # a `.h` header is indexed as c
}
# Languages whose index credits a call in no named function to the file's <module> node.
MODULE_CALLERS = ("javascript", "python")
# Languages with call-site type arguments written without `::`: `f<T>(x)`.
TYPE_ARG_CALLS = ("javascript", "cpp")
TIER_RANK = {t: i for i, t in enumerate(TIERS)}
AMBIGUOUS = "<ambiguous definition>"  # encloser key of a definition whose symbol has several bodies
CALLABLE_NODE_TYPES = ("function", "method")
CLASS_NODE_TYPES = ("class", "struct", "interface")
UTF8_OFFSETS = 1  # scip.proto PositionEncoding.UTF8CodeUnitOffsetFromLineStart
UTF16_OFFSETS = 2
# rust-analyzer declares UTF-8. scip-typescript and scip-python leave the field
# unset (0) and count UTF-16 units, as TypeScript and pyright do: measured on this
# repo, reading their columns as UTF-16 slices 0/787 and 1/25 names wrong on
# non-ASCII lines, as UTF-8 22/787 and 8/25. So 0 means UTF-16 here, except for
# scip-clang, which also leaves it unset but counts bytes (measured: `callee` after
# `"é😀"` at column 48, the byte offset, not 45, the UTF-16 one).

_CALLABLE_DESCRIPTOR = re.compile(r"\([^()]*\)\.$")
# `<owner>#<name>(<disambiguator>).`: a method, and a constructor when <name> is
# scip-typescript's `<constructor>` or, in C++, the owner's own name.
_METHOD_OF = re.compile(r"^(.*[/#\s]`?([^/#`\s]+)`?#)`?([^/#`]+)`?\([^()]*\)\.$")
# A line that binds names by importing them. scip-python turns an import it cannot
# resolve (a module found via `sys.path.insert`) into locals; scip-typescript does
# the same for a `require()` it cannot follow. A call through such a local has an
# unknown callee, so neither side of it can be judged.
_IMPORT_LINE = re.compile(rb"^\s*(import|from)\s|require\(")


def is_callable_symbol(symbol):
    """A SCIP method descriptor ends in `(<disambiguator>).`; locals never do."""
    return not symbol.startswith("local ") and bool(_CALLABLE_DESCRIPTOR.search(symbol))


def maybe_callable_symbol(symbol):
    """A local or a term (`name.`) can hold a function in JS/Python: `const f = () => {}`
    is the term `f.`, a nested `function inner()` is `local N`. Whether it does is
    decided by alignment to a function node, not by the symbol."""
    return symbol.startswith("local ") or (symbol.endswith(".") and not symbol.endswith(")."))


def constructor_owner(symbol):
    """The class symbol (`...Box#`) whose constructor `symbol` is, else None."""
    m = _METHOD_OF.match(symbol)
    if m and m.group(3) in ("<constructor>", m.group(2)):
        return m.group(1)
    return None


_CPP_DTOR = re.compile(rb"~\s*\w+")
_CPP_OPERATOR = re.compile(rb"operator\s*(\(\s*\)|\[\s*\]|[^\s(]+)")
_GTEST = re.compile(rb"TEST(?:_F|_P)?\s*\(\s*(\w+)\s*,\s*(\w+)\s*\)")


def cpp_definition_name(line, start, name, symbol):
    """The index's name for a C++ definition whose SCIP name range is `name`.
    scip-clang's range stops at `~` and at `operator`, and a gtest body's is the
    TEST_F macro; the index says `~Impl`, `operator==` and `Suite.Case`."""
    rest = line[start:]
    if name == "~" and (m := _CPP_DTOR.match(rest)):
        return re.sub(rb"\s", b"", m.group(0)).decode()
    if name == "operator" and (m := _CPP_OPERATOR.match(rest)):
        return "operator" + re.sub(rb"\s", b"", m.group(1)).decode()
    if name.startswith("TEST") and "#TestBody(" in symbol and (m := _GTEST.match(rest)):
        return f"{m.group(1).decode()}.{m.group(2).decode()}"
    return name


def utf16_to_byte(line, col):
    """Byte offset in the UTF-8 `line` of the UTF-16 code unit `col`."""
    units = line.decode("utf-8", "replace").encode("utf-16-le")[:col * 2]
    return len(units.decode("utf-16-le", "replace").encode("utf-8"))


def is_call_site(line, end, type_args=False):
    """True when the name ending at byte `end` is followed by `(`, or `::<...>(`, or
    (with `type_args`, for TypeScript and C++) `<...>(`."""
    rest = line[end:].lstrip()
    if rest.startswith(b"?.("):  # JS optional call `f?.()`
        return True
    if rest.startswith(b"::<") or (type_args and rest.startswith(b"<")):
        depth, i = 0, 2 if rest.startswith(b"::") else 0
        while i < len(rest):
            c = rest[i:i + 1]
            if c == b"<":
                depth += 1
            elif c == b">":
                depth -= 1
                if depth == 0:
                    break
            i += 1
        else:
            return False
        rest = rest[i + 1:].lstrip()
    return rest.startswith(b"(")


def _shape(line, start, language="rust"):
    before = line[:start].rstrip()
    if before.endswith(b".") or before.endswith(b"->"):
        # Rust and C++ `x.f()` is a method call; in JS/Python `a.f()` is just as
        # often a module or namespace member, so it is not called a method there.
        return "method" if language in ("rust", "cpp") else "member"
    if before.endswith(b"::"):
        return "path"
    return "bare"


def _contains(rng, line, col):
    return (rng[0], rng[1]) <= (line, col) <= (rng[2], rng[3])


def _read_lines(root, rel):
    with open(os.path.join(root, rel), "rb") as f:
        return f.read().split(b"\n")


def _to_bytes(lines, rng):
    return (rng[0], utf16_to_byte(lines[rng[0]], rng[1]), rng[2], utf16_to_byte(lines[rng[2]], rng[3]))


def evaluate(docs, db_path, root, samples=15, language="rust", dump_judged=False):
    langs = LANGUAGES[language]
    marks = ",".join("?" * len(langs))
    con = sqlite3.connect(f"file:{db_path}?mode=ro", uri=True)
    lang_files = {p for (p,) in con.execute(f"SELECT path FROM files WHERE language IN ({marks})", langs)}
    scip_paths = {d.relative_path for d in docs}
    docs = [d for d in docs if d.relative_path in lang_files]

    node_meta = {}
    nodes_by_file = defaultdict(list)
    q = (
        "SELECT n.id, n.name, n.start_line, n.end_line, f.path, n.type FROM nodes n "
        f"JOIN files f ON f.id = n.file_id WHERE f.language IN ({marks})"
    )
    node_end = {}
    module_of = {}  # path -> the file's <module> node, where the index puts top-level calls
    classes_by_file = defaultdict(list)
    for nid, name, start, end, path, ntype in con.execute(q, langs):
        node_meta[nid] = (name, path, start)
        node_end[nid] = end
        if ntype == "module" and language in MODULE_CALLERS:
            module_of[path] = nid
        if ntype in CALLABLE_NODE_TYPES:
            nodes_by_file[path].append((nid, name, start, end))
        elif ntype in CLASS_NODE_TYPES:
            classes_by_file[path].append((nid, name, start, end))

    funnel = Counter()
    import_locals = set()
    # scip-typescript: `require('./b').f()` references the export-object property
    # `f0:`, not the function. In `module.exports = { f }` the property's definition
    # and the function's reference share one range; that pair is the alias.
    alias = {}
    occ_starts = defaultdict(set)  # path -> (line, byte col) of every occurrence
    funnel[f"{language}_files_in_index_not_in_scip"] = len(lang_files - scip_paths)
    lines = {}
    parents = defaultdict(set)  # symbol -> the symbols it overrides or implements
    unset_is_utf16 = language != "cpp"
    for d in docs:
        if d.position_encoding not in (0, UTF8_OFFSETS, UTF16_OFFSETS):
            raise SystemExit(f"{d.relative_path}: unsupported SCIP position encoding {d.position_encoding}")
        for child, ps in d.implements.items():
            parents[child].update(ps)
        # scip-clang repeats a header's occurrences once per translation unit.
        seen = set()
        d.occurrences = [o for o in d.occurrences
                         if not ((k := (o.range, o.symbol, o.roles, o.enclosing_range)) in seen or seen.add(k))]
        lines[d.relative_path] = ls = _read_lines(root, d.relative_path)
        if d.position_encoding == UTF16_OFFSETS or (d.position_encoding == 0 and unset_is_utf16):
            for o in d.occurrences:
                o.range = _to_bytes(ls, o.range)
                if o.enclosing_range:
                    o.enclosing_range = _to_bytes(ls, o.enclosing_range)
        # A local is only unique inside its document.
        for o in d.occurrences:
            if o.symbol.startswith("local "):
                o.symbol = f"local {d.relative_path} {o.symbol[6:]}"
                # Any role: scip-python marks the import site a read, not a definition.
                if language != "rust" and _IMPORT_LINE.search(ls[o.range[0]]):
                    import_locals.add(o.symbol)
        if language != "rust":
            defined_at = {o.range: o.symbol for o in d.occurrences if o.is_definition}
            for o in d.occurrences:
                occ_starts[d.relative_path].add((o.range[0], o.range[1]))
                target = defined_at.get(o.range)
                if not o.is_definition and target and target != o.symbol:
                    alias.setdefault(target, o.symbol)

    def text(path, rng):
        return lines[path][rng[0]][rng[1]:rng[3]].decode("utf-8", "replace")

    # Pass 1: callable definitions -> our nodes. Outside Rust a local or a term is a
    # candidate too; it stays only if it aligns to a function node (it held one).
    maybe = language != "rust"
    defs = defaultdict(list)
    type_defs = {}  # class symbol -> (path, range) of its definition, for constructors
    for d in docs:
        for o in d.occurrences:
            if o.is_definition and (is_callable_symbol(o.symbol)
                                    or (maybe and maybe_callable_symbol(o.symbol))):
                defs[o.symbol].append((d.relative_path, o.range, o.enclosing_range))
            elif o.is_definition and o.symbol.endswith("#"):
                type_defs.setdefault(o.symbol, (d.relative_path, o.range))

    def key_of(sym, path):
        """Definition key a reference in `path` resolves to, or None if ambiguous.

        rust-analyzer names every tests/*.rs crate with the same package prefix,
        so a helper defined in two test files is ONE symbol. A crate can only call
        its own copy, so a reference binds to the definition in its own file."""
        sites = defs[sym]
        if len(sites) == 1:
            return sym
        here = sum(1 for p, _r, _e in sites if p == path)
        return (path, sym) if here == 1 else None

    # Drop the candidates that never held a function before anything counts them.
    for sym in [s for s in defs if not is_callable_symbol(s)]:
        if not any(n[1] == text(p, r) and n[2] <= r[0] + 1 <= n[3]
                   for p, r, _e in defs[sym] for n in nodes_by_file[p]):
            del defs[sym]

    sym_to_node = {}
    unmapped = []
    enclosers = defaultdict(list)  # path -> [(enclosing_range, key)]
    for sym, sites in defs.items():
        if len(sites) > 1:
            funnel["colliding_definitions"] += 1
        for path, rng, enc in sites:
            key = key_of(sym, path)
            name, line1 = text(path, rng), rng[0] + 1
            if language == "cpp":
                name = cpp_definition_name(lines[path][rng[0]], rng[1], name, sym)
            cands = [n for n in nodes_by_file[path] if n[1] == name and n[2] <= line1 <= n[3]]
            if key is None:
                # Overload stubs and their implementation (`@overload`, TS overload
                # signatures): one symbol, several bodies. A call in one of them has
                # no known caller, and is not the enclosing scope's either.
                funnel["ambiguous_definitions_in_one_file"] += 1
                for node in cands[:1] if not enc else ():
                    enclosers[path].append(((node[2] - 1, 0, node[3] - 1, 1 << 30), AMBIGUOUS))
                if enc:
                    enclosers[path].append((enc, AMBIGUOUS))
                continue
            if enc:
                enclosers[path].append((enc, key))
            if cands:
                node = min(cands, key=lambda n: n[3] - n[2])
                sym_to_node[key] = node[0]
                if not enc:
                    # scip-typescript gives a nested function (a local) no
                    # enclosing_range; its node's line span stands in, so calls
                    # inside it are not credited to the outer function.
                    funnel["enclosing_from_index_span"] += 1
                    enclosers[path].append(((node[2] - 1, 0, node[3] - 1, 1 << 30), key))
            else:
                funnel["unmapped_definitions"] += 1
                unmapped.append(f"{path}:{line1} {name}")

    def enclosing_key(path, sl, sc):
        inner = [(enc, s) for enc, s in enclosers[path] if _contains(enc, sl, sc)]
        return max(inner, key=lambda e: (e[0][0], e[0][1]))[1] if inner else None

    def caller_of(path, sl, sc):
        """(enclosing function key, caller node). In JS/Python a call in no named
        function (top level, or an anonymous callback there: most JS test code)
        belongs to the file's <module> node, as the index attributes it."""
        key = enclosing_key(path, sl, sc)
        if key is None:
            return None, module_of.get(path)
        return key, sym_to_node.get(key)

    # A constructor call `new Box()` is resolved by the index to the class node.
    class_of = {}  # constructor node -> its class node
    for key, node in sym_to_node.items():
        owner = constructor_owner(key if isinstance(key, str) else key[1])
        if owner not in type_defs:
            continue
        path, rng = type_defs[owner]
        name, line1 = text(path, rng), rng[0] + 1
        cands = [n for n in classes_by_file[path] if n[1] == name and n[2] <= line1 <= n[3]]
        if cands:
            class_of[node] = min(cands, key=lambda n: n[3] - n[2])[0]

    # Virtual dispatch: a call to a method may run any override of it, which the
    # index may name instead. SCIP records overrides as is_implementation.
    children = defaultdict(set)
    for child, ps in parents.items():
        for p in ps:
            children[p].add(child)

    def overriding_nodes(sym):
        out, stack, seen = set(), [sym], {sym}
        while stack:
            for c in children[stack.pop()]:
                if c not in seen:
                    seen.add(c)
                    stack.append(c)
                    if len(defs.get(c, ())) == 1 and c in sym_to_node:
                        out.add(sym_to_node[c])
        return out

    # Pass 2: call-shaped references -> gold (caller node, callee node) pairs.
    type_args = language in TYPE_ARG_CALLS
    gold = {}
    stand_ins = defaultdict(dict)  # gold pair -> {edge that also counts as it: why}
    dispatch_only = set()  # edges to an override of a method that has no body to be gold
    called_names = defaultdict(set)  # caller node -> every name it calls, in-repo or not
    opaque_names = defaultdict(set)  # caller node -> names it calls through a local of unknown binding
    for d in docs:
        path = d.relative_path
        for o in d.occurrences:
            sym = o.symbol
            if not o.is_definition and sym not in defs and alias.get(sym) in defs:
                sym = alias[sym]
            # A call to a symbol that is neither a function defined here nor a
            # method descriptor: a parameter, a const bound to an expression, a
            # destructured factory result, an export property, a class, an
            # unresolved import. What runs is unknown to SCIP, so the call is not
            # gold, and an edge from this caller to that name is not judged.
            if (language != "rust" and not o.is_definition
                    and sym not in defs and not is_callable_symbol(sym)):
                sl, sc, el, ec = o.range
                if sl == el and is_call_site(lines[path][sl], ec, type_args):
                    funnel["call_sites_to_unresolved_import" if sym in import_locals
                           else "call_sites_to_non_function_symbol"] += 1
                    caller = caller_of(path, sl, sc)[1]
                    if caller is not None:
                        opaque_names[caller].add(text(path, o.range))
                continue
            if o.is_definition or not (is_callable_symbol(sym) or sym in defs):
                continue
            sl, sc, el, ec = o.range
            if sl != el or not is_call_site(lines[path][sl], ec, type_args):
                funnel["non_call_references"] += 1
                continue
            funnel["call_sites"] += 1
            if sym != o.symbol:
                funnel["call_sites_via_export_alias"] += 1
            caller_sym, caller = caller_of(path, sl, sc)
            if caller_sym is None and caller is not None:
                funnel["call_sites_attributed_to_module"] += 1
            if caller is not None:
                called_names[caller].add(text(path, o.range))
            if language == "cpp" and caller_sym is None:
                # Outside every function body a call-shaped C++ reference is a
                # declaration (`virtual Status Get(...) = 0;`) or an initializer.
                funnel["call_sites_without_enclosing_fn"] += 1
                continue
            callee = sym_to_node.get(key_of(sym, path)) if sym in defs else None
            over = overriding_nodes(sym) if callee is None and caller is not None else ()
            if over:
                # A pure virtual / abstract method: SCIP has no definition to be the
                # callee, but the call runs one of its overrides.
                funnel["call_sites_to_declaration_only_method"] += 1
                dispatch_only.update((caller, n) for n in over if n != caller)
            elif sym not in defs:
                funnel["call_sites_to_external"] += 1
            elif callee is None:
                funnel["call_sites_to_unmapped_callee"] += 1
            elif caller_sym == AMBIGUOUS:
                funnel["call_sites_in_ambiguous_definition"] += 1
            elif caller is None and caller_sym is None:
                funnel["call_sites_without_enclosing_fn"] += 1
            elif caller is None:
                funnel["call_sites_in_unmapped_caller"] += 1
            elif caller == callee:
                funnel["self_calls_excluded"] += 1  # index_files.rs drops self-edges by design
            else:
                pair = (caller, callee)
                shape = "constructor" if constructor_owner(sym) else _shape(lines[path][sl], sc, language)
                gold.setdefault(pair, (f"{path}:{sl + 1}", shape))
                for n in overriding_nodes(sym):
                    stand_ins[pair].setdefault((caller, n), "edges_credited_via_override")
                if callee in class_of:
                    stand_ins[pair].setdefault((caller, class_of[callee]), "edges_credited_to_constructor_class")

    # Our edges, one per (source, target) pair at its highest tier.
    ours = {}
    for s, t, conf in con.execute("SELECT source_id, target_id, confidence FROM edges WHERE relation = 'calls'"):
        if s not in node_meta:
            continue  # caller outside the language scope
        if t not in node_meta:
            funnel[f"edges_to_outside_{language}_scope"] += 1
            continue
        if (s, t) in ours:
            funnel["duplicate_pair_edges"] += 1
            if TIER_RANK[conf] >= TIER_RANK[ours[(s, t)]]:
                continue
        ours[(s, t)] = conf
    con.close()

    def untyped_call(caller, name):
        """`x.name(` inside the caller where SCIP has no occurrence at all: the
        receiver is untyped (no @types/node here), so what it calls is unknown."""
        if language == "rust":
            return False
        _n, path, start = node_meta[caller]
        if path not in lines:
            return False
        pat = re.compile(rb"\.\s*(" + re.escape(name.encode()) + rb")\s*\(")
        for i in range(start - 1, min(node_end[caller], len(lines[path]))):
            for m in pat.finditer(lines[path][i]):
                if (i, m.start(1)) not in occ_starts[path]:
                    return True
        return False

    mapped = set(sym_to_node.values()) | set(module_of.values())
    callable_nodes = {n[0] for ns in nodes_by_file.values() for n in ns}
    tiers = {t: {"judged": 0, "correct": 0} for t in TIERS}
    credited = {e: "edges_credited_via_override" for e in dispatch_only if e not in gold}
    credited.update((e, why) for alts in stand_ins.values() for e, why in alts.items() if e not in gold)
    wrong = []
    unjudged = []
    judged = []  # (tier, caller, callee, verdict) for every judged edge
    for (s, t), conf in ours.items():
        if (s, t) in credited:
            tiers[conf]["judged"] += 1
            tiers[conf]["correct"] += 1
            funnel[credited[(s, t)]] += 1
            judged.append((conf, s, t, "correct"))
            continue
        if s not in mapped or t not in mapped:
            if s in callable_nodes and t in callable_nodes:
                funnel["unjudged_edges_unmapped_function"] += 1
                unjudged.append((conf, s, t))
            else:
                funnel["unjudged_edges_non_function_endpoint"] += 1
            continue
        if (s, t) not in gold and node_meta[t][0] in opaque_names[s]:
            funnel["unjudged_edges_unknown_binding"] += 1
            continue
        if (s, t) not in gold and node_meta[t][0] not in called_names[s] and untyped_call(s, node_meta[t][0]):
            funnel["unjudged_edges_untyped_call"] += 1
            continue
        tiers[conf]["judged"] += 1
        if (s, t) in gold:
            tiers[conf]["correct"] += 1
            judged.append((conf, s, t, "correct"))
        else:
            kind = "wrong_target" if node_meta[t][0] in called_names[s] else "no_call_by_that_name"
            funnel[f"wrong_{conf}_{kind}"] += 1
            wrong.append((conf, s, t, kind))
            judged.append((conf, s, t, kind))

    def best_tier(p):
        """Rank of the best edge that stands for gold pair p, or None."""
        ranks = [TIER_RANK[ours[e]] for e in (p, *stand_ins[p]) if e in ours]
        return min(ranks) if ranks else None

    recall = {}
    for floor in TIERS:
        found = sum(1 for p in gold if (b := best_tier(p)) is not None and b <= TIER_RANK[floor])
        recall[floor] = {"found": found, "gold": len(gold)}
    by_shape = defaultdict(lambda: {"found": 0, "gold": 0})
    for p, (_site, shape) in gold.items():
        by_shape[shape]["gold"] += 1
        by_shape[shape]["found"] += best_tier(p) is not None

    def loc(n):
        name, path, line = node_meta[n]
        return name, f"{path}:{line}"

    missed = sorted((site, shape, c, e) for (c, e), (site, shape) in gold.items() if best_tier((c, e)) is None)
    return {
        "judged_edges": [
            {"tier": conf, "verdict": v, "caller": loc(s)[0], "caller_at": loc(s)[1],
             "callee": loc(t)[0], "callee_at": loc(t)[1]}
            for conf, s, t, v in sorted(judged, key=lambda j: (loc(j[1])[1], loc(j[2])[1]))
        ] if dump_judged else None,
        "gold_pairs": len(gold),
        "tiers": tiers,
        "recall_at_floor": recall,
        "recall_by_call_shape": dict(sorted(by_shape.items())),
        "funnel": dict(sorted(funnel.items())),
        "samples": {
            "missed": [
                {"caller": loc(c)[0], "callee": loc(e)[0], "site": site, "shape": shape}
                for site, shape, c, e in missed[:samples]
            ],
            "wrong": [
                {"tier": conf, "caller": loc(s)[0], "caller_at": loc(s)[1],
                 "callee": loc(t)[0], "callee_at": loc(t)[1], "kind": kind}
                for conf, s, t, kind in sorted(wrong, key=lambda w: (TIER_RANK[w[0]], loc(w[1])[1], loc(w[2])[1]))[:samples]
            ],
            "unmapped_definitions": sorted(unmapped)[:samples],
            "unjudged_function_edges": [
                {"tier": conf, "caller": loc(s)[0], "caller_at": loc(s)[1],
                 "callee": loc(t)[0], "callee_at": loc(t)[1]}
                for conf, s, t in sorted(unjudged, key=lambda u: (loc(u[1])[1], loc(u[2])[1]))[:samples]
            ],
        },
    }


def _pct(num, den):
    return f"{num / den:.1%}" if den else "n/a"


def render(r):
    out = [f"gold call pairs: {r['gold_pairs']}", "", "precision by tier (judged edges only):"]
    for t, v in r["tiers"].items():
        out.append(f"  {t:<10} {v['correct']:>5}/{v['judged']:<5} {_pct(v['correct'], v['judged'])}")
    out.append("recall at --min-confidence floor:")
    for t, v in r["recall_at_floor"].items():
        out.append(f"  {t:<10} {v['found']:>5}/{v['gold']:<5} {_pct(v['found'], v['gold'])}")
    out.append("recall by call shape (any tier):")
    for s, v in r["recall_by_call_shape"].items():
        out.append(f"  {s:<10} {v['found']:>5}/{v['gold']:<5} {_pct(v['found'], v['gold'])}")
    out.append("funnel (every loss, so no denominator shrinks silently):")
    out += [f"  {k}: {v}" for k, v in r["funnel"].items()]
    for kind, rows in r["samples"].items():
        out.append(f"samples: {kind}")
        out += [f"  {row if isinstance(row, str) else json.dumps(row)}" for row in rows]
    return "\n".join(out)


def main(argv=None):
    ap = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    ap.add_argument("--scip", required=True, help="index.scip from rust-analyzer / scip-typescript / scip-python")
    ap.add_argument("--language", choices=sorted(LANGUAGES), default="rust",
                    help="which of the index's languages to score (default rust)")
    ap.add_argument("--root", default=".", help="repo root the SCIP paths are relative to")
    ap.add_argument("--db", help="code-graph index (default: <root>/.code-graph/index.db)")
    ap.add_argument("--samples", type=int, default=15)
    ap.add_argument("--json-out", help="also write the full report as JSON here")
    ap.add_argument("--dump-judged", help="write every judged edge with its verdict (JSON) here")
    a = ap.parse_args(argv)
    db = a.db or os.path.join(a.root, ".code-graph", "index.db")
    r = evaluate(scip_decode.read_index(a.scip), db, a.root, a.samples, a.language, bool(a.dump_judged))
    judged = r.pop("judged_edges")
    if a.dump_judged:
        with open(a.dump_judged, "w") as f:
            json.dump(judged, f, indent=1)
            f.write("\n")
    print(render(r))
    if a.json_out:
        with open(a.json_out, "w") as f:
            json.dump(r, f, indent=2, sort_keys=True)
            f.write("\n")
    return 0


if __name__ == "__main__":
    sys.exit(main())
