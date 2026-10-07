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

test('spelling marks use UTF-16 offsets and skip code and link destinations', () => {
  const source = '🙂 wordz `codez` [linkz](pathz.md)\n';
  const tokens = context.spellingTokens(source, [[3, 5], [10, 5], [18, 5], [25, 5]]);
  assert.equal(tokens.map(token => token.text).join(''), source);
  assert.equal(tokens.filter(token => token.spelling).map(token => token.text).join(''), 'wordz');
});

test('stale spelling responses cannot update another tab or newer source', () => {
  let calls = 0;
  const textarea = {dataset: {tabId: '2'}, value: 'new source', showSpelling: () => calls++};
  context.document = {querySelector: () => textarea};
  context.receiveEditorSpelling(1, 'new source', []);
  context.receiveEditorSpelling(2, 'old source', []);
  assert.equal(calls, 0);
  context.receiveEditorSpelling(2, 'new source', []);
  assert.equal(calls, 1);
});
