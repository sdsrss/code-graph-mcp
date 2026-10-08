#!/usr/bin/env node
'use strict';
/**
 * Publish the five platform packages, wait until each one can be installed,
 * then publish the main package and wait for it too. Run by release.yml's
 * Publish job; exits 1 on any failure, and before the main publish whenever a
 * platform package is not installable.
 *
 * Usage: node scripts/npm-publish.js <version>
 *
 * Why the order and the waits (D#279, tasks/specs/d279-npm-publish-visibility.md):
 * `@sdsrs/code-graph` pins the platform packages as optionalDependencies, and
 * npm skips one it cannot resolve while still exiting 0 — `added 1 package`,
 * then "binary not found", and that install never heals. `npm publish` exiting
 * 0 does not mean the version is public: npm now processes every new version
 * first ("Your package is being processed and may take a few minutes to become
 * available"). 0.167.0's linux-arm64 printed `+ …@0.167.0` and became public
 * 55 minutes later, with the main package public 49 minutes before it.
 * Measured over 90 publishes (0.155.0–0.167.0): median 2.1 min, p90 4.9 min,
 * max 55.5 min — hence the 60-minute budget.
 *
 * Rerunning the job is how a failed run is finished: a version already public
 * answers "cannot publish over the previously published versions", one still
 * being processed answers `E409 … previously staged version`, and both are
 * waited for instead of published again.
 *
 * Environment (seconds): NPM_PUBLISH_WAIT_SECONDS (platform packages, 3600),
 * NPM_PUBLISH_MAIN_WAIT_SECONDS (main package, 1200), NPM_PUBLISH_POLL_SECONDS
 * (30). NPM_PUBLISH_REGISTRY (https://registry.npmjs.org) and NPM_PUBLISH_ROOT
 * (the repository root) exist for the tests.
 */
const fs = require('fs');
const os = require('os');
const path = require('path');
const { execFile, spawnSync } = require('child_process');

const PLATFORMS = ['linux-x64', 'linux-arm64', 'darwin-x64', 'darwin-arm64', 'win32-x64'];

// Default budgets in seconds. release.yml's Publish `timeout-minutes` must exceed
// their sum; release-smoke.test.js checks that it does, and tests/hardening.rs
// caps any job at 90. The main package's wait is the shorter one because missing
// it costs a red job, not a broken install: until it is public, `latest` still
// names the previous version, whose platform packages are all public (its
// measured delay: at most 6.3 min over 0.155.0-0.167.0).
const WAIT_SECONDS = 3600;
const MAIN_WAIT_SECONDS = 1200;
const POLL_SECONDS = 30;

const ALREADY_PUBLISHED =
  /EPUBLISHCONFLICT|cannot publish over the previously published versions|You cannot publish over/;
const PREVIOUSLY_STAGED = /previously staged version/;

/** `published` | `already` (public before this run) | `submitted` (staged
 * before this run, not yet public) | `error`. */
function classifyPublish({ status, output }) {
  if (status === 0) return 'published';
  if (ALREADY_PUBLISHED.test(output)) return 'already';
  if (PREVIOUSLY_STAGED.test(output)) return 'submitted';
  return 'error';
}

/** The packages to publish, refusing any whose package.json is not at `version`. */
function loadPackages(root, version) {
  const read = (dir) => {
    const pkg = JSON.parse(fs.readFileSync(path.join(dir, 'package.json'), 'utf8'));
    if (pkg.version !== version) {
      throw new Error(`${pkg.name} is at ${pkg.version} in ${dir}, not ${version}`);
    }
    return { name: pkg.name, dir };
  };
  return {
    main: read(root),
    platforms: PLATFORMS.map((p) => read(path.join(root, 'npm', p))),
  };
}

function npmPublish(pkg) {
  const r = spawnSync('npm', ['publish', '--access', 'public', '--provenance'], {
    cwd: pkg.dir,
    encoding: 'utf8',
    windowsHide: true, // no console flash — same rule as claude-plugin/scripts/proc-opts
  });
  return { status: r.status, output: `${r.stdout || ''}${r.stderr || ''}${r.error ? r.error.message : ''}` };
}

/** `{ stdout }`, or `{ error }` saying why curl failed. curl, not Node's
 * `fetch`: fetch ignores `https_proxy`, so behind a proxy every probe timed out
 * and read as "not yet" (measured: 10.5 s per probe, 0.167.0 reported absent).
 * `--max-time 15` keeps one probe pass over the five platform packages (two
 * requests each) at 2.5 min at worst, which the Publish job's bound in
 * release.yml counts. */
function curl(args) {
  return new Promise((resolve) => {
    execFile(
      'curl',
      ['-sS', '--fail', '-L', '--max-time', '15', '-H', 'cache-control: no-cache', ...args],
      { encoding: 'utf8', maxBuffer: 64 * 1024 * 1024, windowsHide: true },
      (err, stdout, stderr) =>
        resolve(err ? { error: (stderr || '').trim().split('\n').pop() || err.message } : { stdout }),
    );
  });
}

/** `true` once `npm install` can get `name@version`: the install-format
 * packument lists it and its tarball is served. Otherwise a string saying what
 * the probe saw, which the final error repeats: a probe that can never succeed
 * (no curl, TLS, a changed registry) must not read as an npm delay. */
async function probeRegistry(name, version, registry) {
  const doc = await curl(['-H', 'accept: application/vnd.npm.install-v1+json', `${registry}/${name.replace('/', '%2f')}`]);
  if (doc.error) return `packument: ${doc.error}`;
  let tarball;
  try {
    tarball = JSON.parse(doc.stdout).versions?.[version]?.dist?.tarball;
  } catch {
    return 'packument: not JSON';
  }
  if (!tarball) return `${version} not listed yet`;
  const t = await curl(['-r', '0-0', '-o', os.devNull, '-w', '%{http_code}', tarball]);
  if (t.error) return `tarball: ${t.error}`;
  return t.stdout === '200' || t.stdout === '206' ? true : `tarball: HTTP ${t.stdout}`;
}

/** Publish one package; false (with the npm output and an ::error:: printed)
 * unless npm published it or it was submitted by an earlier run. */
function publishOne(pkg, version, { publish, log }) {
  const result = publish(pkg);
  const kind = classifyPublish(result);
  if (kind === 'error') {
    log(result.output.trimEnd());
    log(`::error::${pkg.name}@${version} publish failed (not "already published" or "previously staged") — check NPM_TOKEN / the registry`);
    return false;
  }
  if (kind === 'published') log(result.output.trimEnd());
  else if (kind === 'already') log(`::warning::${pkg.name}@${version} was already published — checking it is installable`);
  else log(`::warning::${pkg.name}@${version} was submitted by an earlier run and npm is still processing it (E409 "previously staged") — waiting`);
  return true;
}

/** Poll until every name is installable or `budgetMs` has passed; returns the
 * ones still missing as `name@version (what the last probe saw)`. */
async function waitInstallable(names, version, budgetMs, { probe, sleep, now, pollMs, log }) {
  const start = now();
  const pending = new Map(names.map((name) => [name, 'not probed']));
  for (;;) {
    for (const name of [...pending.keys()]) {
      const seen = await probe(name, version);
      if (seen === true) {
        pending.delete(name);
        log(`${name}@${version} is installable (${Math.round((now() - start) / 1000)} s after the wait began)`);
      } else {
        pending.set(name, typeof seen === 'string' ? seen : 'not installable yet');
      }
    }
    if (pending.size === 0 || now() - start >= budgetMs) {
      return [...pending].map(([name, seen]) => `${name}@${version} (${seen})`);
    }
    await sleep(pollMs);
  }
}

async function publishRelease(o) {
  const minutes = (ms) => Math.round(ms / 60_000);
  for (const pkg of o.platforms) {
    if (!publishOne(pkg, o.version, o)) return 1;
  }
  const missing = await waitInstallable(o.platforms.map((p) => p.name), o.version, o.waitMs, o);
  if (missing.length) {
    o.log(
      `::error::${missing.join(', ')} not installable after ${minutes(o.waitMs)} min, so ` +
        `${o.main.name} was not published. Rerun the workflow once \`npm view <pkg>@${o.version}\` lists it; ` +
        `a version held for approval shows in \`npm stage list\`.`,
    );
    return 1;
  }
  if (!publishOne(o.main, o.version, o)) return 1;
  const mainMissing = await waitInstallable([o.main.name], o.version, o.mainWaitMs, o);
  if (mainMissing.length) {
    o.log(
      `::error::${mainMissing[0]} was published but is not installable after ${minutes(o.mainWaitMs)} min. ` +
        'Rerun the failed jobs; this step waits for it again.',
    );
    return 1;
  }
  return 0;
}

async function main(argv) {
  const version = argv[2];
  if (!version || !/^\d+\.\d+\.\d+$/.test(version)) {
    console.error('Usage: node scripts/npm-publish.js <semver>');
    return 1;
  }
  const seconds = (name, fallback) => {
    const v = Number(process.env[name]);
    return (Number.isFinite(v) && v > 0 ? v : fallback) * 1000;
  };
  const root = process.env.NPM_PUBLISH_ROOT || path.resolve(__dirname, '..');
  const registry = (process.env.NPM_PUBLISH_REGISTRY || 'https://registry.npmjs.org').replace(/\/+$/, '');
  let packages;
  try {
    packages = loadPackages(root, version);
  } catch (e) {
    console.log(`::error::${e.message} — nothing was published`);
    return 1;
  }
  return publishRelease({
    version,
    ...packages,
    waitMs: seconds('NPM_PUBLISH_WAIT_SECONDS', WAIT_SECONDS),
    mainWaitMs: seconds('NPM_PUBLISH_MAIN_WAIT_SECONDS', MAIN_WAIT_SECONDS),
    pollMs: seconds('NPM_PUBLISH_POLL_SECONDS', POLL_SECONDS),
    publish: npmPublish,
    probe: (name, v) => probeRegistry(name, v, registry),
    sleep: (ms) => new Promise((resolve) => setTimeout(resolve, ms)),
    now: () => Date.now(),
    log: (line) => console.log(line),
  });
}

if (require.main === module) {
  main(process.argv).then((code) => process.exit(code));
}

module.exports = {
  PLATFORMS,
  WAIT_SECONDS,
  MAIN_WAIT_SECONDS,
  classifyPublish,
  loadPackages,
  probeRegistry,
  publishRelease,
};
