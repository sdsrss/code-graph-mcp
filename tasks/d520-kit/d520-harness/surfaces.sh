#!/bin/bash
# surfaces.sh <bin> <indexed dir> <out dir>: run user-facing CLI surfaces, one file each
B="$1"; D="$2"; O="$3"; mkdir -p "$O"
export HOME=/var/tmp/d520/home CODE_GRAPH_NO_AUTO_UPDATE=1 CODE_GRAPH_NO_UPDATE_CHECK=1
cd "$D" || exit 1
run() { local n="$1"; shift; "$B" "$@" > "$O/$n" 2>&1; }
run refs_Flask refs Flask
run refs_url_for refs url_for
run impact_Flask impact Flask
run impact_url_for impact url_for
run callers_url_for callgraph url_for --direction callers
run callers_Flask callgraph Flask --direction callers
run callers_make_response callgraph make_response --direction callers
run dead dead-code
run cycles cycles
run map map
run deps_app deps src/flask/app.py
run deps_test deps tests/test_config.py
run affected_app affected src/flask/app.py
run surprising surprising --limit 30
run health health-check
run report report --json
