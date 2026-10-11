import { emptyStreaming } from "../session/store.ts";
import { buildTree } from "../session/tree.ts";
import { TranscriptContent } from "../ui/Transcript.tsx";

const noop = () => {};
const specimens = [
  [
    "Short notification",
    "test-watch",
    "Frontend checks passed: 66 test files and 497 tests.",
  ],
  [
    "Process output · collapsed by default",
    "process",
    "p#2 — exited 0 after 1m11s; final output:\n… [123 earlier lines]\n✓ src/ui/coldLine.test.ts (5 tests)\n✓ src/ui/identity.test.ts (4 tests)\n✓ src/session/entry.test.ts (5 tests)\n\nTest Files 66 passed (66)\nTests 497 passed (497)\nDuration 29.33s",
  ],
  [
    "Terminal formatting · displayed as plain text",
    "test-watch",
    "\u001b[32m✓\u001b[39m All checks passed\n\u001b[1mTest Files\u001b[22m 66 passed (66)\n\u001b[2mDuration\u001b[22m 29.33s",
  ],
  [
    "Wrapped output · narrow viewport",
    "process",
    "p#12 — exited 0; final output:\nhttps://rolldown.rs/reference/OutputOptions.codeSplitting\npython3 -c \"print('a-command-with-an-unbroken-argument-that-must-not-widen-the-phone-transcript')\"",
  ],
];

export function AmbientSpecimens() {
  return specimens.map(([title, source, body], i) => (
    <div className="g-example" key={title}>
      <div className="g-label">{title}</div>
      <TranscriptContent
        nodes={buildTree([
          {
            seq: i + 1,
            turn: 1,
            at: 1760000000000,
            payload: { Ambient: { source, body, slot: null } },
          },
        ])}
        streaming={emptyStreaming}
        pending={[]}
        ask={null}
        gatedCall={null}
        onGateSlot={noop}
        onRecall={noop}
        onInspect={noop}
        onOpenFile={noop}
      />
    </div>
  ));
}
