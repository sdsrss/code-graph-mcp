#!/usr/bin/env bash
# Remove what the plugin's detached auto-update check leaves behind after an
# eval run.
#
#   reap.sh <tmp-base> <before-list-file> <plugin-dir>
#
# SessionStart starts `auto-update.js check` detached, so it outlives the run.
# After claude plugin eval has deleted the run's temporary HOME, the check
# writes update-state.json and a freshly downloaded binary (about 41 MB) back
# into it. Each run then leaves a <tmp-base>/claude-eval-XXXXXX holding nothing
# but home/. That is harmless in a real session, where HOME persists, but in
# /tmp (a tmpfs here) one suite run leaked 15 of them.
#
# Removes only directories that did not exist before the run (listed in
# <before-list-file>) AND whose sole entry is home/. A --keep-temp directory
# also has config/, out/ and sealed/, so it is never touched.
set -euo pipefail
base="${1:?tmp base}"
before="${2:?before-list file}"
plugin="${3:?plugin dir}"

# Let the stragglers finish writing first, or they recreate what we remove.
for _ in $(seq 1 90); do
  pgrep -f "$plugin/scripts/auto-update.js" >/dev/null || break
  sleep 1
done

reaped=0
for d in "$base"/claude-eval-*; do
  [ -d "$d" ] || continue
  grep -qxF "$d" "$before" && continue
  [ "$(ls -A "$d")" = "home" ] || continue
  chmod -R u+rwx "${d:?}"
  rm -rf "${d:?}"
  reaped=$((reaped + 1))
done
echo "reaped $reaped leftover eval dir(s) under $base" >&2
