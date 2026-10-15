// Finds elements by text. Searches text nodes and the aria-label, placeholder, title, value and
// alt attributes, including open shadow roots. Called as (needle, bestMatch, all).
(needle, bestMatch, all) => {
  const want = String(needle).trim().toLowerCase();
  const skip = new Set(['SCRIPT', 'STYLE', 'NOSCRIPT', 'TEMPLATE', 'HEAD', 'TITLE']);
  const attrs = ['aria-label', 'placeholder', 'title', 'value', 'alt'];
  const found = [];
  const visit = (root) => {
    const walker = document.createTreeWalker(root, NodeFilter.SHOW_ELEMENT | NodeFilter.SHOW_TEXT);
    for (let n = walker.currentNode; n; n = walker.nextNode()) {
      if (n.nodeType === Node.TEXT_NODE) {
        const p = n.parentElement;
        if (p && !skip.has(p.tagName) && n.nodeValue && n.nodeValue.toLowerCase().includes(want)) {
          found.push(p);
        }
        continue;
      }
      if (n.nodeType !== Node.ELEMENT_NODE) continue;
      if (n.shadowRoot) visit(n.shadowRoot);
      if (skip.has(n.tagName)) continue;
      for (const a of attrs) {
        const v = n.getAttribute(a);
        if (v && v.toLowerCase().includes(want)) {
          found.push(n);
          break;
        }
      }
    }
  };
  visit(document);
  const unique = [...new Set(found)];
  if (all) return unique;
  if (!unique.length) return null;
  if (!bestMatch) return unique[0];
  const visible = (el) => el.getClientRects().length > 0;
  const text = (el) => (el.innerText || el.textContent || el.getAttribute('aria-label') || el.value || '').trim();
  let best = null;
  let bestScore = Infinity;
  for (const el of unique) {
    const score = Math.abs(text(el).length - want.length) + (visible(el) ? 0 : 1e6);
    if (score < bestScore) {
      best = el;
      bestScore = score;
    }
  }
  return best;
}
