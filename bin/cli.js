#!/usr/bin/env node
'use strict';
/**
 * The npm package's `code-graph-mcp` / `code-graph` bin entry.
 *
 * The dispatcher it used to hold now lives in claude-plugin/scripts/cli-entry.js,
 * shared with the plugin's own launcher (claude-plugin/bin/code-graph-mcp) so
 * the two PATH names cannot drift. `claude-plugin/` is in this package's `files`
 * list and this entry already required from it, so nothing new is depended on
 * here — a comment in the old body claimed some tarballs omit that directory,
 * which was already false of the top-level `find-binary` require above it.
 *
 * `_FIND_BINARY_ROOT` is this package's root, one level up from bin/: it is what
 * lets find-binary.js find a bundled `bin/<binary>` and detect a dev checkout.
 */
const path = require('path');

require('../claude-plugin/scripts/cli-entry').main({
  findBinaryRoot: path.resolve(__dirname, '..'),
});
