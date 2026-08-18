// Syntax highlighting (docs/web-client.md). Shiki, in its fine-grained
// bundle: the app embeds into the binary at compile time, so the full
// grammar set is not affordable and the languages are exactly the ones
// `render/parse.ts`'s `langForPath` can name.
//
// Two choices worth keeping:
//
// - **The JavaScript regex engine, not the Oniguruma WASM one.** No `.wasm`
//   to ship or fetch, and no `wasm-unsafe-eval` needed in the CSP the
//   CSP the bind serves (docs/web-client.md).
// - **Tokens, not HTML.** `codeToTokens` returns plain
//   `{ content, color }[]` grouped by line, which the callers render as
//   spans. `codeToHtml` would hand back a markup string, and nothing on
//   this page turns untrusted text into markup. The line grouping is also
//   exactly the shape the numbered-gutter and diff views already use.
//
// Highlighting is best-effort: until the grammars finish loading, and
// forever if they fail to, callers render the same plain text they did
// before. Nothing here is on the path to *reading* a transcript.

import {
  createHighlighterCore,
  type GrammarState,
  type HighlighterCore,
} from "shiki/core";
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

/**
 * Tokenizing for an append-only buffer — a tool call's contents while they
 * stream. Re-tokenizing the whole buffer per delta is O(n) work per chunk and
 * O(n²) over a large file write; Shiki's grammar state makes tokenizing
 * resumable at a line boundary instead. Tokens for complete lines are kept,
 * and each update tokenizes only what arrived since the last complete line.
 *
 * The correctness property (pinned by test): the result is identical to
 * tokenizing the whole buffer at once.
 */
export class StreamingTokenizer {
  /** Token lines for [`prefix`], which is complete-line (`\n`-terminated)
   * and only ever extends. */
  private done: Token[][] = [];
  private prefix = "";
  /** The grammar's state at the end of [`prefix`] — what makes the next
   * lines tokenizable without the ones already done. */
  private state: GrammarState | undefined = undefined;

  constructor(
    private readonly core: HighlighterCore,
    private readonly lang: string,
    private readonly theme: string,
  ) {}

  tokens(text: string): Token[][] {
    if (!text.startsWith(this.prefix)) {
      // Not an append: a new call reusing this component slot, or a buffer
      // that hit the store's cap and slid. Start over.
      this.done = [];
      this.prefix = "";
      this.state = undefined;
    }
    const rest = text.slice(this.prefix.length);
    const cut = rest.lastIndexOf("\n");
    if (cut >= 0) {
      const closed = this.core.codeToTokensBase(rest.slice(0, cut), {
        lang: this.lang,
        theme: this.theme,
        grammarState: this.state,
      });
      this.done.push(...closed);
      this.state = this.core.getLastGrammarState(closed);
      this.prefix = text.slice(0, this.prefix.length + cut + 1);
    }
    // The line still open re-tokenizes every time — it is one line.
    const open = this.core.codeToTokensBase(text.slice(this.prefix.length), {
      lang: this.lang,
      theme: this.theme,
      grammarState: this.state,
    });
    return [...this.done, ...open];
  }
}

/** [`StreamingTokenizer`] as a hook: `null` outside highlighting's reach
 * (grammars loading or failed, unknown language), tokens otherwise. The
 * tokenizer is memoized on language and theme, so it persists across the
 * deltas of one value and starts over when either changes. Advancing it
 * inside the memo is safe: the same text tokenizes to the same result, so a
 * double invocation redoes only the open line. If React ever drops the memo,
 * the fresh tokenizer's first call is one whole-buffer pass — the cost this
 * hook exists to avoid paying *per delta*, acceptable once. */
export function useStreamingTokens(
  text: string,
  lang: string | null,
): Token[][] | null {
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
  const tokenizer = useMemo(
    () =>
      core !== null && lang !== null && core.getLoadedLanguages().includes(lang)
        ? new StreamingTokenizer(core, lang, dark ? THEMES.dark : THEMES.light)
        : null,
    [core, lang, dark],
  );
  return useMemo(() => {
    if (tokenizer === null) return null;
    try {
      return tokenizer.tokens(text);
    } catch {
      return null;
    }
  }, [tokenizer, text]);
}
