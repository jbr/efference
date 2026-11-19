import type { ReactNode } from "react";
import type { Components } from "react-markdown";

export interface MarkdownAtom {
  text: string;
  render: ReactNode;
}

/** Inline atoms enter the parser as word-like tokens. Parsing the whole source
 * first keeps emphasis, lists and tables intact across occurrence boundaries.
 * The namespace must be absent from authored prose, including the numeric
 * entity spellings the Markdown parser decodes into ASCII letters/digits. */
export function markdownAtomPrefix(prose: string): string {
  const decoded = prose.replace(
    /&#(x[\da-f]{1,6}|\d{1,7});/gi,
    (entity, digits: string) => {
      const hex = digits[0].toLowerCase() === "x";
      const code = hex ? parseInt(digits.slice(1), 16) : Number(digits);
      return code < 128 ? String.fromCharCode(code) : entity;
    },
  );
  const used = new Set(decoded.toLowerCase().match(/efferenceatom\d+x/g));
  let n = 0;
  while (used.has(`efferenceatom${n}x`)) n++;
  return `efferenceatom${n}x`;
}

interface Node {
  type: string;
  children?: Node[];
  value?: string;
  url?: string;
  title?: string | null;
  alt?: string;
  data?: { hName: string; hProperties: Record<string, string> };
}

export interface MarkdownAtoms {
  plugin: () => (tree: Node) => void;
  span: Components["span"];
}

export function markdownAtoms(
  prefix: string,
  atoms: MarkdownAtom[],
): MarkdownAtoms {
  const marker = new RegExp(`${prefix}(\\d+)x`, "g");
  const restore = (s: string) =>
    s.replace(marker, (token, n: string) => atoms[Number(n)]?.text ?? token);
  const text = (value: string): Node => ({ type: "text", value });
  const split = (node: Node): Node[] => {
    const value = node.value ?? "";
    const nodes: Node[] = [];
    let from = 0;
    for (const match of value.matchAll(marker)) {
      const atom = atoms[Number(match[1])];
      if (atom === undefined) continue;
      if (from < match.index) nodes.push(text(value.slice(from, match.index)));
      nodes.push({
        type: "sentReference",
        data: {
          hName: "span",
          hProperties: { "data-markdown-atom": match[1] },
        },
        children: [text(atom.text)],
      });
      from = match.index + match[0].length;
    }
    if (from === 0) return [node];
    if (from < value.length) nodes.push(text(value.slice(from)));
    return nodes;
  };
  const visit = (node: Node, insideLink = false): Node[] => {
    if (node.type === "text" && !insideLink) return split(node);
    // Code, raw markup, and link/image metadata cannot contain inline React.
    // Restore their readable text before the normal HTML/URL handling.
    if (node.value !== undefined) node.value = restore(node.value);
    if (node.url !== undefined) node.url = restore(node.url);
    if (node.alt !== undefined) node.alt = restore(node.alt);
    if (node.title != null) node.title = restore(node.title);
    // The authored link owns navigation: never put a chip control inside it.
    const linked =
      insideLink || node.type === "link" || node.type === "linkReference";
    if (node.children !== undefined)
      node.children = node.children.flatMap((child) => visit(child, linked));
    return [node];
  };
  return {
    plugin: () => (tree) => {
      visit(tree);
    },
    span: ({ node, children }) => {
      const slot = node?.properties["data-markdown-atom"];
      return typeof slot === "string" ? (
        (atoms[Number(slot)]?.render ?? children)
      ) : (
        <span>{children}</span>
      );
    },
  };
}
