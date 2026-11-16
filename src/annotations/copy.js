function createMarkviewCodeCopy(root) {
  const status = document.createElement('div');
  status.className = 'markview-copy-status';
  status.setAttribute('role', 'status');
  document.body.append(status);
  // How long a button shows its result before it offers to copy again.
  const RESULT_MS = 2000;
  // A live region speaks when its text changes. The same result twice in a
  // row is no change, and clearing and writing in one task is one the reader
  // never sees. So clear now and write the result in a later task.
  const ANNOUNCE_MS = 100;
  let announcing = null;
  function announce(message) {
    status.textContent = '';
    clearTimeout(announcing);
    announcing = setTimeout(() => { status.textContent = message; }, ANNOUNCE_MS);
  }
  return function attachCodeCopy(copy) {
    for (const code of root.querySelectorAll('pre > code')) {
      const block = code.parentElement;
      if (block.parentElement.classList.contains('markview-code-block')) continue;
      const wrapper = document.createElement('div');
      wrapper.className = 'markview-code-block';
      block.before(wrapper);
      const button = document.createElement('button');
      button.className = 'markview-copy-code';
      button.type = 'button';
      button.textContent = 'Copy';
      button.setAttribute('data-markview-review-exclude', '');
      let reset = null;
      button.addEventListener('click', async () => {
        if (button.getAttribute('aria-busy') === 'true') return;
        button.setAttribute('aria-busy', 'true');
        clearTimeout(reset);
        let message;
        try {
          await copy(code.textContent);
          message = 'Copied';
        } catch {
          message = 'Copy failed';
        }
        if (!button.isConnected) return;
        button.removeAttribute('aria-busy');
        button.textContent = message;
        announce(message);
        reset = setTimeout(() => { button.textContent = 'Copy'; }, RESULT_MS);
      });
      wrapper.append(block, button);
    }
  };
}
