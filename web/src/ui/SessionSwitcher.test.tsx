import { expect, test } from "vitest";
import { renderToStaticMarkup } from "react-dom/server";
import { Tab } from "./SessionSwitcher.tsx";
import type { CardModel } from "./overview.ts";

test("tabs show session IDs rather than titles, including the selected tab", () => {
  const card = {
    id: "crisp-delta",
    title: "A long changing title",
  } as CardModel;
  for (const current of [false, true]) {
    const markup = renderToStaticMarkup(
      <Tab card={card} inert current={current} />,
    );
    expect(markup).toContain('class="switcher-label">crisp-delta</span>');
    expect(markup).not.toContain(card.title);
    expect(markup.includes("is-current")).toBe(current);
  }
});
