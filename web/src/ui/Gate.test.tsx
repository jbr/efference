// @vitest-environment jsdom
import { act } from "react";
import { createRoot } from "react-dom/client";
import { expect, test, vi } from "vitest";
import { GateControls } from "./Gate.tsx";

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
