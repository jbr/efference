// Values the host sends, written as the types generated from the protocol schema.
// Each is typed, so `pnpm run typecheck` fails when a type stops accepting one.
//
// The host's session collection and its remembered locations. The bytes are the ones
// src/host/test/host/bodies.rs, src/host/test/host/projects.rs,
// src/host/test/host/worktree.rs and src/protocol/sessions/test.rs pin; a pin that changes
// in Rust is changed here too.

import type {
  CreatedSession,
  ForgottenProject,
  ListedSession,
  ProjectList,
  SessionArchived,
  SessionResumed,
  SessionSupervised,
} from "../types.ts";

export const sessions: {
  projectLists: ProjectList[];
  forgottenProjects: ForgottenProject[];
  listedSessions: ListedSession[][];
  createdSessions: CreatedSession[];
  sessionResumed: SessionResumed[];
  sessionSupervised: SessionSupervised[];
  sessionArchived: SessionArchived[];
} = {
  projectLists: [
    {
      projects: [
        {
          config: null,
          config_key: "builtin-default",
          cwd: "/work/two",
          label: "two",
          last_used_at: 300,
        },
        {
          config: "eff.toml",
          config_key: "/work/one/eff.toml",
          cwd: "/work/one",
          label: "one",
          last_used_at: 100,
        },
      ],
    },
    { projects: [] },
  ],
  forgottenProjects: [
    { forgotten: { config_key: "/work/one/eff.toml", cwd: "/work/one" } },
  ],
  listedSessions: [
    [
      { id: "early", cwd: "/work/one", status: "starting", processes: 0 },
      {
        id: "later",
        cwd: "/work/one",
        created_by: "early",
        status: "idle",
        processes: 0,
      },
    ],
    [
      {
        id: "wt-fox",
        title: "Fix the parser",
        cwd: "/work",
        working: "/work/.efference/worktrees/wt-fox",
        status: "running",
        processes: 2,
      },
      { id: "a", status: "gated", processes: 0 },
      { id: "b", status: "asking", processes: 0 },
      { id: "c", status: "stalled", processes: 0 },
      { id: "d", status: "parked", processes: 0 },
      { id: "e", status: "blocked", processes: 0 },
    ],
  ],
  createdSessions: [{ id: "fresh" }],
  sessionResumed: [
    { id: "dormant", resumed: true, state: "active" },
    { id: "dormant", resumed: false, state: "active" },
  ],
  sessionSupervised: [
    { id: "a", state: "active", supervisor: "b" },
    { id: "a", state: "active", supervisor: null },
  ],
  sessionArchived: [
    { id: "dormant", state: "archived" },
    { id: "fresh", state: "discarded" },
    { ended_with: ["child"], id: "parent", state: "archived" },
    { id: "other", resumed: false, state: "archived" },
    {
      id: "wt-fox",
      state: "archived",
      worktree: { branch_deleted: true, removed: true },
    },
    {
      id: "wt-fox",
      resumed: false,
      state: "archived",
      worktree: { kept: "uncommitted changes: ?? notes.txt" },
    },
    {
      id: "wt-fox",
      state: "archived",
      worktree: { failed: "could not run git: not found" },
    },
  ],
};
