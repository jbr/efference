// @vitest-environment jsdom
import { act } from "react";
import { createRoot } from "react-dom/client";
import { expect, test, vi } from "vitest";
import type { Gate, Spot } from "../api/types.ts";
import { GateControls, GateReason, GateSurface } from "./Gate.tsx";

vi.hoisted(() => {
  Object.assign(globalThis, { IS_REACT_ACT_ENVIRONMENT: true });
  Object.defineProperty(window, "matchMedia", {
    value: () => ({
      matches: false,
      addEventListener() {},
      removeEventListener() {},
    }),
  });
});

function fixture() {
  const host = document.createElement("div");
  const root = createRoot(host);
  const approve = vi.fn();
  const abort = vi.fn();
  const redirect = vi.fn();
  const setSteer = vi.fn();
  const render = (steer = "", busy = false, coldLine: string | null = null) =>
    act(() =>
      root.render(
        <GateControls
          busy={busy}
          coldLine={coldLine}
          steer={steer}
          setSteer={setSteer}
          approve={approve}
          abort={abort}
          redirect={redirect}
        />,
      ),
    );
  const button = (text: string) =>
    [...host.querySelectorAll("button")].find((b) => b.textContent === text)!;
  render();
  return {
    host,
    render,
    button,
    approve,
    abort,
    redirect,
    close: () => act(() => root.unmount()),
  };
}

test("gate choices invoke supplied callbacks and decline reveals redirect controls", () => {
  const f = fixture();
  act(() => f.button("Allow").click());
  expect(f.approve).toHaveBeenCalledOnce();
  expect(f.host.querySelector("textarea")).toBeNull();
  act(() => f.button("Decline…").click());
  expect(f.button("Decline…").getAttribute("aria-expanded")).toBe("true");
  expect(f.host.querySelector("textarea")).not.toBeNull();
  f.render("Keep the branch local");
  act(() => f.button("Send").click());
  expect(f.redirect).toHaveBeenCalledOnce();
  act(() => f.button("Or stop this turn entirely").click());
  expect(f.abort).toHaveBeenCalledOnce();
  f.close();
});

test("blank redirect cannot be sent with the button or Enter", () => {
  const f = fixture();
  act(() => f.button("Decline…").click());
  f.render("   ");
  expect(f.button("Send").disabled).toBe(true);
  act(() =>
    f.host
      .querySelector("textarea")!
      .dispatchEvent(
        new KeyboardEvent("keydown", { key: "Enter", bubbles: true }),
      ),
  );
  expect(f.redirect).not.toHaveBeenCalled();
  f.close();
});

test("busy decisions disable buttons and preserve the cold readout", () => {
  const f = fixture();
  act(() => f.button("Decline…").click());
  f.render("Use another command", true, "Approving reads the context uncached");
  expect([...f.host.querySelectorAll("button")].every((b) => b.disabled)).toBe(
    true,
  );
  expect(f.host.querySelector('[role="status"]')?.textContent).toBe(
    "Approving reads the context uncached",
  );
  f.close();
});

const spot: Spot = {
  call_id: "held",
  stance: "gate",
  reason: "publishing changes the remote",
  origin: "Model",
};

function reasonFixture(
  gate: Pick<Gate, "escalations" | "reason">,
  verdict: Spot | null = spot,
) {
  const host = document.createElement("div");
  const root = createRoot(host);
  act(() => root.render(<GateReason gate={gate} spot={verdict} />));
  const text = host.textContent;
  act(() => root.unmount());
  return text;
}

test("ordinary gates keep the unlabeled spotter explanation", () => {
  expect(reasonFixture({})).toBe("Publishing changes the remote");
  expect(reasonFixture({ escalations: [] })).toBe(
    "Publishing changes the remote",
  );
  expect(reasonFixture({ reason: "Fallback reason" }, null)).toBe(
    "Fallback reason",
  );
  expect(reasonFixture({}, null)).toBe("");
});

test.each([
  ["session[calm-finch]", "calm-finch"],
  ["mcp[claude-code]", "claude-code"],
  [null, "you"],
])(
  "escalation by %s replaces spotter prose with the latest note",
  (source, name) => {
    expect(
      reasonFixture({
        escalations: [
          {
            source: "session[earlier]",
            to: "calm-finch",
            note: "Earlier note",
          },
          {
            source,
            to: null,
            note: "Please confirm.\nThis changes the shared remote.",
          },
        ],
      }),
    ).toBe(
      `escalated by ${name}Please confirm.\nThis changes the shared remote.`,
    );
  },
);

test.each([undefined, "", "   "])(
  "escalation without a note (%s) attributes the spotter fallback",
  (note) => {
    expect(
      reasonFixture({
        escalations: [{ source: "session[calm-finch]", to: null, note }],
      }),
    ).toBe("escalated by calm-finchspotter: publishing changes the remote");
  },
);

test("noteless escalation can use the gate reason or stand alone", () => {
  const escalations = [{ source: null, to: null }];
  expect(reasonFixture({ escalations, reason: "Fallback reason" }, null)).toBe(
    "escalated by youspotter: Fallback reason",
  );
  expect(reasonFixture({ escalations }, null)).toBe("escalated by you");
});

test("a held call updates its explanation in both its slot and floating panel", () => {
  vi.stubGlobal(
    "IntersectionObserver",
    class {
      observe() {}
      disconnect() {}
    },
  );
  vi.stubGlobal(
    "ResizeObserver",
    class {
      observe() {}
      unobserve() {}
      disconnect() {}
    },
  );
  const host = document.createElement("div");
  const slot = document.createElement("div");
  const root = createRoot(host);
  const gate: Gate = {
    call_id: "held",
    name: "bash",
    input: { command: "git push" },
  };
  const render = () =>
    act(() =>
      root.render(
        <GateSurface
          gate={gate}
          spot={spot}
          covered={true}
          slot={slot}
          cold={null}
          resolve={async () => "applied"}
        />,
      ),
    );
  try {
    render();
    expect(slot.textContent).toContain("Publishing changes the remote");
    gate.escalations = [
      { source: "session[calm-finch]", to: null, note: "Confirm publication." },
    ];
    render();
    for (const surface of [slot, host.querySelector(".gate-float")!]) {
      expect(surface.textContent).toContain("escalated by calm-finch");
      expect(surface.textContent).toContain("Confirm publication.");
      expect(surface.textContent).not.toContain(
        "Publishing changes the remote",
      );
    }
    gate.escalations = [{ source: "session[calm-finch]", to: null }];
    render();
    for (const surface of [slot, host.querySelector(".gate-float")!]) {
      expect(surface.textContent).toContain(
        "spotter: publishing changes the remote",
      );
      expect(surface.textContent).not.toContain("Confirm publication.");
    }
  } finally {
    act(() => root.unmount());
    vi.unstubAllGlobals();
  }
});
