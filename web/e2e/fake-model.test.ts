import { afterAll, beforeAll, describe, expect, it } from "vitest";
import {
  type ChatMessage,
  CLOSING,
  type FakeModel,
  FIX,
  MODEL,
  startFakeModel,
  TYPO,
} from "./fake-model";

interface Answer {
  text: string;
  finish: string | null;
  call?: { id: string; name: string; args: Record<string, unknown> };
}

/** Sends a streamed chat request and puts the answer back together, as a client would. */
async function chat(
  model: FakeModel,
  messages: ChatMessage[],
  withTools = true,
): Promise<Answer> {
  const response = await fetch(`${model.url}/chat/completions`, {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify({
      model: MODEL,
      stream: true,
      messages,
      ...(withTools ? { tools: [{ type: "function" }] } : {}),
    }),
  });
  expect(response.headers.get("content-type")).toBe("text/event-stream");
  const lines = (await response.text()).split("\n\n").filter(Boolean);
  expect(lines.at(-1)).toBe("data: [DONE]");
  const answer: Answer = { text: "", finish: null };
  let id = "";
  let name = "";
  let args = "";
  for (const line of lines.slice(0, -1)) {
    const chunk = JSON.parse(line.replace(/^data: /, ""));
    for (const choice of chunk.choices) {
      answer.finish = choice.finish_reason ?? answer.finish;
      answer.text += choice.delta.content ?? "";
      for (const call of choice.delta.tool_calls ?? []) {
        id = call.id ?? id;
        name += call.function?.name ?? "";
        args += call.function?.arguments ?? "";
      }
    }
  }
  if (id) answer.call = { id, name, args: JSON.parse(args) };
  return answer;
}

/** The assistant message carrying `answer`'s tool call, then that call's result. */
function round(answer: Answer, result: string): ChatMessage[] {
  const call = answer.call;
  if (!call) throw new Error("expected a tool call");
  return [
    {
      role: "assistant",
      tool_calls: [
        {
          id: call.id,
          type: "function",
          function: { name: call.name, arguments: JSON.stringify(call.args) },
        },
      ],
    },
    { role: "tool", tool_call_id: call.id, content: result },
  ];
}

describe("fake model", () => {
  let model: FakeModel;
  beforeAll(async () => {
    model = await startFakeModel(0);
  });
  afterAll(() => model.close());

  it("lists its model", async () => {
    const response = await fetch(`${model.url}/models`);
    expect(await response.json()).toMatchObject({ data: [{ id: MODEL }] });
  });

  it("reads both files, proposes the fix, then closes", async () => {
    const messages: ChatMessage[] = [
      { role: "system", content: "You help." },
      { role: "user", content: "Fix the typos" },
    ];
    const first = await chat(model, messages);
    expect(first.finish).toBe("tool_calls");
    expect(first.call).toMatchObject({
      name: "read_file",
      args: { path: "main.tex" },
    });

    messages.push(...round(first, "\\documentclass{article}"));
    const second = await chat(model, messages);
    expect(second.call).toMatchObject({
      name: "read_file",
      args: { path: "sections/intro.tex" },
    });

    messages.push(...round(second, TYPO));
    const third = await chat(model, messages);
    expect(third.call).toMatchObject({
      name: "propose_edit",
      args: {
        path: "sections/intro.tex",
        edits: [{ old: TYPO, new: FIX }],
      },
    });
    // Each call gets its own id.
    expect(new Set([first, second, third].map((a) => a.call?.id)).size).toBe(3);

    messages.push(...round(third, "The user accepted the edit."));
    const last = await chat(model, messages);
    expect(last).toMatchObject({ text: CLOSING, finish: "stop" });
    expect(model.requests).toHaveLength(4);
  });

  it("answers requests without tools with nothing", async () => {
    const answer = await chat(
      model,
      [{ role: "user", content: "Continue: The quick" }],
      false,
    );
    expect(answer).toMatchObject({ text: "", finish: "stop" });
  });

  it("refuses other paths", async () => {
    expect((await fetch(`${model.url}/embeddings`)).status).toBe(404);
  });
});
