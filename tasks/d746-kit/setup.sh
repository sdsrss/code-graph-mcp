#!/bin/bash
set -u
W=/var/tmp/agent-tmp-d746; C="$W/corpora"; cd "$C" || exit 1
clone() { # name url commit
  [ -d "$1" ] && return; git clone -q "$2" "$1.tmp" && git -C "$1.tmp" checkout -q "$3" && rm -rf "$1.tmp/.git" && mv "$1.tmp" "$1" && (cd "$1" && git init -q && git add -A && git -c user.email=a@b -c user.name=a commit -qm init) && echo "ok $1"
}
local_copy() { # name srcdir
  [ -d "$1" ] && return; mkdir "$1" && git -C "$2" archive HEAD | tar -C "$1" -xf - && (cd "$1" && git init -q && git add -A && git -c user.email=a@b -c user.name=a commit -qm init) && echo "ok $1"
}
clone hono https://github.com/honojs/hono 4eb101dd03e66e20dfda86e6e5ebce8045d61d05 &
clone express https://github.com/expressjs/express 1faf228935aa0a13111f92c28ee795be64ce3f0f &
clone flask https://github.com/pallets/flask d086db856be187255b8ec61ef409357393020f32 &
clone tokio https://github.com/tokio-rs/tokio bb7ca7507b94d01ffe0e275ddc669734ab3bf783 &
wait
for n in claude-mem-lite daagu claudemd gsd-lite llm_wiki sgc; do local_copy "$n" "/home/ai/dev/$n"; done
local_copy code-graph-mcp /home/ai/dev/code-graph-mcp
if [ ! -d stdlib ]; then mkdir stdlib && tar -C /usr/lib/python3.14 --exclude=site-packages --exclude=dist-packages --exclude=test --exclude=tests --exclude=__pycache__ --exclude=lib-dynload --exclude=idlelib --exclude=turtledemo --exclude='config-*' --exclude=ensurepip -cf - . | tar -C stdlib -xf - && (cd stdlib && git init -q && git add -A && git -c user.email=a@b -c user.name=a commit -qm init) && echo "ok stdlib"; fi
if [ ! -d nltk-c ]; then mkdir -p nltk-c && cp -a /usr/lib/python3/dist-packages/nltk nltk-c/nltk && (cd nltk-c && git init -q && git add -A && git -c user.email=a@b -c user.name=a commit -qm init) && echo "ok nltk-c"; fi
ls "$C"
