import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it } from "vitest";
import type { Arrival, UserContent } from "../api/types.ts";
import { SentDocument } from "./SentDocument.tsx";

const upload: Arrival = {
  name: "trace.bin",
  path: "/uploads/trace.bin",
  kind: { Binary: { bytes: 192 } },
};
const references: UserContent["references"] = {
  file: {
    label: "[draft].md",
    target: {
      type: "filesystem",
      target: "file",
      original: "notes/[draft].md",
      path: "/work/notes/[draft].md",
      access: { read: true, write: true },
    },
  },
  agent: { label: "@helper", target: { type: "agent", session: "helper" } },
  upload: { label: upload.name, target: { type: "attachment", ...upload } },
};
const render = (
  content: UserContent["content"],
  legacyAttachments?: Arrival[],
) =>
  renderToStaticMarkup(
    <SentDocument
      document={{ content, references }}
      legacyAttachments={legacyAttachments}
    />,
  );

const count = (html: string, needle: string) => html.split(needle).length - 1;

describe("sent structured Markdown", () => {
  it("preserves emphasis, links and lists across repeated occurrences; literal spellings stay prose", () => {
    const html = render([
      { Text: "- **Read " },
      { Reference: "file" },
      { Text: "** and [ask " },
      { Reference: "agent" },
      { Text: "](https://example.com).\n- Compare " },
      { Reference: "file" },
      { Text: " with @helper and notes/[draft].md." },
    ]);
    expect(html).toContain('<li><strong>Read <span class="reference-chip"');
    expect(html).toContain('<a href="https://example.com">ask @helper</a>');
    expect(html).toContain(" with @helper and notes/[draft].md.");
    expect(count(html, 'class="reference-chip"')).toBe(2);
    expect(count(html, "<code>/work/notes/[draft].md</code>")).toBe(1);
    expect(html).toContain("Requested access: Read and write");
    expect(html).toContain("<summary>Reference details</summary>");
    expect(html).not.toContain("<button");
  });

  it("links to sessions and only makes in-workspace files actionable", () => {
    const document = {
      content: [
        { Reference: "file" },
        { Text: " and " },
        { Reference: "agent" },
      ],
      references,
    };
    const markup = (workspace: string) =>
      renderToStaticMarkup(
        <SentDocument
          document={document}
          workspace={workspace}
          onOpenReference={() => {}}
        />,
      );
    expect(markup("/work/")).toContain('<button class="reference-chip"');
    expect(markup("/work/")).toContain('href="/sessions/helper/"');
    expect(markup("/wor")).not.toContain("<button");
  });

  it("keeps the image lightbox button at the authored position", () => {
    const image: Arrival = {
      name: "pic.png",
      path: "/uploads/pic.png",
      kind: { Image: { format: "PNG", width: 200, height: 100, bytes: 123 } },
    };
    const html = renderToStaticMarkup(
      <SentDocument
        document={{
          content: [
            { Text: "Before " },
            { Reference: "pic" },
            { Text: " after." },
          ],
          references: {
            pic: {
              label: image.name,
              target: { type: "attachment", ...image },
            },
          },
        }}
        legacyAttachments={[image]}
      />,
    );
    expect(html).toContain(
      '<p>Before <button type="button" class="attachment-image" aria-label="open pic.png full screen">',
    );
    expect(html).toContain("</button> after.</p>");
    expect(count(html, 'class="attachment-image"')).toBe(1);
  });

  it("leaves missing and prototype-name IDs as text, not links or chips", () => {
    const html = render([
      { Text: "Missing: " },
      { Reference: "toString" },
      { Text: " and " },
      { Reference: "unknown" },
      { Text: "\n\n[unknown]: https://example.com" },
    ]);
    expect(html).toContain("[toString]");
    expect(html).toContain("[unknown]");
    expect(html).not.toMatch(/<a |reference-chip|Reference details/);
  });

  it("keeps literal marker lookalikes (including entities) and restores text in code and destinations", () => {
    const html = render([
      { Text: "efferenceatom0x0x &#101;fferenceatom1x0x `" },
      { Reference: "agent" },
      { Text: "` [visit](https://example.com/" },
      { Reference: "agent" },
      { Text: ") then " },
      { Reference: "agent" },
    ]);
    expect(html).toContain(
      "efferenceatom0x0x efferenceatom1x0x <code>@helper</code>",
    );
    expect(html).toContain('href="https://example.com/@helper"');
    expect(count(html, 'class="reference-chip"')).toBe(1);
    expect(html).not.toContain("efferenceatom2x");
  });

  it("shows inline and appended uploads once without altering a closing fence", () => {
    const other = { ...upload, name: "other.bin", path: "/uploads/other.bin" };
    const inline = render(
      [
        { Text: "See " },
        { Reference: "upload" },
        { Text: " above." },
        { Reference: "upload" },
      ],
      [upload, other],
    );
    expect(inline).toContain(
      '<p>See <a class="reference-chip" href="/v1/attachments/trace.bin" download="trace.bin">',
    );
    expect(count(inline, 'class="reference-chip"')).toBe(1);
    expect(inline).not.toContain('class="attachment-name">trace.bin');
    expect(inline).toContain('class="attachment-name">other.bin');
    const trailing = render([
      { Text: "```\nexample\n```" },
      { Reference: "upload" },
    ]);
    expect(trailing).toContain("<code>example\n</code>");
    expect(trailing).toContain('class="sent-document-uploads"');
    expect(count(trailing, 'class="reference-chip"')).toBe(1);
  });
});
