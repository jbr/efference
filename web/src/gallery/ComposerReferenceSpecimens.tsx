import { useId, useState } from "react";
import {
  CompletionMenu,
  CompletionTray,
  ReferenceChip,
  type CompletionOption,
} from "../ui/ComposerReferences.tsx";
import { Specimen, States } from "./Specimens.tsx";
import { InlineComposerSketch } from "./InlineComposerSketch.tsx";
import { UploadSpecimens } from "./UploadSpecimens.tsx";

const paths: CompletionOption[] = [
  {
    id: "./web/src/",
    label: "src/",
    detail: "./web/ · directory",
    kind: "directory",
  },
  {
    id: "./web/public/",
    label: "public/",
    detail: "./web/ · directory",
    kind: "directory",
  },
  {
    id: "./web/package.json",
    label: "package.json",
    detail: "./web/ · file",
    kind: "file",
  },
];
const sourceFiles: CompletionOption[] = [
  { id: "./web/src/main.tsx", label: "main.tsx", detail: "", kind: "file" },
  { id: "./web/src/index.css", label: "index.css", detail: "", kind: "file" },
];
const agents: CompletionOption[] = [
  {
    id: "@new-lilac",
    label: "@new-lilac",
    detail: "Composer references · working",
    kind: "agent",
  },
  {
    id: "@composer-contract",
    label: "@composer-contract",
    detail: "Shared contract · idle",
    kind: "agent",
  },
];

export function CompletionSketch() {
  const id = useId();
  const [text, setText] = useState("Please check ./web/s");
  const [mode, setMode] = useState<"paths" | "agents">("paths");
  const [selected, setSelected] = useState(0);
  const [open, setOpen] = useState(true);
  const [chips, setChips] = useState<(CompletionOption & { write: boolean })[]>(
    [],
  );
  const [notice, setNotice] = useState(
    "Fixture only — no messages or permissions are changed.",
  );
  const prefix = text.slice("Please check ".length);
  const folder = prefix.slice(0, prefix.lastIndexOf("/") + 1);
  const children =
    folder === "./web/"
      ? paths
      : folder === "./web/src/"
        ? sourceFiles
        : folder === "./web/public/"
          ? [
              {
                id: "./web/public/favicon.svg",
                label: "favicon.svg",
                detail: "",
                kind: "file" as const,
              },
            ]
          : [];
  const options: CompletionOption[] =
    mode === "agents"
      ? agents
      : [
          ...(folder && prefix === folder
            ? [
                {
                  id: folder,
                  label: `/home/demo/project/${folder.slice(2, -1)}`,
                  detail: "",
                  kind: "directory" as const,
                  insertFolder: true,
                },
              ]
            : []),
          ...children.filter((child) => child.id.startsWith(prefix)),
        ];
  function accept(index: number) {
    const option = options[index];
    if (!option) return;
    if (option.kind === "directory" && !option.insertFolder) {
      setText(`Please check ${option.id}`);
      setSelected(0);
      setNotice(`Opened ${option.label}. No reference was inserted.`);
      return;
    }
    const label = option.insertFolder
      ? (folder.split("/").filter(Boolean).at(-1) ?? "/") + "/"
      : option.label;
    setChips((current) => [
      ...current,
      { ...option, label, id: `${option.id}-${current.length}`, write: false },
    ]);
    setText("Please check ");
    setOpen(false);
    setNotice(`Accepted ${label} as a reference. Nothing was sent.`);
  }
  function reset(next: "paths" | "agents") {
    setMode(next);
    setSelected(0);
    setOpen(true);
    setText(next === "paths" ? "Please check ./web/s" : "Ask @");
    setChips([]);
    setNotice("Fixture only — no messages or permissions are changed.");
  }
  return (
    <div style={{ width: "100%", maxWidth: "38rem" }}>
      <p className="g-intro">
        <button className="quiet" onClick={() => reset("paths")}>
          Path example
        </button>
        {" · "}
        <button className="quiet" onClick={() => reset("agents")}>
          Agent example
        </button>
      </p>
      <div className="g-completion-scene">
        <div className="g-completion-transcript" aria-hidden="true">
          <p>
            I found the entry point. The client starts by loading the workspace.
          </p>
          <p>The next step is to check how the sources are organized.</p>
        </div>
        <div className="dock">
          {open && (
            <CompletionTray>
              <CompletionMenu
                id={id}
                options={options}
                selected={selected}
                onHighlight={setSelected}
                onAccept={accept}
                state={options.length ? "ready" : "empty"}
              />
              {mode === "agents" && (
                <div className="composer-completion-disclosure">
                  Reference a session · no notification is sent.
                </div>
              )}
            </CompletionTray>
          )}
          <div className="composer">
            <div className="col composer-inner">
              <div className="composer-input">
                <div className="composer-text-row">
                  <textarea
                    aria-label="Completion sketch"
                    role="combobox"
                    aria-autocomplete="list"
                    aria-expanded={open}
                    aria-controls={open ? id : undefined}
                    aria-activedescendant={
                      open && options.length ? `${id}-${selected}` : undefined
                    }
                    rows={2}
                    value={text}
                    onChange={(event) => {
                      setText(event.target.value);
                      setOpen(true);
                      setSelected(0);
                    }}
                    onKeyDown={(event) => {
                      if (!open || event.nativeEvent.isComposing) return;
                      if (
                        event.key === "ArrowDown" ||
                        event.key === "ArrowUp"
                      ) {
                        event.preventDefault();
                        if (options.length)
                          setSelected(
                            (current) =>
                              (current +
                                (event.key === "ArrowDown" ? 1 : -1) +
                                options.length) %
                              options.length,
                          );
                      } else if (
                        event.key === "Enter" ||
                        (event.key === "Tab" && !event.shiftKey)
                      ) {
                        event.preventDefault();
                        if (!event.repeat) accept(selected);
                      } else if (event.key === "Escape") {
                        event.preventDefault();
                        setOpen(false);
                        setNotice("Dismissed without accepting a reference.");
                      }
                    }}
                  />
                </div>
                <div
                  style={{
                    display: "flex",
                    flexWrap: "wrap",
                    gap: "0.4rem",
                    padding: chips.length ? "0.5rem" : 0,
                  }}
                >
                  {chips.map((chip) => (
                    <ReferenceChip
                      key={chip.id}
                      {...chip}
                      onWriteChange={(write) =>
                        setChips((current) =>
                          current.map((item) =>
                            item.id === chip.id ? { ...item, write } : item,
                          ),
                        )
                      }
                      onRemove={() =>
                        setChips((current) =>
                          current.filter((item) => item.id !== chip.id),
                        )
                      }
                    />
                  ))}
                </div>
              </div>
            </div>
          </div>
        </div>
      </div>
      <p className="g-intro" role="status">
        {notice}
      </p>
      <p className="g-intro">
        This fixture uses fixed candidates and does not query discovery. Chip
        placement below the textarea is a sketch, not the final inline editor.
      </p>
    </div>
  );
}

export function ComposerReferenceSpecimens() {
  return (
    <>
      <Specimen name="Inline editor — Tiptap spike">
        <InlineComposerSketch />
      </Specimen>
      <Specimen name="Completion — folder browsing prototype">
        <CompletionSketch />
      </Specimen>
      <Specimen name="References — read, write, agent, attachment">
        <div style={{ display: "flex", flexWrap: "wrap", gap: "0.5rem" }}>
          <ReferenceChip
            label="main.tsx"
            detail="/project/web/src/main.tsx"
            kind="file"
          />
          <ReferenceChip
            label="src/"
            detail="/project/web/src/"
            kind="directory"
            write
          />
          <ReferenceChip
            label="@new-lilac"
            detail="Exact session: new-lilac"
            kind="agent"
          />
          <ReferenceChip
            label="screen.png"
            detail="Stored attachment · screen.png"
            kind="attachment"
          />
        </div>
      </Specimen>
      <Specimen name="Write control — pinned interaction states">
        <States
          on=".reference-chip-access"
          render={() => (
            <ReferenceChip
              label="src/"
              detail="/project/web/src/"
              kind="directory"
            />
          )}
        />
      </Specimen>
      <Specimen name="Remove reference — pinned interaction states">
        <States
          on=".reference-chip-remove"
          render={() => (
            <ReferenceChip
              label="main.tsx"
              detail="/project/web/src/main.tsx"
              kind="file"
            />
          )}
        />
      </Specimen>
      <Specimen name="Suggestion row — pinned interaction states">
        <States on=".reference-completion-row" render={() => <RowState />} />
      </Specimen>
      {(["loading", "empty", "error"] as const).map((state) => (
        <Specimen key={state} name={`Discovery — ${state}`}>
          <div style={{ width: "100%", maxWidth: "38rem" }}>
            <MenuState state={state} />
          </div>
        </Specimen>
      ))}
      <UploadSpecimens />
    </>
  );
}

function MenuState({ state }: { state: "loading" | "empty" | "error" }) {
  const id = useId();
  return (
    <CompletionMenu
      id={id}
      state={state}
      options={[]}
      selected={-1}
      onHighlight={() => {}}
      onAccept={() => {}}
    />
  );
}

function RowState() {
  const id = useId();
  return (
    <div style={{ width: "18rem" }}>
      <CompletionMenu
        id={id}
        options={[paths[0]]}
        selected={-1}
        onHighlight={() => {}}
        onAccept={() => {}}
      />
    </div>
  );
}
