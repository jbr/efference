// Values the host sends, written as the types generated from the protocol schema.
// Each is typed, so `pnpm run typecheck` fails when a type stops accepting one.
//
// The sign-in and enrollment answers. The bytes are the ones
// src/listeners/tcp/routes/test.rs and src/protocol/sign_in/test.rs pin; a pin that changes in
// Rust is changed here too.

import type {
  DeviceList,
  Enrolled,
  PendingSignInList,
  RemovedDevice,
  SignInClaim,
  SignInLink,
  SignInRequest,
} from "../types.ts";

export const signIn: {
  signInRequests: SignInRequest[];
  signInClaims: SignInClaim[];
  signInLinks: SignInLink[];
  pendingSignIns: PendingSignInList[];
  deviceLists: DeviceList[];
  enrolled: Enrolled[];
  removedDevices: RemovedDevice[];
} = {
  signInRequests: [
    { claim: "x7Qn2mQ9wZ3kLr8vB1cYdA", code: "KQ7-M4X", expires_in: 90 },
  ],
  signInClaims: [
    { status: "pending" },
    { token: "5b1f0c0e9c6d4b0a8f3e2d1c0b9a8f7e.AAAA" },
  ],
  signInLinks: [{ url: "http://localhost:7777/#claim=x7Qn2mQ9wZ3kLr8vB1cYdA" }],
  pendingSignIns: [
    { pending: [] },
    { pending: [{ created_at: 1754300000, label: "Firefox" }] },
  ],
  deviceLists: [
    { devices: [] },
    {
      devices: [
        {
          created_at: 1754300000,
          id: "0123456789abcdef0123456789abcdef",
          joined: "add",
          kind: "script",
          label: "deploy",
          last_seen_at: null,
        },
        {
          created_at: 1754300100,
          id: "fedcba9876543210fedcba9876543210",
          joined: "code",
          kind: "browser",
          label: "Firefox on Linux",
          last_seen_at: 1754303700,
        },
      ],
    },
  ],
  enrolled: [
    {
      device: {
        created_at: 1754300000,
        id: "0123456789abcdef0123456789abcdef",
        joined: "add",
        kind: "script",
        label: "deploy",
        last_seen_at: null,
      },
      token: "5b1f0c0e9c6d4b0a8f3e2d1c0b9a8f7e.AAAA",
    },
  ],
  removedDevices: [{ removed: "0123456789abcdef0123456789abcdef" }],
};
