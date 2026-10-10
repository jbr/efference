import { expect, test } from "vitest";
import { askMarkText, askStanding, replyHint } from "./askMark.ts";

const question = { callId: "a", question: "?", n: 1, of: 2 };

test("standing follows the topic, and only while unresolved", () => {
  expect(askStanding(question, { call_id: "a" })).toBe("current");
  expect(askStanding(question, { call_id: "b" })).toBe("upcoming");
  expect(askStanding(question, null)).toBe("asked");
  expect(askStanding({ ...question, resolved: true }, { call_id: "a" })).toBe(
    "asked",
  );
});

test("marks name the position only when there are several", () => {
  expect(askMarkText(question, "current")).toBe("answering · question 1 of 2");
  expect(askMarkText({ ...question, of: 1 }, "current")).toBe("answering");
  expect(askMarkText({ ...question, n: 2 }, "upcoming")).toBe(
    "next · question 2 of 2",
  );
  expect(askMarkText(question, "asked")).toBe("asked · question 1 of 2");
});

test("the composer hint says which question a send answers", () => {
  expect(replyHint({ n: 2, of: 3 })).toBe("your reply answers question 2 of 3");
  expect(replyHint({ n: 1, of: 1 })).toBe("your reply answers the question");
});
