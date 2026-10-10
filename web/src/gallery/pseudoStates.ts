// Pinned interaction states for the gallery. A stylesheet cannot be asked to
// apply :hover to an element the pointer is not over, so this copies every
// rule whose selector names an interaction pseudo-class, with the pseudo
// rewritten to a class the gallery sets (`.is-hover`, `.is-focus-visible`, …).
// The copies are re-derived whenever a stylesheet changes, so under HMR the
// pinned states track the live CSS without a second source of truth.

const PSEUDOS: [RegExp, string][] = [
  [/:hover\b/g, ".is-hover"],
  [/:focus-visible\b/g, ".is-focus-visible"],
  [/:focus-within\b/g, ".is-focus-within"],
  [/:focus\b(?!-)/g, ".is-focus"],
  [/:active\b/g, ".is-active"],
];

const ANY = /:(hover|focus-visible|focus-within|focus|active)\b/;

function rewrite(rule: CSSRule): string | null {
  if (rule instanceof CSSStyleRule) {
    if (!ANY.test(rule.selectorText)) return null;
    let selector = rule.selectorText;
    for (const [pattern, cls] of PSEUDOS) {
      selector = selector.replace(pattern, cls);
    }
    return `${selector} { ${rule.style.cssText} }`;
  }
  // Media, supports, and layer blocks keep their condition around the copies.
  if (rule instanceof CSSGroupingRule) {
    const inner = [...rule.cssRules]
      .map(rewrite)
      .filter((text) => text !== null);
    if (inner.length === 0) return null;
    const head = rule.cssText.slice(0, rule.cssText.indexOf("{"));
    return `${head} { ${inner.join("\n")} }`;
  }
  return null;
}

function derive(own: HTMLStyleElement): string {
  const out: string[] = [];
  for (const sheet of document.styleSheets) {
    if (sheet.ownerNode === own) continue;
    let rules: CSSRuleList;
    try {
      rules = sheet.cssRules;
    } catch {
      continue; // a cross-origin sheet does not expose its rules
    }
    for (const rule of rules) {
      const text = rewrite(rule);
      if (text !== null) out.push(text);
    }
  }
  return out.join("\n");
}

/** Install the pinned-state copies and keep them current; returns the
 * teardown. */
export function installPseudoStates(): () => void {
  const own = document.createElement("style");
  own.dataset.gallery = "pseudo-states";
  document.head.appendChild(own);
  const refresh = () => {
    own.textContent = derive(own);
  };
  refresh();
  // Vite's HMR replaces <style> contents in place; watch the head for both
  // added sheets and edited ones.
  const observer = new MutationObserver((records) => {
    if (records.some((r) => r.target !== own && !own.contains(r.target))) {
      refresh();
    }
  });
  observer.observe(document.head, {
    childList: true,
    subtree: true,
    characterData: true,
  });
  return () => {
    observer.disconnect();
    own.remove();
  };
}
