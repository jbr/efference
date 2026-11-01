// Values the host sends, written as the types generated from the protocol schema.
// Each is typed, so `pnpm run typecheck` fails when a type stops accepting one.
//
// The host's inference routes: the preview, the model catalog, the roles it is read for, and
// the connection inventory. The bytes are the ones src/host/test/host/preview.rs and
// src/host/test/host/bodies.rs pin; a pin that changes in Rust is changed here too.

import type {
  CatalogList,
  ConnectionCatalog,
  InventoryList,
  ModelRole,
  ResolvedOptions,
} from "../types.ts";

export const inference: {
  resolvedOptions: ResolvedOptions[];
  catalogLists: CatalogList[];
  connectionCatalogs: ConnectionCatalog[];
  modelRoles: ModelRole[];
  inventoryLists: InventoryList[];
} = {
  resolvedOptions: [
    {
      effective: {
        main: {
          source: "request",
          value: { connection: "test", model: "m", provider: "anthropic" },
        },
        spotter: {
          source: "main_provider",
          value: {
            connection: "test",
            model: "claude-haiku-4-5",
            provider: "anthropic",
          },
        },
        triage: { source: "shared_main", value: { mode: "shared" } },
        effort: { source: "provider_default", value: null },
        temperature: { source: "provider_default", value: null },
      },
      issues: [],
    },
    {
      effective: {
        main: {
          source: "request",
          value: { connection: "test", model: "m", provider: "anthropic" },
        },
        spotter: {
          source: "main_provider",
          value: {
            connection: "test",
            model: "claude-haiku-4-5",
            provider: "anthropic",
          },
        },
        triage: {
          source: "request",
          value: {
            auto_choice: false,
            connection: "test",
            effort: "low",
            mode: "separate",
            model: "t",
            provider: "anthropic",
            thinking: true,
          },
        },
        effort: { source: "request", value: "high" },
        temperature: { source: null, value: null },
      },
      issues: [
        {
          code: "invalid_type",
          field: "temperature",
          message: "must be a whole number from 0 to 100",
        },
      ],
    },
  ],
  catalogLists: [
    {
      connections: [
        {
          age: 0,
          connection: "test",
          models: [
            {
              connection: "test",
              context: 200000,
              created: 1754000000,
              description: "The larger one.",
              group: "anthropic",
              images: false,
              lifecycle: "active",
              line: "sonnet",
              model: "claude-x",
              name: "Claude X",
              price: { input: 3.0, output: 15.0 },
              reasoning: true,
              recent: 1,
            },
            { connection: "test", kind: "decisions", model: "claude-d" },
            { connection: "test", model: "bare" },
            { connection: "test", model: "typed-id", recent: 2 },
          ],
          provider: "anthropic",
          status: "fresh",
        },
      ],
    },
    {
      connections: [
        {
          connection: "off",
          error: "connection `off` is disabled",
          models: [],
          provider: "openrouter",
          status: "unavailable",
        },
      ],
    },
  ],
  connectionCatalogs: [
    {
      age: 0,
      connection: "test",
      models: [
        {
          connection: "test",
          model: "claude-x",
          group: "anthropic",
          recent: 1,
        },
      ],
      provider: "anthropic",
      status: "fresh",
    },
    {
      connection: "off",
      error: "connection `off` is disabled",
      models: [],
      provider: "openrouter",
      status: "unavailable",
    },
  ],
  modelRoles: ["main", "triage", "spotter"],
  inventoryLists: [
    {
      connections: [{ available: true, name: "test", provider: "anthropic" }],
    },
  ],
};
