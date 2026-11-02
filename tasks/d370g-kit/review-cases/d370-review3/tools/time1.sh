#!/usr/bin/env bash
# time1.sh BIN KIND K [TIMEOUT]: index one generated file, print wall/user seconds
R=/var/tmp/d370-review3; b=$1; kind=$2; k=$3; to=${4:-600}
d="$R/perf/$kind-$k-$b"; rm -rf "${d:?}"; mkdir -p "$d"
python3 -I "$R/tools/gen.py" "$kind" "$k" > "$d/m.py"
cd "$d" && git init -q
s=$(date +%s.%N)
/usr/bin/time -f "%U %S %M" -o "$d.time" timeout "$to" env HOME=$R/home CODE_GRAPH_NO_UPDATE_CHECK=1 CODE_GRAPH_NO_AUTO_UPDATE=1 "$R/bin/$b" incremental-index --no-embed >"$d.out" 2>"$d.err"
rc=$?
e=$(date +%s.%N)
printf "%-10s %-6s k=%-7s bytes=%-8s rc=%-3s wall=%.2fs user/sys/maxrssKB=%s\n" "$b" "$kind" "$k" "$(stat -c %s "$d/m.py")" "$rc" "$(echo "$e - $s" | bc)" "$(cat "$d.time" | tail -n 1)"
