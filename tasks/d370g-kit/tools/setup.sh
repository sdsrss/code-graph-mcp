#!/usr/bin/env bash
set -u
W=/var/tmp/d370
cd $W/scip-tools && npm ci --no-audit --no-fund >$W/out/npm.log 2>&1; echo "npm ci exit $?"
curl -sLo $W/scip-tools/node_modules/.bin/scip-clang https://github.com/sourcegraph/scip-clang/releases/download/v0.4.0/scip-clang-x86_64-linux; echo "curl exit $?"
chmod +x $W/scip-tools/node_modules/.bin/scip-clang
clone() { # name repo ref commit
  local d=$W/corpora/$1
  [ -d "$d" ] && return
  git -c advice.detachedHead=false clone -q --depth 1 --branch "$3" "https://github.com/$2.git" "$d" || { echo "clone $1 failed"; return; }
  echo "$1 $(git -C $d rev-parse HEAD) (want $4)"
}
clone hono honojs/hono v4.6.14 4eb101dd03e66e20dfda86e6e5ebce8045d61d05
clone flask pallets/flask 3.1.0 ab8149664182b662453a563161aa89013c806dc9
clone leveldb google/leveldb 1.23 99b3c03b3284f5886f9ef9a4ef703d57373e61be
git -C $W/corpora/leveldb submodule update -q --init --depth 1 && echo "third_party/" >>$W/corpora/leveldb/.git/info/exclude
clone cobra spf13/cobra v1.8.1 x
clone gin gin-gonic/gin v1.10.0 x
echo setup-done
