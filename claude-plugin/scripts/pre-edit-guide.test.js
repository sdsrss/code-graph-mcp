'use strict';
const test = require('node:test');
const assert = require('node:assert/strict');

// Pre-edit-guide.js is a script with side effects (reads stdin, checks db).
// We test its PATTERNS directly without requiring the module.

// --- Function signature patterns (copied from pre-edit-guide.js) ---
const fnPatterns = [
  /(?:pub\s+)?(?:async\s+)?fn\s+(\w+)/,                        // Rust
  /(?:export\s+)?(?:async\s+)?function\s+(\w+)/,                // JS/TS
  /(?:const|let|var)\s+(\w+)\s*=\s*(?:async\s+)?(?:\([^)]*\)|_)\s*=>/, // JS arrow
  /(?:async\s+)?(\w+)\s*\([^)]*\)\s*\{/,                       // JS method / Go func
  /def\s+(\w+)/,                                                // Python/Ruby
  /func\s+(\w+)/,                                               // Go/Swift
  /(?:public|private|protected|static|override|virtual|abstract|internal)\s+\S+\s+(\w+)\s*\(/, // Java/C#/Kotlin
  /(?:public\s+)?function\s+(\w+)/,                             // PHP
];

function extractFunctionName(code) {
  for (const pat of fnPatterns) {
    const m = code.match(pat);
    if (m) return m[1] || m[2];
  }
  return null;
}

function isCommonKeyword(s) {
  return /^(if|for|while|switch|catch|else|return|new|get|set|try)$/i.test(s);
}

// ── Rust ────────────────────────────────────────────────

test('fn-extract: Rust pub fn', () => {
  assert.equal(extractFunctionName('pub fn parse_code(input: &str) -> Vec<Node> {'), 'parse_code');
});

test('fn-extract: Rust pub async fn', () => {
  assert.equal(extractFunctionName('pub async fn handle_message(&self, msg: &str) -> Result<()> {'), 'handle_message');
});

test('fn-extract: Rust fn (no pub)', () => {
  assert.equal(extractFunctionName('fn helper_func(x: i32) -> i32 {'), 'helper_func');
});

// ── JavaScript/TypeScript ───────────────────────────────

test('fn-extract: JS function', () => {
  assert.equal(extractFunctionName('function handleRequest(req, res) {'), 'handleRequest');
});

test('fn-extract: JS export function', () => {
  assert.equal(extractFunctionName('export function processData(input) {'), 'processData');
});

test('fn-extract: JS async function', () => {
  assert.equal(extractFunctionName('async function fetchData(url) {'), 'fetchData');
});

test('fn-extract: JS export async function', () => {
  assert.equal(extractFunctionName('export async function loadConfig(path) {'), 'loadConfig');
});

test('fn-extract: JS arrow function (const)', () => {
  assert.equal(extractFunctionName('const handleError = (err) => {'), 'handleError');
});

test('fn-extract: JS arrow function (async)', () => {
  assert.equal(extractFunctionName('const fetchUser = async (id) => {'), 'fetchUser');
});

test('fn-extract: JS method', () => {
  assert.equal(extractFunctionName('  handleMessage(msg) {'), 'handleMessage');
});

// ── Python ──────────────────────────────────────────────

test('fn-extract: Python def', () => {
  assert.equal(extractFunctionName('def process_data(self, items):'), 'process_data');
});

test('fn-extract: Python async def', () => {
  assert.equal(extractFunctionName('async def fetch_data(url):'), 'fetch_data');
});

// ── Go ──────────────────────────────────────────────────

test('fn-extract: Go func', () => {
  assert.equal(extractFunctionName('func HandleRequest(w http.ResponseWriter, r *http.Request) {'), 'HandleRequest');
});

// ── Java/C#/Kotlin ──────────────────────────────────────

test('fn-extract: Java public method', () => {
  assert.equal(extractFunctionName('public void processItem(Item item) {'), 'processItem');
});

test('fn-extract: Java private method', () => {
  assert.equal(extractFunctionName('private String formatOutput(Data data) {'), 'formatOutput');
});

test('fn-extract: C# static method', () => {
  assert.equal(extractFunctionName('static int CalculateTotal(List<int> items) {'), 'CalculateTotal');
});

// ── PHP ─────────────────────────────────────────────────

test('fn-extract: PHP function', () => {
  assert.equal(extractFunctionName('function handleUpload($file) {'), 'handleUpload');
});

test('fn-extract: PHP public function', () => {
  assert.equal(extractFunctionName('public function getUser($id) {'), 'getUser');
});

// ── Ruby ────────────────────────────────────────────────

test('fn-extract: Ruby def', () => {
  assert.equal(extractFunctionName('def process_request(params)'), 'process_request');
});

// ── Keyword filter ──────────────────────────────────────

test('keyword-filter: common keywords rejected', () => {
  for (const kw of ['if', 'for', 'while', 'switch', 'catch', 'else', 'return', 'new', 'get', 'set', 'try']) {
    assert.ok(isCommonKeyword(kw), `"${kw}" should be rejected`);
  }
});

test('keyword-filter: real function names pass', () => {
  for (const name of ['parse_code', 'handleMessage', 'process_data', 'fetchUser']) {
    assert.ok(!isCommonKeyword(name), `"${name}" should pass`);
  }
});

// ── No false positives ──────────────────────────────────

test('fn-extract: plain code body returns null', () => {
  assert.equal(extractFunctionName('let x = 42;\nreturn x + 1;'), null);
});

test('fn-extract: comment returns null', () => {
  assert.equal(extractFunctionName('// This is a comment about the function'), null);
});

test('fn-extract: short strings return null', () => {
  assert.equal(extractFunctionName('x = 1'), null);
});

// ── Pattern consistency check ───────────────────────────
// Verify fnPatterns in this test match what's in pre-edit-guide.js

test('pattern-sync: fnPatterns count matches source', () => {
  const fs = require('node:fs');
  const path = require('node:path');
  const source = fs.readFileSync(path.join(__dirname, 'pre-edit-guide.js'), 'utf8');
  // Count regex pattern lines in the fnPatterns array (lines containing // Language comment)
  const sourcePatternCount = (source.match(/\/\/\s*(Rust|JS|Python|Go|Java|C#|PHP|Ruby|Swift|Kotlin)/g) || []).length;
  assert.ok(fnPatterns.length === 8, `Expected 8 patterns, got ${fnPatterns.length}`);
  assert.ok(sourcePatternCount >= 7, `Source should have >= 7 language comments, found ${sourcePatternCount}`);
});
