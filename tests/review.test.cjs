const {test} = require('node:test');
const assert = require('node:assert/strict');
const {createPage} = require('./support/review-dom.cjs');

// node's assert describes both operands of a failed comparison by walking
// them, and a DOM tree is one large cycle. Compare elements by identity.
function focused(page, element) {
  const describe = node => (node ? node[Symbol.for('nodejs.util.inspect.custom')]() : String(node));
  const active = page.document.activeElement;
  assert.ok(active === element, `focus is on ${describe(active)}, expected ${describe(element)}`);
}

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

// The store after the endpoint applied `form`, for tests that follow a write.
function applied(form, annotations) {
  const op = form.get('op');
  if (op === 'delete') return annotations.filter(entry => entry.id !== form.get('id'));
  if (op === 'edit') return annotations.map(entry => (entry.id === form.get('id') ? {...entry, body: form.get('body')} : entry));
  return annotations;
}

async function pageWithComments() {
  let annotations = [comment('a1'), comment('a2')];
  let store = 1;
  const posts = [];
  const page = await servePage((method, form) => {
    if (method === 'POST') {
      posts.push(Object.fromEntries(form));
      annotations = applied(form, annotations);
      store += 1;
    }
    return response(200, review('store-' + store, annotations));
  });
  page.posts = posts;
  page.card = id => page.panel.querySelectorAll('.markview-review-card').find(card => card.dataset.annotation === id);
  page.cardButton = (id, label) => page.card(id).querySelectorAll('button').find(button => button.textContent === label && button.visible);
  return page;
}

test('Delete asks for confirmation in the card and sends nothing until confirmed', async () => {
  const page = await pageWithComments();
  page.cardButton('a1', 'Delete').click();
  await page.settle();
  // No native dialog: WKWebView without a UI delegate answers `false`.
  assert.equal(page.confirmCalls, 0);
  assert.deepEqual(page.posts, []);
  const prompt = page.card('a1').querySelector('.markview-review-confirm');
  assert.equal(prompt.visible, true);
  assert.match(prompt.textContent, /Delete this comment\?/);
  assert.ok(!page.card('a2').querySelector('.markview-review-confirm'));
  // The destructive choice takes focus, as the native dialog's default did.
  focused(page, page.cardButton('a1', 'Confirm'));

  page.cardButton('a1', 'Confirm').click();
  await page.settle();
  assert.deepEqual(page.posts, [{op: 'delete', id: 'a1', store_revision: 'store-1', doc: '/doc.md'}]);
  assert.ok(!page.card('a1'));
  assert.equal(page.confirmCalls, 0);
});

test('cancelling a delete sends nothing and restores the card actions', async () => {
  const page = await pageWithComments();
  page.cardButton('a1', 'Delete').click();
  page.cardButton('a1', 'Cancel').click();
  await page.settle();
  assert.deepEqual(page.posts, []);
  assert.ok(!page.card('a1').querySelector('.markview-review-confirm'));
  focused(page, page.cardButton('a1', 'Delete'));

  // Escape is the keyboard way out of the prompt.
  page.cardButton('a1', 'Delete').click();
  const escape = {type: 'keydown', key: 'Escape'};
  page.cardButton('a1', 'Confirm').dispatchEvent(escape);
  await page.settle();
  assert.equal(escape.defaultPrevented, true);
  assert.deepEqual(page.posts, []);
  assert.ok(!page.card('a1').querySelector('.markview-review-confirm'));
  focused(page, page.cardButton('a1', 'Delete'));
  assert.equal(page.confirmCalls, 0);
});

test('a delete prompt survives a refresh of the list', async () => {
  const page = await pageWithComments();
  page.cardButton('a2', 'Delete').click();
  page.window.markviewReviewRefresh();
  await page.settle();
  assert.equal(page.card('a2').querySelector('.markview-review-confirm').visible, true);
  page.cardButton('a2', 'Confirm').click();
  await page.settle();
  assert.equal(page.posts.length, 1);
  assert.equal(page.posts[0].id, 'a2');
});

test('replacing a draft asks in the composer instead of a native dialog', async () => {
  const page = await pageWithComments();
  page.button('Comment on document').click();
  const body = page.find('textarea');
  body.value = 'Unsaved thought';
  body.dispatchEvent({type: 'input'});

  page.cardButton('a1', 'Edit').click();
  await page.settle();
  assert.equal(page.confirmCalls, 0);
  const prompt = page.find('form').querySelector('.markview-review-confirm');
  assert.equal(prompt.visible, true);
  assert.match(prompt.textContent, /Discard the current comment draft\?/);
  assert.equal(body.value, 'Unsaved thought');

  // Keeping the draft changes nothing and returns to it.
  page.button('Keep draft').click();
  assert.ok(!page.find('form').querySelector('.markview-review-confirm'));
  assert.equal(body.value, 'Unsaved thought');
  focused(page, body);

  // Discarding it starts the edit that was asked for.
  page.cardButton('a1', 'Edit').click();
  page.button('Discard draft').click();
  assert.equal(body.value, 'Body of a1');
  assert.ok(!page.find('form').querySelector('.markview-review-confirm'));
  page.find('form').requestSubmit();
  await page.settle();
  assert.deepEqual(page.posts.map(post => post.op), ['edit']);
  assert.equal(page.confirmCalls, 0);
});

// A response that the test settles by hand, to observe the page meanwhile.
function deferred() {
  let resolve;
  let reject;
  const promise = new Promise((yes, no) => { resolve = yes; reject = no; });
  return {promise, resolve, reject};
}

function openDocumentDraft(page, text) {
  page.button('Comment on document').click();
  const body = page.find('textarea');
  body.value = text;
  body.dispatchEvent({type: 'input'});
  return body;
}

const messageOf = page => {
  const message = page.find('.markview-review-message');
  return message.hidden ? '' : message.textContent;
};

test('a conflict message is cleared once the refetched list arrives', async () => {
  let annotations = [comment('a1')];
  const refetch = deferred();
  let gets = 0;
  const page = await servePage(method => {
    if (method === 'POST') return response(409, {error: 'annotation store changed on disk'});
    gets += 1;
    return gets === 1 ? response(200, review('store-1', annotations)) : refetch.promise;
  });
  page.card = id => page.panel.querySelectorAll('.markview-review-card').find(card => card.dataset.annotation === id);
  page.card('a1').querySelectorAll('button').find(button => button.textContent === 'Resolve').click();
  await page.settle();
  assert.equal(messageOf(page), 'annotation store changed on disk');

  annotations = [comment('a1', {status: 'resolved'})];
  refetch.resolve(response(200, review('store-2', annotations)));
  await page.settle();
  assert.equal(messageOf(page), '');
});

test('an unavailable store message is cleared by the next successful list', async () => {
  let fail = true;
  const page = await servePage(() => (fail
    ? response(500, {error: 'annotation store is unavailable'})
    : response(200, review('store-1', [comment('a1')]))));
  assert.equal(messageOf(page), 'annotation store is unavailable');
  fail = false;
  page.window.markviewReviewRefresh();
  await page.settle();
  assert.equal(messageOf(page), '');
  assert.equal(page.panel.querySelectorAll('.markview-review-card').length, 1);
});

test('a selection over 1,000 characters is refused next to the selection, not in the panel', async () => {
  const page = await servePage(() => response(200, review('store-1')));
  page.main.append(page.h('p', {}, 'x'.repeat(1200)));
  page.mutated(page.main);
  const text = page.main.querySelector('p').childNodes[0];
  const range = page.document.createRange();
  range.setStart(text, 0);
  range.setEnd(text, 1001);
  page.select(range);
  const note = page.document.querySelector('.markview-review-selection-note');
  assert.equal(note.visible, true);
  assert.equal(note.textContent, 'Select at most 1,000 characters to comment.');
  assert.equal(note.getAttribute('role'), 'status');
  assert.ok(note.style.left && note.style.top, 'positioned at the selection');
  assert.equal(page.document.querySelector('.markview-review-selection').visible, false);
  // The keyboard path gets the same answer in the same place.
  page.document.dispatchEvent({type: 'keydown', key: 'K', ctrlKey: true, shiftKey: true, preventDefault() {}});
  assert.equal(note.visible, true);
  assert.equal(messageOf(page), '');
  assert.equal(page.find('form').visible, false);

  range.setEnd(text, 1000);
  page.select(range);
  assert.equal(note.visible, false);
  assert.equal(page.document.querySelector('.markview-review-selection').visible, true);
});

test('Escape cancels an empty draft at once and asks before discarding text', async () => {
  const page = await pageWithComments();
  const composer = page.find('form');
  const press = fields => {
    const event = {type: 'keydown', key: 'Escape', ...fields};
    page.find('textarea').dispatchEvent(event);
    return event;
  };

  openDocumentDraft(page, '');
  assert.equal(press().defaultPrevented, true);
  assert.equal(composer.visible, false);

  const body = openDocumentDraft(page, 'Worth keeping');
  press();
  assert.equal(composer.visible, true);
  assert.equal(body.value, 'Worth keeping');
  const prompt = composer.querySelector('.markview-review-confirm');
  assert.match(prompt.textContent, /Discard the current comment draft\?/);
  page.button('Keep draft').click();
  assert.ok(!composer.querySelector('.markview-review-confirm'));
  focused(page, body);

  press();
  page.button('Discard draft').click();
  assert.equal(composer.visible, false);
  assert.equal(page.confirmCalls, 0);

  // An edit that changed nothing has nothing to lose either.
  page.cardButton('a1', 'Edit').click();
  press();
  assert.equal(composer.visible, false);
});

test('Escape that ends an IME composition leaves the draft alone', async () => {
  const page = await pageWithComments();
  const composer = page.find('form');
  openDocumentDraft(page, '');
  for (const composing of [{isComposing: true}, {keyCode: 229}]) {
    const event = {type: 'keydown', key: 'Escape', ...composing};
    page.find('textarea').dispatchEvent(event);
    assert.equal(event.defaultPrevented, false);
    assert.equal(composer.visible, true);
    assert.ok(!composer.querySelector('.markview-review-confirm'));
  }
});

test('Cmd or Ctrl with Enter saves the draft', async () => {
  for (const modifier of [{metaKey: true}, {ctrlKey: true}]) {
    const page = await pageWithComments();
    openDocumentDraft(page, 'Shortcut');
    const event = {type: 'keydown', key: 'Enter', ...modifier};
    page.find('textarea').dispatchEvent(event);
    await page.settle();
    assert.equal(event.defaultPrevented, true);
    assert.deepEqual(page.posts, [{op: 'add', body: 'Shortcut', doc: '/doc.md'}]);
  }
});

test('the selection can be commented on from the keyboard', async () => {
  const page = await servePage(() => response(200, review('store-1')));
  page.main.append(page.h('p', {}, 'first second third'));
  page.mutated(page.main);
  const text = page.main.querySelector('p').childNodes[0];
  const range = page.document.createRange();
  range.setStart(text, 6);
  range.setEnd(text, 12);
  page.select(range);
  const button = page.document.querySelector('.markview-review-selection');
  assert.match(button.getAttribute('title'), /Shift\+K/);
  assert.equal(button.getAttribute('aria-keyshortcuts'), 'Control+Shift+K Meta+Shift+K');

  let prevented = false;
  page.document.dispatchEvent({type: 'keydown', key: 'K', metaKey: true, shiftKey: true, preventDefault() { prevented = true; }});
  assert.equal(prevented, true);
  assert.equal(page.find('form').visible, true);
  assert.equal(page.find('form').querySelector('blockquote').textContent, 'second');
  focused(page, page.find('textarea'));
  // Without the modifiers the key is just a letter.
  let plain = false;
  page.document.dispatchEvent({type: 'keydown', key: 'k', preventDefault() { plain = true; }});
  assert.equal(plain, false);
});

test('cards and their buttons are labelled with the comment they belong to', async () => {
  const page = await servePage(() => response(200, review('store-1', [
    comment('a1', {quote: 'only referenced assets are served', lines: [3, 3], range: null, attached: false}),
    comment('a2'),
  ])));
  const cards = page.panel.querySelectorAll('.markview-review-card');
  assert.equal(cards[0].getAttribute('aria-label'), 'Comment a1 on “only referenced assets are served”');
  assert.equal(cards[1].getAttribute('aria-label'), 'Comment a2 on the document');
  const labels = card => card.querySelectorAll('button').map(button => button.getAttribute('aria-label'));
  assert.deepEqual(labels(cards[0]), ['Edit comment a1', 'Resolve comment a1', 'Delete comment a1']);
  assert.deepEqual(labels(cards[1]), ['Edit comment a2', 'Resolve comment a2', 'Delete comment a2']);
  // The accessible name starts with the visible label.
  for (const button of cards[0].querySelectorAll('button')) assert.ok(button.getAttribute('aria-label').startsWith(button.textContent));
});

test('focus returns to the control that opened the draft after saving or cancelling', async () => {
  const page = await pageWithComments();
  const whole = page.button('Comment on document');

  whole.click();
  page.button('Cancel').click();
  focused(page, whole);

  openDocumentDraft(page, 'Saved');
  page.find('form').requestSubmit();
  await page.settle();
  assert.equal(page.posts.length, 1);
  focused(page, whole);

  // Cards are rebuilt by a save, so the Edit button is a new element.
  page.cardButton('a2', 'Edit').click();
  focused(page, page.find('textarea'));
  page.button('Cancel').click();
  focused(page, page.cardButton('a2', 'Edit'));

  page.cardButton('a2', 'Edit').click();
  const body = page.find('textarea');
  body.value = 'Edited';
  body.dispatchEvent({type: 'input'});
  page.find('form').requestSubmit();
  await page.settle();
  assert.equal(page.card('a2').querySelector('.markview-review-body').textContent, 'Edited');
  focused(page, page.cardButton('a2', 'Edit'));
});

test('a reload keeps the previous comments on screen until the new list arrives', async () => {
  const next = deferred();
  let gets = 0;
  const page = await servePage(() => {
    gets += 1;
    return gets === 1 ? response(200, review('store-1', [comment('a1')])) : next.promise;
  });
  const cards = () => page.panel.querySelectorAll('.markview-review-card').map(card => card.querySelector('.markview-review-body').textContent);
  assert.deepEqual(cards(), ['Body of a1']);

  // Live reload replaced the document: a new source revision, list pending.
  page.main.dataset.sourceRevision = 'source-2';
  page.window.markviewReviewRefresh();
  await page.settle();
  assert.equal(page.panel.visible, true);
  assert.deepEqual(cards(), ['Body of a1']);

  next.resolve(response(200, {source_revision: 'source-2', store_revision: 'store-1', annotations: [comment('a1', {body: 'Fresh', range: [0, 1]})]}));
  await page.settle();
  assert.deepEqual(cards(), ['Fresh']);
});

test('a failed list fetch says so and keeps what was shown', async () => {
  let gets = 0;
  const page = await servePage(() => {
    gets += 1;
    if (gets === 1) return response(200, review('store-1', [comment('a1')]));
    throw new TypeError('Failed to fetch');
  });
  page.window.markviewReviewRefresh();
  await page.settle();
  assert.equal(messageOf(page), 'Could not load comments. Reload the page to try again.');
  assert.equal(page.panel.querySelectorAll('.markview-review-card').length, 1);
});
