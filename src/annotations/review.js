// Text offsets count non-White_Space Unicode scalars, while DOM offsets
// count UTF-16 code units. Structural breaks are only used in displayed quotes.
const markviewReviewWhitespace = /\p{White_Space}/u;
const markviewReviewExcluded = 'img,input,.footnote-reference,.footnote-definition-label';
const markviewReviewBlocks = 'p,h1,h2,h3,h4,h5,h6,blockquote,pre,li,td,th,.footnote-definition,br,hr';

function markviewReviewText(root) {
  const points = [];
  let gap = false;
  function walk(node) {
    if (node.nodeType === 3) {
      let offset = 0;
      for (const scalar of node.data) {
        if (markviewReviewWhitespace.test(scalar)) gap = true;
        else {
          points.push({node, offset, end: offset + scalar.length, scalar, gap});
          gap = false;
        }
        offset += scalar.length;
      }
    } else if (node.nodeType === 1 && !node.matches(markviewReviewExcluded)) {
      const block = node.matches(markviewReviewBlocks);
      if (block) gap = true;
      for (const child of node.childNodes) walk(child);
      if (block) gap = true;
    }
  }
  walk(root);
  return points;
}

function markviewReviewRange(points, offsets) {
  const first = points[offsets[0]];
  const last = points[offsets[1] - 1];
  if (!first || !last || offsets[0] >= offsets[1]) return null;
  const range = first.node.ownerDocument.createRange();
  range.setStart(first.node, first.offset);
  range.setEnd(last.node, last.end);
  return range;
}

function markviewReviewSelection(root, selection) {
  if (!selection || selection.rangeCount !== 1 || selection.isCollapsed) return null;
  const range = selection.getRangeAt(0);
  if (!root.contains(range.startContainer) || !root.contains(range.endContainer)) return null;
  const points = markviewReviewText(root);
  let start = -1;
  let end = 0;
  let quote = '';
  for (let index = 0; index < points.length; index++) {
    const point = points[index];
    if (range.comparePoint(point.node, point.offset) !== 0 ||
        range.comparePoint(point.node, point.end) !== 0) continue;
    if (start === -1) start = index;
    else if (point.gap) quote += ' ';
    quote += point.scalar;
    end = index + 1;
  }
  return start < 0 ? null : {quote, start, end};
}

function createMarkviewReview(root, mount, submit, reveal = () => {}) {
  function element(tag, className, text) {
    const node = document.createElement(tag);
    if (className) node.className = className;
    if (text) node.textContent = text;
    return node;
  }
  function button(label, action) {
    const node = element('button', '', label);
    node.type = 'button';
    node.addEventListener('click', action);
    return node;
  }
  const panel = element('section', 'markview-review');
  panel.setAttribute('aria-label', 'Annotations');
  const toolbar = element('header', 'markview-review-toolbar');
  const count = element('strong');
  const whole = button('Comment on document', () => startDraft({op: 'add'}));
  const resolvedLabel = element('label');
  const resolvedToggle = element('input');
  resolvedToggle.type = 'checkbox';
  resolvedLabel.append(resolvedToggle, ' Show resolved');
  const composer = element('form', 'markview-review-composer');
  const quote = element('blockquote');
  const body = element('textarea');
  body.setAttribute('aria-label', 'Comment');
  body.placeholder = 'Write a comment';
  const actions = element('div', 'markview-review-actions');
  const save = element('button', '', 'Save comment');
  save.type = 'submit';
  const cancel = button('Cancel', () => {
    keepDraft(null);
    setMessage('');
    renderComposer();
  });
  const retry = button('Use latest version', () => {
    const draft = getDraft();
    if (draft?.op === 'edit' && review?.annotations.some(card => card.id === draft.id)) {
      keepDraft({...draft, store_revision: review.store_revision});
      setMessage('');
      renderComposer();
    }
  });
  const message = element('p', 'markview-review-message');
  message.setAttribute('role', 'status');
  const cards = element('div', 'markview-review-cards');
  const selectionButton = button('Comment', () => addSelection());
  selectionButton.className = 'markview-review-selection';
  selectionButton.addEventListener('pointerdown', event => event.preventDefault());
  actions.append(save, cancel, retry);
  composer.append(quote, body, actions);
  toolbar.append(count, whole, resolvedLabel);
  panel.append(toolbar, composer, message, cards);
  mount.append(panel);
  document.body.append(selectionButton);
  const fallback = element('div', 'markview-review-overlay');
  fallback.setAttribute('aria-hidden', 'true');
  document.body.append(fallback);
  panel.hidden = selectionButton.hidden = true;

  let key = null;
  let revision = null;
  let writable = false;
  let canAdd = false;
  let review = null;
  let selected = null;
  let ranges = [];
  let lastHit = null;
  const drafts = new Map();
  const pending = new Set();
  const errors = new Map();
  const showResolved = new Map();

  function draftKey(documentKey) { return 'markview-review:' + documentKey; }
  function getDraft() {
    if (!key) return null;
    if (!drafts.has(key)) {
      let value = null;
      try { value = JSON.parse(sessionStorage.getItem(draftKey(key))); } catch { /* Optional storage. */ }
      drafts.set(key, value);
    }
    return drafts.get(key);
  }
  function keepDraft(draft, documentKey = key) {
    drafts.set(documentKey, draft);
    try {
      if (draft) sessionStorage.setItem(draftKey(documentKey), JSON.stringify(draft));
      else sessionStorage.removeItem(draftKey(documentKey));
    } catch { /* The in-page draft still survives refreshes. */ }
  }
  function setMessage(text, documentKey = key) {
    errors.set(documentKey, text);
    if (documentKey === key) refreshMessage();
  }
  function refreshMessage() {
    const draft = getDraft();
    const changedSource = draft?.source_revision && draft.source_revision !== revision;
    const changedStore = draft?.op === 'edit' && review && draft.store_revision !== review.store_revision;
    const stale = !pending.has(key) && (changedSource
      ? 'The document changed. Select the text again to continue this draft.'
      : changedStore ? 'The annotations changed. Review the latest comment before saving this draft.' : '');
    const text = errors.get(key) || stale || (writable && !canAdd ? 'Save to comment.' : '');
    message.textContent = text;
    message.hidden = !text;
  }
  function startDraft(fields) {
    if (!writable || (fields.op === 'add' && !canAdd) || pending.has(key)) return;
    const previous = getDraft();
    const reuseBody = previous?.op === 'add' && fields.op === 'add';
    if (previous && !reuseBody && !window.confirm('Discard the current comment draft?')) return;
    keepDraft({body: reuseBody ? previous.body : '', ...fields});
    setMessage('');
    renderComposer();
    reveal();
    if (panel.getClientRects().length) composer.scrollIntoView({block: 'nearest'});
    body.focus();
  }
  function addSelection() {
    const selection = markviewReviewSelection(root, window.getSelection()) || selected;
    if (!key || !canAdd || !selection) return;
    if ([...selection.quote].length > 1000) {
      setMessage('Select at most 1,000 characters.');
      selectionButton.hidden = true;
      return;
    }
    selectionButton.hidden = true;
    selected = null;
    startDraft({op: 'add', ...selection, source_revision: revision});
  }
  function renderComposer() {
    const draft = getDraft();
    composer.hidden = !writable || !draft;
    refreshMessage();
    if (!draft || !writable) return;
    const changedSource = draft.source_revision && draft.source_revision !== revision;
    const changedStore = draft.op === 'edit' && review && draft.store_revision !== review.store_revision;
    const busy = pending.has(key);
    quote.textContent = draft.quote || '';
    quote.hidden = !draft.quote;
    if (body.value !== draft.body) body.value = draft.body;
    // Readonly keeps keyboard focus on the draft while the request is in flight.
    body.readOnly = busy;
    cancel.disabled = busy;
    save.disabled = Boolean(changedSource || changedStore || busy || (draft.op === 'add' && !canAdd));
    retry.hidden = !changedStore || !review.annotations.some(card => card.id === draft.id);
    retry.disabled = busy;
  }
  async function send(fields, draft = null) {
    if (!writable || pending.has(key)) return;
    const documentKey = key;
    const source = revision;
    pending.add(documentKey);
    setMessage('');
    renderComposer();
    try {
      const result = await submit(new URLSearchParams(fields));
      if (draft && drafts.get(documentKey) === draft) keepDraft(null, documentKey);
      if (key === documentKey && revision === source && result?.source_revision === revision) update(result);
    } catch (error) {
      setMessage(error.message || 'Could not save the comment.', documentKey);
    } finally {
      pending.delete(documentKey);
      if (key === documentKey) renderComposer();
    }
  }
  body.addEventListener('input', () => {
    const draft = getDraft();
    if (draft) draft.body = body.value;
    keepDraft(draft);
  });
  composer.addEventListener('submit', event => {
    event.preventDefault();
    const draft = getDraft();
    if (save.disabled || !draft) return;
    if ([...draft.body].length > 10000) {
      setMessage('Comments can contain at most 10,000 characters.');
      return;
    }
    send(draft, draft);
  });
  composer.addEventListener('keydown', event => {
    if (event.key === 'Escape') {
      event.preventDefault();
      cancel.click();
    } else if (event.key === 'Enter' && (event.metaKey || event.ctrlKey)) {
      event.preventDefault();
      composer.requestSubmit();
    }
  });

  function boxes(range) {
    return Array.from(range.getClientRects()).filter(rect => rect.width && rect.height);
  }
  function paintFallback() {
    if (globalThis.CSS?.highlights && globalThis.Highlight) return;
    fallback.replaceChildren();
    for (const {range} of ranges) {
      for (const rect of boxes(range)) {
        const overlay = element('div');
        Object.assign(overlay.style, {
          left: rect.left + 'px', top: rect.top + 'px',
          width: rect.width + 'px', height: rect.height + 'px'
        });
        fallback.append(overlay);
      }
    }
  }
  function repaint() {
    const points = review?.source_revision === revision ? markviewReviewText(root) : [];
    ranges = [];
    for (const entry of review?.annotations || []) {
      if (entry.status !== 'open' || !entry.range) continue;
      const range = markviewReviewRange(points, entry.range);
      if (range) ranges.push({id: entry.id, range});
    }
    if (globalThis.CSS?.highlights && globalThis.Highlight) {
      CSS.highlights.delete('markview-annotations');
      if (ranges.length) CSS.highlights.set('markview-annotations', new Highlight(...ranges.map(entry => entry.range)));
    } else paintFallback();
  }

  function render() {
    const entries = review?.annotations || [];
    const openCount = entries.filter(entry => entry.status === 'open').length;
    panel.hidden = !key || (!writable && !entries.length && !errors.get(key));
    whole.hidden = !writable || !canAdd;
    resolvedToggle.checked = Boolean(showResolved.get(key));
    resolvedLabel.hidden = !entries.some(entry => entry.status === 'resolved');
    count.textContent = openCount + (openCount === 1 ? ' open comment' : ' open comments');
    const focused = document.activeElement?.closest('.markview-review-card')?.dataset.annotation;
    cards.replaceChildren();
    for (const entry of entries) {
      if (entry.status === 'resolved' && !resolvedToggle.checked) continue;
      const card = element('article', 'markview-review-card');
      card.tabIndex = 0;
      card.dataset.annotation = entry.id;
      const line = entry.lines ? (entry.lines[0] === entry.lines[1] ? 'Line ' + entry.lines[0] : 'Lines ' + entry.lines.join('–')) : 'Document';
      const label = [entry.id, line, entry.section, !entry.attached && 'Detached', entry.status === 'resolved' && 'Resolved'].filter(Boolean);
      card.append(element('div', 'markview-review-label', label.join(' · ')));
      if (entry.quote) card.append(element('blockquote', '', entry.quote));
      card.append(element('p', 'markview-review-body', entry.body));
      const navigate = () => {
        const hit = ranges.find(item => item.id === entry.id);
        if (hit) hit.range.startContainer.parentElement.scrollIntoView({block: 'center'});
      };
      card.addEventListener('click', event => { if (!event.target.closest('button')) navigate(); });
      card.addEventListener('keydown', event => { if (event.target === card && event.key === 'Enter') navigate(); });
      if (writable) {
        const cardActions = element('div', 'markview-review-actions');
        const fields = {id: entry.id, store_revision: review.store_revision};
        const nextStatus = entry.status === 'open' ? 'resolved' : 'open';
        cardActions.append(
          button('Edit', () => startDraft({op: 'edit', ...fields, body: entry.body})),
          button(entry.status === 'open' ? 'Resolve' : 'Reopen', () => send({op: 'status', ...fields, status: nextStatus})),
          button('Delete', () => { if (window.confirm('Delete this comment?')) send({op: 'delete', ...fields}); })
        );
        card.append(cardActions);
      }
      cards.append(card);
      if (focused === entry.id) card.focus({preventScroll: true});
    }
    renderComposer();
    repaint();
  }
  resolvedToggle.addEventListener('change', () => {
    showResolved.set(key, resolvedToggle.checked);
    render();
  });
  root.addEventListener('click', event => {
    if (event.target.closest('a') || !window.getSelection().isCollapsed) return;
    const hits = ranges.filter(({range}) => boxes(range).some(rect =>
      event.clientX >= rect.left && event.clientX <= rect.right && event.clientY >= rect.top && event.clientY <= rect.bottom));
    const hit = hits[(hits.findIndex(hit => hit.id === lastHit) + 1) % hits.length];
    lastHit = hit?.id;
    if (hit) {
      const card = Array.from(cards.children).find(card => card.dataset.annotation === hit.id);
      if (card) { reveal(); card.focus({preventScroll: true}); card.scrollIntoView({block: 'nearest'}); }
    }
  });
  document.addEventListener('selectionchange', () => {
    if (!key || !canAdd) return;
    selected = markviewReviewSelection(root, window.getSelection());
    selectionButton.hidden = !selected;
    if (selected) {
      const rect = window.getSelection().getRangeAt(0).getBoundingClientRect();
      selectionButton.style.left = Math.max(4, Math.min(rect.right, window.innerWidth - 95)) + 'px';
      selectionButton.style.top = Math.max(4, rect.top - 34) + 'px';
    }
  });
  document.addEventListener('scroll', () => { selectionButton.hidden = true; paintFallback(); }, true);
  window.addEventListener('resize', paintFallback);

  function update(data) {
    if (!key || data.source_revision !== revision) return;
    if (review?.store_revision === data.store_revision) { repaint(); return; }
    review = data;
    render();
  }
  return {
    show(documentKey, nextRevision, write, add) {
      if (key === documentKey && revision === nextRevision && writable === write && canAdd === add) return;
      if (key !== documentKey || revision !== nextRevision) review = null;
      key = documentKey;
      revision = nextRevision;
      writable = write;
      canAdd = add;
      selected = null;
      selectionButton.hidden = true;
      render();
    },
    update,
    repaint,
    addSelection,
    unavailable(message) {
      review = null;
      setMessage(message);
      render();
    }
  };
}

// Transport for `markview --serve`: reads the annotations of one served route
// and posts changes with the page's write token (null on a read-only page).
function markviewServeReview(route, token, port) {
  const endpoint = '/_markview/annotations';
  const root = document.querySelector('main');
  const review = createMarkviewReview(root, document.body, async (form) => {
    form.set('doc', route);
    const response = await fetch(endpoint, {method: 'POST', headers: {'X-Markview-Token': token}, body: form});
    const data = await response.json();
    if (!response.ok) {
      if (response.status === 409) window.markviewReviewRefresh();
      throw new Error(data.error);
    }
    window.markviewReviewRefresh();
    return data;
  });
  let requested = 0;
  window.markviewReviewRefresh = async () => {
    const sequence = ++requested;
    const revision = root.dataset.sourceRevision;
    review.show(route, revision, Boolean(token), Boolean(token));
    try {
      const response = await fetch(endpoint + '?doc=' + encodeURIComponent(route), {cache: 'no-store'});
      if (response.status === 403 && sequence === requested) {
        // The server answers `/_markview/` only for its loopback Host.
        review.unavailable('Open this page at localhost:' + port + ' to review.');
        return;
      }
      const data = await response.json();
      if (sequence === requested && response.ok && revision === data.source_revision) review.update(data);
    } catch { /* Live reload will retry on the next change. */ }
  };
  window.markviewReviewRoute = route;
  return window.markviewReviewRefresh();
}

if (typeof module === 'object') {
  module.exports = {markviewReviewText, markviewReviewRange, markviewReviewSelection};
}
