// Keep the native textarea as the editing surface so selection, undo, IME,
// and accessibility remain owned by WebKit. Spelling marks come from macOS.
const INLINE_PATTERN = /^ {0,3}(?:>\s?|[-+*]\s|\d+[.)]\s)|(`+)[^`\n]+\1|!?\[[^\]\n]*\]\((?:[^()\n]|\([^()\n]*\))*\)|\*\*[^*\n]+\*\*|(?<!\w)__[^_\n]+__(?!\w)|\*[^*\n]+\*|(?<!\w)_[^_\n]+_(?!\w)|~~[^~\n]+~~/g;
let editorRevision = 0;

function markdownTokens(source) {
  const tokens = [];
  const push = (text, kind) => { if (text) tokens.push({text, kind}); };
  let fence = null;
  for (const line of source.match(/[^\n]*\n|[^\n]+$/g) || []) {
    const marker = line.match(/^ {0,3}(`{3,}|~{3,})(.*)$/m);
    if (fence) {
      push(line, 'code');
      if (marker && marker[1][0] === fence[0] && marker[1].length >= fence.length
          && marker[2].trim() === '') fence = null;
    } else if (marker && (marker[1][0] !== '`' || !marker[2].includes('`'))) {
      fence = marker[1];
      push(line, 'code');
    } else if (/^(?: {4}|\t)/.test(line)) {
      push(line, 'code');
    } else {
      const heading = /^ {0,3}#{1,6}(\s|$)/.test(line) ? 'heading' : '';
      // Bound malformed inline syntax scanning, including unclosed brackets/backticks.
      if (line.length > 2048) { push(line, heading); continue; }
      let cursor = 0;
      for (const match of line.matchAll(INLINE_PATTERN)) {
        push(line.slice(cursor, match.index), heading);
        const text = match[0];
        const kind = text.startsWith('`') ? 'code'
          : /^!?\[/.test(text) ? 'link' : 'markup';
        push(text, kind);
        cursor = match.index + text.length;
      }
      push(line.slice(cursor), heading);
    }
  }
  return tokens;
}

// Retain marks outside the changed span until the asynchronous check completes.
function shiftSpelling(ranges, before, after) {
  let start = 0;
  while (start < before.length && start < after.length && before[start] === after[start]) start++;
  let end = before.length;
  let nextEnd = after.length;
  while (end > start && nextEnd > start && before[end - 1] === after[nextEnd - 1]) { end--; nextEnd--; }
  const delta = nextEnd - end;
  return ranges.flatMap(([offset, length]) => {
    if (offset + length <= start) return [[offset, length]];
    if (offset >= end) return [[offset + delta, length]];
    return [];
  });
}

function spellingTokens(source, ranges) {
  const tokens = [];
  let offset = 0;
  let rangeIndex = 0;
  for (const token of markdownTokens(source)) {
    const end = offset + token.text.length;
    let cursor = offset;
    while (rangeIndex < ranges.length && ranges[rangeIndex][0] + ranges[rangeIndex][1] <= offset) rangeIndex++;
    for (let index = rangeIndex; index < ranges.length; index++) {
      const [start, length] = ranges[index];
      if (start >= end) break;
      const left = Math.max(start, offset);
      const right = Math.min(start + length, end);
      if (left > cursor) tokens.push({text: source.slice(cursor, left), kind: token.kind});
      tokens.push({text: source.slice(left, right), kind: token.kind, spelling: true});
      cursor = right;
    }
    if (end > cursor) tokens.push({text: source.slice(cursor, end), kind: token.kind});
    offset = end;
  }
  return tokens;
}

function receiveEditorSpelling(id, revision, ranges) {
  const textarea = document.querySelector('textarea.editor');
  if (textarea && textarea.dataset.tabId === String(id) && textarea.spellingRevision === revision) {
    textarea.showSpelling(ranges);
  }
}

function decorateEditor(textarea, pane) {
  let spelling = [];
  let spellingTimer;
  let refreshFrame = null;
  let previousSource = textarea.value;
  textarea.spellingRevision = ++editorRevision;
  const tools = document.createElement('div');
  tools.className = 'editor-tools';
  const toggle = document.createElement('button');
  toggle.type = 'button';
  toggle.textContent = 'Markdown syntax';
  toggle.setAttribute('aria-expanded', 'false');
  const layout = document.createElement('div');
  layout.className = 'editor-layout';
  const surface = document.createElement('div');
  surface.className = 'editor-surface';
  const highlight = document.createElement('pre');
  highlight.className = 'editor-highlight';
  highlight.setAttribute('aria-hidden', 'true');
  const reference = document.createElement('aside');
  reference.className = 'editor-reference';
  reference.hidden = true;
  reference.setAttribute('aria-label', 'Markdown syntax reference');
  for (const [label, syntax] of [
    ['Heading', '# Heading'], ['Emphasis', '*italic* / **bold**'],
    ['Link', '[title](path.md)'], ['Image', '![alt text](image.png)'],
    ['List', '- item\n1. item'], ['Task', '- [ ] task'],
    ['Quote', '> quote'], ['Inline code', '`code`'],
    ['Code block', '```language\ncode\n```'], ['Strikethrough', '~~text~~'],
    ['Table', '| A | B |\n| --- | --- |\n| one | two |']
  ]) {
    const title = document.createElement('strong');
    title.textContent = label;
    const example = document.createElement('pre');
    example.textContent = syntax;
    reference.append(title, example);
  }
  toggle.onclick = () => {
    reference.hidden = !reference.hidden;
    toggle.setAttribute('aria-expanded', String(!reference.hidden));
    requestAnimationFrame(syncScroll);
  };
  const syncScroll = () => {
    highlight.scrollTop = textarea.scrollTop;
    highlight.scrollLeft = textarea.scrollLeft;
  };
  textarea.syncHighlightScroll = syncScroll;
  const refresh = () => {
    const fragment = document.createDocumentFragment();
    for (const token of spellingTokens(textarea.value, spelling)) {
      if (!token.text) continue;
      if (!token.kind && !token.spelling) {
        fragment.appendChild(document.createTextNode(token.text));
        continue;
      }
      const span = document.createElement('span');
      span.className = token.kind ? `md-${token.kind}` : '';
      if (token.spelling) span.classList.add('md-spelling');
      span.textContent = token.text;
      fragment.appendChild(span);
    }
    // A final newline keeps the overlay's last empty line aligned with the textarea.
    fragment.appendChild(document.createTextNode('\n'));
    highlight.replaceChildren(fragment);
    syncScroll();
  };
  const scheduleRefresh = () => {
    if (refreshFrame !== null) return;
    refreshFrame = requestAnimationFrame(() => {
      refreshFrame = null;
      if (textarea.isConnected) refresh();
    });
  };
  const checkSpelling = () => {
    clearTimeout(spellingTimer);
    spellingTimer = setTimeout(() => {
      if (textarea.isConnected) {
        window.ipc.postMessage(`spell:${textarea.dataset.tabId}:${textarea.spellingRevision}`);
      }
    }, 350);
  };
  textarea.showSpelling = ranges => { spelling = ranges; scheduleRefresh(); };
  textarea.addEventListener('input', () => {
    spelling = shiftSpelling(spelling, previousSource, textarea.value);
    previousSource = textarea.value;
    textarea.spellingRevision = ++editorRevision;
    scheduleRefresh();
    checkSpelling();
  });
  textarea.addEventListener('scroll', syncScroll);
  textarea.setAttribute('aria-label', 'Markdown source');
  textarea.spellcheck = false;
  textarea.autocapitalize = 'off';
  textarea.setAttribute('autocorrect', 'off');
  tools.appendChild(toggle);
  surface.append(highlight, textarea);
  layout.append(surface, reference);
  pane.append(tools, layout);
  scheduleRefresh();
  checkSpelling();
}
