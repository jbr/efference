import { expect, test } from "vitest";
import { costLabel } from "./cost.ts";

test("a fully priced total is an exact figure", () => {
  expect(costLabel({ usd: 0.1, unpriced: 0 })).toBe("$0.10");
});

test("any unpriced request marks the total a floor", () => {
  expect(costLabel({ usd: 12.5, unpriced: 3 })).toBe("~$12.50");
});
