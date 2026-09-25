// Prose rendering (docs/web-client.md). User turns, assistant text,
// thinking, and consolidation-mark notes are markdown and render as such;
// tool output is not prose and does not come through here — the cordon owns
// its representation. Thinking is markdown on the same footing as the rest; the
// reversal trigger it landed with was retired as measured-inert (docs/web-client.md).
//
// The renderer produces a React element tree, never an HTML string: the
// text is model-authored, and a page holding a session credential must
// never turn untrusted content into markup (docs/web-client.md).
// Two properties of react-markdown carry that, and both are defaults rather
// than configuration — raw HTML is escaped unless `rehype-raw` is added, and
// `defaultUrlTransform` passes only http/https/irc/ircs/mailto/xmpp, so a
// `javascript:` or `data:` link in a model's output does not survive into an
// href. Neither should be switched off without reading that section.

import { useMemo } from "react";
import type { Components } from "react-markdown";
import ReactMarkdown from "react-markdown";
import remarkGfm from "remark-gfm";
import { Code } from "./Code.tsx";

/** A fenced block goes through the same highlighter as a tool result body,
 * so prose code and tool code are coloured by one language map rather than
 * two. Only a fence carries `language-*`; inline code has no class and stays
 * an ordinary `<code>`. `<pre>` unwraps to nothing because `Code` renders
 * its own. */
const components: Components = {
  pre: ({ children }) => <>{children}</>,
  code: ({ className, children }) => {
    const lang = /language-(\w+)/.exec(className ?? "")?.[1];
    if (lang === undefined) return <code>{children}</code>;
    return <Code text={String(children).replace(/\n$/, "")} lang={lang} />;
  },
};

/** The shape of a hast node this file needs — structural, because hast's own
 * types are a transitive dependency this package does not declare. */
interface StampNode {
  type: string;
  position?: { start: { line: number }; end: { line: number } };
  properties?: Record<string, unknown>;
  children?: StampNode[];
}

/** Stamp `data-changed` onto every top-level block whose source lines overlap
 * a changed range — the file viewer's markdown highlight, at block granularity
 * by design (backlog/web/file-view/viewer.md). mdast/hast `position` is the
 * source-to-output mapping, so the one line diff serves this view too; the
 * stamp is an attribute and the tint is CSS. */
const stampChanged =
  (ranges: [number, number][]) => () => (tree: StampNode) => {
    for (const node of tree.children ?? []) {
      if (node.type !== "element" || node.position === undefined) continue;
      const start = node.position.start.line;
      const end = node.position.end.line;
      if (ranges.some(([a, b]) => a <= end && start <= b)) {
        node.properties = { ...node.properties, "data-changed": "" };
      }
    }
  };

export function Markdown({
  children,
  changedLines,
}: {
  children: string;
  /** 1-based source line ranges to stamp as changed (the file viewer);
   * absent everywhere else. */
  changedLines?: [number, number][];
}) {
  // Memoized because a fresh plugin array makes react-markdown reprocess on
  // every render of the caller.
  const rehypePlugins = useMemo(
    () =>
      changedLines === undefined || changedLines.length === 0
        ? []
        : [stampChanged(changedLines)],
    [changedLines],
  );
  return (
    <div className="prose">
      <ReactMarkdown
        remarkPlugins={[remarkGfm]}
        rehypePlugins={rehypePlugins}
        components={components}
      >
        {children}
      </ReactMarkdown>
    </div>
  );
}

/** The inline marks only — code, emphasis, links — for a one-line annotation
 * (the distiller's note) that sits in running text rather than as a block.
 * Block structure is unwrapped rather than rendered: a note is one run of
 * text, and a stray `#` or list marker in it is not a heading or a list. The
 * same safety defaults as [`Markdown`] hold. */
const INLINE_ELEMENTS = ["p", "code", "em", "strong", "a", "del"];

const inlineComponents: Components = {
  p: ({ children }) => <>{children}</>,
};

export function InlineMarkdown({ children }: { children: string }) {
  return (
    <ReactMarkdown
      remarkPlugins={[remarkGfm]}
      allowedElements={INLINE_ELEMENTS}
      unwrapDisallowed
      components={inlineComponents}
    >
      {children}
    </ReactMarkdown>
  );
}
