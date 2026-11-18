"use client";

import { useQuery, useQueryClient } from "@tanstack/react-query";
import {
  Bot,
  Brain,
  ChevronDown,
  CircleAlert,
  Info,
  LoaderCircle,
  MessageSquarePlus,
  Search,
  Send,
  Settings,
  Square,
  Trash2,
  Wrench,
} from "lucide-react";
import Link from "next/link";
import {
  type KeyboardEvent,
  type Ref,
  useCallback,
  useEffect,
  useImperativeHandle,
  useMemo,
  useRef,
  useState,
} from "react";
import { Button, IconButton } from "@/components/ui/button";
import { parseReasoning, REASONING_OPTIONS } from "@/lib/ai/reasoning";
import {
  ApiError,
  apiFetch,
  apiJson,
  errorMessage,
  fileUrl,
} from "@/lib/api/client";
import { postEventStream } from "@/lib/api/sse";
import type { AiMessage } from "@/lib/api/types/AiMessage";
import type { ChatEvent } from "@/lib/api/types/ChatEvent";
import type { CompileResult } from "@/lib/api/types/CompileResult";
import type { EditorContext } from "@/lib/api/types/EditorContext";
import type { ModelRef } from "@/lib/api/types/ModelRef";
import type { Patch } from "@/lib/api/types/Patch";
import type { PatchDecision } from "@/lib/api/types/PatchDecision";
import type { ProviderView } from "@/lib/api/types/ProviderView";
import type { Roles } from "@/lib/api/types/Roles";
import type { ThreadSummary } from "@/lib/api/types/ThreadSummary";
import type { ThreadView } from "@/lib/api/types/ThreadView";
import type { DocumentStore } from "@/lib/workspace/documents";
import { Markdown } from "./markdown";
import { PatchReview } from "./patch-review";

/** What the panel shows for the turn in progress, before it is reloaded from the server. */
type LiveItem =
  | { kind: "user"; text: string }
  | { kind: "text"; text: string }
  | { kind: "reasoning"; text: string }
  | {
      kind: "tool";
      id: string;
      name: string;
      arguments: unknown;
      result?: { content: string; isError: boolean };
    }
  | { kind: "notice"; message: string }
  | { kind: "error"; message: string };

export type AssistantHandle = {
  /** Sends a message as if typed (e.g. "Fix the compile errors"). */
  send: (text: string, options?: { fix?: boolean }) => void;
};

/** How many rounds "Fix compile errors" may take before giving up. */
const MAX_FIX_ROUNDS = 3;

function threadKey(projectId: string) {
  return `openprism:thread:${projectId}`;
}

export function AssistantPanel({
  projectId,
  store,
  editorContext,
  compileResult,
  compiling,
  onCompile,
  onOpenFile,
  handle,
}: {
  projectId: string;
  store: DocumentStore;
  editorContext: () => EditorContext;
  compileResult: CompileResult | null;
  compiling: boolean;
  onCompile: () => void;
  onOpenFile: (path: string) => void;
  handle?: Ref<AssistantHandle>;
}) {
  const queryClient = useQueryClient();
  const threads = useQuery({
    queryKey: ["threads", projectId],
    queryFn: () =>
      apiJson<ThreadSummary[]>(`/api/ai/threads?projectId=${projectId}`),
  });
  const roles = useQuery({
    queryKey: ["roles"],
    queryFn: () => apiJson<Roles>("/api/settings/roles"),
  });
  const providers = useQuery({
    queryKey: ["providers"],
    queryFn: () => apiJson<ProviderView[]>("/api/settings/providers"),
  });

  const [threadId, setThreadId] = useState<string | null>(null);
  useEffect(() => {
    try {
      setThreadId(window.localStorage.getItem(threadKey(projectId)));
    } catch {
      // No storage: start without a remembered conversation.
    }
  }, [projectId]);
  const selectThread = useCallback(
    (id: string | null) => {
      setThreadId(id);
      try {
        if (id) window.localStorage.setItem(threadKey(projectId), id);
        else window.localStorage.removeItem(threadKey(projectId));
      } catch {
        // Ignore.
      }
    },
    [projectId],
  );

  const thread = useQuery({
    queryKey: ["thread", threadId],
    queryFn: async () => {
      try {
        return await apiJson<ThreadView>(`/api/ai/threads/${threadId}`);
      } catch (error) {
        if (error instanceof ApiError && error.status === 404) {
          selectThread(null);
          return null;
        }
        throw error;
      }
    },
    enabled: threadId !== null,
  });

  const [modelChoice, setModelChoice] = useState<ModelRef | null>(null);
  const modelOptions = useMemo(() => {
    const refs: Array<{ ref: ModelRef; role: string }> = [];
    const add = (ref: ModelRef | null | undefined, role: string) => {
      if (
        !ref ||
        refs.some(
          (o) =>
            o.ref.providerId === ref.providerId && o.ref.model === ref.model,
        )
      )
        return;
      refs.push({ ref, role });
    };
    add(roles.data?.agent, "agent");
    add(roles.data?.fast, "fast");
    add(thread.data?.thread.model, "");
    // Name the provider only when models of several providers are offered.
    const providersUsed = new Set(refs.map((o) => o.ref.providerId));
    return refs.map(({ ref, role }) => {
      const provider = providers.data?.find((p) => p.id === ref.providerId);
      const parts = [ref.model];
      if (providersUsed.size > 1) parts.push(`· ${provider?.name ?? "?"}`);
      if (role) parts.push(`(${role})`);
      return {
        ref,
        label: parts.join(" "),
        title: `${ref.model} — ${provider?.name ?? ""}`,
      };
    });
  }, [roles.data, providers.data, thread.data]);
  const model = modelChoice ?? roles.data?.agent ?? null;

  const [input, setInput] = useState("");
  const [live, setLive] = useState<LiveItem[]>([]);
  const [busy, setBusy] = useState(false);
  const [problem, setProblem] = useState<string | null>(null);
  const abort = useRef<AbortController | null>(null);
  const bottom = useRef<HTMLDivElement>(null);
  const [choices, setChoices] = useState<Record<string, boolean[]>>({});
  const fixRounds = useRef(0);
  const waitingForCompile = useRef(false);

  const pending = useMemo(
    () =>
      (thread.data?.patches ?? []).filter(
        (patch) => patch.status === "pending",
      ),
    [thread.data],
  );

  // Scroll to the newest content.
  // biome-ignore lint/correctness/useExhaustiveDependencies: scroll when content changes.
  useEffect(() => {
    bottom.current?.scrollIntoView({ block: "end" });
  }, [live, thread.data]);

  const onEvent = useCallback((event: ChatEvent) => {
    setLive((items) => {
      const next = [...items];
      const last = next[next.length - 1];
      switch (event.type) {
        case "textDelta":
          if (last?.kind === "text")
            next[next.length - 1] = { ...last, text: last.text + event.text };
          else next.push({ kind: "text", text: event.text });
          break;
        case "reasoningDelta":
          if (last?.kind === "reasoning")
            next[next.length - 1] = { ...last, text: last.text + event.text };
          else next.push({ kind: "reasoning", text: event.text });
          break;
        case "toolCall":
          next.push({
            kind: "tool",
            id: event.id,
            name: event.name,
            arguments: event.arguments,
          });
          break;
        case "toolResult": {
          const index = next.findIndex(
            (item) => item.kind === "tool" && item.id === event.id,
          );
          if (index !== -1) {
            next[index] = {
              ...(next[index] as Extract<LiveItem, { kind: "tool" }>),
              result: { content: event.content, isError: event.isError },
            };
          }
          break;
        }
        case "notice":
          next.push({ kind: "notice", message: event.message });
          break;
        case "error":
          next.push({ kind: "error", message: event.message });
          break;
        default:
          break;
      }
      return next;
    });
  }, []);

  const finishTurn = useCallback(
    async (id: string) => {
      await queryClient.invalidateQueries({ queryKey: ["thread", id] });
      await queryClient.invalidateQueries({ queryKey: ["threads", projectId] });
      setLive((items) =>
        items.filter((item) => item.kind === "error" || item.kind === "notice"),
      );
      setBusy(false);
      abort.current = null;
    },
    [queryClient, projectId],
  );

  const run = useCallback(
    async (
      path: (id: string) => `/api/${string}`,
      body: Record<string, unknown>,
      userText?: string,
    ) => {
      setProblem(null);
      let id = threadId;
      try {
        if (!id) {
          const created = await apiJson<ThreadSummary>("/api/ai/threads", {
            method: "POST",
            json: { projectId },
          });
          id = created.id;
          selectThread(id);
        }
        setLive(userText ? [{ kind: "user", text: userText }] : []);
        setBusy(true);
        const controller = new AbortController();
        abort.current = controller;
        await store.flush();
        await postEventStream<ChatEvent>(
          path(id),
          { ...body, editor: editorContext(), model: modelChoice ?? undefined },
          onEvent,
          controller.signal,
        );
      } catch (error) {
        if (!(error instanceof DOMException && error.name === "AbortError")) {
          setProblem(errorMessage(error));
        }
      } finally {
        if (id) await finishTurn(id);
        else setBusy(false);
      }
    },
    [
      threadId,
      projectId,
      selectThread,
      store,
      editorContext,
      modelChoice,
      onEvent,
      finishTurn,
    ],
  );

  const send = useCallback(
    (text: string, options?: { fix?: boolean }) => {
      const trimmed = text.trim();
      if (!trimmed || busy) return;
      if (options?.fix) fixRounds.current = 1;
      else fixRounds.current = 0;
      setInput("");
      void run(
        (id) => `/api/ai/threads/${id}/messages`,
        { text: trimmed },
        trimmed,
      );
    },
    [busy, run],
  );

  useImperativeHandle(handle, () => ({ send }), [send]);

  const decide = useCallback(
    async (acceptAll: boolean | null) => {
      setProblem(null);
      const decisions: PatchDecision[] = [];
      let anyApplied = false;
      try {
        for (const patch of pending) {
          const accepted =
            acceptAll === null
              ? (choices[patch.id] ?? patch.hunks.map(() => true))
              : patch.hunks.map(() => acceptAll);
          let failed: number[] = [];
          if (accepted.some(Boolean)) {
            failed = await applyPatch(patch, accepted, store, projectId);
            if (failed.length < accepted.filter(Boolean).length)
              anyApplied = true;
          }
          decisions.push({ patchId: patch.id, accepted, failed });
        }
        await store.flush();
      } catch (error) {
        setProblem(`Could not apply the edits: ${errorMessage(error)}`);
        return;
      }
      setChoices({});
      if (anyApplied && fixRounds.current > 0) waitingForCompile.current = true;
      await run((id) => `/api/ai/threads/${id}/decisions`, { decisions });
      if (waitingForCompile.current) onCompile();
    },
    [pending, choices, store, projectId, run, onCompile],
  );

  // "Fix compile errors": after the edits were applied and the document compiled again,
  // ask for another round while errors remain.
  useEffect(() => {
    if (!waitingForCompile.current || compiling || !compileResult || busy)
      return;
    waitingForCompile.current = false;
    const errors = compileResult.diagnostics.filter(
      (d) => d.severity === "error",
    ).length;
    if (errors === 0) {
      fixRounds.current = 0;
      setLive((items) => [
        ...items,
        {
          kind: "notice",
          message: "The document compiles without errors now.",
        },
      ]);
      return;
    }
    if (fixRounds.current >= MAX_FIX_ROUNDS) {
      fixRounds.current = 0;
      setLive((items) => [
        ...items,
        {
          kind: "notice",
          message: `Still ${errors} error(s) after ${MAX_FIX_ROUNDS} rounds; stopping here.`,
        },
      ]);
      return;
    }
    fixRounds.current += 1;
    void run(
      (id) => `/api/ai/threads/${id}/messages`,
      {
        text: `The document still has ${errors} compile error(s). Look at compile_log and fix them.`,
      },
      `The document still has ${errors} compile error(s). Fix them.`,
    );
  }, [compileResult, compiling, busy, run]);

  const onKey = (event: KeyboardEvent<HTMLTextAreaElement>) => {
    if (event.key === "Enter" && !event.shiftKey) {
      event.preventDefault();
      send(input);
    }
  };

  const noModel = roles.isSuccess && !roles.data.agent && !modelChoice;
  const messages = thread.data?.messages ?? [];
  const errorCount =
    compileResult?.diagnostics.filter((d) => d.severity === "error").length ??
    0;

  return (
    <div className="flex h-full min-h-0 flex-col bg-panel">
      <div className="flex h-9 shrink-0 items-center gap-1 border-b border-border bg-background px-2">
        <Bot className="size-4 text-accent" />
        <select
          aria-label="Conversation"
          value={threadId ?? ""}
          onChange={(e) => selectThread(e.target.value || null)}
          className="min-w-0 flex-1 truncate bg-transparent text-xs outline-none"
        >
          <option value="">New conversation</option>
          {threads.data?.map((t) => (
            <option key={t.id} value={t.id}>
              {t.awaitingDecision ? "● " : ""}
              {t.title}
            </option>
          ))}
        </select>
        <IconButton
          label="New conversation"
          onClick={() => selectThread(null)}
          disabled={busy}
        >
          <MessageSquarePlus className="size-4" />
        </IconButton>
        {threadId && (
          <IconButton
            label="Delete conversation"
            className="hover:text-danger"
            disabled={busy}
            onClick={async () => {
              if (!window.confirm("Delete this conversation?")) return;
              await apiFetch(`/api/ai/threads/${threadId}`, {
                method: "DELETE",
              });
              selectThread(null);
              void queryClient.invalidateQueries({
                queryKey: ["threads", projectId],
              });
            }}
          >
            <Trash2 className="size-4" />
          </IconButton>
        )}
        <Link
          href="/settings"
          aria-label="AI settings"
          title="AI settings"
          className="inline-flex size-7 items-center justify-center rounded-md text-muted hover:bg-zinc-200/60 hover:text-foreground dark:hover:bg-zinc-800/60"
        >
          <Settings className="size-4" />
        </Link>
      </div>

      <div className="min-h-0 flex-1 overflow-y-auto px-3 py-2">
        {noModel ? (
          <div className="mt-10 text-center text-sm text-muted">
            <p>No AI model is set up yet.</p>
            <Link href="/settings" className="text-accent hover:underline">
              Add a provider and choose a model
            </Link>
          </div>
        ) : messages.length === 0 && live.length === 0 ? (
          <div className="mt-10 flex flex-col items-center gap-3 text-center text-sm text-muted">
            <p>
              Ask about your document, or ask for changes. Edits are shown for
              review first.
            </p>
            {errorCount > 0 && (
              <Button
                onClick={() => send("Fix the compile errors.", { fix: true })}
              >
                <Wrench className="size-4" /> Fix {errorCount} compile error
                {errorCount === 1 ? "" : "s"}
              </Button>
            )}
          </div>
        ) : null}
        <ol className="flex flex-col gap-3">
          {messages.map((stored) => (
            <MessageView key={stored.id} message={stored.message} />
          ))}
          {live.map((item, index) => (
            // biome-ignore lint/suspicious/noArrayIndexKey: live items are append-only.
            <LiveView key={`live-${index}`} item={item} />
          ))}
        </ol>
        {busy && (
          <p className="mt-2 flex items-center gap-1.5 text-xs text-muted">
            <LoaderCircle className="size-3.5 animate-spin" /> Working…
          </p>
        )}
        {problem && <p className="mt-2 text-xs text-danger">{problem}</p>}

        {pending.length > 0 && !busy && (
          <section className="mt-3 flex flex-col gap-2">
            <p className="text-xs font-medium uppercase tracking-wide text-muted">
              Proposed edits
            </p>
            {pending.map((patch) => (
              <PatchReview
                key={patch.id}
                patch={patch}
                accepted={choices[patch.id] ?? patch.hunks.map(() => true)}
                onChange={(accepted) =>
                  setChoices((c) => ({ ...c, [patch.id]: accepted }))
                }
                onOpen={onOpenFile}
              />
            ))}
            <div className="flex flex-wrap gap-2">
              <Button variant="primary" onClick={() => void decide(null)}>
                Apply selected
              </Button>
              <Button onClick={() => void decide(false)}>Reject all</Button>
            </div>
          </section>
        )}
        <div ref={bottom} />
      </div>

      <div className="shrink-0 border-t border-border bg-background p-2">
        <textarea
          value={input}
          onChange={(e) => setInput(e.target.value)}
          onKeyDown={onKey}
          rows={3}
          placeholder={
            pending.length > 0
              ? "Decide on the edits above, or write a new message (rejects them)"
              : "Ask or describe a change… (Enter to send, Shift+Enter for a new line)"
          }
          disabled={noModel}
          className="w-full resize-none rounded-md border border-border bg-background px-2 py-1.5 text-sm outline-none focus:border-accent"
        />
        <div className="mt-1 flex items-center gap-2">
          <div className="relative min-w-0 flex-1">
            <select
              aria-label="Model"
              value={model ? `${model.providerId}::${model.model}` : ""}
              onChange={(e) => {
                const chosen = modelOptions.find(
                  (o) =>
                    `${o.ref.providerId}::${o.ref.model}` === e.target.value,
                );
                setModelChoice(chosen ? chosen.ref : null);
              }}
              className="w-full appearance-none truncate rounded-md bg-transparent py-1 pr-5 text-xs text-muted outline-none hover:text-foreground"
            >
              {modelOptions.length === 0 && <option value="">No model</option>}
              {modelOptions.map((option) => (
                <option
                  key={`${option.ref.providerId}::${option.ref.model}`}
                  value={`${option.ref.providerId}::${option.ref.model}`}
                  title={option.title}
                >
                  {option.label}
                </option>
              ))}
            </select>
            <ChevronDown className="pointer-events-none absolute top-1.5 right-1 size-3 text-muted" />
          </div>
          <Brain className="size-3.5 shrink-0 text-muted" aria-hidden />
          <select
            aria-label="Reasoning"
            title="Reasoning effort for this conversation"
            value={model?.reasoning ?? ""}
            disabled={!model}
            onChange={(e) =>
              model &&
              setModelChoice({
                ...model,
                reasoning: parseReasoning(e.target.value),
              })
            }
            className="shrink-0 rounded-md bg-transparent py-1 text-xs text-muted outline-none hover:text-foreground"
          >
            {REASONING_OPTIONS.map((option) => (
              <option key={option.value} value={option.value}>
                {option.label.toLowerCase()}
              </option>
            ))}
          </select>
        </div>
        <div className="mt-1 flex items-center justify-end gap-2">
          {thread.data && (
            <span
              className="shrink-0 text-[11px] text-muted tabular-nums"
              title="Tokens used in this conversation (input / output)"
            >
              {formatTokens(thread.data.thread.inputTokens)} /{" "}
              {formatTokens(thread.data.thread.outputTokens)}
            </span>
          )}
          {busy ? (
            <Button onClick={() => abort.current?.abort()} title="Stop">
              <Square className="size-3.5" /> Stop
            </Button>
          ) : (
            <Button
              variant="primary"
              onClick={() => send(input)}
              disabled={!input.trim() || noModel}
            >
              <Send className="size-3.5" /> Send
            </Button>
          )}
        </div>
      </div>
    </div>
  );
}

function formatTokens(count: number): string {
  return count >= 1000 ? `${(count / 1000).toFixed(1)}k` : String(count);
}

/** Applies the accepted hunks of a patch in the editor; returns the hunks that failed. */
async function applyPatch(
  patch: Patch,
  accepted: boolean[],
  store: DocumentStore,
  projectId: string,
): Promise<number[]> {
  if (patch.createsFile) {
    try {
      await apiFetch(fileUrl(projectId, patch.file), {
        method: "PUT",
        headers: { "if-none-match": "*" },
        body: patch.hunks[0]?.new ?? "",
      });
      await store.open(patch.file);
      return [];
    } catch {
      return [0];
    }
  }
  const indexes = accepted.flatMap((yes, i) => (yes ? [i] : []));
  const { failed } = await store.applyEdits(
    patch.file,
    indexes.map((i) => patch.hunks[i]),
  );
  return failed.map((i) => indexes[i]);
}

function toolLabel(name: string, args: unknown): string {
  const a = (args && typeof args === "object" ? args : {}) as Record<
    string,
    unknown
  >;
  switch (name) {
    case "read_file":
      return `Read ${String(a.path ?? "")}`;
    case "list_files":
      return "Listed the files";
    case "search":
      return `Searched for “${String(a.query ?? "")}”`;
    case "outline":
      return "Looked at the outline";
    case "compile_log":
      return "Read the compile log";
    case "propose_edit":
      return `Proposed an edit to ${String(a.path ?? "")}`;
    default:
      return name;
  }
}

function ToolChip({
  name,
  args,
  result,
}: {
  name: string;
  args: unknown;
  result?: { content: string; isError: boolean };
}) {
  const icon =
    name === "search" ? (
      <Search className="size-3" />
    ) : (
      <Wrench className="size-3" />
    );
  const label = (
    <span
      className={`inline-flex items-center gap-1 ${result?.isError ? "text-danger" : ""}`}
    >
      {icon}
      {toolLabel(name, args)}
    </span>
  );
  if (!result) return <li className="text-xs text-muted">{label}</li>;
  return (
    <li className="text-xs text-muted">
      <details>
        <summary className="cursor-pointer list-none hover:text-foreground">
          {label}
        </summary>
        <pre className="mt-1 max-h-48 overflow-auto whitespace-pre-wrap rounded bg-background p-2 font-mono text-[11px]">
          {result.content}
        </pre>
      </details>
    </li>
  );
}

function UserBubble({ text }: { text: string }) {
  return (
    <li className="ml-6 self-end rounded-lg bg-accent-soft px-3 py-2 text-sm whitespace-pre-wrap">
      {text}
    </li>
  );
}

function MessageView({ message }: { message: AiMessage }) {
  if (message.role === "user") {
    // Decisions on SEARCH/REPLACE edits are stored as user messages.
    if (/^(The user|Edit to)\b/.test(message.text)) {
      return <li className="text-xs text-muted italic">{message.text}</li>;
    }
    return <UserBubble text={message.text} />;
  }
  if (message.role === "tool") {
    const decisions = message.results.filter((r) => r.name === "propose_edit");
    return (
      <>
        {decisions.map((result) => (
          <li key={result.callId} className="text-xs text-muted italic">
            {result.content}
          </li>
        ))}
      </>
    );
  }
  return (
    <>
      {message.parts.map((part, index) => {
        switch (part.type) {
          case "text":
            return (
              // biome-ignore lint/suspicious/noArrayIndexKey: parts are positional.
              <li key={index}>
                <Markdown text={part.text} />
              </li>
            );
          case "reasoning":
            return (
              // biome-ignore lint/suspicious/noArrayIndexKey: parts are positional.
              <li key={index} className="text-xs text-muted">
                <details>
                  <summary className="cursor-pointer">Thinking</summary>
                  <p className="mt-1 whitespace-pre-wrap">{part.text}</p>
                </details>
              </li>
            );
          case "toolCall":
            return (
              <ToolChip key={part.id} name={part.name} args={part.arguments} />
            );
          default:
            return null;
        }
      })}
    </>
  );
}

function LiveView({ item }: { item: LiveItem }) {
  switch (item.kind) {
    case "user":
      return <UserBubble text={item.text} />;
    case "text":
      return (
        <li>
          <Markdown text={item.text} />
        </li>
      );
    case "reasoning":
      return (
        <li className="text-xs whitespace-pre-wrap text-muted italic">
          {item.text}
        </li>
      );
    case "tool":
      return (
        <ToolChip name={item.name} args={item.arguments} result={item.result} />
      );
    case "notice":
      return (
        <li className="flex items-start gap-1.5 text-xs text-muted">
          <Info className="mt-px size-3.5 shrink-0" /> {item.message}
        </li>
      );
    case "error":
      return (
        <li className="flex items-start gap-1.5 text-xs text-danger">
          <CircleAlert className="mt-px size-3.5 shrink-0" /> {item.message}
        </li>
      );
  }
}
