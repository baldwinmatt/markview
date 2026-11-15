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

    const mount = document.createElement('aside');
    document.body.append(mount);
    let submitted = null;
    const entry = (id, range) => ({id, status: 'open', body: '<b>literal</b>\nnext', lines: [3, 3], range, attached: true});
    const revision = 'test-source';
    const data = {source_revision: revision, store_revision: 'test-store', annotations: [entry('a1', [emoji - 2, emoji + 1]), entry('a2', [emoji, emoji + 1])]};
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

    mount.querySelector('.markview-review-toolbar button').click();
    const editor = mount.querySelector('textarea');
    editor.value = 'Fixture draft';
    editor.dispatchEvent(new Event('input'));
    mount.querySelector('form').requestSubmit();
    await Promise.resolve();
    require(submitted.body === 'Fixture draft', 'The composer sent the wrong payload');
    require(editor.value === 'Fixture draft', 'The composer lost its draft on conflict');
    review.update({...data, store_revision: 'next-store'});
    require(root.firstElementChild === preview && editor.value === 'Fixture draft', 'A store refresh replaced the preview or draft');
    review.show(null, null, false, false);
    checks.textContent = 'PASS — DOM projection, nonce policy, Unicode selection, overlaps, and draft retention';
  } catch (error) {
    checks.textContent = 'FAIL — ' + error.message;
  }
})();
