// Text offsets count non-White_Space Unicode scalars, while DOM offsets
// count UTF-16 code units. Structural breaks are only used in displayed quotes.
const markviewReviewWhitespace = /\p{White_Space}/u;
const markviewReviewExcluded = 'img,input,.footnote-reference,.footnote-definition-label,[data-markview-review-exclude]';
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

// `points` is the result of markviewReviewText(root) when the caller has it.
function markviewReviewSelection(root, selection, points) {
  if (!selection || selection.rangeCount !== 1 || selection.isCollapsed) return null;
  const range = selection.getRangeAt(0);
  if (!root.contains(range.startContainer) || !root.contains(range.endContainer)) return null;
  points ??= markviewReviewText(root);
  // Points are in document order, so the selected ones form a single run.
  // Find its two ends by bisection: asking the range about every point is
  // quadratic in a large document, because each comparison walks the tree.
  const firstIndex = test => {
    let low = 0;
    let high = points.length;
    while (low < high) {
      const middle = (low + high) >> 1;
      if (test(points[middle])) high = middle;
      else low = middle + 1;
    }
    return low;
  };
  const start = firstIndex(point => range.comparePoint(point.node, point.offset) >= 0);
  const end = firstIndex(point => range.comparePoint(point.node, point.end) > 0);
  if (start >= end) return null;
  let quote = '';
  for (let index = start; index < end; index++) {
    if (index > start && points[index].gap) quote += ' ';
    quote += points[index].scalar;
  }
  return {quote, start, end};
}

function createMarkviewReview(root, mount, submit, {
  reveal = () => {}, shortcuts = true, saveShortcuts = () => {}, shortcutsBlocked = () => false
} = {}) {
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
  // The in-page stand-in for window.confirm: a question and two buttons. The
  // GUI's WKWebView has no dialog delegate, so a native dialog there answers
  // "no" without ever being shown.
  function confirmation(question, confirmLabel, cancelLabel, onConfirm, onCancel) {
    const prompt = element('div', 'markview-review-confirm');
    prompt.setAttribute('role', 'group');
    prompt.setAttribute('aria-label', question);
    const confirm = button(confirmLabel, onConfirm);
    prompt.append(element('span', '', question), confirm, button(cancelLabel, onCancel));
    prompt.addEventListener('keydown', event => {
      if (event.key !== 'Escape') return;
      event.preventDefault();
      event.stopPropagation();
      onCancel();
    });
    return {prompt, confirm};
  }
  const panel = element('section', 'markview-review');
  panel.setAttribute('aria-label', 'Annotations');
  panel.tabIndex = -1;
  const toolbar = element('header', 'markview-review-toolbar');
  const count = element('strong');
  const whole = button('Comment on document', () => startDraft({op: 'add'}, {control: whole}));
  const resolvedLabel = element('label');
  const resolvedToggle = element('input');
  resolvedToggle.type = 'checkbox';
  resolvedLabel.append(resolvedToggle, ' Show resolved');
  const shortcutsLabel = element('label');
  const shortcutsToggle = element('input');
  shortcutsToggle.type = 'checkbox';
  shortcutsToggle.checked = shortcuts;
  shortcutsLabel.title = 'J/K navigate, C comments, E edits, R resolves or reopens';
  shortcutsLabel.append(shortcutsToggle, ' Single-key shortcuts');
  shortcutsToggle.addEventListener('change', () => saveShortcuts(shortcutsToggle.checked));
  whole.title = 'Comment on document (C)';
  const composer = element('form', 'markview-review-composer');
  const quote = element('blockquote');
  const labels = element('div', 'markview-review-actions');
  labels.setAttribute('role', 'group');
  labels.setAttribute('aria-label', 'Quick labels');
  const quickLabels = ['Question', 'Needs a test', 'Out of scope', 'Nit'];
  for (const label of quickLabels) {
    labels.append(button(label, () => applyLabel(label)));
  }
  const body = element('textarea');
  body.setAttribute('aria-label', 'Comment');
  body.placeholder = 'Write a comment';
  const actions = element('div', 'markview-review-actions');
  const save = element('button', '', 'Save comment');
  save.type = 'submit';
  const cancel = button('Cancel', () => cancelDraft());
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
  selectionButton.setAttribute('title', 'Comment on the selection (C or Ctrl/Cmd+Shift+K)');
  selectionButton.setAttribute('aria-keyshortcuts', 'Control+Shift+K Meta+Shift+K');
  selectionButton.addEventListener('pointerdown', event => event.preventDefault());
  // Shown in place of the Comment button when the selection cannot be used,
  // so the reason appears where the reader is looking.
  const selectionNote = element('div', 'markview-review-selection-note', 'Select at most 1,000 characters to comment.');
  selectionNote.setAttribute('role', 'status');
  actions.append(save, cancel, retry);
  composer.append(quote, labels, body, actions);
  toolbar.append(count, whole, resolvedLabel, shortcutsLabel);
  panel.append(toolbar, composer, message, cards);
  mount.append(panel);
  document.body.append(selectionButton, selectionNote);
  const fallback = element('div', 'markview-review-overlay');
  fallback.setAttribute('aria-hidden', 'true');
  document.body.append(fallback);
  panel.hidden = selectionButton.hidden = selectionNote.hidden = true;

  let key = null;
  let revision = null;
  let writable = false;
  let canAdd = false;
  let review = null;
  let selected = null;
  let ranges = [];
  let lastHit = null;
  // The card whose Delete awaits confirmation, and the control to focus once
  // the cards are rebuilt: {id, label}.
  let deleting = null;
  let focusNext = null;
  // A pending question about discarding the open draft: {next, opener} when
  // another draft should replace it, {next: null} when Escape would drop it.
  let discarding = null;
  let discardPrompt = null;
  // What opened the current draft, to give focus back when it closes:
  // {control} for a fixed button, {id, label} for a button on a card, and
  // {id, label: null} for the card itself.
  let opener = null;
  // The text points of `root`, kept until the document changes. Walking the
  // whole document on every `selectionchange` makes dragging a selection slow.
  let points = null;
  const observer = typeof MutationObserver === 'function' ? new MutationObserver(() => { points = null; }) : null;
  observer?.observe(root, {childList: true, subtree: true, characterData: true});
  function textPoints() {
    // Changes made earlier in this task have not reached the callback yet.
    if (observer?.takeRecords().length) points = null;
    return points ??= markviewReviewText(root);
  }
  const drafts = new Map();
  const cardActionsByElement = new WeakMap();
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
  function startDraft(fields, from = null, discard = false) {
    if (!writable || (fields.op === 'add' && !canAdd) || pending.has(key)) return false;
    const previous = getDraft();
    const reuseBody = previous?.op === 'add' && fields.op === 'add';
    if (previous && !reuseBody && !discard) {
      discarding = {next: fields, opener: from};
      renderComposer();
      reveal();
      return true;
    }
    discarding = null;
    opener = from;
    keepDraft({body: reuseBody ? previous.body : '', ...fields});
    setMessage('');
    renderComposer();
    reveal();
    if (panel.getClientRects().length) composer.scrollIntoView({block: 'nearest'});
    body.focus();
    return true;
  }
  // Whether the current draft holds text that cancelling would lose.
  function draftHasText() {
    const draft = getDraft();
    if (!draft) return false;
    if (draft.op !== 'edit') return draft.body.trim() !== '';
    return draft.body !== review?.annotations.find(card => card.id === draft.id)?.body;
  }
  function cancelDraft() {
    discarding = null;
    keepDraft(null);
    setMessage('');
    renderComposer();
    restoreFocus();
  }
  // Returns focus to whatever opened the draft. Cards are rebuilt when the
  // list changes, so a card button is found again by its card and label.
  function restoreFocus() {
    const from = opener;
    opener = null;
    if (from?.control) from.control.focus();
    else if (from?.id) {
      const card = Array.from(cards.children).find(card => card.dataset.annotation === from.id);
      const control = card && Array.from(card.querySelectorAll('button')).find(button => button.textContent === from.label);
      (control || card)?.focus();
    }
  }
  function tooLong(selection) { return [...selection.quote].length > 1000; }
  // Places the Comment button, or the note that replaces it, at the selection.
  function showSelection(selection) {
    selectionButton.hidden = !selection || tooLong(selection);
    selectionNote.hidden = !selection || !tooLong(selection);
    if (!selection) return;
    const rect = window.getSelection().getRangeAt(0).getBoundingClientRect();
    for (const node of [selectionButton, selectionNote]) {
      node.style.left = Math.max(4, Math.min(rect.right, window.innerWidth - 95)) + 'px';
      node.style.top = Math.max(4, rect.top - 34) + 'px';
    }
  }
  function addSelection(selection = markviewReviewSelection(root, window.getSelection(), textPoints()) || selected) {
    if (!key || !canAdd || !selection || pending.has(key)) return false;
    if (tooLong(selection)) {
      showSelection(selection);
      return true;
    }
    showSelection(null);
    selected = null;
    return startDraft({op: 'add', ...selection, source_revision: revision});
  }
  function renderComposer() {
    const draft = getDraft();
    composer.hidden = !writable || !draft;
    refreshMessage();
    if (!draft || !writable) discarding = null;
    if (!discarding && discardPrompt) {
      discardPrompt.remove();
      discardPrompt = null;
    } else if (discarding && !discardPrompt) {
      const {prompt, confirm} = confirmation('Discard the current comment draft?', 'Discard draft', 'Keep draft',
        () => {
          const {next, opener: from} = discarding;
          if (next) startDraft(next, from, true);
          else cancelDraft();
        },
        () => {
          discarding = null;
          renderComposer();
          body.focus();
        });
      discardPrompt = prompt;
      composer.append(prompt);
      confirm.focus();
    }
    if (!draft || !writable) return;
    labels.hidden = draft.op !== 'add';
    labels.querySelectorAll('button').forEach(label => { label.disabled = pending.has(key); });
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
      if (draft && drafts.get(documentKey) === draft) {
        keepDraft(null, documentKey);
      }
      if (key === documentKey && revision === source && result?.source_revision === revision) update(result);
      if (draft && key === documentKey) {
        renderComposer();
        restoreFocus();
      }
    } catch (error) {
      setMessage(error.message || 'Could not save the comment.', documentKey);
    } finally {
      pending.delete(documentKey);
      if (key === documentKey) {
        renderComposer();
      }
    }
  }
  body.addEventListener('input', () => {
    const draft = getDraft();
    if (draft) draft.body = body.value;
    keepDraft(draft);
  });
  function hasLabel(text, label) {
    while (text) {
      if (text === label) return true;
      const prefix = quickLabels.map(item => item + ': ').find(item => text.startsWith(item));
      if (!prefix) return false;
      if (prefix === label + ': ') return true;
      text = text.slice(prefix.length);
    }
    return false;
  }
  function applyLabel(label) {
    const draft = getDraft();
    if (!writable || !canAdd || pending.has(key) || draft?.op !== 'add') return;
    const blank = !draft.body.trim();
    if (blank) draft.body = label;
    else if (!hasLabel(draft.body, label)) draft.body = label + ': ' + draft.body;
    keepDraft(draft);
    renderComposer();
    body.focus();
    if (blank) submitDraft();
  }
  function submitDraft() {
    const draft = getDraft();
    if (save.disabled || !draft) return;
    if ([...draft.body].length > 10000) {
      setMessage('Comments can contain at most 10,000 characters.');
      return;
    }
    send(draft, draft);
  }
  composer.addEventListener('submit', event => {
    event.preventDefault();
    submitDraft();
  });
  composer.addEventListener('keydown', event => {
    if (!event.defaultPrevented && event.key === 'Escape') {
      // An input method uses Escape to close its candidate window.
      if (event.isComposing || event.keyCode === 229) return;
      if (cancel.disabled) return;
      event.preventDefault();
      if (draftHasText()) {
        discarding = {next: null};
        renderComposer();
      } else cancelDraft();
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
    const current = review?.source_revision === revision ? textPoints() : [];
    ranges = [];
    for (const entry of review?.annotations || []) {
      if (entry.status !== 'open' || !entry.range) continue;
      const range = markviewReviewRange(current, entry.range);
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
    const active = document.activeElement;
    const focused = active?.closest('.markview-review-card')?.dataset.annotation;
    const focusedLabel = active?.tagName === 'BUTTON' ? active.textContent : null;
    if (!writable || !entries.some(entry => entry.id === deleting)) deleting = null;
    cards.replaceChildren();
    for (const entry of [...entries].sort((a, b) => Number(b.attached) - Number(a.attached))) {
      if (entry.status === 'resolved' && !resolvedToggle.checked) continue;
      const card = element('article', 'markview-review-card');
      card.tabIndex = 0;
      card.dataset.annotation = entry.id;
      // Every card has the same buttons, so each one names its comment.
      const about = 'comment ' + entry.id;
      const excerpt = entry.quote && (Array.from(entry.quote).length > 60 ? Array.from(entry.quote).slice(0, 59).join('') + '…' : entry.quote);
      card.setAttribute('aria-label', 'Comment ' + entry.id + (excerpt ? ' on “' + excerpt + '”' : ' on the document'));
      const cardButton = (label, purpose, action) => {
        const node = button(label, action);
        node.setAttribute('aria-label', label + ' ' + purpose);
        return node;
      };
      const line = entry.lines ? (entry.lines[0] === entry.lines[1] ? 'Line ' + entry.lines[0] : 'Lines ' + entry.lines.join('–')) : 'Document';
      const label = [entry.id, line, entry.section, !entry.attached && 'Detached', entry.status === 'resolved' && 'Resolved'].filter(Boolean);
      card.append(element('div', 'markview-review-label', label.join(' · ')));
      if (entry.quote) card.append(element('blockquote', '', entry.quote));
      card.append(element('p', 'markview-review-body', entry.body));
      const navigate = () => scrollToAnnotation(card, entry);
      card.addEventListener('click', event => { if (!event.target.closest('button')) navigate(); });
      card.addEventListener('keydown', event => { if (!event.defaultPrevented && event.target === card && event.key === 'Enter') { navigate(); event.preventDefault(); } });
      const controls = {};
      if (writable && deleting === entry.id) {
        const fields = {id: entry.id, store_revision: review.store_revision};
        const {prompt, confirm} = confirmation('Delete this comment?', 'Confirm', 'Cancel',
          () => {
            deleting = null;
            send({op: 'delete', ...fields});
          },
          () => {
            deleting = null;
            focusNext = {id: entry.id, label: 'Delete'};
            render();
          });
        for (const node of prompt.querySelectorAll('button')) node.setAttribute('aria-label', node.textContent + ' deleting ' + about);
        controls.Confirm = confirm;
        card.append(prompt);
      } else if (writable) {
        const cardActions = element('div', 'markview-review-actions');
        const fields = {id: entry.id, store_revision: review.store_revision};
        const nextStatus = entry.status === 'open' ? 'resolved' : 'open';
        const statusLabel = entry.status === 'open' ? 'Resolve' : 'Reopen';
        const editDraft = () => startDraft({op: 'edit', ...fields, body: entry.body}, {id: entry.id, label: 'Edit'});
        const changeStatus = () => send({op: 'status', ...fields, status: nextStatus});
        cardActionsByElement.set(card, {editDraft, changeStatus});
        controls.Edit = cardButton('Edit', about, editDraft);
        controls.Edit.title = 'Edit (E)';
        controls[statusLabel] = cardButton(statusLabel, about, changeStatus);
        controls[statusLabel].title = statusLabel + ' (R)';
        controls.Delete = cardButton('Delete', about, () => {
          deleting = entry.id;
          focusNext = {id: entry.id, label: 'Confirm'};
          render();
        });
        cardActions.append(controls.Edit, controls[statusLabel], controls.Delete);
        card.append(cardActions);
      }
      cards.append(card);
      if (focusNext?.id === entry.id && controls[focusNext.label]) controls[focusNext.label].focus();
      else if (focused === entry.id) (controls[focusedLabel] || card).focus({preventScroll: true});
    }
    focusNext = null;
    renderComposer();
    repaint();
  }
  function cardById(id) {
    return Array.from(cards.children).find(card => card.dataset.annotation === id);
  }
  function scrollToAnnotation(card, entry) {
    const range = ranges.find(item => item.id === entry.id)?.range ||
      (entry.range && markviewReviewRange(markviewReviewText(root), entry.range));
    if (range) range.startContainer.parentElement.scrollIntoView({block: 'center'});
    else card.scrollIntoView({block: 'nearest'});
  }
  window.addEventListener('keydown', event => {
    if (!key || panel.hidden || deleting || discarding || event.defaultPrevented || event.isComposing || event.keyCode === 229 || event.repeat ||
        event.altKey || event.ctrlKey || event.metaKey || event.shiftKey ||
        event.target?.closest?.('input, textarea, select, [role="textbox"]') || event.target?.isContentEditable ||
        document.querySelector('dialog[open], [aria-modal="true"]') || shortcutsBlocked()) return;
    const active = document.activeElement;
    const card = active?.closest('.markview-review-card');
    const focused = cards.contains(card) ? card : null;
    if (event.key === 'Escape' && focused) {
      panel.focus({preventScroll: true});
    } else if (!shortcutsToggle.checked) {
      return;
    } else if (event.key === 'j' || event.key === 'k') {
      const visible = Array.from(cards.children);
      if (!visible.length) return;
      const index = visible.indexOf(focused);
      const next = visible[event.key === 'j' ? (index + 1) % visible.length :
        index <= 0 ? visible.length - 1 : index - 1];
      const entry = review.annotations.find(item => item.id === next.dataset.annotation);
      reveal();
      next.focus({preventScroll: true});
      scrollToAnnotation(next, entry);
    } else if (event.key === 'c' && writable && canAdd && !pending.has(key)) {
      const selection = window.getSelection();
      if (selection && !selection.isCollapsed) {
        if (!addSelection(markviewReviewSelection(root, selection, textPoints()))) return;
      } else {
        // A save rebuilds every card, so a card or its button is remembered
        // by comment and label. Only a fixed control is remembered itself.
        const from = focused
          ? {id: focused.dataset.annotation, label: active.tagName === 'BUTTON' ? active.textContent : null}
          : {control: active === document.body ? whole : active};
        if (!startDraft({op: 'add'}, from)) return;
      }
    } else if ((event.key === 'e' || event.key === 'r') && focused && writable && !pending.has(key)) {
      const actions = cardActionsByElement.get(focused);
      if (!actions) return;
      if (event.key === 'e') {
        if (!actions.editDraft()) return;
      } else actions.changeStatus();
    } else return;
    event.preventDefault();
  });
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
      const card = cardById(hit.id);
      if (card) { reveal(); card.focus({preventScroll: true}); card.scrollIntoView({block: 'nearest'}); }
    }
  });
  document.addEventListener('selectionchange', () => {
    if (!key || !canAdd) return;
    const selection = window.getSelection();
    selected = selection && !selection.isCollapsed ? markviewReviewSelection(root, selection, textPoints()) : null;
    showSelection(selected);
  });
  document.addEventListener('scroll', () => { showSelection(null); paintFallback(); }, true);
  window.addEventListener('resize', paintFallback);

  function update(data) {
    if (!key || data.source_revision !== revision) return;
    // A list that arrived supersedes what the last request complained about.
    const hadMessage = Boolean(errors.get(key));
    errors.delete(key);
    const same = review?.store_revision === data.store_revision && review.source_revision === data.source_revision;
    if (same && !hadMessage) { repaint(); return; }
    review = data;
    render();
  }
  return {
    show(documentKey, nextRevision, write, add) {
      if (key === documentKey && revision === nextRevision && writable === write && canAdd === add) return;
      if (key !== documentKey || revision !== nextRevision) points = null;
      // Another revision of the same document keeps its comments on screen
      // until the new list arrives, instead of flashing an empty panel.
      if (key !== documentKey) review = deleting = discarding = opener = null;
      key = documentKey;
      revision = nextRevision;
      writable = write;
      canAdd = add;
      selected = null;
      showSelection(null);
      render();
    },
    update,
    // For the host to call after it changed the document, for example to
    // mark find matches: the text nodes may have been split or replaced.
    repaint() {
      points = null;
      repaint();
    },
    addSelection,
    hasDraft: () => Boolean(getDraft()),
    setShortcuts: enabled => { shortcutsToggle.checked = enabled; },
    // The annotations cannot be read at all: show why instead of a list.
    unavailable(message) {
      review = null;
      setMessage(message);
      render();
    },
    // One request for them failed: say so and keep what is on screen.
    failed(message) {
      setMessage(message);
      render();
    }
  };
}

// Transport for `markview --serve`: reads the annotations of one served route
// and posts changes with the page's write token (null on a read-only page).
function markviewServeReview(route, token, port, blockingReview) {
  const endpoint = '/_markview/annotations';
  const root = document.querySelector('main');
  const review = createMarkviewReview(root, document.body, async (form) => {
    form.set('doc', root.dataset.markviewRoute || route);
    const response = await fetch(endpoint, {method: 'POST', headers: {'X-Markview-Token': token}, body: form});
    const data = await response.json();
    if (!response.ok) {
      if (response.status === 409) window.markviewReviewRefresh();
      throw new Error(data.error);
    }
    // The response is the updated list, so there is nothing to fetch again.
    return data;
  }, {
    shortcuts: (() => {
      try { return localStorage.getItem('markview-single-key-shortcuts') !== 'off'; }
      catch { return true; }
    })(),
    saveShortcuts: enabled => {
      try { localStorage.setItem('markview-single-key-shortcuts', enabled ? 'on' : 'off'); }
      catch { /* Preference storage is optional. */ }
    }
  });
  const verdict = blockingReview ? createMarkviewVerdict(root, review, token) : null;
  window.markviewVerdict = verdict;
  let requested = 0;
  window.markviewReviewRefresh = async () => {
    const sequence = ++requested;
    const revision = root.dataset.sourceRevision;
    const currentRoute = root.dataset.markviewRoute || route;
    const writable = Boolean(token) && !verdict?.finished();
    review.show(currentRoute, revision, writable, writable);
    try {
      const response = await fetch(endpoint + '?doc=' + encodeURIComponent(currentRoute), {cache: 'no-store'});
      if (sequence !== requested) return;
      if (response.status === 403) {
        // The server answers `/_markview/` only for its loopback Host.
        review.unavailable('Open this page at localhost:' + port + ' to review.');
        return;
      }
      const data = await response.json();
      if (sequence !== requested) return;
      if (!response.ok) review.unavailable(data.error || 'Could not load comments.');
      else if (revision === data.source_revision) review.update(data);
    } catch {
      if (sequence === requested) review.failed('Could not load comments. Reload the page to try again.');
    }
  };
  // The Comment button follows the pointer's selection. This is the same
  // action for the keyboard, with the shortcut the macOS app uses.
  document.addEventListener('keydown', event => {
    if (!(event.metaKey || event.ctrlKey) || !event.shiftKey || event.key.toLowerCase() !== 'k') return;
    event.preventDefault();
    review.addSelection();
  });
  window.markviewReviewRoute = route;
  return window.markviewReviewRefresh();
}

if (typeof module === 'object') {
  module.exports = {markviewReviewText, markviewReviewRange, markviewReviewSelection};
}
