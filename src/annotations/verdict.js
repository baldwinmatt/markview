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
  // Set by a conflict and shown with the next counts only: a later refresh
  // or anything the reviewer does on the bar is not a conflict.
  let reconfirm = false;

  // The one place that decides which buttons can be used. Without a
  // message it re-applies those rules and leaves the status line alone.
  function render(message, retryable = false) {
    approve.disabled = completed || submitting || !summary;
    changes.disabled = approve.disabled || !summary?.comments;
    if (message === undefined) return;
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
  // `cause` is the server's: the document that cannot be read, and why.
  function unavailable(cause) {
    if (completed) return;
    sequence++;
    summary = null;
    render(cause ? 'The review cannot be read. ' + cause : 'Could not refresh the review summary.', true);
  }
  // At most one summary request is in flight and one more is pending. A
  // burst of events while a request is out is answered by a single request
  // made after it, which sees everything the burst reported.
  let loading = null;
  let again = false;
  function refresh() {
    if (completed) return Promise.resolve();
    invalidate();
    if (loading) {
      again = true;
      return loading;
    }
    loading = (async () => {
      do {
        again = false;
        await load();
      } while (again && !completed);
    })().finally(() => { loading = null; });
    return loading;
  }
  async function load() {
    const request = sequence;
    try {
      const response = await fetch(endpoint, {cache: 'no-store'});
      const data = await response.json();
      if (completed || request !== sequence) return;
      if (!response.ok) return unavailable(data.error);
      if (data.finished) return done();
      summary = data;
      render(counts() + (reconfirm ? '. The review changed. Confirm the verdict again.' : '.'));
      reconfirm = false;
    } catch {
      if (!completed && request === sequence) unavailable();
    }
  }
  async function finish(verdict) {
    if (!summary || completed || submitting || (verdict === 'changes' && !summary.comments)) return;
    // The reviewer has read the warning and is acting on it.
    render(counts() + '.');
    const label = verdict === 'approved' ? 'Approve' : 'Request changes for';
    // Drafts with text, here or on another served document opened in this tab.
    const drafts = review.unsavedDrafts();
    const draft = !drafts ? '' : '\n\n' + (drafts === 1 ? '1 document has an unsaved comment draft.' :
      drafts + ' documents have unsaved comment drafts.') + ' Only saved comments count.';
    if (!window.confirm(label + ' the entire served collection?\n' + counts() + '.' + draft)) return;
    const body = new URLSearchParams({op: 'finish', verdict, review_revision: summary.review_revision,
      doc: page.dataset.markviewRoute, source_revision: page.dataset.sourceRevision});
    submitting = true;
    invalidate();
    try {
      const response = await fetch(endpoint, {method: 'POST', headers: {'X-Markview-Token': token}, body});
      const data = await response.json();
      // A session that another tab finished answers 409 with `finished`.
      if (completed || response.ok || data.finished) return done();
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
      render();
    }
  }

  // The first summary is requested when the page's event stream opens, so
  // no change can fall between the counts and the events that update them.
  render('Refreshing collection counts…');
  return {done, refresh, invalidate, unavailable, finished: () => completed};
}
