// Keep the native textarea as the editing surface so selection, undo, IME,
// and accessibility remain owned by WebKit. Spelling marks come from macOS.
function markdownTokens(source) {
  const tokens = [];
  let fence = null;
  for (const line of source.match(/[^\n]*\n|[^\n]+$/g) || []) {
    const marker = line.match(/^ {0,3}(`{3,}|~{3,})/);
    if (fence) {
      tokens.push({text: line, kind: 'code'});
      if (marker && marker[1][0] === fence[0] && marker[1].length >= fence.length
          && line.slice(marker[0].length).trim() === '') fence = null;
    } else if (marker) {
      fence = marker[1];
      tokens.push({text: line, kind: 'code'});
    } else if (/^ {0,3}#{1,6}(\s|$)/.test(line)) {
      tokens.push({text: line, kind: 'heading'});
    } else {
      const pattern = /(`+)[^`\n]+\1|!?\[[^\]\n]*\]\([^\n)]*\)|\*\*[^*\n]+\*\*|__[^_\n]+__|\*[^*\n]+\*|_[^_\n]+_|~~[^~\n]+~~|^ {0,3}(?:>\s?|[-+*]\s|\d+[.)]\s)/g;
      let cursor = 0;
      for (const match of line.matchAll(pattern)) {
        tokens.push({text: line.slice(cursor, match.index), kind: ''});
        const text = match[0];
        const kind = text.startsWith('`') ? 'code'
          : /^!?\[/.test(text) ? 'link' : 'markup';
        tokens.push({text, kind});
        cursor = match.index + text.length;
      }
      tokens.push({text: line.slice(cursor), kind: ''});
    }
  }
  return tokens;
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
      if (token.kind === 'code' || token.kind === 'link') break;
      const left = Math.max(start, offset);
      const right = Math.min(start + length, end);
      if (right <= left) continue;
      tokens.push({text: source.slice(cursor, left), kind: token.kind});
      tokens.push({text: source.slice(left, right), kind: token.kind, spelling: true});
      cursor = right;
    }
    tokens.push({text: source.slice(cursor, end), kind: token.kind});
    offset = end;
  }
  return tokens;
}

function receiveEditorSpelling(id, source, ranges) {
  const textarea = document.querySelector('textarea.editor');
  if (textarea && textarea.dataset.tabId === String(id) && textarea.value === source) {
    textarea.showSpelling(ranges);
  }
}

function decorateEditor(textarea, pane) {
  let spelling = [];
  let spellingTimer;
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
    refresh();
  };
  const refresh = () => {
    const fragment = document.createDocumentFragment();
    for (const token of spellingTokens(textarea.value, spelling)) {
      const span = document.createElement('span');
      span.className = token.kind ? `md-${token.kind}` : '';
      if (token.spelling) span.classList.add('md-spelling');
      span.textContent = token.text;
      fragment.appendChild(span);
    }
    // A final newline keeps the overlay's last empty line aligned with the textarea.
    fragment.appendChild(document.createTextNode('\n'));
    highlight.replaceChildren(fragment);
    highlight.scrollTop = textarea.scrollTop;
    highlight.scrollLeft = textarea.scrollLeft;
  };
  const checkSpelling = () => {
    clearTimeout(spellingTimer);
    spellingTimer = setTimeout(() => {
      if (textarea.isConnected) {
        window.ipc.postMessage(`spell:${textarea.dataset.tabId}:${textarea.value}`);
      }
    }, 350);
  };
  textarea.showSpelling = ranges => { spelling = ranges; refresh(); };
  textarea.addEventListener('input', () => {
    spelling = [];
    refresh();
    checkSpelling();
  });
  textarea.addEventListener('scroll', () => {
    highlight.scrollTop = textarea.scrollTop;
    highlight.scrollLeft = textarea.scrollLeft;
  });
  textarea.setAttribute('aria-label', 'Markdown source');
  textarea.spellcheck = false;
  textarea.autocapitalize = 'off';
  textarea.setAttribute('autocorrect', 'off');
  textarea.wrap = 'off';
  tools.appendChild(toggle);
  surface.append(highlight, textarea);
  layout.append(surface, reference);
  pane.append(tools, layout);
  refresh();
  requestAnimationFrame(refresh);
  checkSpelling();
}
