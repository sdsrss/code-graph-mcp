'use strict';
// D#279: the main package pins its five platform packages as optional
// dependencies, and npm skips one it cannot resolve without failing the
// install. 0.167.0's Publish printed `+ …linux-arm64@0.167.0` and published the
// main package seconds later; linux-arm64 became public 55 minutes after that,
// and a rerun of the job failed on `E409 … previously staged version`.
// tasks/specs/d279-npm-publish-visibility.md has the measured delays.
const test = require('node:test');
const assert = require('node:assert/strict');
const fs = require('fs');
const http = require('http');
const os = require('os');
const path = require('path');
const { spawn } = require('child_process');

const {
  PLATFORMS,
  classifyPublish,
  loadPackages,
  publishRelease,
} = require('./npm-publish');

const VERSION = '1.2.3';
const MAIN = { name: '@sdsrs/code-graph', dir: '/r' };
const PLATFORM_PKGS = PLATFORMS.map((p) => ({ name: `@sdsrs/code-graph-${p}`, dir: `/r/npm/${p}` }));

const OK = { status: 0, output: 'npm notice Your package is being processed\n+ x@1.2.3\n' };
const STAGED = {
  status: 1,
  output:
    'npm error code E409\nnpm error 409 Conflict - PUT https://registry.npmjs.org/@sdsrs%2fcode-graph-linux-arm64' +
    ' - Cannot publish over previously staged version "1.2.3".\n',
};
const ALREADY = {
  status: 1,
  output:
    'npm error code E403\nnpm error 403 403 Forbidden - PUT https://registry.npmjs.org/@sdsrs%2fcode-graph-linux-x64' +
    ' - You cannot publish over the previously published versions: 1.2.3.\n',
};
const AUTH = { status: 1, output: 'npm error code E401\nnpm error 401 Unauthorized - PUT …\n' };

// A fake world: `visibleAfter[name]` is how many probes of that package answer
// "not yet" before it is installable (Infinity: never). Time advances only by
// the sleeps the code asks for.
function world({ publishResults = {}, visibleAfter = {} } = {}) {
  const events = [];
  const probes = {};
  let clock = 0;
  const lines = [];
  return {
    events,
    lines,
    deps: {
      publish(pkg) {
        events.push(`publish ${pkg.name}`);
        return publishResults[pkg.name] || OK;
      },
      async probe(name, version) {
        assert.equal(version, VERSION);
        probes[name] = (probes[name] || 0) + 1;
        const visible = probes[name] > (visibleAfter[name] ?? 0);
        if (visible) events.push(`visible ${name}`);
        return visible;
      },
      async sleep(ms) {
        clock += ms;
      },
      now: () => clock,
      log: (line) => lines.push(line),
    },
    elapsed: () => clock,
  };
}

const opts = (w, extra = {}) => ({
  version: VERSION,
  main: MAIN,
  platforms: PLATFORM_PKGS,
  waitMs: 60 * 60_000,
  mainWaitMs: 30 * 60_000,
  pollMs: 30_000,
  ...w.deps,
  ...extra,
});

test('classifyPublish: published, already published, previously staged, anything else', () => {
  assert.equal(classifyPublish(OK), 'published');
  assert.equal(classifyPublish(ALREADY), 'already');
  assert.equal(classifyPublish({ status: 1, output: 'npm error code EPUBLISHCONFLICT\n' }), 'already');
  assert.equal(classifyPublish(STAGED), 'submitted');
  assert.equal(classifyPublish(AUTH), 'error');
  // E409 is only "submitted" when npm says the version was staged.
  assert.equal(classifyPublish({ status: 1, output: 'npm error code E409\nnpm error 409 Conflict - other\n' }), 'error');
  // A zero exit is a publish whatever it printed.
  assert.equal(classifyPublish({ status: 0, output: 'previously staged' }), 'published');
});

test('the main package is published only after every platform package is installable', async () => {
  const slow = '@sdsrs/code-graph-linux-arm64';
  const w = world({ visibleAfter: { [slow]: 5 } });
  assert.equal(await publishRelease(opts(w)), 0);
  const mainAt = w.events.indexOf(`publish ${MAIN.name}`);
  assert.ok(mainAt > 0, w.events.join('\n'));
  for (const p of PLATFORM_PKGS) {
    const at = w.events.indexOf(`visible ${p.name}`);
    assert.ok(at >= 0 && at < mainAt, `${p.name} not visible before main:\n${w.events.join('\n')}`);
  }
  // The main package is then waited for too, so a green job means six.
  assert.ok(w.events.indexOf(`visible ${MAIN.name}`) > mainAt);
  assert.equal(w.events.filter((e) => e.startsWith('publish ')).length, 6);
});

test('E409 "previously staged" is a submitted version: wait for it, then go on', async () => {
  const staged = '@sdsrs/code-graph-linux-arm64';
  const w = world({ publishResults: { [staged]: STAGED }, visibleAfter: { [staged]: 3 } });
  assert.equal(await publishRelease(opts(w)), 0);
  assert.ok(w.events.includes(`publish ${MAIN.name}`));
  assert.ok(w.lines.some((l) => l.includes('::warning::') && l.includes(staged)), w.lines.join('\n'));
});

test('"already published" is waited for like a fresh publish', async () => {
  const done = '@sdsrs/code-graph-linux-x64';
  const w = world({ publishResults: { [done]: ALREADY }, visibleAfter: { [done]: 2 } });
  assert.equal(await publishRelease(opts(w)), 0);
  const mainAt = w.events.indexOf(`publish ${MAIN.name}`);
  assert.ok(w.events.indexOf(`visible ${done}`) < mainAt, w.events.join('\n'));
});

test('a platform package that never becomes installable fails the run before the main publish', async () => {
  const lost = '@sdsrs/code-graph-darwin-x64';
  const w = world({ visibleAfter: { [lost]: Infinity } });
  assert.equal(await publishRelease(opts(w)), 1);
  assert.ok(!w.events.includes(`publish ${MAIN.name}`), w.events.join('\n'));
  // It waited the whole budget, not one probe, and stopped within a poll of it.
  assert.ok(w.elapsed() >= 60 * 60_000 && w.elapsed() <= 60 * 60_000 + 30_000, `waited ${w.elapsed()} ms`);
  const error = w.lines.find((l) => l.startsWith('::error::'));
  assert.ok(error && error.includes(`${lost}@${VERSION} (not installable yet)`) && error.includes('not published'), error);
  // Only the missing package is named.
  assert.ok(!error.includes('linux-x64'), error);
});

test('the final error repeats what the last probe saw, so a broken probe does not read as an npm delay', async () => {
  const w = world();
  const probe = async (name) => (name.endsWith('win32-x64') ? 'packument: curl: (6) Could not resolve host' : true);
  assert.equal(await publishRelease(opts(w, { probe })), 1);
  const error = w.lines.find((l) => l.startsWith('::error::'));
  assert.ok(error.includes('@sdsrs/code-graph-win32-x64@1.2.3 (packument: curl: (6) Could not resolve host)'), error);
});

test('a main package publish error fails the run after the platform packages are out', async () => {
  const w = world({ publishResults: { [MAIN.name]: AUTH } });
  assert.equal(await publishRelease(opts(w)), 1);
  assert.equal(w.events.at(-1), `publish ${MAIN.name}`);
  assert.ok(!w.events.includes(`visible ${MAIN.name}`), w.events.join('\n'));
  assert.ok(w.lines.some((l) => l.startsWith('::error::') && l.includes(MAIN.name)), w.lines.join('\n'));
});

test('any other publish error fails at once: no later publish, no wait', async () => {
  const w = world({ publishResults: { '@sdsrs/code-graph-linux-arm64': AUTH } });
  assert.equal(await publishRelease(opts(w)), 1);
  assert.deepEqual(w.events, ['publish @sdsrs/code-graph-linux-x64', 'publish @sdsrs/code-graph-linux-arm64']);
  assert.equal(w.elapsed(), 0);
  assert.ok(w.lines.some((l) => l.includes('E401')), 'the npm output is printed');
});

test('a main package that does not become installable fails the run', async () => {
  const w = world({ visibleAfter: { [MAIN.name]: Infinity } });
  assert.equal(await publishRelease(opts(w)), 1);
  assert.ok(w.events.includes(`publish ${MAIN.name}`));
  // The main package has its own, shorter budget (mainWaitMs, not waitMs).
  assert.ok(w.elapsed() >= 30 * 60_000 && w.elapsed() <= 30 * 60_000 + 30_000, `waited ${w.elapsed()} ms`);
  assert.ok(w.lines.some((l) => l.startsWith('::error::') && l.includes(MAIN.name)), w.lines.join('\n'));
});

function layout(version, overrides = {}) {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), 'npm-publish-'));
  const write = (rel, name) => {
    fs.mkdirSync(path.join(root, rel), { recursive: true });
    fs.writeFileSync(
      path.join(root, rel, 'package.json'),
      JSON.stringify({ name, version: overrides[name] || version }),
    );
  };
  write('.', '@sdsrs/code-graph');
  for (const p of PLATFORMS) write(path.join('npm', p), `@sdsrs/code-graph-${p}`);
  return root;
}

test('loadPackages refuses a package.json at another version, before anything is published', () => {
  const root = layout(VERSION, { '@sdsrs/code-graph-win32-x64': '1.2.2' });
  try {
    assert.throws(() => loadPackages(root, VERSION), /code-graph-win32-x64.*1\.2\.2.*1\.2\.3/);
    const good = layout(VERSION);
    try {
      const { main, platforms } = loadPackages(good, VERSION);
      assert.equal(main.name, '@sdsrs/code-graph');
      assert.equal(main.dir, good);
      assert.deepEqual(
        platforms.map((p) => [p.name, path.relative(good, p.dir)]),
        PLATFORMS.map((p) => [`@sdsrs/code-graph-${p}`, path.join('npm', p)]),
      );
    } finally {
      fs.rmSync(good, { recursive: true, force: true });
    }
  } finally {
    fs.rmSync(root, { recursive: true, force: true });
  }
});

test('probeRegistry: installable means listed in the install packument AND the tarball is served', async () => {
  const seen = [];
  const server = http.createServer((req, res) => {
    seen.push([req.url, req.headers.accept, req.headers.range]);
    const base = `http://127.0.0.1:${server.address().port}`;
    const docs = {
      '/@sdsrs%2Flisted': { [VERSION]: { dist: { tarball: `${base}/t/ok.tgz` } } },
      '/@sdsrs%2Fno-tarball': { [VERSION]: { dist: { tarball: `${base}/t/missing.tgz` } } },
      '/@sdsrs%2Fother-version': { '1.2.2': { dist: { tarball: `${base}/t/ok.tgz` } } },
    };
    if (req.url === '/t/ok.tgz') {
      res.writeHead(206);
      return res.end('x');
    }
    const doc = docs[req.url.replace('%2f', '%2F')];
    if (!doc) {
      res.writeHead(req.url.includes('broken') ? 500 : 404);
      return res.end();
    }
    res.writeHead(200, { 'content-type': 'application/json' });
    res.end(JSON.stringify({ versions: doc }));
  });
  await new Promise((resolve) => server.listen(0, '127.0.0.1', resolve));
  const registry = `http://127.0.0.1:${server.address().port}`;
  // curl honours a developer's proxy variables; the local server is not behind one.
  const savedProxy = [process.env.no_proxy, process.env.NO_PROXY];
  process.env.no_proxy = process.env.NO_PROXY = '127.0.0.1,localhost';
  try {
    const { probeRegistry } = require('./npm-publish');
    assert.equal(await probeRegistry('@sdsrs/listed', VERSION, registry), true);
    // Anything else is a string saying what the probe saw.
    assert.match(await probeRegistry('@sdsrs/no-tarball', VERSION, registry), /^tarball: .*404/);
    assert.equal(await probeRegistry('@sdsrs/other-version', VERSION, registry), `${VERSION} not listed yet`);
    assert.match(await probeRegistry('@sdsrs/absent', VERSION, registry), /^packument: .*404/);
    assert.match(await probeRegistry('@sdsrs/broken', VERSION, registry), /^packument: .*500/);
    // Read what `npm install` reads, and only the first byte of the tarball.
    assert.deepEqual(seen[0], ['/@sdsrs%2flisted', 'application/vnd.npm.install-v1+json', undefined]);
    assert.deepEqual([seen[1][0], seen[1][2]], ['/t/ok.tgz', 'bytes=0-0']);
    server.close();
    // Nothing listening: a network error is "not yet", not a crash.
    assert.match(await probeRegistry('@sdsrs/listed', VERSION, registry), /^packument: curl: \(7\)/);
  } finally {
    server.close();
    for (const [key, value] of [['no_proxy', savedProxy[0]], ['NO_PROXY', savedProxy[1]]]) {
      if (value === undefined) delete process.env[key];
      else process.env[key] = value;
    }
  }
});

// End to end through the CLI: a stub `npm` on PATH records each call, and a
// local server plays the registry, listing a version only after its publish
// plus `lag` packument reads, and serving its tarball.
async function runCli({ stagedName = null, neverVisible = null, lag = 2 }) {
  const root = layout(VERSION);
  const bin = fs.mkdtempSync(path.join(os.tmpdir(), 'npm-publish-bin-'));
  const callLog = path.join(bin, 'calls.jsonl');
  fs.writeFileSync(
    path.join(bin, 'npm'),
    `#!/usr/bin/env node
const fs = require('fs');
const name = JSON.parse(fs.readFileSync('package.json', 'utf8')).name;
fs.appendFileSync(${JSON.stringify(callLog)}, JSON.stringify({ cwd: process.cwd(), args: process.argv.slice(2), name }) + '\\n');
if (name === ${JSON.stringify(stagedName)}) {
  console.error('npm error code E409');
  console.error('npm error 409 Conflict - PUT x - Cannot publish over previously staged version "${VERSION}".');
  process.exit(1);
}
console.log('+ ' + name + '@${VERSION}');
`,
    { mode: 0o755 },
  );
  const reads = {};
  const published = () =>
    fs.existsSync(callLog)
      ? fs.readFileSync(callLog, 'utf8').trim().split('\n').map((l) => JSON.parse(l).name)
      : [];
  const server = http.createServer((req, res) => {
    const url = decodeURIComponent(req.url);
    if (url.startsWith('/tarballs/')) {
      res.writeHead(206, { 'content-range': 'bytes 0-0/1' });
      return res.end('x');
    }
    const name = url.slice(1);
    reads[name] = (reads[name] || 0) + 1;
    const listed =
      name !== neverVisible && (published().includes(name) || name === stagedName) && reads[name] > lag;
    const versions = listed
      ? { [VERSION]: { dist: { tarball: `http://127.0.0.1:${server.address().port}/tarballs/${name}.tgz` } } }
      : {};
    res.writeHead(200, { 'content-type': 'application/json' });
    res.end(JSON.stringify({ name, versions }));
  });
  await new Promise((resolve) => server.listen(0, '127.0.0.1', resolve));
  try {
    const child = spawn(process.execPath, [path.join(__dirname, 'npm-publish.js'), VERSION], {
      cwd: root,
      env: {
        ...process.env,
        PATH: `${bin}${path.delimiter}${process.env.PATH}`,
        NPM_PUBLISH_ROOT: root,
        no_proxy: '127.0.0.1,localhost',
        NO_PROXY: '127.0.0.1,localhost',
        NPM_PUBLISH_REGISTRY: `http://127.0.0.1:${server.address().port}`,
        NPM_PUBLISH_POLL_SECONDS: '0.02',
        NPM_PUBLISH_WAIT_SECONDS: '2',
        NPM_PUBLISH_MAIN_WAIT_SECONDS: '2',
      },
    });
    let out = '';
    child.stdout.on('data', (d) => (out += d));
    child.stderr.on('data', (d) => (out += d));
    const status = await new Promise((resolve) => child.on('close', resolve));
    const calls = fs.existsSync(callLog)
      ? fs.readFileSync(callLog, 'utf8').trim().split('\n').map((l) => JSON.parse(l))
      : [];
    return { status, out, calls, root };
  } finally {
    server.close();
    fs.rmSync(root, { recursive: true, force: true });
    fs.rmSync(bin, { recursive: true, force: true });
  }
}

test('CLI: publishes the five platform packages, then the main package, with the same npm arguments',
  { skip: process.platform === 'win32' && 'the stub npm is a shebang script' },
  async () => {
    const { status, out, calls, root } = await runCli({ stagedName: '@sdsrs/code-graph-linux-arm64' });
    assert.equal(status, 0, out);
    assert.deepEqual(
      calls.map((c) => path.relative(root, c.cwd) || '.'),
      [...PLATFORMS.map((p) => path.join('npm', p)), '.'],
    );
    for (const c of calls) assert.deepEqual(c.args, ['publish', '--access', 'public', '--provenance']);
    assert.match(out, /::warning::.*code-graph-linux-arm64/);
  });

test('CLI: a platform package that never appears exits 1 without publishing the main package',
  { skip: process.platform === 'win32' && 'the stub npm is a shebang script' },
  async () => {
    const lost = '@sdsrs/code-graph-darwin-arm64';
    const { status, out, calls } = await runCli({ neverVisible: lost });
    assert.equal(status, 1, out);
    assert.ok(!calls.some((c) => c.name === '@sdsrs/code-graph'), out);
    assert.match(out, /::error::.*code-graph-darwin-arm64/);
  });
