(() => {
  const formatTime = (date) => date.toLocaleTimeString([], { hour: 'numeric', minute: '2-digit', second: '2-digit' });
  const updateModifiedTimes = () => {
    document.querySelectorAll('[data-markview-modified-at]').forEach((element) => {
      const millis = Number(element.dataset.timestampMs);
      if (!Number.isFinite(millis)) {
        element.textContent = 'unknown';
        return;
      }
      const modified = new Date(millis);
      element.dateTime = modified.toISOString();
      element.textContent = formatTime(modified);
    });
  };
  const updateRefreshTime = () => {
    const now = new Date();
    document.querySelectorAll('[data-markview-refreshed-at]').forEach((element) => {
      element.dateTime = now.toISOString();
      element.textContent = formatTime(now);
    });
  };
  updateModifiedTimes();
  updateRefreshTime();
  const themeButtons = document.querySelectorAll('[data-markview-theme]');
  const showTheme = (choice) => {
    if (choice === 'light' || choice === 'dark') document.documentElement.dataset.theme = choice;
    else delete document.documentElement.dataset.theme;
    themeButtons.forEach((button) => {
      button.setAttribute('aria-pressed', String(button.dataset.markviewTheme === choice));
    });
  };
  let savedTheme = 'auto';
  try {
    const stored = localStorage.getItem('markview-theme');
    if (stored === 'light' || stored === 'dark') savedTheme = stored;
  } catch { /* Preference storage is optional. */ }
  showTheme(savedTheme);
  themeButtons.forEach((button) => {
    button.addEventListener('click', () => {
      const choice = button.dataset.markviewTheme;
      showTheme(choice);
      try {
        if (choice === 'auto') localStorage.removeItem('markview-theme');
        else localStorage.setItem('markview-theme', choice);
      } catch { /* Preference storage is optional. */ }
    });
  });
  const outline = document.querySelector('[data-markview-outline]');
  const comments = document.querySelector('[data-markview-comments]');
  const buildOutline = () => {
    if (!outline) return;
    outline.replaceChildren();
    const headings = document.querySelectorAll('main h1[id], main h2[id], main h3[id], main h4[id], main h5[id], main h6[id]');
    headings.forEach((heading) => {
      const link = document.createElement('a');
      link.href = '#' + heading.id;
      link.dataset.level = heading.tagName.slice(1);
      link.textContent = heading.textContent;
      outline.append(link);
    });
    if (!headings.length) {
      const empty = document.createElement('p');
      empty.textContent = 'This document has no headings.';
      outline.append(empty);
    }
  };
  document.querySelectorAll('[data-markview-tab]').forEach((tab) => {
    tab.addEventListener('click', () => {
      const showOutline = tab.dataset.markviewTab === 'outline';
      document.querySelectorAll('[data-markview-tab]').forEach((other) => {
        other.setAttribute('aria-selected', String(other === tab));
      });
      if (comments) comments.hidden = showOutline;
      if (outline) outline.hidden = !showOutline;
    });
  });
  buildOutline();
  const attachCodeCopy = createMarkviewCodeCopy(document.querySelector('main'));
  window.markviewAttachCodeCopy = () => attachCodeCopy(text => navigator.clipboard.writeText(text));
  window.markviewAttachCodeCopy();
  const events = new EventSource('/events');
  let loadingDocument = false;
  const refreshVerdict = () => { if (!loadingDocument) window.markviewVerdict?.refresh(); };
  events.addEventListener('annotations', (event) => {
    if (event.data === document.querySelector('main').dataset.markviewRoute) window.markviewReviewRefresh?.();
    refreshVerdict();
  });
  events.addEventListener('review-finished', () => {
    events.close();
    window.markviewVerdict?.done();
  });
  events.addEventListener('open', refreshVerdict);
  let refresh = 0;
  window.markviewReloadDocument = async () => {
    window.markviewVerdict?.invalidate();
    const sequence = ++refresh;
    loadingDocument = true;
    try {
      await reloadDocument(sequence);
    } catch {
      if (sequence === refresh) window.markviewVerdict?.unavailable();
    } finally {
      if (sequence === refresh) loadingDocument = false;
    }
  };
  events.onmessage = (event) => {
    if (window.markviewVerdict?.finished()) return;
    if (event.data === 'rescan') {
      location.reload();
      return;
    }
    if (event.data === 'reload') window.markviewReloadDocument();
  };
  async function reloadDocument(sequence) {
    const savedY = window.scrollY;
    const response = await fetch(location.pathname, { cache: 'no-store' });
    const text = await response.text();
    const next = new DOMParser().parseFromString(text, 'text/html');
    const currentMain = document.querySelector('main');
    const nextMain = next.querySelector('main');
    if (sequence !== refresh || window.markviewVerdict?.finished()) return;
    if (!response.ok) {
      if (response.status === 404 && window.markviewVerdict) location.replace('/');
      else window.markviewVerdict?.unavailable();
      return;
    }
    if (currentMain && nextMain) {
      currentMain.innerHTML = nextMain.innerHTML;
      // Only a page with review hooks has these. Assigning a missing one
      // would write the string "undefined" into the attribute.
      for (const name of ['sourceRevision', 'markviewRoute']) {
        if (name in nextMain.dataset) currentMain.dataset[name] = nextMain.dataset[name];
        else delete currentMain.dataset[name];
      }
      window.markviewAttachCodeCopy();
      window.markviewReviewRefresh?.();
    }
    const currentNav = document.querySelector('[data-markview-nav]');
    const nextNav = next.querySelector('[data-markview-nav]');
    if (currentNav && nextNav) {
      const openDirs = new Set(
        Array.from(currentNav.querySelectorAll('details[open]')).map((el) => el.dataset.markviewDir)
      );
      currentNav.innerHTML = nextNav.innerHTML;
      currentNav.querySelectorAll('details').forEach((el) => {
        el.open = openDirs.has(el.dataset.markviewDir);
      });
    }
    const currentFooter = document.querySelector('footer');
    const nextFooter = next.querySelector('footer');
    if (currentFooter && nextFooter) currentFooter.innerHTML = nextFooter.innerHTML;
    if (next.title) document.title = next.title;
    updateModifiedTimes();
    updateRefreshTime();
    buildOutline();
    window.scrollTo(0, savedY);
    loadingDocument = false;
    await window.markviewVerdict?.refresh();
  }
})();
