import { useEffect, useRef, useState } from "react";
import type { Editor } from "@tiptap/core";
import { closeHistory } from "@tiptap/pm/history";
import { discoverAgents, discoverPaths } from "../api/discovery.ts";
import { sessionId } from "../api/base.ts";
import type { ReferenceDefinition } from "../api/types.ts";
import type { CompletionOption } from "./ComposerReferences.tsx";
import {
  completionMatch,
  matchKey,
  type CompletionMatch,
} from "./completionMatch.ts";

interface Candidate extends CompletionOption {
  definition: ReferenceDefinition;
  spelling?: string;
}
interface Completion {
  match: CompletionMatch;
  options: Candidate[];
  selected: number;
  state: "ready" | "loading" | "empty" | "error";
  truncated: boolean;
}

function folderAction(
  label: string,
  target: Extract<ReferenceDefinition["target"], { type: "filesystem" }>,
): Candidate {
  return {
    id: "insert-current-folder",
    label: target.path,
    detail: "",
    kind: "directory",
    insertFolder: true,
    definition: { label, target },
  };
}

/** Discovery has operator authority but edits no policy. Only accepting a row
 * for insertion creates a reference; browsing replaces text, and permissions remain admission-time
 * work (docs/web-client.md, docs/permissions.md). */
export function useComposerCompletion(editor: Editor | null, base: string) {
  const [snapshot, setSnapshot] = useState<Completion | null>(null);
  const live = useRef<Completion | null>(null);
  const dismissed = useRef<string | null>(null);
  const opened = useRef<{
    from: number;
    prefix: string;
    option: Candidate;
  } | null>(null);
  const refresh = useRef<() => void>(() => {});

  const publish = (value: Completion | null) => {
    live.current = value;
    setSnapshot(value);
  };

  useEffect(() => {
    if (!editor) return;
    let generation = 0;
    let timer: ReturnType<typeof setTimeout> | undefined;
    let controller: AbortController | undefined;
    const cancel = () => {
      generation++;
      clearTimeout(timer);
      controller?.abort();
    };
    const update = () => {
      const match = editor.isFocused
        ? completionMatch(editor.state, opened.current)
        : null;
      if (!match) {
        dismissed.current = null;
      }
      if (!match || dismissed.current === matchKey(match)) {
        cancel();
        publish(null);
        return;
      }
      if (live.current && matchKey(live.current.match) === matchKey(match))
        return;
      cancel();
      const known = opened.current;
      const folder =
        known?.prefix === match.prefix && known.from === match.from
          ? known.option
          : undefined;
      publish({
        match,
        options: folder ? [folder] : [],
        selected: 0,
        state: "loading",
        truncated: false,
      });
      const requestGeneration = generation;
      const query = match;
      controller = new AbortController();
      const signal = controller.signal;
      timer = setTimeout(() => {
        const load = async () => {
          let options: Candidate[];
          let problem: string | null | undefined;
          let truncated: boolean;
          let insert = folder;
          if (query.kind === "paths") {
            const result = await discoverPaths(base, query.prefix, signal);
            if (result.prefix !== query.prefix)
              throw new Error("Discovery prefix mismatch");
            problem = result.problem;
            const { directory } = result;
            if (
              !problem &&
              !insert &&
              query.prefix.endsWith("/") &&
              directory
            ) {
              insert = folderAction(
                directory.split("/").filter(Boolean).at(-1) ?? "/",
                {
                  type: "filesystem",
                  original: query.prefix,
                  path: directory,
                  target: "directory",
                  access: { read: true, write: false },
                },
              );
            }
            truncated = result.truncated;
            options = result.entries.map((entry) => ({
              id: entry.path,
              label: entry.name + (entry.target === "directory" ? "/" : ""),
              detail: entry.path,
              kind: entry.target,
              spelling: entry.original,
              definition: {
                label: entry.name,
                target: {
                  type: "filesystem",
                  original: entry.original,
                  path: entry.path,
                  target: entry.target,
                  access: { read: true, write: false },
                },
              },
            }));
          } else {
            const result = await discoverAgents(base, query.prefix, signal);
            if (result.prefix !== query.prefix)
              throw new Error("Discovery prefix mismatch");
            problem = result.problem;
            truncated = result.truncated;
            options = result.agents
              .filter((agent) => agent.id !== sessionId(base))
              .map((agent) => ({
                id: agent.id,
                label: `@${agent.id}`,
                detail:
                  agent.title ?? agent.working ?? agent.cwd ?? agent.status,
                kind: "agent",
                spelling: `@${agent.id}`,
                definition: {
                  label: agent.id,
                  target: { type: "agent", session: agent.id },
                },
              }));
          }
          if (requestGeneration !== generation || signal.aborted) return;
          const rows = [
            ...(insert ? [insert] : []),
            ...(problem ? [] : options),
          ];
          publish({
            match: query,
            options: rows,
            selected: 0,
            state: problem ? "error" : rows.length ? "ready" : "empty",
            truncated,
          });
        };
        void load().catch(() => {
          if (requestGeneration === generation && !signal.aborted)
            publish({
              match: query,
              options: folder ? [folder] : [],
              selected: 0,
              state: "error",
              truncated: false,
            });
        });
      }, 100);
    };
    const blur = () => {
      cancel();
      publish(null);
    };
    refresh.current = update;
    editor.on("transaction", update);
    editor.on("focus", update);
    editor.on("blur", blur);
    update();
    return () => {
      cancel();
      editor.off("transaction", update);
      editor.off("focus", update);
      editor.off("blur", blur);
      live.current = null;
      opened.current = null;
    };
  }, [editor, base]);

  const dismiss = () => {
    if (live.current) dismissed.current = matchKey(live.current.match);
    publish(null);
    // Invalidate any request that was in flight when Escape was pressed.
    refresh.current();
  };
  const accept = (index: number) => {
    const current = live.current;
    const candidate = current?.options[index];
    if (!editor || !current || !candidate) return;
    const { from, to } = current.match;
    const { label, target } = candidate.definition;
    if (
      candidate.kind === "directory" &&
      !candidate.insertFolder &&
      candidate.spelling &&
      target.type === "filesystem"
    ) {
      const prefix = `${candidate.spelling}/`;
      opened.current = {
        from,
        prefix,
        option: folderAction(label, target),
      };
      editor.view.dispatch(
        closeHistory(editor.state.tr).insertText(prefix, from, to),
      );
      return;
    }
    opened.current = null;
    publish(null);
    editor.view.dispatch(
      closeHistory(editor.state.tr).replaceWith(
        from,
        to,
        editor.schema.nodes.reference.create({
          id: crypto.randomUUID(),
          definition: candidate.definition,
        }),
      ),
    );
    editor.commands.focus(null, { scrollIntoView: false });
  };
  const highlight = (selected: number) => {
    if (live.current) publish({ ...live.current, selected });
  };
  const keyDown = (event: KeyboardEvent): boolean => {
    const current = live.current;
    if (
      !editor ||
      !current ||
      event.isComposing ||
      editor.view.composing ||
      event.keyCode === 229
    )
      return false;
    if (event.key === "Escape") {
      event.preventDefault();
      dismiss();
      return true;
    }
    if (
      (event.key === "ArrowDown" || event.key === "ArrowUp") &&
      !event.shiftKey &&
      !event.metaKey &&
      !event.ctrlKey &&
      !event.altKey
    ) {
      event.preventDefault();
      if (current.options.length)
        highlight(
          (current.selected +
            (event.key === "ArrowDown" ? 1 : current.options.length - 1)) %
            current.options.length,
        );
      return true;
    }
    if (event.key === "Enter" || (event.key === "Tab" && !event.shiftKey)) {
      event.preventDefault();
      if (!event.repeat) accept(current.selected);
      return true;
    }
    return false;
  };
  return { snapshot, keyDown, accept, highlight };
}
