const {test} = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const vm = require('node:vm');
const path = require('node:path');
const context = vm.createContext({});
vm.runInContext(fs.readFileSync(path.join(__dirname, '../src/bin/markview_gui_support/editor.js'), 'utf8'), context);

test('highlighting preserves source including Unicode, HTML and empty lines', () => {
  for (const source of ['', '# Title\n\n**bold** and *italic*\n', '<script>alert(1)</script>\n🙂漢字\n\n', '```rust\n# code\n```\n', '[title](guide.md)\n']) {
    const tokens = context.markdownTokens(source);
    assert.equal(tokens.map(token => token.text).join(''), source);
  }
});

test('headings, links, emphasis and fenced code receive highlighting', () => {
  assert.equal(context.markdownTokens('# Title\n')[0].kind, 'heading');
  assert.ok(context.markdownTokens('[x](a.md)').some(token => token.kind === 'link'));
  assert.ok(context.markdownTokens('**bold**').some(token => token.kind === 'markup'));
  assert.ok(context.markdownTokens('~~~\n# code\n~~~').every(token => token.kind === 'code'));
  assert.ok(context.markdownTokens('````\n```\n# still code\n````').every(token => token.kind === 'code'));
});

test('spelling marks use UTF-16 offsets supplied by the Rust prose mask', () => {
  const source = '🙂 wordz `codez` [linkz](pathz.md)\n';
  const tokens = context.spellingTokens(source, [[3, 5], [18, 5]]);
  assert.equal(tokens.map(token => token.text).join(''), source);
  assert.equal(tokens.filter(token => token.spelling).map(token => token.text).join(''), 'wordzlinkz');
});

test('stale spelling responses cannot update another tab or newer source', () => {
  let calls = 0;
  const textarea = {dataset: {tabId: '2'}, spellingRevision: 2, showSpelling: () => calls++};
  context.document = {querySelector: () => textarea};
  context.receiveEditorSpelling(1, 2, []);
  context.receiveEditorSpelling(2, 1, []);
  assert.equal(calls, 0);
  context.receiveEditorSpelling(2, 2, []);
  assert.equal(calls, 1);
});

test('list markers precede emphasis and intraword underscores stay plain', () => {
  for (const source of ['* item with *emph*', '* **bold**']) {
    const tokens = context.markdownTokens(source);
    assert.equal(tokens[0].text, '* ');
    assert.equal(tokens[0].kind, 'markup');
    assert.equal(tokens.at(-1).kind, 'markup');
  }
  assert.ok(context.markdownTokens('snake_case_name a__b__c').every(t => !t.kind));
});

test('invalid backtick info strings do not open fences', () => {
  const tokens = context.markdownTokens('```bad`info\nprose\n');
  assert.equal(tokens.at(-1).kind, '');
});

test('nested destinations and code inside headings are separate tokens', () => {
  assert.equal(context.markdownTokens('[label](a(b).md)')[0].text, '[label](a(b).md)');
  assert.ok(context.markdownTokens('# title `code` [label](dest)').some(t => t.kind === 'code'));
});

test('empty tokens are omitted and pathological lines have bounded scanning', () => {
  for (const source of ['', '**bold**', '['.repeat(100000), '!['.repeat(50000), '`'.repeat(100000) + 'x`']) {
    const tokens = context.markdownTokens(source);
    assert.ok(tokens.every(t => t.text.length > 0));
    assert.equal(tokens.map(t => t.text).join(''), source);
  }
});

test('edits shift surviving spelling marks and discard overlapping marks', () => {
  assert.deepEqual(JSON.parse(JSON.stringify(context.shiftSpelling([[0, 3], [4, 3], [8, 3]], 'one two bad', 'one three bad'))), [[0, 3], [10, 3]]);
});
