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
  if (op === 'add') return [...annotations, comment('a' + (annotations.length + 1), {body: form.get('body')})];
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
  assert.deepEqual(cards.map(card => card.dataset.annotation), ['a2', 'a1']);
  assert.equal(cards[1].getAttribute('aria-label'), 'Comment a1 on “only referenced assets are served”');
  assert.equal(cards[0].getAttribute('aria-label'), 'Comment a2 on the document');
  const labels = card => card.querySelectorAll('button').map(button => button.getAttribute('aria-label'));
  assert.deepEqual(labels(cards[1]), ['Edit comment a1', 'Resolve comment a1', 'Delete comment a1']);
  assert.deepEqual(labels(cards[0]), ['Edit comment a2', 'Resolve comment a2', 'Delete comment a2']);
  // The accessible name starts with the visible label.
  for (const button of cards[1].querySelectorAll('button')) assert.ok(button.getAttribute('aria-label').startsWith(button.textContent));
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

// Delivers a key the way a browser does for the single-key shortcuts: to the
// window, targeted at whatever has focus. Returns whether it was consumed.
function press(page, key, fields = {}) {
  const event = {type: 'keydown', key, target: page.document.activeElement, defaultPrevented: false,
    preventDefault() { this.defaultPrevented = true; }, ...fields};
  page.window.listeners.get('keydown')(event);
  return event.defaultPrevented;
}

function saveDraft(page, text) {
  const body = page.find('textarea');
  body.value = text;
  body.dispatchEvent({type: 'input'});
  page.find('form').requestSubmit();
  return page.settle();
}

test('C from a card gives focus back to that card once the save has rebuilt the list', async () => {
  const page = await pageWithComments();
  assert.ok(press(page, 'j'));
  focused(page, page.card('a1'));
  const before = page.card('a1');
  assert.ok(press(page, 'c'));
  focused(page, page.find('textarea'));
  await saveDraft(page, 'From the card');
  assert.equal(page.posts.length, 1);
  // The save replaced every card, so the card that opened the draft is a
  // new element. Focus must not be left on the detached one, or on <body>.
  assert.ok(page.card('a1') !== before);
  focused(page, page.card('a1'));

  // From a button on a card, focus returns to that button.
  page.cardButton('a2', 'Edit').focus();
  assert.ok(press(page, 'c'));
  await saveDraft(page, 'From the Edit button');
  assert.equal(page.posts.length, 2);
  focused(page, page.cardButton('a2', 'Edit'));

  // Cancelling an empty draft gives focus back the same way.
  page.card('a2').focus();
  assert.ok(press(page, 'c'));
  page.find('.markview-review-composer').dispatchEvent({type: 'keydown', key: 'Escape'});
  focused(page, page.card('a2'));

  // A fixed control that opened the draft is still focused directly.
  const whole = page.button('Comment on document');
  whole.focus();
  assert.ok(press(page, 'c'));
  await saveDraft(page, 'From the toolbar');
  focused(page, whole);
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

// --- The pure functions that map between Rust's offsets and the DOM ---

function documentOf(page, ...children) {
  page.main.append(...children);
  return page.context.markviewReviewText(page.main);
}

test('text points skip every Unicode White_Space character and nothing else', () => {
  const page = createPage();
  const {h} = page;
  // Space, tab, line feed, no-break space, NEL, line separator, ideographic
  // space and thin space are White_Space. A zero-width space is not.
  const points = documentOf(page, h('p', {}, 'a b\tc\nd e\u0085f g　h i​j'));
  assert.equal(points.map(point => point.scalar).join(''), 'abcdefghi​j');
  // A gap is recorded where whitespace separated two points, for quotes.
  // (The script runs in its own realm, so copy its array before comparing.)
  assert.deepEqual(Array.from(points, point => point.gap), [true, true, true, true, true, true, true, true, true, false, false]);
});

test('text points follow DOM order and leave out images, controls and footnote numerals', () => {
  const page = createPage();
  const {h} = page;
  const points = documentOf(page,
    h('h1', {}, 'Title'),
    h('p', {}, 'one', h('img', {alt: 'not text', src: 'x.png'}), h('strong', {}, 'two'),
      h('sup', {class: 'footnote-reference'}, h('a', {href: '#1'}, '1'))),
    h('ul', {}, h('li', {}, h('input', {type: 'checkbox'}), 'task')),
    h('pre', {}, h('code', {}, h('span', {class: 'diff-line diff-addition'}, '+new\n'), ' context\n')),
    h('div', {class: 'footnote-definition'}, h('sup', {class: 'footnote-definition-label'}, '1'), h('p', {}, 'Note')));
  assert.equal(points.map(point => point.scalar).join(''), 'Titleonetwotask+newcontextNote');
  // Block boundaries separate words in a quote even with no whitespace
  // text between them. Inline elements do not: "one" and "two" touch.
  const words = points.map((point, index) => (index && point.gap ? ' ' : '') + point.scalar).join('');
  assert.equal(words, 'Title onetwo task +new context Note');
});

test('scalar offsets become DOM ranges that do not split supplementary characters', () => {
  const page = createPage();
  const {h} = page;
  // Each emoji is one scalar and two UTF-16 code units.
  const points = documentOf(page, h('p', {}, 'a😀b ', h('em', {}, '𝒳y')), h('p', {}, 'z'));
  assert.equal(points.length, 6);
  const text = range => range.startContainer === range.endContainer
    ? range.startContainer.data.slice(range.startOffset, range.endOffset)
    : range.startContainer.data.slice(range.startOffset) + '|' + range.endContainer.data.slice(0, range.endOffset);
  const range = offsets => page.context.markviewReviewRange(points, offsets);
  assert.equal(text(range([1, 2])), '😀');
  assert.deepEqual([range([1, 2]).startOffset, range([1, 2]).endOffset], [1, 3]);
  assert.equal(text(range([0, 3])), 'a😀b');
  assert.equal(text(range([3, 4])), '𝒳');
  // A range may cross inline and block boundaries.
  assert.equal(text(range([2, 6])), 'b |z');
  // Empty, reversed and out-of-bounds offsets give no range.
  for (const offsets of [[2, 2], [3, 1], [0, 7], [6, 7]]) assert.equal(range(offsets), null);
});

test('a selection round-trips through offsets, including across elements', () => {
  const page = createPage();
  const {h} = page;
  const points = documentOf(page, h('p', {}, 'one ', h('strong', {}, 'two'), ' 😀 three'));
  const range = page.context.markviewReviewRange(points, [3, 7]);
  const selection = {rangeCount: 1, isCollapsed: false, getRangeAt: () => range};
  assert.deepEqual({...page.context.markviewReviewSelection(page.main, selection)}, {quote: 'two 😀', start: 3, end: 7});
  // Collapsed, missing, multi-range and outside selections are not captured.
  const outside = page.document.createRange();
  const elsewhere = h('p', {}, 'navigation');
  page.document.body.append(elsewhere);
  outside.setStart(elsewhere.childNodes[0], 0);
  outside.setEnd(elsewhere.childNodes[0], 4);
  for (const rejected of [null, {...selection, isCollapsed: true}, {...selection, rangeCount: 2}, {...selection, getRangeAt: () => outside}]) {
    assert.equal(page.context.markviewReviewSelection(page.main, rejected), null);
  }
});

test('overlapping open comments are all highlighted and resolved ones are not', async () => {
  const page = await servePage(() => response(200, review('store-1', [
    comment('a1', {range: [0, 9], quote: 'first sec', lines: [1, 1]}),
    comment('a2', {range: [5, 11], quote: 'second', lines: [1, 1]}),
    comment('a3', {range: [5, 11], quote: 'second', lines: [1, 1], status: 'resolved'}),
    comment('a4', {range: null, quote: 'gone', lines: [9, 9], attached: false}),
  ])));
  page.main.append(page.h('p', {}, 'first second third'));
  page.mutated(page.main);
  page.window.markviewReviewRefresh();
  await page.settle();
  const highlight = page.highlights.get('markview-annotations');
  assert.equal(highlight.size, 2);
  const spans = highlight.ranges.map(range => range.startContainer.data.slice(range.startOffset, range.endOffset));
  assert.deepEqual(spans, ['first seco', 'second']);
  // Highlighting never edits the document.
  assert.equal(page.main.querySelectorAll('p').length, 1);
  assert.equal(page.main.textContent, 'first second third');
  // The detached comment keeps its card, quote and last known line.
  const detached = page.panel.querySelectorAll('.markview-review-card').find(card => card.dataset.annotation === 'a4');
  assert.match(detached.querySelector('.markview-review-label').textContent, /a4 · Line 9 · Detached/);
  assert.equal(detached.querySelector('blockquote').textContent, 'gone');
});

test('comment text is shown as text, never as markup', async () => {
  const page = await servePage(() => response(200, review('store-1', [comment('a1', {body: '<b>bold</b>\n<script>alert(1)</script>'})])));
  const body = page.find('.markview-review-body');
  assert.equal(body.textContent, '<b>bold</b>\n<script>alert(1)</script>');
  assert.equal(body.children.length, 0);
});

test('an annotation-only update leaves the document and an open draft alone', async () => {
  let annotations = [comment('a1', {range: [0, 5]})];
  let store = 1;
  const page = await servePage(() => response(200, review('store-' + store, annotations)));
  page.main.append(page.h('p', {}, 'first second'));
  page.mutated(page.main);
  const paragraph = page.main.querySelector('p');
  const text = paragraph.childNodes[0];
  const body = openDocumentDraft(page, 'Half-written');

  // The store changes on disk: the page refetches the list only.
  annotations = [comment('a1', {range: [0, 5], status: 'resolved'}), comment('a2')];
  store = 2;
  page.window.markviewReviewRefresh();
  await page.settle();
  assert.ok(page.main.querySelector('p') === paragraph && paragraph.childNodes[0] === text, 'document nodes were replaced');
  assert.equal(page.main.scrolledIntoView + paragraph.scrolledIntoView, 0);
  assert.equal(body.value, 'Half-written');
  assert.equal(page.find('form').visible, true);
  assert.equal(page.find('strong').textContent, '1 open comment');
  // The draft also survives a page reload, through session storage.
  assert.equal(JSON.parse(page.storage.get('markview-review:/doc.md')).body, 'Half-written');
});

// --- Behaviour the specification promises, one promise per test ---

// A panel driven directly, as the macOS app drives it: `submit` receives the
// form and returns a promise the test settles.
function directPage(submit = () => new Promise(() => {})) {
  const page = createPage();
  page.mount = page.h('aside');
  page.document.body.append(page.mount);
  page.review = page.context.createMarkviewReview(page.main, page.mount, submit);
  page.panel = page.mount.querySelector('.markview-review');
  page.find = selector => page.panel.querySelector(selector);
  page.button = label => page.panel.querySelectorAll('button').find(button => button.textContent === label && button.visible);
  page.cards = () => page.panel.querySelectorAll('.markview-review-card');
  return page;
}

test('resolved comments stay out of the list until asked for, and can be reopened', async () => {
  const page = await pageWithComments();
  page.cardButton('a1', 'Resolve').click();
  await page.settle();
  assert.deepEqual(page.posts, [{op: 'status', id: 'a1', store_revision: 'store-1', status: 'resolved', doc: '/doc.md'}]);
});

test('the resolved toggle is off by default and reveals a Reopen action', async () => {
  const posts = [];
  const page = await servePage((method, form) => {
    if (method === 'POST') posts.push(Object.fromEntries(form));
    return response(200, review('store-1', [comment('a1'), comment('a2', {status: 'resolved'})]));
  });
  const ids = () => page.panel.querySelectorAll('.markview-review-card').map(card => card.dataset.annotation);
  assert.deepEqual(ids(), ['a1']);
  assert.equal(page.find('strong').textContent, '1 open comment');
  const toggle = page.find('input');
  assert.equal(toggle.checked, false);
  toggle.checked = true;
  toggle.dispatchEvent({type: 'change'});
  assert.deepEqual(ids(), ['a1', 'a2']);
  page.button('Reopen').click();
  await page.settle();
  assert.deepEqual(posts, [{op: 'status', id: 'a2', store_revision: 'store-1', status: 'open', doc: '/doc.md'}]);
});

test('a read-only page lists comments without any way to change them', async () => {
  const page = createPage();
  page.fetch = async () => response(200, review('store-1', [comment('a1', {quote: 'quoted', lines: [2, 3], section: 'Intro > Scope', range: null, attached: false})]));
  await page.context.markviewServeReview('/doc.md', null, 7878);
  const panel = page.document.querySelector('.markview-review');
  assert.equal(panel.visible, true);
  const card = panel.querySelector('.markview-review-card');
  assert.equal(card.querySelector('.markview-review-label').textContent, 'a1 · Lines 2–3 · Intro > Scope · Detached');
  assert.equal(card.querySelector('blockquote').textContent, 'quoted');
  assert.deepEqual(panel.querySelectorAll('button').filter(button => button.visible), []);
  assert.equal(panel.querySelector('form').visible, false);
});

test('a comment over 10,000 characters is refused with a message and kept as a draft', async () => {
  const page = await pageWithComments();
  const body = openDocumentDraft(page, '😀'.repeat(10001));
  page.find('form').requestSubmit();
  await page.settle();
  assert.deepEqual(page.posts, []);
  assert.equal(messageOf(page), 'Comments can contain at most 10,000 characters.');
  assert.equal([...body.value].length, 10001);
  // Exactly 10,000 scalars, each two UTF-16 units, is accepted.
  body.value = '😀'.repeat(10000);
  body.dispatchEvent({type: 'input'});
  page.find('form').requestSubmit();
  await page.settle();
  assert.equal(page.posts.length, 1);
});

test('a text draft is kept but cannot be saved once the document has changed', () => {
  const page = directPage();
  page.main.append(page.h('p', {}, 'first second'));
  page.review.show('doc', 'source-1', true, true);
  page.review.update(review('store-1'));
  const text = page.main.querySelector('p').childNodes[0];
  const range = page.document.createRange();
  range.setStart(text, 6);
  range.setEnd(text, 12);
  page.select(range);
  page.review.addSelection();
  const body = page.find('textarea');
  body.value = 'About this word';
  body.dispatchEvent({type: 'input'});
  assert.equal(page.find('form').querySelectorAll('button').find(button => button.textContent === 'Save comment').disabled, false);

  // The document is edited and re-rendered: the selection's offsets are stale.
  page.review.show('doc', 'source-2', true, true);
  page.review.update({source_revision: 'source-2', store_revision: 'store-1', annotations: []});
  assert.equal(page.find('form').visible, true);
  assert.equal(body.value, 'About this word');
  assert.equal(page.find('form').querySelector('blockquote').textContent, 'second');
  assert.equal(page.find('form').querySelectorAll('button').find(button => button.textContent === 'Save comment').disabled, true);
  assert.equal(page.find('.markview-review-message').textContent, 'The document changed. Select the text again to continue this draft.');
  // The stored draft still names the revision it was made against.
  assert.equal(JSON.parse(page.storage.get('markview-review:doc')).source_revision, 'source-1');
});

test('an edit draft made against an older list offers the latest version', () => {
  const page = directPage();
  page.review.show('doc', 'source-1', true, true);
  page.review.update(review('store-1', [comment('a1')]));
  page.cards()[0].querySelectorAll('button').find(button => button.textContent === 'Edit').click();
  const body = page.find('textarea');
  body.value = 'My edit';
  body.dispatchEvent({type: 'input'});
  page.review.update(review('store-2', [comment('a1', {body: 'Changed elsewhere'})]));
  assert.equal(body.value, 'My edit');
  assert.equal(page.find('.markview-review-message').textContent, 'The annotations changed. Review the latest comment before saving this draft.');
  assert.equal(page.button('Save comment').disabled, true);
  page.button('Use latest version').click();
  assert.equal(page.button('Save comment').disabled, false);
  assert.equal(JSON.parse(page.storage.get('markview-review:doc')).store_revision, 'store-2');
});

test('clicking a highlight focuses its card and clicking a card scrolls to its highlight', () => {
  const page = directPage();
  page.main.append(page.h('p', {}, 'first second'));
  page.review.show('doc', 'source-1', false, false);
  page.review.update(review('store-1', [
    comment('a1', {range: [0, 5], quote: 'first', lines: [1, 1]}),
    comment('a2', {range: null, quote: 'gone', lines: [4, 4], attached: false}),
  ]));
  const paragraph = page.main.querySelector('p');
  const [attached, detached] = page.cards();
  // The stub lays every range out at (0,0)-(10,10).
  page.main.dispatchEvent({type: 'click', target: paragraph, clientX: 5, clientY: 5});
  focused(page, attached);
  page.main.dispatchEvent({type: 'click', target: paragraph, clientX: 50, clientY: 50});
  focused(page, attached);

  attached.dispatchEvent({type: 'click', target: attached});
  assert.equal(paragraph.scrolledIntoView, 1);
  // A detached comment has no highlight to go to.
  detached.dispatchEvent({type: 'click', target: detached});
  detached.dispatchEvent({type: 'keydown', key: 'Enter', target: detached});
  assert.equal(paragraph.scrolledIntoView, 1);
  attached.dispatchEvent({type: 'keydown', key: 'Enter', target: attached});
  assert.equal(paragraph.scrolledIntoView, 2);
});

test('an unsaved tab shows comments and a hint, but takes no new ones', () => {
  const page = directPage();
  page.review.show('doc', 'source-1', true, false);
  page.review.update(review('store-1', [comment('a1')]));
  assert.equal(page.find('.markview-review-message').textContent, 'Save to comment.');
  assert.equal(page.button('Comment on document'), undefined);
  assert.equal(page.cards().length, 1);
  page.review.addSelection();
  assert.equal(page.find('form').visible, false);
  // Existing comments can still be resolved.
  assert.ok(page.cards()[0].querySelectorAll('button').some(button => button.textContent === 'Resolve'));
});

test('a response that arrives after switching documents is not applied to the new one', async () => {
  const saved = deferred();
  const page = directPage(() => saved.promise);
  page.review.show('first', 'source-1', true, true);
  page.review.update(review('store-1'));
  openDocumentDraft(page, 'For the first document');
  page.find('form').requestSubmit();
  // The reader switches tabs while the save is in flight.
  page.review.show('second', 'source-9', true, true);
  page.review.update({source_revision: 'source-9', store_revision: 'other', annotations: []});
  saved.resolve(review('store-2', [comment('a1', {body: 'For the first document'})]));
  await page.settle();
  assert.equal(page.cards().length, 0);
  assert.equal(page.find('strong').textContent, '0 open comments');
  // Back on the first document, its draft is gone because it was saved.
  page.review.show('first', 'source-1', true, true);
  assert.equal(page.find('form').visible, false);
});

// A blocking review page as `markview --review` serves it: the review
// script, the verdict bar and the live-reload shell. `page.summaries` holds
// one function per summary request still in flight; calling it answers.
async function reviewPage(summary = {review_revision: 'r1', comments: 0, documents: 0}) {
  const page = createPage();
  page.summary = summary;
  page.summaries = [];
  page.fetch = (url, init = {}) => {
    if (url === '/_markview/review' && !init.method) {
      return new Promise(resolve => page.summaries.push(() => resolve(response(200, page.summary))));
    }
    return Promise.resolve(response(200, review('store-1')));
  };
  page.loadVerdict();
  await page.context.markviewServeReview('/doc.md', 'token', 7878, true);
  page.events = page.loadShell();
  page.bar = page.document.querySelector('.markview-verdict');
  page.status = () => page.bar.querySelector('p').textContent;
  // Answers every summary request in flight, and any that follows from it.
  page.answerSummaries = async () => {
    for (let index = 0; index < page.summaries.length; index++) {
      page.summaries[index]();
      await page.settle();
    }
  };
  return page;
}

test('a review page asks for the collection summary once, and an event burst at most twice', async () => {
  const page = await reviewPage();
  page.events.dispatchEvent({type: 'open'});
  await page.settle();
  assert.equal(page.summaries.length, 1, 'page load');
  await page.answerSummaries();
  assert.equal(page.summaries.length, 1, 'page load, answered');
  assert.equal(page.status(), '0 open comments in 0 documents.');

  // Five saves on other pages arrive before the first summary is answered:
  // one request is in flight and one more covers everything after it.
  page.summary = {review_revision: 'r2', comments: 5, documents: 2};
  for (let index = 0; index < 5; index++) page.events.dispatchEvent({type: 'annotations', data: '/other.md'});
  await page.settle();
  assert.equal(page.summaries.length, 2, 'in flight');
  await page.answerSummaries();
  assert.equal(page.summaries.length, 3, 'one pending request, not four');
  assert.equal(page.status(), '5 open comments in 2 documents.');
});

// A served page without review hooks, and the page its next live reload
// fetches: `attributes` are those of the new page's <main>.
async function liveReload(page, events, attributes) {
  page.next = {root: page.h('html', {}, page.h('main', attributes, page.h('p', {}, 'Edited')))};
  page.fetch = async () => ({ok: true, status: 200, text: async () => ''});
  events.dispatchEvent({type: 'message', data: 'reload'});
  await page.settle();
}

test('a live reload copies the review attributes of <main> only when the new page has them', async () => {
  const page = createPage({revision: null});
  const events = page.loadShell();
  const attributes = () => ({...page.main.dataset});

  // A plain `--serve` page has neither attribute, before or after an edit.
  await liveReload(page, events, {});
  assert.deepEqual(attributes(), {});

  // Its first annotation makes the next page a review page.
  await liveReload(page, events, {'data-source-revision': 'source-2', 'data-markview-route': '/doc.md'});
  assert.deepEqual(attributes(), {sourceRevision: 'source-2', markviewRoute: '/doc.md'});

  // And when the page is plain again, the attributes go with the hooks.
  await liveReload(page, events, {});
  assert.deepEqual(attributes(), {});
});
