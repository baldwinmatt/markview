// Run from the rendered browser fixture produced by the Rust serve test.
(async () => {
  const checks = document.createElement('output');
  checks.style.display = 'block';
  document.body.prepend(checks);
  try {
    const require = (condition, message) => { if (!condition) throw new Error(message); };
    const root = document.querySelector('main');
    const points = markviewReviewText(root);
    const text = points.map((point, index) => (index && point.gap ? ' ' : '') + point.scalar).join('');
    require(text === expectedText, 'DOM and Rust projection differ: ' + text);
    require(!window.untrustedScriptRan, 'The policy allowed an untrusted script');
    require(typeof window.markviewReviewRefresh === 'function', 'The Markview script did not run');
    const emoji = points.findIndex(point => point.scalar === '😀');
    const emojiRange = markviewReviewRange(points, [emoji, emoji + 1]);
    require(emojiRange.toString() === '😀', 'UTF-16 offsets split the supplementary scalar');
    const selection = window.getSelection();
    selection.removeAllRanges();
    selection.addRange(emojiRange);
    require(markviewReviewSelection(root, selection).quote === '😀', 'DOM selection did not round-trip');
    selection.removeAllRanges();

    const alert = root.querySelector('.markdown-alert-note');
    const title = alert.querySelector('.markdown-alert-title');
    const alertRange = document.createRange();
    alertRange.selectNodeContents(title);
    selection.addRange(alertRange);
    require(!markviewReviewSelection(root, selection), 'Generated alert title was annotatable');
    selection.removeAllRanges();
    alertRange.setEndAfter(alert.lastElementChild);
    selection.addRange(alertRange);
    require(markviewReviewSelection(root, selection).quote === 'Alert prose.', 'Selecting across an alert title changed the quote');
    selection.removeAllRanges();

    const copyButtons = [...root.querySelectorAll('.markview-copy-code')];
    const payloads = [...root.querySelectorAll('pre > code')].map(code => code.textContent);
    require(JSON.stringify(payloads) === JSON.stringify(['-old\n+new\n', '\tlet ready = true;\n', 'indented & text\n']),
      'Code text changed after adding buttons: ' + JSON.stringify(payloads));
    require(copyButtons.length === payloads.length && copyButtons.every(btn => btn.tabIndex === 0), 'Code copy buttons are missing');
    let copied = '';
    Object.defineProperty(navigator, 'clipboard', {configurable: true, value: {writeText: async text => { copied = text; }}});
    copyButtons[1].focus();
    copyButtons[1].click();
    await Promise.resolve();
    require(copyButtons[1].textContent === 'Copied' && copied === payloads[1] &&
      document.activeElement === copyButtons[1], 'Clipboard copy changed the payload or focus');
    Object.defineProperty(navigator, 'clipboard', {configurable: true, value: {writeText: async () => { throw new Error('blocked'); }}});
    copyButtons[2].click();
    await Promise.resolve();
    require(copyButtons[2].textContent === 'Copy failed', 'Clipboard failure was not reported');
    window.markviewAttachCodeCopy();
    require(root.querySelectorAll('.markview-copy-code').length === copyButtons.length, 'Copy buttons duplicated on reattach');
    const extra = document.createElement('pre');
    extra.append(document.createElement('code'));
    root.append(extra);
    window.markviewAttachCodeCopy();
    require(extra.parentElement.querySelector('.markview-copy-code'), 'Refreshed code block lacks a copy button');
    extra.parentElement.remove();

    document.querySelector('.markview-review-toolbar label:last-child input').click();
    const mount = document.createElement('aside');
    document.body.append(mount);
    let submitted = null;
    const entry = (id, range) => ({id, status: 'open', body: '<b>literal</b>\nnext', lines: [3, 3], range, attached: true});
    const revision = 'test-source';
    const data = {source_revision: revision, store_revision: 'test-store', annotations: [
      entry('a1', [emoji - 2, emoji + 1]), entry('a2', [emoji, emoji + 1]),
      {...entry('a3', null), attached: false}, {...entry('a4', null), status: 'resolved'}
    ]};
    const preview = root.firstElementChild;
    const review = createMarkviewReview(root, mount, async fields => {
      submitted = Object.fromEntries(fields);
      throw new Error('Expected test conflict');
    });
    review.show('browser-fixture', revision, true, true);
    review.update(data);
    require(root.firstElementChild === preview, 'Painting replaced the document DOM');
    require(CSS.highlights.get('markview-annotations').size === 2, 'Overlapping ranges were not retained');
    require(mount.querySelector('.markview-review-body').textContent === '<b>literal</b>\nnext', 'Comment text was not preserved');
    require(!mount.querySelector('.markview-review-body b'), 'Comment text was interpreted as HTML');

    const press = (key, target = document.activeElement, extra = {}) => {
      const event = new KeyboardEvent('keydown', {key, bubbles: true, cancelable: true, ...extra});
      target.dispatchEvent(event);
      return event.defaultPrevented;
    };
    const focused = () => document.activeElement.dataset.annotation;
    require(press('k') && focused() === 'a3', 'K did not start on the last detached card');
    require(press('j') && focused() === 'a1', 'J did not wrap or move keyboard focus');
    const [showResolved, shortcuts] = mount.querySelectorAll('input');
    showResolved.click();
    document.body.tabIndex = -1;
    document.body.focus();
    require(press('k') && focused() === 'a3', 'Resolved cards displaced the detached cards');
    require(press('k') && focused() === 'a4', 'Resolved card was not visible to keyboard navigation');
    shortcuts.click();
    require(!press('j') && !press('c'), 'Shortcuts still ran while disabled');
    shortcuts.click();
    for (const options of [{isComposing: true}, {repeat: true}, {shiftKey: true}, {ctrlKey: true}]) {
      require(!press('j', document.body, options), 'A composing, repeated, or modified event was consumed');
    }
    const dialog = document.createElement('dialog');
    document.body.append(dialog);
    dialog.showModal();
    require(!press('c', dialog), 'A shortcut ran with a dialog open');
    dialog.close();
    dialog.remove();
    const firstCard = mount.querySelector('.markview-review-card');
    firstCard.focus();
    require(press('e'), 'E did not open the focused comment for editing');
    const draftField = mount.querySelector('textarea');
    require(document.activeElement === draftField && !press('j'), 'A shortcut ran in the active text field');
    require(press('Escape') && document.activeElement === firstCard, 'Escape did not restore card focus');
    review.show('browser-fixture', revision, false, false);
    require(!press('c') && !press('e') && !press('r'), 'Read-only annotation writes were enabled');
    review.show('browser-fixture', revision, true, false);
    require(!press('c') && press('e'), 'Dirty Preview permissions were not respected');
    require(press('Escape'), 'Dirty Preview editor did not cancel');
    review.show('browser-fixture', revision, true, true);
    document.body.focus();
    require(press('c'), 'C did not open a whole-document draft');
    const editor = mount.querySelector('textarea');
    const labelButtons = [...mount.querySelectorAll('[aria-label="Quick labels"] button')];
    require(labelButtons.length === 4 && labelButtons.every(button => button.tabIndex === 0), 'Quick labels are missing or not keyboard-accessible');
    labelButtons[0].click();
    await Promise.resolve();
    require(submitted.body === 'Question' && editor.value === 'Question', 'A blank labelled comment did not submit and preserve the draft');
    editor.value = 'Fixture draft';
    editor.dispatchEvent(new Event('input'));
    labelButtons[1].click();
    labelButtons[3].click();
    labelButtons[1].click();
    require(document.activeElement === editor, 'Label activation did not restore textarea focus');
    mount.querySelector('form').requestSubmit();
    await Promise.resolve();
    require(submitted.body === 'Nit: Needs a test: Fixture draft', 'The composer sent the wrong payload');
    require(editor.value === 'Nit: Needs a test: Fixture draft', 'The composer lost its draft on conflict');
    review.update({...data, store_revision: 'next-store'});
    require(root.firstElementChild === preview && editor.value === 'Nit: Needs a test: Fixture draft', 'A store refresh replaced the preview or draft');
    review.show(null, null, false, false);
    const bar = document.querySelector('.markview-verdict');
    const [approve, changes] = bar.querySelectorAll('button');
    const response = (data, status = 200) => ({
      ok: status === 200, status, json: async () => data
    });
    let summary = {comments: 0, documents: 0, review_revision: 'empty'};
    let conflict = true;
    const confirmations = [];
    const finishes = [];
    window.confirm = text => { confirmations.push(text); return true; };
    window.fixtureFetch = async (path, options) => {
      if (path === '/_markview/review' && options?.method === 'POST') {
        finishes.push(Object.fromEntries(options.body));
        return conflict ? response({error: 'Stale'}, 409) : response({finished: true});
      }
      if (path === '/_markview/review') return response(summary);
      if (path.startsWith('/_markview/annotations')) return response({});
      return {ok: true, text: async () => '<main data-markview-route="/review.md" data-source-revision="new-source"><p>Updated</p></main>'};
    };
    await window.markviewVerdict.refresh();
    require(!approve.disabled && changes.disabled, 'An empty review offered Request changes');
    summary = {comments: 2, documents: 1, review_revision: 'saved'};
    window.fixtureEvents.dispatchEvent(new MessageEvent('annotations', {data: '/other.md'}));
    const settle = () => new Promise(requestAnimationFrame);
    await settle();
    require(!changes.disabled && bar.textContent.includes('2 open comments in 1 document'), 'Another page did not refresh collection counts');

    const realFetch = window.fixtureFetch;
    window.fixtureFetch = async () => response({error: 'Read failure'}, 500);
    await window.markviewVerdict.refresh();
    const retrySummary = bar.querySelector('button:last-child');
    require(approve.disabled && changes.disabled && !retrySummary.hidden,
      'An unreadable collection allowed a verdict or lacked a retry');
    window.fixtureFetch = realFetch;
    retrySummary.click();
    await settle();
    document.querySelector('.markview-review-toolbar button').click();
    require(!document.querySelector('.markview-review-composer').hidden, 'The saved draft warning could not be exercised');
    changes.click();
    await settle();
    require(confirmations[0].includes('entire served collection') && confirmations[0].includes('unsaved comment draft'),
      'Verdict confirmation omitted its scope or unsaved draft');
    require(finishes[0].review_revision === 'saved' && finishes[0].verdict === 'changes', 'The confirmed revision was not posted');
    require(bar.textContent.includes('Confirm the verdict again') && !changes.disabled,
      'A conflict did not refresh the review and require confirmation');
    conflict = false;
    changes.click();
    await settle();
    require(confirmations.length === 2 && finishes[1].source_revision === 'new-source',
      'The verdict was not reconfirmed against the refreshed page');
    window.fixtureEvents.dispatchEvent(new MessageEvent('review-finished'));
    require(approve.disabled && changes.disabled && document.querySelector('.markview-review-toolbar button').hidden &&
      bar.textContent.includes('Review finished. You can close this tab.'), 'Finished review left write controls enabled');
    checks.textContent = 'PASS — Projection, nonce policy, alerts, copy, drafts, shortcuts, and collection verdicts';
  } catch (error) {
    checks.textContent = 'FAIL — ' + error.message;
  }
})();
