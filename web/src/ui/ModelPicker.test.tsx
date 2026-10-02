// @vitest-environment jsdom
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, expect, test, vi } from "vitest";
import { ModelPicker, type PickerBrowseState } from "./ModelPicker.tsx";
import { fetchConnection, type CatalogEntry } from "../api/sessionForm.ts";

vi.hoisted(() => {
  Object.assign(globalThis, { IS_REACT_ACT_ENVIRONMENT: true });
});

const catalog: CatalogEntry[] = [
  {
    connection: "openrouter",
    provider: "openrouter",
    status: "fresh",
    models: [
      { model: "acme/alpha-new", group: "acme", recent: 1 },
      { model: "acme/alpha-old", group: "acme" },
    ],
  },
];

vi.mock("../api/sessionForm.ts", () => ({
  fetchCatalog: async () => ({ connections: catalog }),
  fetchConnection: vi.fn(async () => catalog[0]),
}));

let root: Root | undefined;
let host: HTMLDivElement;
afterEach(() => {
  act(() => root?.unmount());
  host?.remove();
  vi.clearAllMocks();
});

async function render(initialBrowse?: PickerBrowseState) {
  host = document.createElement("div");
  document.body.append(host);
  root = createRoot(host);
  const onPick = vi.fn();
  const onClose = vi.fn();
  const onBrowse = vi.fn<(state: PickerBrowseState) => void>();
  await act(async () => {
    root!.render(
      <ModelPicker
        role="main"
        cwd="/project"
        config={null}
        initialBrowse={initialBrowse}
        onBrowse={onBrowse}
        onPick={onPick}
        onClose={onClose}
      />,
    );
  });
  return { onPick, onClose, onBrowse };
}

function button(id: string): HTMLButtonElement {
  const row = [...host.querySelectorAll<HTMLButtonElement>(".picker-row")].find(
    (row) =>
      row.querySelector(".picker-id")?.textContent === id ||
      row.parentElement?.querySelector(".picker-parameters dd")?.textContent ===
        id,
  );
  if (!row) throw new Error(`Missing picker row ${id}`);
  return row;
}

test("root separates selectable recents from counted connection folders", async () => {
  await render();
  expect(
    [...host.querySelectorAll(".picker-section")].map(
      (node) => node.textContent,
    ),
  ).toEqual(["Recent models", "Browse connections"]);
  expect(
    button("openrouter/acme/alpha-new").classList.contains("picker-model"),
  ).toBe(true);
  expect(button("openrouter").classList.contains("picker-folder")).toBe(true);
  expect(button("openrouter").textContent).toContain("2 models");
  expect(button("openrouter").querySelector(".picker-next")).not.toBeNull();
  await act(async () => button("openrouter").click());
  expect(button("acme").textContent).toContain("2 models");
  expect(button("acme").querySelector(".picker-next")).not.toBeNull();
});

test("re-entry restores path, query, keyboard selection, and scroll", async () => {
  const remembered: PickerBrowseState = {
    chips: ["openrouter", "acme"],
    query: "alpha",
    selected: 1,
    scrollTop: 80,
  };
  const { onBrowse, onPick, onClose } = await render(remembered);
  expect(host.querySelector<HTMLInputElement>("input")?.value).toBe("alpha");
  expect(
    [...host.querySelectorAll(".picker-breadcrumb")].map(
      (node) => node.textContent,
    ),
  ).toEqual(["openrouter", "acme"]);
  expect(host.querySelector(".picker-row.selected")).toBeNull();
  await act(async () => {
    const input = host.querySelector("input")!;
    input.dispatchEvent(
      new KeyboardEvent("keydown", { key: "ArrowUp", bubbles: true }),
    );
  });
  await act(async () =>
    host
      .querySelector("input")!
      .dispatchEvent(
        new KeyboardEvent("keydown", { key: "ArrowDown", bubbles: true }),
      ),
  );
  expect(
    host.querySelector(".picker-row.selected .picker-id")?.textContent,
  ).toBe("alpha-old");
  expect(host.querySelector(".picker-rows")?.scrollTop).toBe(80);
  expect(onBrowse.mock.calls.at(-1)?.[0]).toEqual(remembered);
  await act(async () =>
    host
      .querySelector("input")!
      .dispatchEvent(
        new KeyboardEvent("keydown", { key: "Enter", bubbles: true }),
      ),
  );
  expect(onPick).toHaveBeenCalledWith({
    connection: "openrouter",
    model: "acme/alpha-old",
  });
  expect(onClose).toHaveBeenCalledOnce();
});

test("All models resets browsing without selecting a model", async () => {
  const { onPick, onBrowse } = await render({
    chips: ["openrouter", "acme"],
    query: "alpha",
    selected: 1,
    scrollTop: 80,
  });
  await act(async () =>
    host.querySelector<HTMLButtonElement>(".picker-root")!.click(),
  );
  expect(host.querySelector(".picker-breadcrumb")).toBeNull();
  expect(host.querySelector<HTMLInputElement>("input")?.value).toBe("");
  expect(onBrowse.mock.calls.at(-1)?.[0]).toEqual({
    chips: [],
    query: "",
    selected: 0,
    scrollTop: 0,
  });
  expect(onPick).not.toHaveBeenCalled();
});

test("descriptions are optional disclosures, not part of model selection", async () => {
  catalog[0].models[1].description =
    "A provider description. Use the stable version when preview changes are unwanted.";
  try {
    const { onPick, onClose } = await render({
      chips: ["openrouter", "acme"],
      query: "",
      selected: 0,
      scrollTop: 0,
    });
    expect(host.querySelectorAll(".picker-description")).toHaveLength(1);
    const description =
      button("alpha-old").parentElement!.querySelector<HTMLDivElement>(
        ".picker-details",
      )!;
    expect(description.hidden).toBe(true);
    expect(description.textContent).toContain(catalog[0].models[1].description);
    expect(
      button("alpha-new").parentElement?.querySelector(".picker-description"),
    ).toBeNull();
    const toggle = description.parentElement!.querySelector<HTMLButtonElement>(
      ".picker-details-toggle",
    )!;
    expect(toggle.getAttribute("aria-expanded")).toBe("false");
    expect(toggle.getAttribute("aria-controls")).toBe(description.id);
    const enter = new KeyboardEvent("keydown", {
      key: "Enter",
      bubbles: true,
      cancelable: true,
    });
    await act(async () => {
      toggle.dispatchEvent(enter);
      toggle.click();
    });
    expect(enter.defaultPrevented).toBe(false);
    expect(description.hidden).toBe(false);
    expect(toggle.getAttribute("aria-expanded")).toBe("true");
    expect(onPick).not.toHaveBeenCalled();
    expect(onClose).not.toHaveBeenCalled();
    await act(async () => toggle.click());
    expect(description.hidden).toBe(true);
    expect(toggle.getAttribute("aria-expanded")).toBe("false");
    await act(async () => button("alpha-old").click());
    expect(onPick).toHaveBeenCalledOnce();
    expect(onPick).toHaveBeenCalledWith({
      connection: "openrouter",
      model: "acme/alpha-old",
    });
    expect(onClose).toHaveBeenCalledOnce();
  } finally {
    delete catalog[0].models[1].description;
  }
});

test("an author matched at root opens its connection and author path", async () => {
  const { onPick } = await render({
    chips: [],
    query: "acme",
    selected: 0,
    scrollTop: 0,
  });
  await act(async () => button("openrouter/acme").click());
  expect(
    [...host.querySelectorAll(".picker-breadcrumb")].map(
      (node) => node.textContent,
    ),
  ).toEqual(["openrouter", "acme"]);
  expect(host.querySelector(".picker-folder")).toBeNull();
  expect(
    [...host.querySelectorAll(".picker-id")].map((node) => node.textContent),
  ).toEqual(["alpha-new", "alpha-old"]);
  expect(onPick).not.toHaveBeenCalled();
});

test("matching an author and identical typed id does not leave a stale folder", async () => {
  const errors = vi.spyOn(console, "error");
  try {
    await render({
      chips: ["openrouter"],
      query: "acme",
      selected: 0,
      scrollTop: 0,
    });
    expect(host.querySelectorAll(".picker-row")).toHaveLength(2);
    expect(fetchConnection).not.toHaveBeenCalled();
    await act(async () =>
      host.querySelector<HTMLButtonElement>(".picker-folder")!.click(),
    );
    expect(host.querySelector(".picker-folder")).toBeNull();
    expect(host.querySelectorAll(".picker-row")).toHaveLength(2);
    expect(
      [...host.querySelectorAll(".picker-breadcrumb")].map(
        (node) => node.textContent,
      ),
    ).toEqual(["openrouter", "acme"]);
    expect(
      errors.mock.calls.filter((call) => String(call[0]).includes("same key")),
    ).toEqual([]);
  } finally {
    errors.mockRestore();
  }
});

test("breadcrumbs navigate to an ancestor and clear its search", async () => {
  const { onPick } = await render({
    chips: ["openrouter", "acme"],
    query: "alpha",
    selected: 0,
    scrollTop: 0,
  });
  const breadcrumbs =
    host.querySelectorAll<HTMLButtonElement>(".picker-breadcrumb");
  expect(breadcrumbs[1].getAttribute("aria-current")).toBe("location");
  expect(
    host.querySelector(".picker-path")?.textContent?.trim().endsWith("/"),
  ).toBe(true);
  await act(async () => breadcrumbs[0].click());
  expect(
    [...host.querySelectorAll(".picker-breadcrumb")].map(
      (node) => node.textContent,
    ),
  ).toEqual(["openrouter"]);
  expect(host.querySelector<HTMLInputElement>("input")?.value).toBe("");
  expect(button("acme").classList.contains("picker-folder")).toBe(true);
  expect(onPick).not.toHaveBeenCalled();
});

test("touch entry does not focus search or highlight a keyboard choice", async () => {
  vi.stubGlobal(
    "matchMedia",
    vi.fn(() => ({ matches: false })),
  );
  try {
    await render();
    expect(document.activeElement).not.toBe(host.querySelector("input"));
    expect(host.querySelector(".picker-row.selected")).toBeNull();
    await act(async () =>
      host
        .querySelector("input")!
        .dispatchEvent(
          new KeyboardEvent("keydown", { key: "ArrowDown", bubbles: true }),
        ),
    );
    expect(host.querySelector(".picker-row.selected")).not.toBeNull();
    await act(async () =>
      host
        .querySelector(".picker")!
        .dispatchEvent(new Event("pointerdown", { bubbles: true })),
    );
    expect(host.querySelector(".picker-row.selected")).toBeNull();
  } finally {
    vi.unstubAllGlobals();
  }
});

test("labelled metadata lives in details even when no description is supplied", async () => {
  catalog[0].models[1].context = 262144;
  catalog[0].models[1].price = { input: 0, output: 0.5 };
  catalog[0].models[1].images = false;
  try {
    await render({
      chips: ["openrouter", "acme"],
      query: "",
      selected: 0,
      scrollTop: 0,
    });
    const row = button("alpha-old");
    expect(row.textContent).not.toContain("262");
    expect(row.textContent).not.toContain("$0.5");
    const details =
      row.parentElement!.querySelector<HTMLDivElement>(".picker-details")!;
    expect(details.hidden).toBe(true);
    expect(details.textContent).toContain("Context window262,144 tokens");
    expect(details.textContent).toContain("Input price / 1M tokens$0");
    expect(details.textContent).toContain("Output price / 1M tokens$0.5");
    expect(details.textContent).toContain("Image inputNot supported");
    expect(details.querySelector(".picker-description")).toBeNull();
  } finally {
    delete catalog[0].models[1].context;
    delete catalog[0].models[1].price;
    delete catalog[0].models[1].images;
  }
});

test("search communicates the current connection and author scope", async () => {
  await render();
  const input = () => host.querySelector<HTMLInputElement>("input")!;
  expect(input().getAttribute("aria-label")).toBe("Search models");
  expect(input().placeholder).toBe("Search models");
  await act(async () => button("openrouter").click());
  expect(input().getAttribute("aria-label")).toBe("Search in openrouter");
  expect(input().placeholder).toBe("Search in openrouter");
  await act(async () => button("acme").click());
  expect(input().getAttribute("aria-label")).toBe(
    "Search in openrouter / acme",
  );
  expect(input().placeholder).toBe("Search in openrouter / acme");
  await act(async () =>
    host.querySelector<HTMLButtonElement>(".picker-root")!.click(),
  );
  expect(input().getAttribute("aria-label")).toBe("Search models");
});

test("connection folders show model counts, not wire dialects", async () => {
  const provider = catalog[0].provider;
  catalog[0].provider = "custom-responses";
  try {
    await render();
    expect(button("openrouter").textContent).toContain("2 models");
    expect(button("openrouter").textContent).not.toContain("custom-responses");
    expect(button("openrouter").querySelector(".picker-name")).toBeNull();
  } finally {
    catalog[0].provider = provider;
  }
});

test("unnamed models still have their exact identifier in the information panel", async () => {
  await render({
    chips: ["openrouter", "acme"],
    query: "",
    selected: 0,
    scrollTop: 0,
  });
  expect(
    button("alpha-old").parentElement!.querySelector(".picker-details-toggle"),
  ).not.toBeNull();
  expect(
    button("alpha-old").parentElement!.querySelector(".picker-details")
      ?.textContent,
  ).toContain("Full identifieropenrouter/acme/alpha-old");
  const toggle = button("alpha-new").parentElement!.querySelector(
    ".picker-details-toggle",
  )!;
  expect(toggle.tagName).toBe("BUTTON");
  expect(toggle.textContent).toBe("");
  expect(toggle.querySelector("svg")?.getAttribute("aria-hidden")).toBe("true");
});

test("names are primary, root badges identify the connection, and slugs stay in information", async () => {
  catalog[0].models[0].name = "Alpha New";
  try {
    const { onPick } = await render();
    const rootRow = button("openrouter/acme/alpha-new");
    expect(rootRow.querySelector(".picker-name")?.textContent).toBe(
      "Alpha New",
    );
    expect(rootRow.querySelector(".picker-origin")?.textContent).toBe(
      "openrouter",
    );
    expect(rootRow.querySelector(".picker-id")).toBeNull();
    expect(rootRow.textContent).not.toContain("acme/");
    await act(async () => button("openrouter").click());
    await act(async () => button("acme").click());
    const scopedRow = button("openrouter/acme/alpha-new");
    expect(scopedRow.querySelector(".picker-origin")).toBeNull();
    expect(scopedRow.textContent).toBe("Alpha New");
    expect(
      scopedRow.parentElement!.querySelector(".picker-details")?.textContent,
    ).toContain("Full identifieropenrouter/acme/alpha-newAuthoracme");
    await act(async () => scopedRow.click());
    expect(onPick).toHaveBeenCalledWith({
      connection: "openrouter",
      model: "acme/alpha-new",
    });
  } finally {
    delete catalog[0].models[0].name;
  }
});

test("matching names across connections have distinct root badges", async () => {
  catalog[0].models[0].name = "Alpha New";
  catalog.push({
    connection: "direct",
    provider: "custom-responses",
    status: "fresh",
    models: [{ model: "alpha", name: "Alpha New", recent: 2 }],
  });
  try {
    await render();
    const rows = [button("openrouter/acme/alpha-new"), button("direct/alpha")];
    expect(
      rows.map((row) => row.querySelector(".picker-name")?.textContent),
    ).toEqual(["Alpha New", "Alpha New"]);
    expect(
      rows.map((row) => row.querySelector(".picker-origin")?.textContent),
    ).toEqual(["openrouter", "direct"]);
    expect(rows.every((row) => row.querySelector(".picker-id") === null)).toBe(
      true,
    );
  } finally {
    catalog.pop();
    delete catalog[0].models[0].name;
  }
});

test("grouped connection browsing separates recents from author folders without a leaf-model tail", async () => {
  await render();
  await act(async () => button("openrouter").click());
  expect(
    [...host.querySelectorAll(".picker-section")].map(
      (node) => node.textContent,
    ),
  ).toEqual(["Recent models", "Browse authors"]);
  expect(host.querySelectorAll(".picker-model")).toHaveLength(1);
  expect(host.querySelectorAll(".picker-folder")).toHaveLength(1);
  expect(button("acme").textContent).toContain("2 models");
  await act(async () => button("acme").click());
  expect(host.querySelectorAll(".picker-model")).toHaveLength(2);
  expect(host.querySelector(".picker-folder")).toBeNull();
  expect(host.querySelector(".picker-section")).toBeNull();
});

test("Backspace returns to the parent namespace and resets the keyboard index", async () => {
  const { onBrowse, onPick } = await render({
    chips: ["openrouter", "acme"],
    query: "",
    selected: 1,
    scrollTop: 0,
  });
  await act(async () =>
    host
      .querySelector("input")!
      .dispatchEvent(
        new KeyboardEvent("keydown", { key: "Backspace", bubbles: true }),
      ),
  );
  expect(onBrowse.mock.calls.at(-1)?.[0]).toMatchObject({
    chips: ["openrouter"],
    query: "",
    selected: 0,
  });
  expect(onPick).not.toHaveBeenCalled();
});
