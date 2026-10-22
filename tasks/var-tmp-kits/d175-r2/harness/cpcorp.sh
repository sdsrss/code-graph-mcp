#!/bin/bash
# cpcorp.sh SRC DEST : copy a corpus without its .code-graph
set -e
S=$1; D=$2
rm -rf "${D:?}"; mkdir -p "$D"
tar -C "$S" --exclude=./.code-graph -cf - . | tar -C "$D" -xf -
