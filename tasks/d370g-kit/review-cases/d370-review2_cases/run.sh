#!/usr/bin/env bash
# run.sh CASE_DIR [filter] : index with base and head, print call edges of callers named t_*
set -u
D="$1"; F="${2:-t_}"
for b in base-6f4e17de head-4; do
  echo "== $b"
  /var/tmp/d370-review2/tools/idx.sh /var/tmp/d370/bin/$b "$D"
  python3 -I /var/tmp/d370-review2/tools/edges.py "$D" "$F" | grep -v "^PENDING" | sed 's/^/  /'
done
