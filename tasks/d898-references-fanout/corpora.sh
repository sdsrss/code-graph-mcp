#!/usr/bin/env bash
# corpora.sh <dir>: the five full-index corpora of the D#901/D#898 checks, each a
# git repository with one commit: the Python standard library, nltk, flask at a
# fixed commit, this repository's HEAD and daagu's 10b3ce5d.
set -euo pipefail
D="${1:?usage: corpora.sh <dir>}"
mkdir -p "$D"
cd "$D"
commit() { git -C "$1" init -q && git -C "$1" add -A && git -C "$1" -c user.email=x@x -c user.name=x commit -qm c; }
mkdir std && tar -C /usr/lib/python3.14 --exclude=site-packages --exclude=dist-packages --exclude=test \
  --exclude=tests --exclude=__pycache__ --exclude=lib-dynload --exclude=idlelib --exclude=turtledemo \
  --exclude='config-*' --exclude=ensurepip -cf - . | tar -C std -xf - && commit std
mkdir -p nltk/nltk && tar -C /usr/lib/python3/dist-packages/nltk --exclude=__pycache__ -cf - . | tar -C nltk/nltk -xf - && commit nltk
git clone -q https://github.com/pallets/flask flask-src
git -C flask-src checkout -q ab8149664182b662453a563161aa89013c806dc9
mkdir flask && git -C flask-src archive HEAD | tar -x -C flask && commit flask && rm -rf "${D:?}/flask-src"
mkdir cgm && git -C /home/ai/dev/code-graph-mcp archive HEAD | tar -x -C cgm && commit cgm
mkdir daagu && git -C /home/ai/dev/daagu archive 10b3ce5d | tar -x -C daagu && commit daagu
