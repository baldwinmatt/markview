const {test} = require('node:test');
const assert = require('node:assert/strict');
const {createPage} = require('./support/review-dom.cjs');

function response(status, body) {
  return {status, ok: status >= 200 && status < 300, json: async () => body};
}

test('a forbidden annotations fetch tells the reader where to open the page', async () => {
  // The server refuses `/_markview/` for any Host but loopback, for example
  // when the page was opened through a LAN name. Say so instead of showing
  // an empty page with no explanation.
  const page = createPage();
  page.fetch = async () => response(403, {error: 'invalid Host'});
  await page.context.markviewServeReview('/doc.md', null, 7878);
  const panel = page.document.querySelector('.markview-review');
  const message = panel.querySelector('.markview-review-message');
  assert.equal(message.textContent, 'Open this page at localhost:7878 to review.');
  assert.equal(message.hidden, false);
  assert.equal(panel.visible, true);
});

test('a read-only page with no annotations and no error shows no panel', async () => {
  const page = createPage();
  page.fetch = async () => response(200, {source_revision: 'source-1', store_revision: 's', annotations: []});
  await page.context.markviewServeReview('/doc.md', null, 7878);
  assert.equal(page.document.querySelector('.markview-review').visible, false);
});

// A served, writable page whose annotation endpoint is `respond(method, form)`.
async function servePage(respond, options) {
  const page = createPage(options);
  page.requests = [];
  page.fetch = async (url, init = {}) => {
    const method = init.method || 'GET';
    page.requests.push(method);
    return respond(method, init.body, url);
  };
  await page.context.markviewServeReview('/doc.md', 'token', 7878);
  page.panel = page.document.querySelector('.markview-review');
  page.find = selector => page.panel.querySelector(selector);
  page.button = label => page.panel.querySelectorAll('button').find(button => button.textContent === label && button.visible);
  return page;
}

function review(store, annotations = []) {
  return {source_revision: 'source-1', store_revision: store, annotations};
}

function comment(id, fields = {}) {
  return {id, status: 'open', body: 'Body of ' + id, quote: null, lines: null, section: null, attached: true, range: null, ...fields};
}

test('a saved comment is shown from the POST response without fetching the list again', async () => {
  const page = await servePage(method => (method === 'POST'
    ? response(200, review('store-2', [comment('a1', {body: 'Saved'})]))
    : response(200, review('store-1'))));
  page.button('Comment on document').click();
  const body = page.find('textarea');
  body.value = 'Saved';
  body.dispatchEvent({type: 'input'});
  page.find('form').requestSubmit();
  await page.settle();
  assert.equal(page.find('.markview-review-body').textContent, 'Saved');
  assert.deepEqual(page.requests, ['GET', 'POST']);
});

// A document of `paragraphs` paragraphs with 100 non-whitespace scalars each.
function longDocument(page, paragraphs) {
  const text = 'abcdefghi '.repeat(11) + 'x';
  for (let index = 0; index < paragraphs; index++) page.main.append(page.h('p', {}, text));
  return page.main.children;
}

function selectionBetween(page, startNode, startOffset, endNode, endOffset) {
  const range = page.document.createRange();
  range.setStart(startNode, startOffset);
  range.setEnd(endNode, endOffset);
  return {range, selection: {rangeCount: 1, isCollapsed: false, getRangeAt: () => range}};
}

test('a selection is located with a handful of comparisons, whatever the document size', () => {
  const page = createPage();
  const paragraphs = longDocument(page, 2420);
  // From the third scalar of paragraph 1000 to the fifth of paragraph 1002.
  const {range, selection} = selectionBetween(page, paragraphs[1000].childNodes[0], 2, paragraphs[1002].childNodes[0], 5);
  const selected = page.context.markviewReviewSelection(page.main, selection);
  assert.equal(selected.start, 1000 * 100 + 2);
  assert.equal(selected.end, 1002 * 100 + 5);
  assert.equal(selected.quote.replaceAll(' ', '').length, selected.end - selected.start);
  assert.ok(selected.quote.startsWith('cdefghi abcdefghi '));
  assert.ok(selected.quote.endsWith('x abcde'));
  // 242,000 scalars: a scan asks the range about every one of them.
  assert.ok(range.comparisons <= 64, `${range.comparisons} range comparisons`);
});

test('selection time grows no faster than the document', () => {
  const time = paragraphs => {
    const page = createPage();
    const nodes = longDocument(page, paragraphs);
    const middle = Math.floor(paragraphs / 2);
    const {selection} = selectionBetween(page, nodes[middle].childNodes[0], 2, nodes[middle + 2].childNodes[0], 5);
    let best = Infinity;
    for (let run = 0; run < 3; run++) {
      const start = process.hrtime.bigint();
      page.context.markviewReviewSelection(page.main, selection);
      best = Math.min(best, Number(process.hrtime.bigint() - start));
    }
    return best;
  };
  const small = time(240);    //  24,000 scalars
  const large = time(2420);   // 242,000 scalars
  // Ten times the text. Linear work is about 10x; the old per-point range
  // comparisons made it about 100x. Allow 5x headroom over linear.
  assert.ok(large < small * 50, `24K scalars: ${small / 1e6} ms, 242K scalars: ${large / 1e6} ms`);
});

test('the text points are reused until the document or its revision changes', async () => {
  const page = await servePage(() => response(200, review('store-1', [comment('a1', {range: [0, 3]})])));
  page.main.append(page.h('p', {}, 'first second'));
  page.mutated(page.main);
  let walks = 0;
  const walk = page.context.markviewReviewText;
  page.context.markviewReviewText = root => { walks += 1; return walk(root); };

  const text = page.main.querySelector('p').childNodes[0];
  const range = page.document.createRange();
  range.setStart(text, 0);
  range.setEnd(text, 5);
  // Dragging a selection fires `selectionchange` continuously.
  for (let step = 0; step < 5; step++) page.select(range);
  assert.equal(walks, 1);
  assert.equal(page.document.querySelector('.markview-review-selection').visible, true);

  // The host reports a re-rendered document: walk it again, once.
  page.main.querySelector('p').textContent = 'changed text here';
  page.mutated(page.main);
  for (let step = 0; step < 3; step++) page.select(null);
  page.window.markviewReviewRefresh();
  await page.settle();
  assert.equal(walks, 2);
});
