---
status: approved
revision: 3
---

# D#279 part 1: publish the npm packages in an order users can always install

## goal

`@sdsrs/code-graph` pins its five platform packages as `optionalDependencies`.
npm skips an optional dependency it cannot resolve and still exits 0, so a user
who installs the main package while one platform package is not yet public gets
`added 1 package` and a CLI that answers "binary not found", and that install
never heals. The Publish job must not make the main package public until every
platform package it pins can be installed, and its success must mean that all
six packages can be installed.

## non-goals

- Trusted publishing (OIDC). It needs a per-package configuration on npmjs.com
  that only the package owner can create, and Node >= 22.14 with npm >= 11.5.1
  in the job. That is part 2 of D#279.
- Changing what is published, `--provenance`, or the post-publish smoke jobs.

## constraints

- `npm publish` exiting 0 does not mean the version is public. 0.167.0:
  `+ @sdsrs/code-graph-linux-arm64@0.167.0` at 20:28:03, packument `time` entry
  21:23:34; the main package was public 49 minutes before it. Every publish in
  that run printed "Your package is being processed and may take a few minutes
  to become available."
- A rerun of Publish on such a version got `E409 Cannot publish over previously
  staged version "0.167.0"`. The workflow only recognised EPUBLISHCONFLICT, so
  the rerun failed. E409 "previously staged" for the version being published
  means it was already submitted: wait for it, do not fail.
- Measured publish-to-public delay, `time` map minus the publish log's `+` line,
  90 package publishes in 0.155.0 to 0.167.0: median 2.1 min, p90 4.9 min, max
  55.5 min; 3 of 90 above 10 min, all in 0.166.0 and 0.167.0. The wait budget
  for the platform packages is 60 minutes; for the main package 20 (its own
  delay was at most 6.3 min, and missing it costs a red job, not a broken
  install). With 10 minutes for the rest of the job (measured 162-219 s) that
  is the 90-minute job cap `tests/hardening.rs` enforces.
- Fail before the main package is published whenever a platform package is not
  installable. Users then still get the previous version, and rerunning the
  workflow completes the release: published packages hit the "already
  published" or "previously staged" branch and are waited for.
- "Installable" is what `npm install` reads: the version listed in the
  install-format packument (`accept: application/vnd.npm.install-v1+json`) and
  its tarball served (first byte). Probed with `curl`, which honours
  `https_proxy`; Node's `fetch` does not, and behind a proxy it reported the
  published 0.167.0 as absent (10.5 s connect timeout per probe).

## success-criteria

- Unit tests: the main package is published only after all five platform
  packages are visible; E409 "previously staged" waits and succeeds; a package
  that never becomes visible fails the run with the main package unpublished;
  any other publish error fails at once; "already published" is waited for like
  a fresh publish; a main package that does not become visible fails the run.
- End-to-end test through a stub `npm` and a local tarball server: the script's
  real `npm` arguments, working directories and exit codes.
- Read-only probe against the real registry: 0.167.0 visible, 9.9.9 not.
- `release.yml` publishes only through the script, and a test says so.
- On the first real run (v0.168.0): Publish green, all six packages installable,
  smokes green without a rerun.

## open-questions

- What a stage-only publish prints (`(staged)`?) is unverified. It needs no
  branch of its own: the version never becomes public, so the wait times out
  and the error names `npm stage list`.
- Whether E409 "previously staged" is npm's answer for a version held by the
  publish-time scan is unverified. The handling is the same either way.

# Change log

- r1 2026-10-08: created.
- r2 2026-10-08: probe with curl instead of `fetch` (proxy); constraint
  reworded to the install-format packument the probe actually reads.
- r3 2026-10-08: main-package wait 30 -> 20 min so the job fits the
  hardening guard's 90-minute cap.
