/**
 * A fake model behind the OpenAI Chat Completions API, so the end-to-end tests drive the real
 * provider path (an "OpenAI-compatible" provider, streamed answers, tool calls) without a key.
 *
 * It follows one script, chosen from what the request holds:
 * 1. a question from the user → read `main.tex`;
 * 2. that file's content → read `sections/intro.tex`;
 * 3. that file's content → propose fixing the typo in it;
 * 4. the outcome of the proposal → a closing sentence.
 * Requests without tools (suggestions as you type, quick actions) get an empty answer.
 */

import { createServer, type IncomingMessage, type Server } from "node:http";

export const MODEL = "e2e-model";
export const TYPO = "This paper descibes how";
export const FIX = "This paper describes how";
export const CLOSING = "I fixed the typo in sections/intro.tex.";

interface ToolCall {
  id: string;
  type: "function";
  function: { name: string; arguments: string };
}

export interface ChatMessage {
  role: "system" | "user" | "assistant" | "tool";
  content?: unknown;
  tool_calls?: ToolCall[];
  tool_call_id?: string;
}

export interface ChatRequest {
  model: string;
  messages: ChatMessage[];
  tools?: unknown[];
  stream?: boolean;
}

export type Reply =
  | { kind: "text"; text: string }
  | { kind: "tool"; name: string; arguments: Record<string, unknown> };

/** What the fake model answers to `request`. */
export function reply(request: ChatRequest): Reply {
  if (!request.tools?.length) return { kind: "text", text: "" };
  const last = request.messages.at(-1);
  if (last?.role !== "tool") {
    return { kind: "tool", name: "read_file", arguments: { path: "main.tex" } };
  }
  const call = request.messages
    .flatMap((message) => message.tool_calls ?? [])
    .find((toolCall) => toolCall.id === last.tool_call_id);
  const name = call?.function.name;
  const path = call ? parseArguments(call.function.arguments).path : undefined;
  if (name === "read_file" && path === "main.tex") {
    return {
      kind: "tool",
      name: "read_file",
      arguments: { path: "sections/intro.tex" },
    };
  }
  if (name === "read_file") {
    return {
      kind: "tool",
      name: "propose_edit",
      arguments: {
        path: "sections/intro.tex",
        summary: "Fix a typo in the introduction.",
        edits: [{ old: TYPO, new: FIX }],
      },
    };
  }
  return { kind: "text", text: CLOSING };
}

function parseArguments(text: string): Record<string, unknown> {
  try {
    const value: unknown = JSON.parse(text);
    return value && typeof value === "object"
      ? (value as Record<string, unknown>)
      : {};
  } catch {
    return {};
  }
}

/** The text of a tool result sent back to the model, or "" when `message` is not one. */
export function toolResult(message: ChatMessage): string {
  if (message.role !== "tool") return "";
  if (typeof message.content === "string") return message.content;
  if (Array.isArray(message.content)) {
    return message.content
      .map((part) =>
        part && typeof part === "object" && "text" in part
          ? String(part.text)
          : "",
      )
      .join("");
  }
  return "";
}

/** `reply` as Chat Completions stream chunks, split the way real providers split them. */
export function chunks(reply: Reply, callId: string): unknown[] {
  const base = {
    id: `chatcmpl-${callId}`,
    object: "chat.completion.chunk",
    created: 1,
    model: MODEL,
  };
  const choice = (delta: unknown, finish: string | null = null) => ({
    ...base,
    choices: [{ index: 0, delta, finish_reason: finish }],
  });
  const out: unknown[] = [choice({ role: "assistant", content: "" })];
  if (reply.kind === "text") {
    for (const word of reply.text.match(/\S+\s*/g) ?? []) {
      out.push(choice({ content: word }));
    }
    out.push(choice({}, "stop"));
  } else {
    const args = JSON.stringify(reply.arguments);
    const middle = Math.floor(args.length / 2);
    out.push(
      choice({
        tool_calls: [
          {
            index: 0,
            id: callId,
            type: "function",
            function: { name: reply.name, arguments: "" },
          },
        ],
      }),
      choice({
        tool_calls: [
          { index: 0, function: { arguments: args.slice(0, middle) } },
        ],
      }),
      choice({
        tool_calls: [{ index: 0, function: { arguments: args.slice(middle) } }],
      }),
      choice({}, "tool_calls"),
    );
  }
  out.push({
    ...base,
    choices: [],
    usage: { prompt_tokens: 100, completion_tokens: 10, total_tokens: 110 },
  });
  return out;
}

export interface FakeModel {
  /** Base URL to configure in Settings, ending in `/v1`. */
  url: string;
  /** Chat requests received so far, oldest first. */
  requests: ChatRequest[];
  close(): Promise<void>;
}

async function body(request: IncomingMessage): Promise<string> {
  const parts: Buffer[] = [];
  for await (const part of request) parts.push(part as Buffer);
  return Buffer.concat(parts).toString("utf8");
}

/** Starts the fake model on 127.0.0.1:`port` (0: any free port). */
export async function startFakeModel(port: number): Promise<FakeModel> {
  const requests: ChatRequest[] = [];
  let calls = 0;
  const server: Server = createServer(async (request, response) => {
    if (request.method === "GET" && request.url === "/v1/models") {
      response.writeHead(200, { "content-type": "application/json" });
      response.end(
        JSON.stringify({
          object: "list",
          data: [{ id: MODEL, object: "model" }],
        }),
      );
      return;
    }
    if (request.method === "POST" && request.url === "/v1/chat/completions") {
      const chat = JSON.parse(await body(request)) as ChatRequest;
      requests.push(chat);
      calls += 1;
      response.writeHead(200, {
        "content-type": "text/event-stream",
        "cache-control": "no-cache",
      });
      for (const chunk of chunks(reply(chat), `call_${calls}`)) {
        response.write(`data: ${JSON.stringify(chunk)}\n\n`);
      }
      response.end("data: [DONE]\n\n");
      return;
    }
    response.writeHead(404, { "content-type": "application/json" });
    response.end(JSON.stringify({ error: { message: "not found" } }));
  });
  await new Promise<void>((resolve, reject) => {
    server.once("error", reject);
    server.listen(port, "127.0.0.1", resolve);
  });
  const address = server.address();
  const bound = address && typeof address === "object" ? address.port : port;
  return {
    url: `http://127.0.0.1:${bound}/v1`,
    requests,
    close: () =>
      new Promise<void>((resolve) => {
        server.closeAllConnections();
        server.close(() => resolve());
      }),
  };
}
