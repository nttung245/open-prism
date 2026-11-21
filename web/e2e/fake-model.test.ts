import { afterAll, beforeAll, describe, expect, it } from "vitest";
import {
  type ChatMessage,
  CLOSING,
  type FakeModel,
  FIGURE_BOX,
  FIX,
  IMPORT_PREAMBLE,
  MODEL,
  PAGE_PROMPT_START,
  PREAMBLE_PROMPT_START,
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

  describe("PDF import", () => {
    const image = {
      type: "image_url",
      image_url: { url: "data:image/png;base64,iVBORw0KGgo=" },
    };
    const ask = (system: string, text: string): ChatMessage[] => [
      { role: "system", content: `${system}. More rules.` },
      { role: "user", content: [{ type: "text", text }, image] },
    ];

    it("writes the preamble from the first page", async () => {
      const answer = await chat(
        model,
        ask(PREAMBLE_PROMPT_START, "This is page 1 of the PDF."),
        false,
      );
      expect(answer).toMatchObject({ text: IMPORT_PREAMBLE, finish: "stop" });
    });

    it("copies a page from its text layer, with a picture on page 1", async () => {
      const first = await chat(
        model,
        ask(
          PAGE_PROMPT_START,
          "The preamble:\n<preamble>\n...\n</preamble>\n\nPage 1 of the PDF.\n\n" +
            "The PDF's text layer for this page follows.\n<text-layer>\n" +
            "A \ufb01rst page: 50% of $5 & more_text\n</text-layer>",
        ),
        false,
      );
      expect(first.text).toContain(
        String.raw`A first page: 50\% of \$5 \& more\_text`,
      );
      expect(first.text).toContain(
        `\\includegraphics[width=0.4\\linewidth]{${FIGURE_BOX}}`,
      );

      const second = await chat(
        model,
        ask(PAGE_PROMPT_START, "Page 2 of the PDF."),
        false,
      );
      expect(second.text).toBe("Page 2 of the PDF.");
    });

    it("leaves requests without a page image alone", async () => {
      const answer = await chat(
        model,
        [
          { role: "system", content: PAGE_PROMPT_START },
          { role: "user", content: "Page 1 of the PDF." },
        ],
        false,
      );
      expect(answer.text).toBe("");
    });
  });

  it("refuses other paths", async () => {
    expect((await fetch(`${model.url}/embeddings`)).status).toBe(404);
  });
});
