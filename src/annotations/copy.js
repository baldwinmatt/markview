function createMarkviewCodeCopy(root) {
  const status = document.createElement('div');
  status.className = 'markview-copy-status';
  status.setAttribute('role', 'status');
  document.body.append(status);
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
      button.addEventListener('click', async () => {
        if (button.getAttribute('aria-busy') === 'true') return;
        button.setAttribute('aria-busy', 'true');
        status.textContent = '';
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
        status.textContent = message;
      });
      wrapper.append(block, button);
    }
  };
}
