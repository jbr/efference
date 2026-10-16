import type { ProjectEntry, SessionPreview } from "../api/sessionForm.ts";
import type { Row } from "../ui/pickerRows.ts";
import type { CardModel } from "../ui/overview.ts";

export const projects: ProjectEntry[] = [
  "efference",
  "semantic-edit-mcp",
  "trillium",
].map((name, index) => ({
  label: name,
  cwd: `/Users/jbr/code/${name}`,
  config: null,
  config_key: "",
  last_used_at: 1_760_000_000 - index * 3600,
}));

export const preview: SessionPreview = {
  effective: {
    main: {
      value: {
        connection: "anthropic",
        provider: "anthropic",
        model: "claude-sonnet-5-5",
      },
      source: "user_default",
    },
    spotter: {
      value: {
        connection: "typesafe",
        provider: "openrouter",
        model: "jev-latest",
      },
      source: "user_default",
    },
    triage: { value: { mode: "shared" }, source: "shared_main" },
    effort: { value: null, source: "provider_default" },
    temperature: { value: null, source: "provider_default" },
  },
  issues: [],
};

export const pickerRows: Row[] = [
  {
    kind: "model",
    node: {
      kind: "model",
      connection: "anthropic",
      model: "claude-sonnet-5-5",
      name: "Claude Sonnet 5.5",
      recent: 1,
    },
    model: { model: "claude-sonnet-5-5", name: "Claude Sonnet 5.5" },
    recent: true,
  },
  {
    kind: "model",
    node: { kind: "model", connection: "meta", model: "muse-spark-1.3" },
    model: { model: "muse-spark-1.3" },
    recent: true,
  },
  {
    kind: "connection",
    node: { kind: "connection", connection: "anthropic" },
    entry: {
      connection: "anthropic",
      provider: "anthropic",
      status: "fresh",
      models: Array.from({ length: 14 }, (_, i) => ({ model: `m${i}` })),
    },
  },
  {
    kind: "connection",
    node: { kind: "connection", connection: "codex" },
    entry: {
      connection: "codex",
      provider: "openai",
      status: "fresh",
      models: Array.from({ length: 7 }, (_, i) => ({ model: `m${i}` })),
    },
  },
];

export const card = (over: Partial<CardModel>): CardModel => ({
  id: "quick-sparrow",
  title: "Implementing worktree UI in the TUI",
  cwd: "/Users/jbr/code/efference",
  status: "idle",
  attention: "your_turn",
  processes: 0,
  supervisor: null,
  supervises: [],
  activity: {
    kind: "reply",
    detail: "Landed. It wasn't a fast-forward as it stood, so I rebased first:",
  },
  park: null,
  blocked: null,
  context: { used: 233_000, limit: 1_000_000 },
  cost: null,
  models: { main: "deepseek-flash", spotter: "jev-latest", triage: null },
  ...over,
});

export const tabCards: CardModel[] = [
  card({
    id: "still-kestrel",
    title: "Smoke-testing worktrees",
    status: "gated",
    attention: "blocked",
    activity: { kind: "gate", tool: "bash", detail: "git push origin main" },
  }),
  card({ id: "quick-sparrow", title: "Worktree UI in the TUI" }),
  card({
    id: "fold-probe",
    title: "Probing context folding",
    status: "stalled",
    attention: "blocked",
    activity: { kind: "stall", detail: "429 from the provider" },
  }),
  card({
    id: "cool-harbor",
    title: "Cold parks",
    status: "parked",
    attention: "blocked",
    park: "expired",
  }),
  card({
    id: "deft-walnut",
    title: "Web worktree controls",
    status: "running",
    attention: "working",
  }),
  card({ id: "keen-lynx", title: null, status: "idle", attention: "new" }),
];

export const archived = [
  ["Implementing worktree UI in the TUI", "quick-sparrow", "48 minutes ago"],
  [null, "lucid-plover", "yesterday"],
  ["Probing context folding behavior", "fold-probe", "2 days ago"],
] as const;
