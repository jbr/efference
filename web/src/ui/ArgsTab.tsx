// The inspector's args tab (docs/web-client.md "The inspector"): what the call
// sent, destructured against the tool's declared schema.
//
// Three sources meet here and stay separate. `callArgs` (render/parse.ts) parses
// the input the model sent; `argsAgainstSchema` (render/argSchema.ts) is the pure
// join against the definition; `useToolEntry` fetches that definition lazily and
// memoizes it. With no definition — a recorded session opened offline, a 404, a
// failed fetch — the join yields the sent fields unglossed, which is exactly the
// rendering this tab had before it fetched anything, with no error surface.
//
// Prose from a mounted server is third-party content shown to a human: it is
// rendered as plain text (never through Markdown.tsx) and attributed to the
// server that supplied it, the line the built-ins' annotations already draw.

import { useMemo, useState } from "react";
import { Code } from "./Code.tsx";
import { callArgs, splitToolName, type ArgField } from "../render/parse.ts";
import {
  argsAgainstSchema,
  type AbsentArg,
  type SentArg,
} from "../render/argSchema.ts";
import { useToolEntry } from "./toolDefinition.ts";

/** Lines a code-valued argument shows before it asks. Past this the tab stops
 * being a list of arguments and becomes one long scroll of the biggest one —
 * edit `content` runs to 16kB at the top of the corpus. */
const ARG_FOLD_LINES = 14;

/** A description as it is displayed: plain text, whoever wrote it. The class
 * says so in the markup — a server's sentence must not become markup, and the
 * distinction has to survive to the pixel rather than living in a comment. */
function Prose({ text }: { text: string }) {
  return <div className="tool-prose plain-text">{text}</div>;
}

/** The tool this call was made against — the one place a tool description is
 * paid for: on demand, for a call someone is already asking about. The
 * transcript deliberately never shows it, because a static sentence repeated
 * across every call of the tool is noise. */
function ToolHeading({
  name,
  title,
  description,
  server,
}: {
  name: string;
  title: string | undefined;
  description: string | undefined;
  server: string | null;
}) {
  const split = splitToolName(name);
  return (
    <div className="tool-def">
      <div className="tool-def-name">
        {split.server !== null && (
          <span className="call-server">{split.server}.</span>
        )}
        {split.tool}
        {title !== undefined && <span className="dim"> · {title}</span>}
      </div>
      {description !== undefined && <Prose text={description} />}
      {server !== null && (
        // Attributed once for the definition: the tool's own prose and every
        // argument description below it come from this server.
        <div className="dim prose-source">
          described by {server} · shown as plain text
        </div>
      )}
    </div>
  );
}

/** One argument. A scalar is inline beside its key; a multi-line value is a
 * code block under it, folded to a readable head when it is long. The schema's
 * gloss sits under the value, and the two flags a reader is looking for —
 * `required`, and a key the schema does not declare — ride the key. */
function ArgRow({ arg }: { arg: SentArg }) {
  const [open, setOpen] = useState(false);
  const field: ArgField = arg.field;
  const marks = (
    <>
      {arg.required && <span className="arg-required">required</span>}
      {arg.undeclared && (
        <span
          className="arg-undeclared"
          title="the schema declares no such key"
        >
          not in schema
        </span>
      )}
    </>
  );
  if (field.code === null) {
    return (
      <div className="arg-row">
        <dt className="arg-key">
          {field.key}
          {marks}
        </dt>
        <dd>
          <div className="arg-scalar">{field.scalar}</div>
          {arg.description !== null && <Prose text={arg.description} />}
        </dd>
      </div>
    );
  }
  const lines = field.code.text.split("\n");
  const folded = !open && lines.length > ARG_FOLD_LINES;
  const shown = folded
    ? lines.slice(0, ARG_FOLD_LINES).join("\n")
    : field.code.text;
  return (
    <div className="arg-row block">
      <dt className="arg-key">
        {field.key}
        {marks}
        <span className="dim"> · {lines.length} lines</span>
        {lines.length > ARG_FOLD_LINES && (
          <button className="linkish" onClick={() => setOpen(!open)}>
            {open ? "fold" : "show all"}
          </button>
        )}
      </dt>
      <dd>
        {arg.description !== null && <Prose text={arg.description} />}
        <Code text={shown} lang={field.code.lang} />
      </dd>
    </div>
  );
}

/** What the schema declares and the call did not pass. Dimmer than the sent
 * arguments — an omitted optional is ordinary — except for a required one,
 * which is the shape of a whole class of failed calls. */
function AbsentList({ absent }: { absent: AbsentArg[] }) {
  return (
    <div className="args-absent">
      <div className="dim args-absent-title">declared, not passed</div>
      <dl className="arg-list">
        {absent.map((arg) => (
          <div className="arg-row absent" key={arg.key}>
            <dt className="arg-key">
              {arg.key}
              {arg.required && (
                <span className="arg-required missing">required</span>
              )}
            </dt>
            <dd>
              {arg.description !== null && <Prose text={arg.description} />}
            </dd>
          </div>
        ))}
      </dl>
    </div>
  );
}

export function ArgsTab({
  name,
  input,
  intent,
}: {
  /** The call's wire name — the address the definition is fetched by. */
  name: string;
  input: unknown;
  /** The call's `intent`, which leads the tab as it leads the transcript entry;
   * it is filtered out of the arguments on both the sent and declared sides. */
  intent: string | null;
}) {
  const entry = useToolEntry(name);
  // The tab's own view of the input, not the header's: `callView.body` is null
  // for the edit family by design, which is why this tab used to claim the
  // header said everything while `anchor` and `content` went unshown.
  const fields = useMemo(() => callArgs(name, input), [name, input]);
  const { sent, absent } = useMemo(
    () => argsAgainstSchema(fields, entry === null ? null : entry.input_schema),
    [fields, entry],
  );
  const server = entry?.server ?? null;
  return (
    <>
      {entry !== null && (
        <ToolHeading
          name={entry.name}
          title={entry.title}
          description={entry.description}
          server={server}
        />
      )}
      {intent !== null && <div className="intent">{intent}</div>}
      {sent.length > 0 ? (
        <dl className="arg-list">
          {sent.map((arg) => (
            <ArgRow key={arg.field.key} arg={arg} />
          ))}
        </dl>
      ) : (
        <div className="dim">this call took no arguments</div>
      )}
      {absent.length > 0 && <AbsentList absent={absent} />}
    </>
  );
}
