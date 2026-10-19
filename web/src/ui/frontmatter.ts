// A markdown file's leading `---` metadata block, for the file viewer's rendered
// view (docs/web-client.md "The file viewer"). Markdown has no such construct,
// so rendered as-is the opening fence is a rule and the closing one turns the
// fields into a setext heading.

/** The fields of a leading `---` … `---` block, one `key: value` per line, and
 * the markdown that follows it. The block's lines become empty lines in `body`
 * rather than being removed, so a source line number still names the same line
 * — the changed-line stamp maps through it (ui/Markdown.tsx). `fields` is
 * `null` for a file with no such block. A line that is not `key: value` is kept
 * as a field with an empty key rather than dropped. */
export function splitFrontmatter(text: string): {
  fields: [string, string][] | null;
  body: string;
} {
  const lines = text.split("\n");
  if (lines[0]?.trimEnd() !== "---") return { fields: null, body: text };
  const end = lines.findIndex((line, i) => i > 0 && line.trimEnd() === "---");
  if (end === -1) return { fields: null, body: text };
  const fields = lines.slice(1, end).flatMap((line): [string, string][] => {
    if (line.trim() === "") return [];
    const field = /^([\w-]+):\s*(.*)$/.exec(line);
    if (field === null) return [["", line.trim()]];
    return [[field[1], unquote(field[2])]];
  });
  const body = [
    ...lines.slice(0, end + 1).map(() => ""),
    ...lines.slice(end + 1),
  ];
  return { fields, body: body.join("\n") };
}

/** A YAML scalar written in double quotes, with its quotes and escaped quotes
 * undone; anything else as written. */
function unquote(value: string): string {
  const quoted = /^"(.*)"$/.exec(value);
  return quoted === null ? value : quoted[1].replace(/\\"/g, '"');
}
