// The body of `PUT /approvals/:call_id` (docs/control-api.md, docs/permissions.md):
// the decision a person or a supervising session returns for the held call. The
// host reads the body by hand, by its `decision` word (src/daemon/http/turns.rs),
// so the type is declared here rather than generated.

import type { ConfigChange } from "./types.ts";

export type Decision =
  | { decision: "approve" }
  | { decision: "abort" }
  | { decision: "redirect"; message: string }
  // Allow-with-prefix: approve and persist the change (docs/permissions.md).
  | {
      decision: "config_edit";
      change: ConfigChange;
      note?: string;
    };
