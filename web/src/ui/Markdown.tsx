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
import type { Seq } from "../api/types.ts";
import { Artifact } from "./Artifact.tsx";
import { Code } from "./Code.tsx";

/** A fenced block goes through the same highlighter as a tool result body,
 * so prose code and tool code are coloured by one language map rather than
 * two. Only a fence carries `language-*`; inline code has no class and stays
 * an ordinary `<code>`. `<pre>` unwraps to nothing because `Code` renders
 * its own. */
interface ArtifactContext {
  seq: Seq;
  /** Zero-based position among the assistant's Text blocks. */
  textBlock: number;
}

/** The shape of a hast node this file needs — structural, because hast's own
 * types are a transitive dependency this package does not declare. */
interface StampNode {
  type: string;
  tagName?: string;
  position?: { start: { line: number }; end: { line: number } };
  properties?: Record<string, unknown>;
  children?: StampNode[];
}

interface FenceOpening {
  marker: "`" | "~";
  length: number;
  info: string;
  blockquoteDepth: number;
  listIndent: number;
}

/** Remove Markdown container prefixes from a fence opener so fenced blocks in
 * blockquotes and list items share the same ordinal sequence as top-level
 * fences. The ordinary three-space limit still distinguishes a top-level
 * fence from an indented code block. */
function stripBlockquotes(line: string): string {
  let content = line;
  for (let depth = 0; depth < 16; depth += 1) {
    const prefix = /^ {0,3}>[ \\t]?/.exec(content);
    if (prefix === null) break;
    content = content.slice(prefix[0].length);
  }
  return content;
}

function blockquoteDepth(line: string): number {
  let content = line;
  let depth = 0;
  for (; depth < 16; depth += 1) {
    const prefix = /^ {0,3}>[ \\t]?/.exec(content);
    if (prefix === null) break;
    content = content.slice(prefix[0].length);
  }
  return depth;
}

/** Find the active list item's content indentation on a source line. This
 * lets fences nested under a list remain distinct from four-space code. */
function listIndentAt(lines: string[], lineIndex: number): number {
  const lists: { marker: number; content: number }[] = [];
  for (const line of lines.slice(0, lineIndex)) {
    const content = stripBlockquotes(line);
    const indent = /^ */.exec(content)?.[0].length ?? 0;
    const rest = content.slice(indent);
    if (rest.trim() === "") continue;
    while (lists.length > 0 && indent < (lists.at(-1)?.content ?? 0)) {
      lists.pop();
    }
    const item = /^([-+*]|[0-9]{1,9}[.)])([ \\t]+)/.exec(rest);
    if (item === null) continue;
    while (lists.length > 0 && indent <= (lists.at(-1)?.marker ?? 0)) {
      lists.pop();
    }
    const spacing = (item[2] ?? "").replace(/\t/g, "    ").length;
    lists.push({
      marker: indent,
      content: indent + (item[1]?.length ?? 0) + spacing,
    });
  }
  const current = stripBlockquotes(lines[lineIndex] ?? "");
  const indent = /^ */.exec(current)?.[0].length ?? 0;
  return lists.reduce(
    (active, list) =>
      list.content <= indent ? Math.max(active, list.content) : active,
    0,
  );
}

function listIndentForFence(lines: string[], lineIndex: number): number {
  const activeIndent = listIndentAt(lines, lineIndex);
  const line = stripBlockquotes(lines[lineIndex] ?? "");
  const indentation = /^ */.exec(line)?.[0].length ?? 0;
  const item = /^([-+*]|[0-9]{1,9}[.)])([ \t]+)/.exec(line.slice(indentation));
  if (item === null) return activeIndent;
  const spacing = (item[2] ?? "").replace(/\t/g, "    ").length;
  return Math.max(activeIndent, indentation + (item[1]?.length ?? 0) + spacing);
}

function fenceContent(line: string, listIndent: number): string {
  let content = stripBlockquotes(line);
  const indentation = /^ */.exec(content)?.[0].length ?? 0;
  if (listIndent > 0 && indentation >= listIndent) {
    content = content.slice(listIndent);
  }
  for (let depth = 0; depth < 16; depth += 1) {
    const indent = /^ {0,3}/.exec(content)?.[0].length ?? 0;
    if (indent > 0) content = content.slice(indent);
    const list = /^(?:[-+*]|[0-9]{1,9}[.)])[ \\t]+/.exec(content);
    if (list !== null) {
      content = content.slice(list[0].length);
      continue;
    }
    break;
  }
  return content;
}

/** A `<pre>` also represents an indented code block. Its source start line
 * distinguishes that from a fenced block, including a fence with no info. */
function fenceOpening(
  lines: string[],
  lineIndex: number | undefined,
): FenceOpening | undefined {
  if (lineIndex === undefined) return undefined;
  const line = lines[lineIndex];
  if (line === undefined) return undefined;
  const listIndent = listIndentAt(lines, lineIndex);
  const normalized = fenceContent(line, listIndent);
  const match = /^(`{3,}|~{3,})(.*)$/.exec(normalized);
  if (match === null) return undefined;
  const marker = match[1]?.[0];
  const info = (match[2] ?? "").trim();
  if (marker !== "`" && marker !== "~") return undefined;
  // CommonMark does not allow a backtick in a backtick fence's info string.
  if (marker === "`" && info.includes("`")) return undefined;
  return {
    marker,
    length: match[1]?.length ?? 0,
    info,
    blockquoteDepth: blockquoteDepth(line),
    listIndent: listIndentForFence(lines, lineIndex),
  };
}

function closesFence(
  lines: string[],
  lineIndex: number | undefined,
  opening: FenceOpening,
): boolean {
  if (lineIndex === undefined) return false;
  const line = lines[lineIndex] ?? "";
  if (blockquoteDepth(line) !== opening.blockquoteDepth) return false;
  let content = stripBlockquotes(line);
  const indentation = /^ */.exec(content)?.[0].length ?? 0;
  if (indentation < opening.listIndent) return false;
  if (opening.listIndent > 0) {
    content = content.slice(opening.listIndent);
  }
  return new RegExp(
    `^ {0,3}${opening.marker}{${opening.length},}[ \\t]*$`,
  ).test(content);
}

/** Stamp coordinates on each fenced `<code>` node. Markdown is rendered one
 * assistant Text block at a time, so an unfinished fence cannot consume the
 * next block and each block's ordinal matches the resolver. */
const stampArtifactFences =
  (textBlock: number | undefined, source: string) =>
  () =>
  (tree: StampNode) => {
    if (textBlock === undefined) return;
    const lines = source.split(/\r?\n/);
    let fence = 0;
    const visit = (node: StampNode) => {
      if (node.type === "element" && node.tagName === "pre") {
        const code = node.children?.find(
          (child) => child.type === "element" && child.tagName === "code",
        );
        const startLine = node.position?.start.line;
        const opening =
          startLine === undefined
            ? undefined
            : fenceOpening(lines, startLine - 1);
        if (code !== undefined && opening !== undefined) {
          const endLine = node.position?.end.line;
          const closed =
            endLine !== undefined && closesFence(lines, endLine - 1, opening);
          code.properties = {
            ...code.properties,
            "data-text-block-ordinal": textBlock,
            "data-fence-ordinal": fence,
            "data-fence-info": opening.info,
            "data-fence-closed": closed,
          };
          fence += 1;
        }
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
  artifact,
}: {
  children: string;
  /** 1-based source line ranges to stamp as changed (the file viewer);
   * absent everywhere else. */
  changedLines?: [number, number][];
  /** Present in the file viewer, where a link the caller does not follow
   * opens in a new tab rather than replacing the page the viewer is on. */
  followLink?: FollowLink;
  /** Only supplied for committed assistant prose in the real transcript. */
  artifact?: ArtifactContext;
}) {
  const stampFences = useMemo(
    () => stampArtifactFences(artifact?.textBlock, children),
    [artifact?.textBlock, children],
  );
  // Memoized because a fresh plugin array makes react-markdown reprocess on
  // every render of the caller.
  const rehypePlugins = useMemo(
    () => [
      stampFences,
      ...(changedLines === undefined || changedLines.length === 0
        ? []
        : [stampChanged(changedLines)]),
    ],
    [changedLines, stampFences],
  );
  const linked = useMemo<Components>(() => {
    const proseComponents: Components = {
      pre: ({ children }) => <>{children}</>,
      code: ({ className, children, node }) => {
        const lang = /language-([A-Za-z0-9_-]+)/.exec(className ?? "")?.[1];
        if (lang === undefined) return <code>{children}</code>;
        const source = String(children).replace(/\n$/, "");
        const textBlock = ordinal(node?.properties["data-text-block-ordinal"]);
        const fence = ordinal(node?.properties["data-fence-ordinal"]);
        const closed = node?.properties["data-fence-closed"] === true;
        const fenceInfo = node?.properties["data-fence-info"];
        if (
          lang === "html-artifact" &&
          fenceInfo === "html-artifact" &&
          artifact !== undefined &&
          closed &&
          textBlock !== undefined &&
          fence !== undefined
        ) {
          return (
            <Artifact
              source={source}
              seq={artifact.seq}
              textBlock={textBlock}
              fence={fence}
            />
          );
        }
        return (
          <Code text={source} lang={lang === "html-artifact" ? "html" : lang} />
        );
      },
    };
    if (followLink === undefined) return proseComponents;
    return {
      ...proseComponents,
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
  }, [artifact, followLink]);
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

function ordinal(value: unknown): number | undefined {
  if (typeof value === "number" && Number.isInteger(value) && value >= 0)
    return value;
  if (typeof value === "string" && /^[0-9]+$/.test(value)) return Number(value);
  return undefined;
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
