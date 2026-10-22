#!/usr/bin/env bash
# dump.sh <db> — every edge, one line each, sorted (multiset)
sqlite3 -separator '|' "$1" "SELECT fs.path, COALESCE(ns.qualified_name, ns.name), ns.start_line, e.relation, ft.path, COALESCE(nt.qualified_name, nt.name), nt.start_line, COALESCE(e.confidence,'-'), COALESCE(e.metadata,'-') FROM edges e JOIN nodes ns ON ns.id=e.source_id JOIN files fs ON fs.id=ns.file_id JOIN nodes nt ON nt.id=e.target_id JOIN files ft ON ft.id=nt.file_id" | LC_ALL=C sort
