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

import { useMemo, type ReactNode } from "react";
import type { Components } from "react-markdown";
import ReactMarkdown from "react-markdown";
import remarkGfm from "remark-gfm";
import type { ArtifactLink } from "../api/types.ts";
import { Artifact } from "./Artifact.tsx";
import { Code } from "./Code.tsx";

/** A fenced block goes through the same highlighter as a tool result body,
 * so prose code and tool code are coloured by one language map rather than
 * two. Only a fence carries `language-*`; inline code has no class and stays
 * an ordinary `<code>`. `<pre>` unwraps to nothing because `Code` renders
 * its own. */
const components: Components = {
  pre: ({ children }) => <>{children}</>,
  code: ({ className, children }) => fencedCode(className, children),
};

function fencedCode(className: string | undefined, children: ReactNode) {
  const lang = /language-(\w+)/.exec(className ?? "")?.[1];
  if (lang === undefined) return <code>{children}</code>;
  return <Code text={String(children).replace(/\n$/, "")} lang={lang} />;
}

/** One assistant text block's place among its entry's, and the links the entry
 * view carries for its artifacts (docs/artifacts.md). */
interface ArtifactContext {
  textBlock: number;
  links: readonly ArtifactLink[];
}

/** The hast property the fence numbering leaves on an artifact's `<code>`. */
const ARTIFACT_FENCE = "dataArtifactFence";

/** An `html-artifact` fence whose link the entry view carries renders as its
 * sandboxed frame; one without a link (a mismatch, a session with no id) stays
 * code, which `language-(\w+)` reads as HTML. */
function artifactComponents({ textBlock, links }: ArtifactContext): Components {
  return {
    ...components,
    code: ({ className, children, node }) => {
      const fence = node?.properties[ARTIFACT_FENCE];
      const link = links.find(
        (link) => link.text_block === textBlock && link.fence === fence,
      );
      if (link === undefined) return fencedCode(className, children);
      return (
        <Artifact source={String(children).replace(/\n$/, "")} url={link.url} />
      );
    },
  };
}

/** The shape of a hast node this file needs — structural, because hast's own
 * types are a transitive dependency this package does not declare. */
interface StampNode {
  type: string;
  tagName?: string;
  position?: { start: { line: number }; end: { line: number } };
  properties?: Record<string, unknown>;
  data?: { meta?: string | null };
  children?: StampNode[];
}

/** Number the text block's `html-artifact` fences in document order: the count
 * the server makes over the same text (src/artifact.rs), each by CommonMark's
 * rules; `artifact-fences.json` holds the cases both sides are tested against.
 * remark puts an info string's first word in the class and the rest in `meta`,
 * so `html-artifact preview` is not one. */
const numberArtifactFences = () => (tree: StampNode) => {
  let fence = 0;
  const visit = (node: StampNode) => {
    const className = node.properties?.className;
    if (
      node.type === "element" &&
      node.tagName === "code" &&
      Array.isArray(className) &&
      className.includes("language-html-artifact") &&
      (node.data?.meta ?? "") === ""
    ) {
      node.properties = { ...node.properties, [ARTIFACT_FENCE]: fence };
      fence += 1;
    }
    for (const child of node.children ?? []) visit(child);
  };
  visit(tree);
};

/** Stamp `data-changed` onto every top-level block whose source lines overlap
 * a changed range — the file viewer's markdown highlight, at block granularity
 * by design (docs/web-client.md). mdast/hast `position` is the
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

/** A link the caller follows itself, or `null` for one it leaves to the
 * browser — the file viewer's relative links (ui/fileLinks.ts). */
export type FollowLink = (href: string) => (() => void) | null;

export function Markdown({
  children,
  changedLines,
  followLink,
  artifacts,
}: {
  children: string;
  /** 1-based source line ranges to stamp as changed (the file viewer);
   * absent everywhere else. */
  changedLines?: [number, number][];
  /** Present in the file viewer, where a link the caller does not follow
   * opens in a new tab rather than replacing the page the viewer is on. */
  followLink?: FollowLink;
  /** Supplied only by the committed session transcript, for an entry whose
   * view carries artifact links. */
  artifacts?: ArtifactContext;
}) {
  // Both memos key on the context's values, not its object: callers build it
  // inline, and a fresh `code` component on every render would remount every
  // code block, reloading each artifact frame on every streamed token.
  const textBlock = artifacts?.textBlock;
  const links = artifacts?.links;
  // Memoized because a fresh plugin array makes react-markdown reprocess on
  // every render of the caller.
  const rehypePlugins = useMemo(
    () => [
      ...(links === undefined ? [] : [numberArtifactFences]),
      ...(changedLines === undefined || changedLines.length === 0
        ? []
        : [stampChanged(changedLines)]),
    ],
    [changedLines, links],
  );
  const linked = useMemo<Components>(() => {
    const base =
      textBlock === undefined || links === undefined
        ? components
        : artifactComponents({ textBlock, links });
    return followLink === undefined
      ? base
      : {
          ...base,
          a: ({ href, children }) => {
            const follow = href === undefined ? null : followLink(href);
            if (follow === null)
              return (
                <a href={href} target="_blank" rel="noopener noreferrer">
                  {children}
                </a>
              );
            return (
              <a
                href={href}
                onClick={(e) => {
                  e.preventDefault();
                  follow();
                }}
              >
                {children}
              </a>
            );
          },
        };
  }, [followLink, links, textBlock]);
  return (
    <div className="prose">
      <ReactMarkdown
        remarkPlugins={[remarkGfm]}
        rehypePlugins={rehypePlugins}
        components={linked}
      >
        {children}
      </ReactMarkdown>
    </div>
  );
}

/** The inline marks only — code, emphasis, links — for a one-line annotation
 * (the triager's note) that sits in running text rather than as a block.
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
