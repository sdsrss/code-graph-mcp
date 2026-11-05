#!/bin/bash
# edges with metadata (no 'contains'); node ids excluded
db="$1/.code-graph/index.db"
sqlite3 -readonly "file:$db?mode=ro" "SELECT COALESCE(sf.path,'?')||':'||s.name||'@'||s.start_line||' -'||e.relation||'-> '||COALESCE(tf.path,'?')||':'||t.name||'@'||t.start_line||':'||t.type||' '||e.confidence||' '||COALESCE(e.metadata,'') FROM edges e JOIN nodes s ON s.id=e.source_id JOIN nodes t ON t.id=e.target_id LEFT JOIN files sf ON sf.id=s.file_id LEFT JOIN files tf ON tf.id=t.file_id WHERE e.relation NOT IN ('contains')" | sort
