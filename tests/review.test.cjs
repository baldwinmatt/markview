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
