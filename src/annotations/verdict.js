function createMarkviewVerdict(page, review, token) {
  const endpoint = '/_markview/review';
  const bar = document.createElement('section');
  bar.className = 'markview-verdict';
  bar.setAttribute('aria-label', 'Collection verdict');
  const scope = document.createElement('strong');
  scope.textContent = 'Review the entire served collection';
  const status = document.createElement('p');
  status.setAttribute('role', 'status');
  function button(label, action) {
    const node = document.createElement('button');
    node.type = 'button';
    node.textContent = label;
    node.addEventListener('click', action);
    return node;
  }
  const approve = button('Approve', () => finish('approved'));
  const changes = button('Request changes', () => finish('changes'));
  const retry = button('Retry review summary', () => refresh());
  bar.append(scope, status, approve, changes, retry);
  document.body.prepend(bar);
  let summary = null;
  let sequence = 0;
  let submitting = false;
  let completed = false;
  let reconfirm = false;

  function render(message, retryable = false) {
    approve.disabled = completed || submitting || !summary;
    changes.disabled = approve.disabled || !summary?.comments;
    retry.hidden = !retryable;
    status.textContent = message;
  }
  function counts() {
    const {comments, documents} = summary;
    return comments + (comments === 1 ? ' open comment' : ' open comments') +
      ' in ' + documents + (documents === 1 ? ' document' : ' documents');
  }
  function invalidate() {
    if (completed) return;
    sequence++;
    summary = null;
    render(submitting ? 'Submitting verdict…' : 'Refreshing collection counts…');
  }
  function done() {
    completed = true;
    summary = null;
    render('Review finished. You can close this tab.');
    review.show(page.dataset.markviewRoute, page.dataset.sourceRevision, false, false);
  }
  function unavailable() {
    if (completed) return;
    sequence++;
    summary = null;
    render('Could not refresh the review summary.', true);
  }
  async function refresh() {
    if (completed) return;
    invalidate();
    const request = sequence;
    try {
      const response = await fetch(endpoint, {cache: 'no-store'});
      const data = await response.json();
      if (completed || request !== sequence) return;
      if (!response.ok) throw new Error(data.error);
      if (data.finished) return done();
      summary = data;
      render(counts() + (reconfirm ? '. The review changed. Confirm the verdict again.' : '.'));
    } catch {
      if (!completed && request === sequence) unavailable();
    }
  }
  async function finish(verdict) {
    if (!summary || completed || submitting || (verdict === 'changes' && !summary.comments)) return;
    const label = verdict === 'approved' ? 'Approve' : 'Request changes for';
    const draft = review.hasDraft() ? '\n\nThis page has an unsaved comment draft. Only saved comments count.' : '';
    if (!window.confirm(label + ' the entire served collection?\n' + counts() + '.' + draft)) return;
    const body = new URLSearchParams({op: 'finish', verdict, review_revision: summary.review_revision,
      doc: page.dataset.markviewRoute, source_revision: page.dataset.sourceRevision});
    submitting = true;
    invalidate();
    try {
      const response = await fetch(endpoint, {method: 'POST', headers: {'X-Markview-Token': token}, body});
      const data = await response.json();
      if (completed || response.ok || data.error === 'Review finished. You can close this tab.') return done();
      if (response.status === 409) {
        reconfirm = true;
        // Reload through the same path as a live edit. Drafts survive in sessionStorage.
        await window.markviewReloadDocument();
      } else {
        await refresh();
        if (!completed && summary) render(counts() + ' · ' + data.error);
      }
    } catch {
      await refresh();
    } finally {
      submitting = false;
      if (!completed && summary) {
        approve.disabled = false;
        changes.disabled = !summary.comments;
      }
    }
  }

  refresh();
  return {done, refresh, invalidate, unavailable, finished: () => completed};
}
