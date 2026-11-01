// Values the host sends, written as the types generated from the protocol schema.
// Each is typed, so `pnpm run typecheck` fails when a type stops accepting one.
//
// A directory listing from the `/files` surface. The bytes are the ones
// src/listeners/files/test/listing.rs pins; a pin that changes in Rust is changed here too.

import type { DirListing } from "../types.ts";

export const files: {
  dirListings: DirListing[];
} = {
  dirListings: [
    {
      path: "sub",
      recursive: false,
      truncated: false,
      entries: [
        {
          name: "b.rs",
          kind: "file",
          size: 2,
          modified_at: 1754300060000,
          symlink: false,
        },
      ],
    },
    {
      path: "",
      recursive: false,
      truncated: false,
      entries: [
        {
          name: "a.txt",
          kind: "file",
          size: 1,
          modified_at: 1754300000000,
          symlink: false,
        },
        {
          name: "dangling",
          kind: "other",
          size: 0,
          modified_at: null,
          symlink: true,
        },
        {
          name: "link",
          kind: "file",
          size: 1,
          modified_at: 1754300000000,
          symlink: true,
        },
        {
          name: "src",
          kind: "dir",
          size: 128,
          modified_at: 1754300000000,
          symlink: false,
        },
      ],
    },
  ],
};
