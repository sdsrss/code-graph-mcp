'use strict';
const test = require('node:test');
const assert = require('node:assert/strict');

const { launchBackgroundAutoUpdate, syncLifecycleConfig } = require('./session-init');

test('syncLifecycleConfig is exported as a callable helper', () => {
  assert.equal(typeof syncLifecycleConfig, 'function');
});

test('launchBackgroundAutoUpdate spawns detached silent updater', () => {
  const calls = [];

  const ok = launchBackgroundAutoUpdate((command, args, options) => {
    const record = { command, args, options, unrefCalled: false };
    calls.push(record);
    return {
      unref() {
        record.unrefCalled = true;
      },
    };
  }, { HOME: '/tmp/fake-home' });

  assert.equal(ok, true);
  assert.equal(calls.length, 1);
  assert.equal(calls[0].command, process.execPath);
  assert.match(calls[0].args[0], /auto-update\.js$/);
  assert.equal(calls[0].args[1], 'check');
  assert.equal(calls[0].args[2], '--silent');
  assert.equal(calls[0].options.detached, true);
  assert.equal(calls[0].options.stdio, 'ignore');
  assert.equal(calls[0].options.env.CODE_GRAPH_AUTO_UPDATE_SILENT, '1');
  assert.equal(calls[0].unrefCalled, true);
});

