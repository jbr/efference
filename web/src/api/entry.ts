// A log entry as the control channel serves it (docs/control-api.md): the stored
// `LogEntry` of types.ts with what the host's entry view changes. The view is the
// entry verbatim except for three things, each derived on every read and none in
// the log (src/routes/entry_view.rs):
//
// - a result image's base64 `data` is left out; its bytes are at
//   `GET /log/{seq}/images/{idx}`;
// - a committed assistant entry carries `artifacts`, the signed links of its
//   artifact fences (docs/artifacts.md);
// - a triaged result, and the verdict that decides a captured one, carries
//   `selection`, the line arithmetic of the verdict against the whole content.
//
// The host writes the view by hand, so these three are declared here rather than
// generated; everything else is the generated type.

import type {
  ArtifactLink,
  LogEntry as StoredEntry,
  Payload as StoredPayload,
  RawResult as StoredRawResult,
  ResultImage as StoredResultImage,
  SelectionLines,
} from "./types.ts";

/** An entry's address in the log. */
export type Seq = number;

/** An image of a result as the view carries it: its dimensions, and no bytes. */
export type ResultImage = Omit<StoredResultImage, "data">;

/** A result as the view carries it: images by reference, and a `content` that may be
 * a prefix (`content_elided`). */
export type RawResult = Omit<StoredRawResult, "images"> & {
  images?: ResultImage[];
};

type Variant<K extends string> = Record<K, unknown>;
type Inner<K extends string> = Extract<StoredPayload, Variant<K>>[K];

export type Payload =
  | Exclude<
      StoredPayload,
      Variant<"Assistant"> | Variant<"ToolResult"> | Variant<"ResultVerdict">
    >
  | {
      Assistant: Inner<"Assistant"> & {
        /** Derived on every read: present when the entry's text has
         * `html-artifact` or `mermaid` fences. */
        artifacts?: ArtifactLink[];
      };
    }
  | {
      ToolResult: Omit<Inner<"ToolResult">, "raw"> & {
        raw: RawResult;
        /** The verdict's line arithmetic against the *whole* content. Present
         * only on a `Triaged` result; the authority for "kept N of M lines"
         * when `raw.content` is a truncated prefix, which cannot be counted
         * client-side. */
        selection?: SelectionLines;
      };
    }
  | {
      ResultVerdict: Inner<"ResultVerdict"> & {
        /** The line arithmetic of a `Triaged` wire, against the result's
         * whole content — the same keys a directly-logged result carries. */
        selection?: SelectionLines;
      };
    };

export interface LogEntry extends Omit<StoredEntry, "payload"> {
  payload: Payload;
}
