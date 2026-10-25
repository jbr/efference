import type { ArchivedSession, DormantSession } from "../api/http.ts";
import type { SessionIdentity } from "../api/types.ts";
import { SwitchModelPage } from "../ui/SwitchModelPage.tsx";
import { archivedTarget, keeping, liveTarget } from "../ui/switchModel.ts";
import { Specimen } from "./Specimens.tsx";

// The switch page (ui/SwitchModelPage.tsx) on fixture state. It opens with no
// role changed, so the primary button is disabled and no row offers a reset;
// choosing a model in the running app adds those. The model picker is left out
// here because it reads the catalog, which a gallery has no host for.

const identity: SessionIdentity = {
  id: "still-kestrel",
  title: "Smoke-testing worktrees",
  model: "deepseek-flash",
  cwd: "/Users/jbr/code/efference",
  inference: {
    main: {
      connection: "deepseek",
      provider: "custom-messages",
      model: "deepseek-flash",
    },
    spotter: {
      connection: "anthropic",
      provider: "anthropic",
      model: "jev-latest",
    },
    triage: {
      connection: "deepseek",
      provider: "custom-messages",
      model: "deepseek-flash",
      shared: true,
    },
  },
};

const cold = { since: 1_760_000_000_000, context_tokens: 76_200 };

// A session the overview lists as archived, and the row reading its log answers
// with: the roles it last ran on, in the flat shape a resume patch takes
// (`src/inference/switch.rs`).
const archived: ArchivedSession = {
  id: "still-kestrel",
  cwd: "/Users/jbr/code/efference",
  title: "Smoke-testing worktrees",
  last_active_at: 1_760_000_000_000,
  state: "archived",
};

const archivedRead: DormantSession = {
  ...archived,
  context_tokens: 76_200,
  selections: {
    connection: "deepseek",
    model: "deepseek-flash",
    spotter_connection: "anthropic",
    spotter_model: "jev-latest",
    triage: { mode: "shared" },
  },
};

export function SwitchModelSpecimens() {
  return (
    <>
      <Specimen name="switch model — as it opens, nothing changed">
        <div className="g-switch">
          <SwitchModelPage
            target={liveTarget(identity.id, identity, cold, 76_200)}
            onClose={() => {}}
            onSwitched={() => {}}
          />
        </div>
      </Specimen>
      <Specimen name="switch model — a model chosen">
        <div className="g-switch">
          <SwitchModelPage
            target={liveTarget(identity.id, identity, cold, 76_200)}
            initialDraft={{
              ...keeping,
              main: { connection: "openrouter", model: "gemini-3-pro" },
            }}
            onClose={() => {}}
            onSwitched={() => {}}
          />
        </div>
      </Specimen>
      <Specimen name="switch model — state not read yet">
        <div className="g-switch">
          <SwitchModelPage
            target={liveTarget(identity.id, null, null, null)}
            onClose={() => {}}
            onSwitched={() => {}}
          />
        </div>
      </Specimen>
      <Specimen name="switch model — a session that is already archived">
        <div className="g-switch">
          <SwitchModelPage
            target={archivedTarget(archived, archivedRead)}
            onClose={() => {}}
            onSwitched={() => {}}
          />
        </div>
      </Specimen>
    </>
  );
}
