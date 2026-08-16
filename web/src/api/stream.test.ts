import { describe, expect, it } from "vitest";
import { sseData } from "./stream.ts";

function streamOf(...chunks: string[]): ReadableStream<Uint8Array> {
  const encoder = new TextEncoder();
  return new ReadableStream({
    start(controller) {
      for (const chunk of chunks) controller.enqueue(encoder.encode(chunk));
      controller.close();
    },
  });
}

async function collect(body: ReadableStream<Uint8Array>): Promise<string[]> {
  const out: string[] = [];
  for await (const data of sseData(body)) out.push(data);
  return out;
}

describe("sseData", () => {
  it("yields each event's data and ignores event:/id: lines", async () => {
    const events = await collect(
      streamOf(
        'event: user\nid: 1\ndata: {"class":"log","seq":1}\n\n',
        'event: gate\ndata: {"class":"state"}\n\n',
      ),
    );
    expect(events).toEqual(['{"class":"log","seq":1}', '{"class":"state"}']);
  });

  it("reassembles events split across reads", async () => {
    const events = await collect(
      streamOf('data: {"a"', ":1}\n", "\n", 'data: {"b":2}\n\n'),
    );
    expect(events).toEqual(['{"a":1}', '{"b":2}']);
  });

  it("discards heartbeat comments without dispatching", async () => {
    const events = await collect(
      streamOf(":\n\n", "data: x\n\n", ": keepalive\n\n"),
    );
    expect(events).toEqual(["x"]);
  });

  it("joins multi-line data fields with newlines", async () => {
    const events = await collect(streamOf("data: a\ndata: b\n\n"));
    expect(events).toEqual(["a\nb"]);
  });
});
