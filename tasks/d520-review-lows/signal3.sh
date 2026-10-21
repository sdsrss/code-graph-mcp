#!/bin/bash
# signal3.sh <published-binary>: the release's change through the published binary.
# (1) a11: `m.X()` and `X()` unresolved, then m.py gains X: incremental == fresh.
# (2) flask: rename src/flask/sansio/blueprints.py: incremental == fresh.
# (3) flask full index: edges byte-equal to the source build cgm-fix2.
set -u
B="$1"; W=/var/tmp/d521/sig3
export HOME=/var/tmp/d518r2/home CODE_GRAPH_NO_AUTO_UPDATE=1 CODE_GRAPH_NO_UPDATE_CHECK=1
Q="select coalesce(sf.path,'?'), sn.name, coalesce(tf.path,'?'), tn.name, e.relation, coalesce(e.metadata,''), e.confidence from edges e join nodes sn on sn.id=e.source_id left join files sf on sf.id=sn.file_id join nodes tn on tn.id=e.target_id left join files tf on tf.id=tn.file_id order by 1,2,3,4,5,6,7"
dumpe() { sqlite3 "file:$1/.code-graph/index.db?mode=ro" "$Q"; }
rm -rf "${W:?}"; mkdir -p "$W"
# (1)
mkdir -p "$W/a/inc" "$W/a/fresh"
for d in inc fresh; do (cd "$W/a/$d" && git init -q); done
printf 'def other():\n    pass\n' > "$W/a/inc/m.py"; printf 'import m\n\ndef f():\n    m.X()\n    X()\n' > "$W/a/inc/app.py"
(cd "$W/a/inc" && "$B" incremental-index --no-embed >/dev/null 2>&1)
sleep 1; printf 'def other():\n    pass\n\ndef X():\n    pass\n' > "$W/a/inc/m.py"
(cd "$W/a/inc" && "$B" incremental-index --no-embed >/dev/null 2>&1)
cp "$W/a/inc/m.py" "$W/a/inc/app.py" "$W/a/fresh/"; (cd "$W/a/fresh" && "$B" incremental-index --no-embed >/dev/null 2>&1)
if [ "$(dumpe "$W/a/inc")" = "$(dumpe "$W/a/fresh")" ]; then echo "a11: incremental == fresh ($(dumpe "$W/a/inc" | grep -c calls) calls)"; else echo "a11: DIFF"; diff <(dumpe "$W/a/inc") <(dumpe "$W/a/fresh"); fi
# (2)
mkdir -p "$W/f/inc" "$W/f/fresh"; git -C /var/tmp/d446/corpora/flask archive HEAD | tar -x -C "$W/f/inc"; (cd "$W/f/inc" && git init -q && "$B" incremental-index --no-embed >/dev/null 2>&1)
mv "$W/f/inc/src/flask/sansio/blueprints.py" "$W/f/inc/src/flask/sansio/blueprints_renamed.py"; (cd "$W/f/inc" && "$B" incremental-index --no-embed >/dev/null 2>&1)
(cd "$W/f/inc" && tar --exclude=./.code-graph --exclude=./.git -cf - .) | tar -x -C "$W/f/fresh"; (cd "$W/f/fresh" && git init -q && "$B" incremental-index --no-embed >/dev/null 2>&1)
if [ "$(dumpe "$W/f/inc")" = "$(dumpe "$W/f/fresh")" ]; then echo "flask rename: incremental == fresh ($(dumpe "$W/f/inc" | grep -c 'Blueprint.register_blueprint\|register_blueprint') register_blueprint edges)"; else echo "flask rename: DIFF $(diff <(dumpe "$W/f/inc") <(dumpe "$W/f/fresh") | grep -c '^[<>]')"; fi
# (3)
for a in pub src; do mkdir -p "$W/full-$a"; git -C /var/tmp/d446/corpora/flask archive HEAD | tar -x -C "$W/full-$a"; done
(cd "$W/full-pub" && git init -q && "$B" incremental-index --no-embed >/dev/null 2>&1)
(cd "$W/full-src" && git init -q && /var/tmp/d521/bin/cgm-fix2 incremental-index --no-embed >/dev/null 2>&1)
echo "flask full: published $(dumpe "$W/full-pub" | md5sum | cut -c1-12) source-build $(dumpe "$W/full-src" | md5sum | cut -c1-12) edges $(dumpe "$W/full-pub" | wc -l)"
rm -rf "${W:?}"
