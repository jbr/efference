import type { RawResult } from "../../api/entry.ts";
import type { WireState } from "../../api/types.ts";
import { diffRows, langForPath, resultRender } from "../../render/parse.ts";
import { RangeContent } from "../RangeContent.tsx";
import { ResultBody } from "../ToolBlocks.tsx";
import { capturedReads, currentReadPath } from "./fileCapture.ts";
import { openablePaths } from "../../session/touchedFiles.ts";
import "./FileOperation.css";
export function FileOperation({
  name,
  input,
  raw,
  wire,
  onCurrent,
}: {
  name: string;
  input: unknown;
  raw: RawResult | null;
  wire: WireState;
  onCurrent: (path: string) => void;
}) {
  const obj = (
    typeof input === "object" && input !== null ? input : {}
  ) as Record<string, unknown>;
  const files =
    raw !== null && !raw.is_error && name === "read"
      ? capturedReads(raw.content, wire)
      : [];
  const paths = openablePaths(name, input);
  const note =
    typeof wire === "object" && "Triaged" in wire ? wire.Triaged.note : null;
  const content = typeof obj.contents === "string" ? obj.contents : null;
  const diff = raw !== null && !raw.is_error ? diffRows(raw.content) : null;
  return (
    <div className="file-operation">
      <p className="file-operation-kind">
        {name === "read"
          ? "Captured read"
          : name === "write"
            ? obj.append === true
              ? "Appended content"
              : "Written content"
            : "Recorded edit"}{" "}
        · historical operation
      </p>
      {note && <p className="dim">{note}</p>}
      {raw?.content_elided && (
        <p role="status">
          Only a captured prefix is available; remaining content has not loaded.
        </p>
      )}
      {name === "write" && content !== null ? (
        <>
          <p className="dim">
            {obj.append === true
              ? "These are the appended bytes, not the whole file."
              : "Content submitted by this call; not the file as it is now."}
            {raw?.is_error
              ? " The write failed; this content was not confirmed written."
              : raw === null
                ? " The write has not returned yet."
                : ""}
          </p>
          <RangeContent
            text={content}
            retained={[[1, content.split("\n").length]]}
            lang={langForPath(String(obj.path ?? ""))}
          />
          {raw && (
            <p className={raw.is_error ? "error" : "dim"}>{raw.content}</p>
          )}
        </>
      ) : files.length > 0 ? (
        files.map((file, i) => (
          <section key={`${file.path}-${i}`}>
            <h3>{file.path}</h3>
            <p className="dim">
              Captured content returned by this read. It may be a requested
              range or truncated excerpt.
            </p>
            <RangeContent
              text={file.text}
              retained={file.retained}
              lang={langForPath(file.path)}
              startLine={file.startLine}
            />
            {currentReadPath(file, input) !== null && (
              <button
                className="linkish"
                onClick={() => onCurrent(currentReadPath(file, input)!)}
              >
                View current file →
              </button>
            )}
          </section>
        ))
      ) : raw !== null ? (
        <ResultBody
          body={
            diff !== null
              ? { kind: "diff", lines: diff.map(([, line]) => line) }
              : resultRender(name, raw, "Raw").body
          }
        />
      ) : (
        <p className="dim">Waiting for the operation's result…</p>
      )}
      {files.length === 0 &&
        paths.map((path) => (
          <button
            className="linkish"
            key={path}
            onClick={() => onCurrent(path)}
          >
            {name === "edit" || name === "retarget_edit" || name === "undo_edit"
              ? "Locate in current file"
              : "View current file"}
            {paths.length > 1 ? ` · ${path}` : ""} →
          </button>
        ))}
    </div>
  );
}
