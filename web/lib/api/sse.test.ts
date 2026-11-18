import { describe, expect, it } from "vitest";
import { SseParser } from "./sse";

describe("SseParser", () => {
  it("splits events across arbitrary chunk boundaries", () => {
    const parser = new SseParser();
    expect(parser.push('data: {"a":')).toEqual([]);
    expect(parser.push('1}\n\ndata: {"b":2}\n')).toEqual(['{"a":1}']);
    expect(parser.push("\n: keep-alive\n\n")).toEqual(['{"b":2}']);
  });

  it("joins multi-line data and handles CRLF", () => {
    const parser = new SseParser();
    expect(parser.push("event: x\r\ndata: one\r\ndata: two\r\n\r\n")).toEqual([
      "one\ntwo",
    ]);
  });
});
