// Syntax highlighting (docs/web-client.md). Shiki, in its fine-grained
// bundle: the app embeds into the binary at compile time, so the full
// grammar set is not affordable and the languages are exactly the ones
// `render/parse.ts`'s `langForPath` can name.
//
// Two choices worth keeping:
//
// - **The JavaScript regex engine, not the Oniguruma WASM one.** No `.wasm`
//   to ship or fetch, and no `wasm-unsafe-eval` needed in the CSP the
//   untrusted-output pass will add (backlog/web/untrusted-output.md).
// - **Tokens, not HTML.** `codeToTokens` returns plain
//   `{ content, color }[]` grouped by line, which the callers render as
//   spans. `codeToHtml` would hand back a markup string, and nothing on
//   this page turns untrusted text into markup. The line grouping is also
//   exactly the shape the numbered-gutter and diff views already use.
//
// Highlighting is best-effort: until the grammars finish loading, and
// forever if they fail to, callers render the same plain text they did
// before. Nothing here is on the path to *reading* a transcript.

import { createHighlighterCore, type HighlighterCore } from "shiki/core";
import { createJavaScriptRegexEngine } from "shiki/engine/javascript";
import { useEffect, useMemo, useSyncExternalStore } from "react";

/** One entry per language `langForPath` returns. Dynamic imports so the
 * grammars are their own chunks rather than main-bundle weight. */
const LANGS = [
  import("@shikijs/langs/rust"),
  import("@shikijs/langs/toml"),
  import("@shikijs/langs/markdown"),
  import("@shikijs/langs/json"),
  import("@shikijs/langs/javascript"),
  import("@shikijs/langs/typescript"),
  import("@shikijs/langs/python"),
  import("@shikijs/langs/bash"),
  import("@shikijs/langs/yaml"),
  import("@shikijs/langs/html"),
  import("@shikijs/langs/css"),
];

const THEMES = { light: "github-light", dark: "github-dark" } as const;

export interface Token {
  content: string;
  color?: string;
}

let highlighter: HighlighterCore | null = null;
let started = false;
const listeners = new Set<() => void>();

function announce() {
  for (const listener of listeners) listener();
}

function start() {
  if (started) return;
  started = true;
  void createHighlighterCore({
    langs: LANGS,
    themes: [
      import("@shikijs/themes/github-light"),
      import("@shikijs/themes/github-dark"),
    ],
    engine: createJavaScriptRegexEngine(),
  })
    .then((core) => {
      highlighter = core;
      announce();
    })
    .catch(() => {
      // Stays null; every caller falls back to plain text.
    });
}

function subscribe(listener: () => void) {
  listeners.add(listener);
  return () => listeners.delete(listener);
}

const darkQuery = () => window.matchMedia("(prefers-color-scheme: dark)");

function subscribeScheme(listener: () => void) {
  const query = darkQuery();
  query.addEventListener("change", listener);
  return () => query.removeEventListener("change", listener);
}

/** A `(code, lang) => lines of tokens` function, or `null` while the
 * grammars are still loading (or if they failed). Re-renders its caller
 * once when highlighting becomes available, and again whenever the system
 * colour scheme flips — the theme is chosen per call, not baked in.
 *
 * The returned function is **stable** across renders (it changes only when the
 * highlighter or the theme does) so that callers can memoize their tokens on
 * it. That is load-bearing, not tidiness: a store update re-renders the whole
 * transcript, so an unstable identity re-tokenizes every code block in the
 * session on every streamed token. */
export function useHighlight():
  ((code: string, lang: string) => Token[][] | null) | null {
  useEffect(start, []);
  const core = useSyncExternalStore(
    subscribe,
    () => highlighter,
    () => null,
  );
  const dark = useSyncExternalStore(
    subscribeScheme,
    () => darkQuery().matches,
    () => false,
  );
  return useMemo(() => {
    if (core === null) return null;
    const theme = dark ? THEMES.dark : THEMES.light;
    return (code: string, lang: string) => {
      if (!core.getLoadedLanguages().includes(lang)) return null;
      try {
        return core.codeToTokens(code, { lang, theme }).tokens;
      } catch {
        return null;
      }
    };
  }, [core, dark]);
}
