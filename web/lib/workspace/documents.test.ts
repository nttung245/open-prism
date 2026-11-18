import { EditorState } from "@codemirror/state";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { DocumentStore, type Transport, type WriteResult } from "./documents";

/** An in-memory server with content-hash-like versions. */
class FakeServer implements Transport {
  files = new Map<string, { text: string; version: string }>();
  writes: Array<{ path: string; text: string; base: string | null }> = [];
  private counter = 0;
  failNextWrite = false;

  put(path: string, text: string) {
    this.counter += 1;
    this.files.set(path, { text, version: `v${this.counter}` });
  }

  async read(path: string) {
    const file = this.files.get(path);
    return file ? { ...file } : null;
  }

  async write(
    path: string,
    text: string,
    base: string | null,
  ): Promise<WriteResult> {
    this.writes.push({ path, text, base });
    if (this.failNextWrite) {
      this.failNextWrite = false;
      throw new Error("network down");
    }
    const current = this.files.get(path);
    if (base !== null && current && current.version !== base) {
      return { ok: false, current: current.version };
    }
    this.put(path, text);
    return { ok: true, version: this.files.get(path)?.version ?? "" };
  }
}

function setup() {
  const server = new FakeServer();
  server.put("main.tex", "hello");
  const saved: string[] = [];
  const store = new DocumentStore({
    transport: server,
    createState: (text) => EditorState.create({ doc: text }),
    autosaveMs: 100,
    retryMs: 200,
    onSaved: (path) => saved.push(path),
  });
  const edit = (path: string, insert: string) => {
    const state = store.getState(path);
    if (!state) throw new Error(`${path} is not loaded`);
    store.update(
      path,
      state.update({ changes: { from: state.doc.length, insert } }).state,
    );
  };
  const status = (path: string) =>
    store.getSnapshot().docs.find((doc) => doc.path === path);
  return { server, store, edit, status, saved };
}

describe("DocumentStore", () => {
  beforeEach(() => {
    vi.useFakeTimers();
  });
  afterEach(() => {
    vi.useRealTimers();
  });

  it("opens, edits and autosaves with the loaded version", async () => {
    const { server, store, edit, status, saved } = setup();
    await store.open("main.tex");
    expect(store.getSnapshot().active).toBe("main.tex");

    edit("main.tex", " world");
    expect(status("main.tex")?.dirty).toBe(true);
    await vi.advanceTimersByTimeAsync(150);

    expect(server.writes).toEqual([
      { path: "main.tex", text: "hello world", base: "v1" },
    ]);
    expect(status("main.tex")?.dirty).toBe(false);
    expect(saved).toEqual(["main.tex"]);
    expect(server.files.get("main.tex")?.text).toBe("hello world");
  });

  it("debounces bursts of edits into one save", async () => {
    const { server, store, edit } = setup();
    await store.open("main.tex");
    for (const char of "abc") {
      edit("main.tex", char);
      await vi.advanceTimersByTimeAsync(50);
    }
    await vi.advanceTimersByTimeAsync(150);
    expect(server.writes.map((w) => w.text)).toEqual(["helloabc"]);
  });

  it("flush saves immediately", async () => {
    const { server, store, edit } = setup();
    await store.open("main.tex");
    edit("main.tex", "!");
    await store.flush();
    expect(server.writes).toHaveLength(1);
    expect(store.hasUnsavedChanges()).toBe(false);
  });

  it("reloads clean documents changed on disk", async () => {
    const { server, store, status } = setup();
    await store.open("main.tex");
    server.put("main.tex", "changed elsewhere");

    await store.refresh(["main.tex"]);

    expect(store.getState("main.tex")?.doc.toString()).toBe(
      "changed elsewhere",
    );
    expect(status("main.tex")?.dirty).toBe(false);
    expect(status("main.tex")?.conflict).toBeNull();
    await vi.advanceTimersByTimeAsync(500);
    expect(server.writes).toHaveLength(0);
  });

  it("ignores change events caused by its own saves", async () => {
    const { server, store, edit } = setup();
    await store.open("main.tex");
    edit("main.tex", "!");
    await store.flush();
    await store.refresh(["main.tex"]);
    expect(store.getState("main.tex")?.doc.toString()).toBe("hello!");
    expect(server.writes).toHaveLength(1);
  });

  it("reports a conflict instead of overwriting unsaved edits", async () => {
    const { server, store, edit, status } = setup();
    await store.open("main.tex");
    edit("main.tex", " mine");
    server.put("main.tex", "theirs");

    await store.refresh(["main.tex"]);

    expect(status("main.tex")?.conflict).toEqual({
      kind: "external",
      version: "v2",
    });
    await vi.advanceTimersByTimeAsync(500);
    expect(server.writes).toHaveLength(0);

    await store.resolve("main.tex", "mine");
    expect(server.files.get("main.tex")?.text).toBe("hello mine");
    expect(status("main.tex")?.conflict).toBeNull();
  });

  it("turns a rejected save into a conflict and can take the server copy", async () => {
    const { server, store, edit, status } = setup();
    await store.open("main.tex");
    server.put("main.tex", "theirs");
    edit("main.tex", " mine");
    await store.flush();

    expect(status("main.tex")?.conflict?.kind).toBe("rejected");
    await store.resolve("main.tex", "theirs");
    expect(store.getState("main.tex")?.doc.toString()).toBe("theirs");
    expect(status("main.tex")?.dirty).toBe(false);
  });

  it("retries after a network error", async () => {
    const { server, store, edit, status } = setup();
    await store.open("main.tex");
    server.failNextWrite = true;
    edit("main.tex", "!");
    await vi.advanceTimersByTimeAsync(150);
    expect(status("main.tex")?.error).toContain("network down");
    await vi.advanceTimersByTimeAsync(250);
    expect(server.files.get("main.tex")?.text).toBe("hello!");
    expect(status("main.tex")?.error).toBeNull();
  });

  it("follows renames and deletions", async () => {
    const { server, store } = setup();
    server.put("ch/a.tex", "a");
    await store.open("ch/a.tex");
    await store.open("main.tex");
    store.renamed("ch", "chapters");
    expect(store.getSnapshot().docs.map((d) => d.path)).toEqual([
      "chapters/a.tex",
      "main.tex",
    ]);
    store.removed("main.tex");
    expect(store.getSnapshot().active).toBe("chapters/a.tex");
  });

  it("closes documents deleted on disk when they have no edits", async () => {
    const { server, store } = setup();
    await store.open("main.tex");
    server.files.delete("main.tex");
    await store.refresh([""]);
    expect(store.getSnapshot().docs).toHaveLength(0);
  });
});
