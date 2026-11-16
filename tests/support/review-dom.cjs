// A small DOM for running src/annotations/review.js under node:test.
// It implements only what the review script touches, with the semantics the
// tests rely on: tree structure, text nodes, event dispatch with bubbling,
// focus, visibility through `hidden`, ranges and document-order comparison.
const fs = require('node:fs');
const path = require('node:path');
const vm = require('node:vm');

class StubNode {
  constructor(document) {
    this.ownerDocument = document;
    this.parentNode = null;
    this.childNodes = [];
  }
  get parentElement() { return this.parentNode; }
  get isConnected() {
    for (let node = this; node; node = node.parentNode) if (node === this.ownerDocument.documentElement) return true;
    return false;
  }
  contains(other) {
    for (let node = other; node; node = node.parentNode) if (node === this) return true;
    return false;
  }
  // A failed assertion prints its operands. Without this, node walks the
  // whole cyclic tree to describe one element and runs out of memory.
  [Symbol.for('nodejs.util.inspect.custom')]() {
    const name = this.nodeType === 3 ? '#text' : this.tagName.toLowerCase() + (this.className ? '.' + this.className : '');
    return `<${name} ${JSON.stringify(this.textContent.slice(0, 40))}>`;
  }
}

class StubText extends StubNode {
  constructor(document, data) {
    super(document);
    this.nodeType = 3;
    this.data = data;
  }
  get textContent() { return this.data; }
}

function matchesSimple(element, selector) {
  // tag, .class, tag.class, [attribute] and [attribute="value"] forms are
  // all the scripts use.
  const match = /^([a-z0-9]*)((?:\.[\w-]+)*)(?:\[([\w-]+)(?:="([^"]*)")?\])?$/i.exec(selector.trim());
  if (!match) throw new Error('unsupported selector: ' + selector);
  const [, tag, classes, attribute, value] = match;
  if (tag && element.tagName !== tag.toUpperCase()) return false;
  if (attribute && !(attribute in element.attributes)) return false;
  if (value !== undefined && element.attributes[attribute] !== value) return false;
  const names = element.className.split(/\s+/);
  return classes.split('.').filter(Boolean).every(name => names.includes(name));
}

class StubElement extends StubNode {
  constructor(document, tag) {
    super(document);
    this.nodeType = 1;
    this.tagName = tag.toUpperCase();
    this.className = '';
    this.attributes = {};
    // Like DOMStringMap, every value is stored as a string: assigning
    // `undefined` leaves an attribute reading "undefined".
    this.dataset = new Proxy({}, {set(target, name, value) { target[name] = String(value); return true; }});
    this.style = {};
    this.hidden = false;
    this.listeners = new Map();
    this.scrolledIntoView = 0;
    if (tag === 'textarea' || tag === 'input') this.value = '';
  }
  get children() { return this.childNodes.filter(node => node.nodeType === 1); }
  get firstElementChild() { return this.children[0] || null; }
  get textContent() { return this.childNodes.map(node => node.textContent).join(''); }
  set textContent(text) {
    this.replaceChildren();
    if (text) this.append(String(text));
  }
  append(...nodes) {
    for (let node of nodes) {
      if (typeof node === 'string') node = new StubText(this.ownerDocument, node);
      if (node.parentNode) node.parentNode.childNodes.splice(node.parentNode.childNodes.indexOf(node), 1);
      node.parentNode = this;
      this.childNodes.push(node);
    }
  }
  prepend(...nodes) {
    const rest = this.childNodes.slice();
    this.append(...nodes, ...rest);
  }
  remove() {
    if (!this.parentNode) return;
    this.parentNode.childNodes.splice(this.parentNode.childNodes.indexOf(this), 1);
    this.parentNode = null;
  }
  replaceChildren(...nodes) {
    for (const node of this.childNodes) node.parentNode = null;
    this.childNodes = [];
    if (this.contains(this.ownerDocument.activeElement) && this.ownerDocument.activeElement !== this) {
      this.ownerDocument.activeElement = this.ownerDocument.body;
    }
    this.append(...nodes);
  }
  setAttribute(name, value) { this.attributes[name] = String(value); }
  getAttribute(name) { return name in this.attributes ? this.attributes[name] : null; }
  removeAttribute(name) { delete this.attributes[name]; }
  matches(selectors) { return selectors.split(',').some(selector => matchesSimple(this, selector)); }
  closest(selectors) {
    for (let node = this; node && node.nodeType === 1; node = node.parentNode) if (node.matches(selectors)) return node;
    return null;
  }
  querySelectorAll(selectors) {
    const found = [];
    const walk = node => {
      for (const child of node.children) {
        if (child.matches(selectors)) found.push(child);
        walk(child);
      }
    };
    walk(this);
    return found;
  }
  querySelector(selectors) { return this.querySelectorAll(selectors)[0] || null; }
  addEventListener(type, listener) {
    if (!this.listeners.has(type)) this.listeners.set(type, []);
    this.listeners.get(type).push(listener);
  }
  dispatchEvent(event) {
    event.target ??= this;
    event.defaultPrevented ??= false;
    event.preventDefault ??= () => { event.defaultPrevented = true; };
    event.stopPropagation ??= () => { event.stopped = true; };
    for (let node = this; node && !event.stopped; node = node.parentNode) {
      for (const listener of node.listeners?.get(event.type) || []) listener.call(node, event);
    }
    if (!event.stopped) for (const listener of this.ownerDocument.listeners.get(event.type) || []) listener(event);
    return !event.defaultPrevented;
  }
  click() {
    if (this.disabled) return;
    const proceed = this.dispatchEvent({type: 'click'});
    if (proceed && this.tagName === 'BUTTON' && this.type === 'submit') this.closest('form')?.requestSubmit();
  }
  requestSubmit() { this.dispatchEvent({type: 'submit'}); }
  // As in a browser, an element that is not in the document cannot take focus.
  focus() { if (this.isConnected) this.ownerDocument.activeElement = this; }
  blur() { if (this.ownerDocument.activeElement === this) this.ownerDocument.activeElement = this.ownerDocument.body; }
  get visible() {
    for (let node = this; node; node = node.parentNode) if (node.hidden) return false;
    return this.isConnected;
  }
  getClientRects() { return this.visible ? [{left: 0, top: 0, right: 10, bottom: 10, width: 10, height: 10}] : []; }
  getBoundingClientRect() { return {left: 0, top: 0, right: 10, bottom: 10, width: 10, height: 10}; }
  scrollIntoView() { this.scrolledIntoView += 1; }
}

// Document order of two boundary points, as Range.comparePoint needs it.
function position(node, offset) {
  const steps = [];
  for (let current = node; current.parentNode; current = current.parentNode) {
    steps.unshift(current.parentNode.childNodes.indexOf(current));
  }
  return [...steps, offset];
}
function compare(left, right) {
  for (let index = 0; index < Math.max(left.length, right.length); index++) {
    if (left[index] === undefined) return -1;
    if (right[index] === undefined) return 1;
    if (left[index] !== right[index]) return left[index] < right[index] ? -1 : 1;
  }
  return 0;
}

class StubRange {
  constructor(document) {
    this.ownerDocument = document;
    this.comparisons = 0;
  }
  setStart(node, offset) { this.startContainer = node; this.startOffset = offset; }
  setEnd(node, offset) { this.endContainer = node; this.endOffset = offset; }
  comparePoint(node, offset) {
    this.comparisons += 1;
    const point = position(node, offset);
    if (compare(point, position(this.startContainer, this.startOffset)) < 0) return -1;
    if (compare(point, position(this.endContainer, this.endOffset)) > 0) return 1;
    return 0;
  }
  toString() {
    if (this.startContainer === this.endContainer) return this.startContainer.data.slice(this.startOffset, this.endOffset);
    return this.startContainer.data.slice(this.startOffset) + '…' + this.endContainer.data.slice(0, this.endOffset);
  }
  getClientRects() { return [{left: 0, top: 0, right: 10, bottom: 10, width: 10, height: 10}]; }
  getBoundingClientRect() { return this.getClientRects()[0]; }
}

// Builds an element tree: h('p', {class: 'x'}, 'text', h('em', {}, 'more')).
function builder(document) {
  return function h(tag, attributes = {}, ...children) {
    const element = document.createElement(tag);
    for (const [name, value] of Object.entries(attributes)) {
      if (name === 'class') element.className = value;
      else if (name.startsWith('data-')) element.dataset[name.slice(5).replace(/-(\w)/g, (_, ch) => ch.toUpperCase())] = value;
      else element.setAttribute(name, value);
    }
    element.append(...children);
    return element;
  };
}

// A page with <main> ready for the review script, which is loaded into its
// own VM context so each test starts from a clean script state.
function createPage({revision = 'source-1'} = {}) {
  const document = {listeners: new Map(), activeElement: null};
  document.createElement = tag => new StubElement(document, tag);
  document.createTextNode = data => new StubText(document, data);
  document.createRange = () => new StubRange(document);
  document.addEventListener = (type, listener) => {
    if (!document.listeners.has(type)) document.listeners.set(type, []);
    document.listeners.get(type).push(listener);
  };
  document.dispatchEvent = event => {
    for (const listener of document.listeners.get(event.type) || []) listener(event);
  };
  document.documentElement = document.createElement('html');
  document.body = document.createElement('body');
  document.documentElement.append(document.body);
  document.activeElement = document.body;
  document.querySelector = selector => document.documentElement.querySelector(selector);
  document.querySelectorAll = selector => document.documentElement.querySelectorAll(selector);
  const main = document.createElement('main');
  if (revision !== null) main.dataset.sourceRevision = revision;
  document.body.append(main);

  const storage = new Map();
  const selection = {rangeCount: 0, isCollapsed: true, getRangeAt() { return this.range; }};
  const page = {
    document,
    main,
    h: builder(document),
    selection,
    storage,
    highlights: new Map(),
    confirmCalls: 0,
    fetchCalls: [],
    // Makes `range` the current selection and tells the script about it.
    select(range) {
      Object.assign(selection, {range, rangeCount: range ? 1 : 0, isCollapsed: !range});
      document.dispatchEvent({type: 'selectionchange'});
    },
    // Reports a DOM change under `root`, as a MutationObserver would.
    mutated(root) {
      for (const observer of page.observers) if (observer.root === root || observer.root?.contains(root)) observer.callback([], observer);
    },
    observers: [],
    // Lets pending promise callbacks in the script run.
    async settle() { for (let turn = 0; turn < 10; turn++) await Promise.resolve(); },
  };
  const window = {
    listeners: new Map(),
    innerWidth: 1000,
    getSelection: () => selection,
    addEventListener(type, listener) { this.listeners.set(type, listener); },
    // The GUI's WKWebView has no dialog delegate, so native dialogs are
    // unusable there. Any call is a bug.
    confirm() { page.confirmCalls += 1; throw new Error('window.confirm must not be used'); },
    alert() { throw new Error('window.alert must not be used'); },
    prompt() { throw new Error('window.prompt must not be used'); },
  };
  page.window = window;
  page.context = vm.createContext({
    document,
    window,
    URLSearchParams,
    encodeURIComponent,
    console,
    sessionStorage: {
      getItem: key => (storage.has(key) ? storage.get(key) : null),
      setItem: (key, value) => storage.set(key, String(value)),
      removeItem: key => storage.delete(key),
    },
    CSS: {highlights: page.highlights},
    Highlight: class { constructor(...ranges) { this.ranges = ranges; this.size = ranges.length; } },
    MutationObserver: class {
      constructor(callback) { this.callback = callback; page.observers.push(this); }
      observe(root) { this.root = root; }
      takeRecords() { return []; }
      disconnect() { this.root = null; }
    },
    fetch: (...args) => {
      page.fetchCalls.push(args);
      return page.fetch(...args);
    },
  });
  const run = file => vm.runInContext(fs.readFileSync(path.join(__dirname, '../../src', file), 'utf8'), page.context);
  run('annotations/review.js');
  // The verdict bar of a blocking review. It confirms with a native dialog,
  // which a browser has; `page.confirmations` records what it asked.
  page.loadVerdict = (answer = true) => {
    page.confirmations = [];
    window.confirm = text => { page.confirmations.push(text); return answer; };
    run('annotations/verdict.js');
  };
  // The live-reload shell every served page ends with. Returns its
  // EventSource, so a test can deliver server events.
  page.loadShell = () => {
    let events = null;
    Object.assign(page.context, {
      location: {pathname: '/doc.md', reload() { page.reloads = (page.reloads || 0) + 1; }, replace() {}},
      navigator: {clipboard: {writeText: async () => {}}},
      EventSource: class {
        constructor() { this.listeners = new Map(); this.closed = false; events = this; }
        addEventListener(type, listener) { this.listeners.set(type, [...(this.listeners.get(type) || []), listener]); }
        dispatchEvent(event) {
          for (const listener of this.listeners.get(event.type) || []) listener(event);
          if (event.type === 'message') this.onmessage?.(event);
        }
        close() { this.closed = true; }
      },
    });
    page.context.createMarkviewCodeCopy ??= () => () => {};
    // A live reload fetches the page again and parses it. `page.next` is the
    // document that parse yields: set it before delivering a reload event.
    page.context.DOMParser = class {
      parseFromString() {
        return {title: page.next.title || '', querySelector: selector => page.next.root.querySelector(selector)};
      }
    };
    window.scrollTo = () => {};
    run('serve_reload.js');
    return events;
  };
  return page;
}

module.exports = {createPage};
