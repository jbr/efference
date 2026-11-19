import { plainId, sessionHref } from "../api/base.ts";
import { attachmentUrl } from "../api/http.ts";
import { useMemo } from "react";
import { readableOccurrence } from "../api/document.ts";
import type {
  Arrival,
  ReferenceDefinition,
  UserContent,
} from "../api/types.ts";
import { SentAttachments, SentThumbnail } from "./Attachments.tsx";
import { ReferenceChipFrame } from "./ComposerReferences.tsx";
import { Disclosure } from "./Disclosure.tsx";
import { Markdown } from "./Markdown.tsx";
import {
  markdownAtomPrefix,
  markdownAtoms,
  type MarkdownAtom,
} from "./markdownAtoms.tsx";
import { describeKind, isImage } from "./staging.ts";
import "./SentDocument.css";

export type OpenReference = (path: string, directory: boolean) => void;

/** The Files sidebar serves only the working directory. Use the resolved
 * path, never the historical input spelling or its requested permissions. */
function workspacePath(path: string, workspace?: string): string | null {
  if (!workspace?.startsWith("/")) return null;
  const root = workspace.replace(/\/+$/, "");
  if (path === root || path === `${root}/`) return "";
  if (!path.startsWith(`${root}/`)) return null;
  const relative = path.slice(root.length + 1).replace(/\/+$/, "");
  return relative.split("/").some((s) => s === ".." || s === "." || s === "")
    ? null
    : relative;
}

function sentChip(
  { target }: ReferenceDefinition,
  text: string,
  workspace?: string,
  onOpenReference?: OpenReference,
) {
  switch (target.type) {
    case "filesystem": {
      const path = workspacePath(target.path, workspace);
      const directory = target.target === "directory";
      return (
        <ReferenceChipFrame
          kind={target.target}
          label={text}
          title={target.path}
          onClick={
            path === null || onOpenReference === undefined
              ? undefined
              : () => onOpenReference(path, directory)
          }
        />
      );
    }
    case "agent":
      return (
        <ReferenceChipFrame
          kind="agent"
          label={text}
          href={
            plainId(target.session) ? sessionHref(target.session) : undefined
          }
        />
      );
    case "attachment":
      return isImage(target.kind) ? (
        <SentThumbnail attachment={target} />
      ) : (
        <ReferenceChipFrame
          kind="attachment"
          label={text}
          href={attachmentUrl(target.name)}
          download={target.name}
        />
      );
  }
}

function ReferenceDetails({ definition }: { definition: ReferenceDefinition }) {
  const { target, label } = definition;
  if (target.type === "filesystem") {
    const requested = target.access.write
      ? target.access.read
        ? "Read and write"
        : "Write only"
      : target.access.read
        ? "Read only"
        : "None";
    return (
      <>
        <code>{target.original}</code> <span>→</span> <code>{target.path}</code>
        <div>Requested access: {requested}</div>
      </>
    );
  }
  if (target.type === "agent") return <code>@{target.session}</code>;
  return (
    <>
      <code>{target.name}</code> · {describeKind(target.kind)}
      <div>
        <code>{target.path}</code>
      </div>
      {label !== target.name && <div>{label}</div>}
    </>
  );
}

function sentContent(
  document: UserContent,
  workspace?: string,
  onOpenReference?: OpenReference,
) {
  const source = document.content
    .flatMap((s) => ("Text" in s ? [s.Text] : []))
    .join("");
  const prefix = markdownAtomPrefix(source);
  const atoms: MarkdownAtom[] = [];
  const details = new Map<string, ReferenceDefinition>();
  const uploads = new Set<string>();
  const trailing: {
    id: string;
    definition: ReferenceDefinition;
    text: string;
  }[] = [];
  // The host appends uploads after prose without a separator. Keep a trailing
  // run outside Markdown so it can't alter a closing fence or link definition.
  let tail = document.content.length;
  const definitionFor = (id: string) =>
    Object.hasOwn(document.references, id)
      ? document.references[id]
      : undefined;
  while (tail > 0) {
    const segment = document.content[tail - 1];
    if (
      !("Reference" in segment) ||
      definitionFor(segment.Reference)?.target.type !== "attachment"
    )
      break;
    tail--;
  }
  const prose = document.content
    .map((segment, index) => {
      if ("Text" in segment) return segment.Text;
      const id = segment.Reference;
      const definition = definitionFor(id);
      let text = `[${id}]`;
      if (definition !== undefined) {
        const { target } = definition;
        if (target.type === "attachment") {
          if (uploads.has(target.name)) return "";
          uploads.add(target.name);
          text = target.name;
        } else {
          text = readableOccurrence(definition);
        }
        details.set(id, definition);
        if (index >= tail) {
          trailing.push({ id, definition, text });
          return "";
        }
      }
      const slot = atoms.length;
      atoms.push({
        text,
        render:
          definition === undefined
            ? text
            : sentChip(definition, text, workspace, onOpenReference),
      });
      return `${prefix}${slot}x`;
    })
    .join("");
  return {
    prose,
    atoms: atoms.length > 0 ? markdownAtoms(prefix, atoms) : undefined,
    details,
    trailing,
    uploads,
  };
}

export function SentDocument({
  document,
  legacyAttachments = [],
  workspace,
  onOpenReference,
}: {
  document: UserContent;
  legacyAttachments?: Arrival[];
  workspace?: string;
  onOpenReference?: OpenReference;
}) {
  const { prose, atoms, details, trailing, uploads } = useMemo(
    () => sentContent(document, workspace, onOpenReference),
    [document, workspace, onOpenReference],
  );
  return (
    <div className="sent-document">
      {prose !== "" && <Markdown atoms={atoms}>{prose}</Markdown>}
      {trailing.length !== 0 && (
        <div className="sent-document-uploads">
          {trailing.map(({ id, definition, text }) => (
            <span key={id}>
              {sentChip(definition, text, workspace, onOpenReference)}
            </span>
          ))}
        </div>
      )}
      <SentAttachments
        attachments={legacyAttachments.filter((a) => !uploads.has(a.name))}
      />
      {details.size !== 0 && (
        <Disclosure label="Reference details" className="sent-document-details">
          <ul>
            {[...details].map(([id, definition]) => (
              <li key={id}>
                <ReferenceDetails definition={definition} />
              </li>
            ))}
          </ul>
        </Disclosure>
      )}
    </div>
  );
}
