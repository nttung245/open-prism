"use client";

import type { EditorView } from "@codemirror/view";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import {
  ArrowLeft,
  Bot,
  CircleCheck,
  CircleX,
  Download,
  History,
  LoaderCircle,
  Play,
  Settings,
  X,
} from "lucide-react";
import Link from "next/link";
import {
  useCallback,
  useEffect,
  useMemo,
  useRef,
  useState,
  useSyncExternalStore,
} from "react";
import { Group, Panel, Separator } from "react-resizable-panels";
import {
  type AssistantHandle,
  AssistantPanel,
} from "@/components/assistant/assistant-panel";
import { InlineActions } from "@/components/assistant/inline-actions";
import { HistoryPanel } from "@/components/history/history-panel";
import { Button, IconButton } from "@/components/ui/button";
import {
  ApiError,
  apiFetch,
  apiJson,
  errorMessage,
  fileUrl,
} from "@/lib/api/client";
import type { CompileResult } from "@/lib/api/types/CompileResult";
import type { CompileStarted } from "@/lib/api/types/CompileStarted";
import type { Diagnostic } from "@/lib/api/types/Diagnostic";
import type { FileOperation } from "@/lib/api/types/FileOperation";
import type { ForwardSearchResponse } from "@/lib/api/types/ForwardSearchResponse";
import type { InverseSearchResponse } from "@/lib/api/types/InverseSearchResponse";
import type { Project } from "@/lib/api/types/Project";
import type { ProjectEngine } from "@/lib/api/types/ProjectEngine";
import type { ProjectEvent } from "@/lib/api/types/ProjectEvent";
import type { TreeResponse } from "@/lib/api/types/TreeResponse";
import {
  basename,
  buildTree,
  isWithin,
  joinPath,
  validateName,
} from "@/lib/files/paths";
import { DocumentStore, httpTransport } from "@/lib/workspace/documents";
import {
  CodeEditor,
  type CodeEditorHandle,
  createEditorState,
  type EditorActions,
} from "./code-editor";
import { FileTree, type TreeActions } from "./file-tree";
import { type Highlight, PdfViewer } from "./pdf-viewer";
import {
  countBySeverity,
  isProjectFile,
  ProblemsPanel,
} from "./problems-panel";

const ENGINES: Array<{ value: ProjectEngine; label: string }> = [
  { value: "auto", label: "Auto" },
  { value: "pdflatex", label: "pdfLaTeX" },
  { value: "xelatex", label: "XeLaTeX" },
  { value: "lualatex", label: "LuaLaTeX" },
  { value: "tectonic", label: "Tectonic" },
];

const AUTO_COMPILE_KEY = "openprism:auto-compile";
const AUTO_COMPILE_DELAY_MS = 1500;

function readAutoCompile(): boolean {
  try {
    return window.localStorage.getItem(AUTO_COMPILE_KEY) !== "off";
  } catch {
    return true;
  }
}

export default function Workspace({
  initialProject,
}: {
  initialProject: Project;
}) {
  const id = initialProject.id;
  const queryClient = useQueryClient();
  const project = useQuery({
    queryKey: ["project", id],
    queryFn: () => apiJson<Project>(`/api/projects/${id}`),
    initialData: initialProject,
  }).data;
  const tree = useQuery({
    queryKey: ["tree", id],
    queryFn: () => apiJson<TreeResponse>(`/api/projects/${id}/tree`),
  });
  const nodes = useMemo(() => buildTree(tree.data?.entries ?? []), [tree.data]);

  const [notice, setNotice] = useState<string | null>(null);
  // The right-hand panel shows the assistant or the history, never both.
  const [sidebar, setSidebar] = useState<Sidebar>(null);
  const assistantOpen = sidebar === "assistant";
  const assistant = useRef<AssistantHandle>(null);
  const pendingAssistantMessage = useRef<string | null>(null);
  useEffect(() => {
    setSidebar(loadSidebar());
  }, []);
  const showSidebar = (next: Sidebar) => {
    setSidebar(next);
    try {
      window.localStorage.setItem(SIDEBAR_KEY, next ?? "none");
    } catch {
      // Storage may be unavailable; the choice then lasts for this page only.
    }
  };
  const toggleAssistant = (open: boolean) =>
    showSidebar(open ? "assistant" : null);
  // Send a message queued while the panel was still opening.
  useEffect(() => {
    if (assistantOpen && pendingAssistantMessage.current && assistant.current) {
      assistant.current.send(pendingAssistantMessage.current, { fix: true });
      pendingAssistantMessage.current = null;
    }
  });
  const fixWithAi = () => {
    if (assistant.current && assistantOpen) {
      assistant.current.send("Fix the compile errors.", { fix: true });
    } else {
      pendingAssistantMessage.current = "Fix the compile errors.";
      toggleAssistant(true);
    }
  };
  const editorContext = () => {
    const path = store.getSnapshot().active;
    const cursor = editor.current?.cursor();
    const selection = editor.current?.selection();
    return {
      activeFile: path ?? undefined,
      cursorLine: cursor?.line,
      selection: selection?.text ? selection.text : undefined,
    };
  };
  const [autoCompile, setAutoCompile] = useState(true);
  useEffect(() => setAutoCompile(readAutoCompile()), []);

  // --- documents ---------------------------------------------------------------------
  const actions = useRef<EditorActions>({
    save: () => {},
    compile: () => {},
    syncToPdf: () => {},
  });
  const onSaved = useRef<(path: string) => void>(() => {});
  const [store] = useState(
    () =>
      new DocumentStore({
        transport: httpTransport(id),
        createState: (text) => createEditorState(text, actions),
        onSaved: (path) => onSaved.current(path),
      }),
  );
  useEffect(() => () => store.dispose(), [store]);
  const snapshot = useSyncExternalStore(
    store.subscribe,
    store.getSnapshot,
    store.getSnapshot,
  );
  const activeDoc = snapshot.docs.find((doc) => doc.path === snapshot.active);
  const dirtyPaths = useMemo(
    () => new Set(snapshot.docs.filter((d) => d.dirty).map((d) => d.path)),
    [snapshot.docs],
  );
  const editor = useRef<CodeEditorHandle>(null);
  const pendingGoto = useRef<{
    path: string;
    line: number;
    column: number | null;
  } | null>(null);

  const openAt = useCallback(
    async (path: string, line?: number | null, column?: number | null) => {
      if (line) pendingGoto.current = { path, line, column: column ?? null };
      await store.open(path);
    },
    [store],
  );

  // Jump to a requested line once its document is on screen.
  useEffect(() => {
    const goto = pendingGoto.current;
    if (!goto || snapshot.active !== goto.path || !editor.current) return;
    if (activeDoc?.loading) return;
    editor.current.goTo(goto.line, goto.column);
    pendingGoto.current = null;
  });

  // Open the main file first.
  const openedMain = useRef(false);
  useEffect(() => {
    if (openedMain.current || !tree.data) return;
    openedMain.current = true;
    if (tree.data.entries.some((e) => e.path === project.mainFile)) {
      void store.open(project.mainFile);
    }
  }, [tree.data, project.mainFile, store]);

  // --- compilation -------------------------------------------------------------------
  const latest = useQuery({
    queryKey: ["compile-latest", id],
    queryFn: () =>
      apiJson<CompileResult | null>(`/api/projects/${id}/compile/latest`),
  });
  const [result, setResult] = useState<CompileResult | null>(null);
  const [pdfRun, setPdfRun] = useState<string | null>(null);
  const [running, setRunning] = useState(false);
  const requested = useRef<string | null>(null);

  const showResult = useCallback((next: CompileResult) => {
    setResult(next);
    if (next.hasPdf) setPdfRun(next.runId);
  }, []);

  useEffect(() => {
    if (latest.data && !requested.current) showResult(latest.data);
  }, [latest.data, showResult]);

  const compile = useCallback(async () => {
    try {
      setRunning(true);
      await store.flush();
      const started = await apiJson<CompileStarted>(
        `/api/projects/${id}/compile`,
        {
          method: "POST",
        },
      );
      requested.current = started.runId;
    } catch (error) {
      setRunning(false);
      setNotice(`Cannot compile: ${errorMessage(error)}`);
    }
  }, [id, store]);

  const autoTimer = useRef<ReturnType<typeof setTimeout> | null>(null);
  onSaved.current = () => {
    if (!autoCompile) return;
    if (autoTimer.current) clearTimeout(autoTimer.current);
    autoTimer.current = setTimeout(() => void compile(), AUTO_COMPILE_DELAY_MS);
  };
  useEffect(
    () => () => {
      if (autoTimer.current) clearTimeout(autoTimer.current);
    },
    [],
  );

  // --- SyncTeX -----------------------------------------------------------------------
  const [highlight, setHighlight] = useState<Highlight | null>(null);

  const syncToPdf = useCallback(async () => {
    const cursor = editor.current?.cursor();
    const path = store.getSnapshot().active;
    if (!cursor || !path || !pdfRun) return;
    try {
      const query = new URLSearchParams({
        file: path,
        line: String(cursor.line),
        column: String(cursor.column),
      });
      const found = await apiJson<ForwardSearchResponse>(
        `/api/projects/${id}/runs/${pdfRun}/synctex/forward?${query}`,
      );
      if (found.rects.length === 0) {
        setNotice("This line has no position in the PDF.");
      } else {
        setHighlight({ rects: found.rects, nonce: Date.now() });
      }
    } catch (error) {
      setNotice(`Cannot find the line in the PDF: ${errorMessage(error)}`);
    }
  }, [id, pdfRun, store]);

  const inverseSearch = useCallback(
    async (page: number, x: number, y: number) => {
      if (!pdfRun) return;
      try {
        const query = new URLSearchParams({
          page: String(page),
          x: x.toFixed(2),
          y: y.toFixed(2),
        });
        const found = await apiJson<InverseSearchResponse>(
          `/api/projects/${id}/runs/${pdfRun}/synctex/inverse?${query}`,
        );
        if (found.file) await openAt(found.file, found.line, found.column);
        else setNotice("This part of the PDF comes from outside the project.");
      } catch (error) {
        if (error instanceof ApiError && error.status === 404) {
          setNotice("No source position here.");
        } else {
          setNotice(`Cannot find the source: ${errorMessage(error)}`);
        }
      }
    },
    [id, pdfRun, openAt],
  );

  actions.current = {
    save: () => {
      const path = store.getSnapshot().active;
      if (path) void store.save(path);
    },
    compile: () => void compile(),
    syncToPdf: () => void syncToPdf(),
  };

  // --- live updates ------------------------------------------------------------------
  const onEvent = useRef<(event: ProjectEvent) => void>(() => {});
  onEvent.current = (event) => {
    switch (event.type) {
      case "filesChanged":
        void queryClient.invalidateQueries({ queryKey: ["tree", id] });
        void store.refresh(event.paths);
        break;
      case "compileStarted":
        if (event.runId === requested.current) setRunning(true);
        break;
      case "compileFinished": {
        const mine = event.result.runId === requested.current;
        if (mine) {
          requested.current = null;
          setRunning(false);
        }
        if (mine || event.result.status !== "cancelled")
          showResult(event.result);
        break;
      }
      case "historyChanged":
        void queryClient.invalidateQueries({ queryKey: ["history", id] });
        break;
      case "resync":
        void queryClient.invalidateQueries({ queryKey: ["tree", id] });
        void queryClient.invalidateQueries({
          queryKey: ["compile-latest", id],
        });
        void store.refresh([""]);
        void queryClient.invalidateQueries({ queryKey: ["history", id] });
        break;
    }
  };
  useEffect(() => {
    const source = new EventSource(`/api/projects/${id}/events`);
    let reconnecting = false;
    source.onmessage = (message) => {
      try {
        onEvent.current(JSON.parse(message.data) as ProjectEvent);
      } catch {
        // Ignore malformed events.
      }
    };
    source.onerror = () => {
      reconnecting = true;
    };
    source.onopen = () => {
      if (reconnecting) onEvent.current({ type: "resync" });
      reconnecting = false;
    };
    return () => source.close();
  }, [id]);

  // Global shortcuts (also when the editor is not focused) and unsaved-changes guard.
  useEffect(() => {
    const onKey = (event: KeyboardEvent) => {
      // The editor's own key bindings already handled it.
      if (event.defaultPrevented) return;
      const mod = event.ctrlKey || event.metaKey;
      if (mod && event.key === "Enter") {
        event.preventDefault();
        actions.current.compile();
      } else if (mod && event.key.toLowerCase() === "s") {
        event.preventDefault();
        actions.current.save();
      }
    };
    const onBeforeUnload = (event: BeforeUnloadEvent) => {
      if (store.hasUnsavedChanges()) event.preventDefault();
    };
    window.addEventListener("keydown", onKey);
    window.addEventListener("beforeunload", onBeforeUnload);
    return () => {
      window.removeEventListener("keydown", onKey);
      window.removeEventListener("beforeunload", onBeforeUnload);
    };
  }, [store]);

  // --- project settings and file operations ------------------------------------------
  const updateProject = async (
    patch: Partial<Pick<Project, "mainFile" | "engine">>,
  ) => {
    try {
      const updated = await apiJson<Project>(`/api/projects/${id}`, {
        method: "PATCH",
        json: patch,
      });
      queryClient.setQueryData(["project", id], updated);
    } catch (error) {
      setNotice(errorMessage(error));
    }
  };

  const fileOp = async (operation: FileOperation) => {
    await apiJson<void>(`/api/projects/${id}/file-ops`, {
      method: "POST",
      json: operation,
    });
    await queryClient.invalidateQueries({ queryKey: ["tree", id] });
  };

  const askName = (message: string, initial: string): string | null => {
    const value = window.prompt(message, initial);
    if (value === null) return null;
    const problem = validateName(value);
    if (problem) {
      setNotice(problem);
      return null;
    }
    return value.trim();
  };

  const treeActions: TreeActions = {
    open: (path) => void store.open(path),
    createFile: async (dir) => {
      const name = askName("New file name", "untitled.tex");
      if (!name) return;
      const path = joinPath(dir, name);
      try {
        await fileOp({ op: "createFile", path });
        await store.open(path);
      } catch (error) {
        setNotice(errorMessage(error));
      }
    },
    createFolder: async (dir) => {
      const name = askName("New folder name", "figures");
      if (!name) return;
      try {
        await fileOp({ op: "mkdir", path: joinPath(dir, name) });
      } catch (error) {
        setNotice(errorMessage(error));
      }
    },
    upload: async (dir, files) => {
      const form = new FormData();
      for (const file of Array.from(files)) form.append("file", file);
      try {
        const query = dir ? `?dir=${encodeURIComponent(dir)}` : "";
        await apiFetch(`/api/projects/${id}/upload${query}`, {
          method: "POST",
          body: form,
        });
        await queryClient.invalidateQueries({ queryKey: ["tree", id] });
      } catch (error) {
        setNotice(`Upload failed: ${errorMessage(error)}`);
      }
    },
    rename: async (from) => {
      const to = window.prompt("Rename or move to (path in the project)", from);
      if (!to || to.trim() === from) return;
      const target = to.trim().replace(/^\/+/, "");
      try {
        await store.save(from);
        await fileOp({ op: "rename", from, to: target });
        store.renamed(from, target);
        if (isWithin(project.mainFile, from)) {
          await updateProject({
            mainFile: target + project.mainFile.slice(from.length),
          });
        }
      } catch (error) {
        setNotice(errorMessage(error));
      }
    },
    remove: async (path, kind) => {
      const what =
        kind === "dir"
          ? `the folder "${path}" and everything in it`
          : `"${path}"`;
      if (!window.confirm(`Delete ${what}?`)) return;
      try {
        await fileOp({ op: "delete", path });
        store.removed(path);
      } catch (error) {
        setNotice(errorMessage(error));
      }
    },
    setMain: (path) => void updateProject({ mainFile: path }),
  };

  // --- rendering ---------------------------------------------------------------------
  const diagnosticsFor = useMemo(() => {
    const byFile = new Map<string, Diagnostic[]>();
    for (const diagnostic of result?.diagnostics ?? []) {
      if (!diagnostic.file) continue;
      const list = byFile.get(diagnostic.file) ?? [];
      list.push(diagnostic);
      byFile.set(diagnostic.file, list);
    }
    return byFile;
  }, [result]);
  const activeDiagnostics = useMemo(
    () => (snapshot.active ? (diagnosticsFor.get(snapshot.active) ?? []) : []),
    [diagnosticsFor, snapshot.active],
  );
  const counts = countBySeverity(result?.diagnostics ?? []);
  const pdfUrl = pdfRun
    ? `/api/projects/${id}/runs/${pdfRun}/output.pdf`
    : null;
  const logUrl = result
    ? `/api/projects/${id}/runs/${result.runId}/output.log`
    : null;
  const attachView = useCallback(
    (view: EditorView | null) => store.attachView(view),
    [store],
  );
  const getState = useCallback((path: string) => store.getState(path), [store]);
  const onChange = useCallback(
    (path: string, state: Parameters<DocumentStore["update"]>[1]) =>
      store.update(path, state),
    [store],
  );

  return (
    <div className="flex h-full min-h-0 flex-col">
      <header className="flex h-11 shrink-0 items-center gap-2 border-b border-border px-2">
        <Link
          href="/"
          aria-label="All projects"
          title="All projects"
          className="inline-flex size-7 items-center justify-center rounded-md text-muted hover:bg-zinc-200/60 hover:text-foreground dark:hover:bg-zinc-800/60"
        >
          <ArrowLeft className="size-4" />
        </Link>
        <h1 className="min-w-0 truncate text-sm font-semibold">
          {project.name}
        </h1>
        <span className="hidden truncate text-xs text-muted sm:inline">
          main: {project.mainFile}
        </span>
        <span className="flex-1" />
        <label className="hidden items-center gap-1.5 text-xs text-muted md:flex">
          Engine
          <select
            value={project.engine}
            onChange={(event) =>
              void updateProject({
                engine: event.target.value as ProjectEngine,
              })
            }
            className="rounded-md border border-border bg-background px-1.5 py-1 text-xs text-foreground"
          >
            {ENGINES.map((engine) => (
              <option key={engine.value} value={engine.value}>
                {engine.label}
              </option>
            ))}
          </select>
        </label>
        <label
          className="flex items-center gap-1.5 text-xs text-muted"
          title="Compile after every save"
        >
          <input
            type="checkbox"
            checked={autoCompile}
            onChange={(event) => {
              setAutoCompile(event.target.checked);
              try {
                window.localStorage.setItem(
                  AUTO_COMPILE_KEY,
                  event.target.checked ? "on" : "off",
                );
              } catch {
                // Storage may be unavailable; the setting then lasts for this page only.
              }
            }}
            className="accent-accent"
          />
          Auto
        </label>
        <CompileStatus running={running} result={result} counts={counts} />
        <Button
          variant="primary"
          onClick={() => void compile()}
          title="Compile (Ctrl+Enter)"
        >
          {running ? (
            <LoaderCircle className="size-4 animate-spin" />
          ) : (
            <Play className="size-4" />
          )}
          Compile
        </Button>
        <IconButton
          label={assistantOpen ? "Hide the assistant" : "Show the assistant"}
          onClick={() => toggleAssistant(!assistantOpen)}
          className={assistantOpen ? "text-accent" : ""}
        >
          <Bot className="size-4" />
        </IconButton>
        <IconButton
          label={
            sidebar === "history" ? "Hide the history" : "Show the history"
          }
          onClick={() => showSidebar(sidebar === "history" ? null : "history")}
          className={sidebar === "history" ? "text-accent" : ""}
        >
          <History className="size-4" />
        </IconButton>
        <Link
          href="/settings"
          aria-label="Settings"
          title="Settings"
          className="inline-flex size-7 items-center justify-center rounded-md text-muted hover:bg-zinc-200/60 hover:text-foreground dark:hover:bg-zinc-800/60"
        >
          <Settings className="size-4" />
        </Link>
        <a
          href={`/api/projects/${id}/export`}
          aria-label="Download project as ZIP"
          title="Download project as ZIP"
          className="inline-flex size-7 items-center justify-center rounded-md text-muted hover:bg-zinc-200/60 hover:text-foreground dark:hover:bg-zinc-800/60"
        >
          <Download className="size-4" />
        </a>
      </header>

      {notice && (
        <output className="flex items-center gap-2 border-b border-border bg-accent-soft/60 px-3 py-1.5 text-xs">
          <span className="flex-1">{notice}</span>
          <IconButton
            label="Dismiss"
            className="size-5"
            onClick={() => setNotice(null)}
          >
            <X className="size-3.5" />
          </IconButton>
        </output>
      )}

      <Group orientation="horizontal" className="min-h-0 flex-1">
        <Panel
          id="files"
          defaultSize="18"
          minSize="10"
          collapsible
          className="bg-panel"
        >
          {tree.error ? (
            <p className="p-3 text-xs text-danger">
              {errorMessage(tree.error)}
            </p>
          ) : (
            <FileTree
              nodes={nodes}
              active={snapshot.active}
              mainFile={project.mainFile}
              dirty={dirtyPaths}
              actions={treeActions}
            />
          )}
        </Panel>
        <Separator className="w-px bg-border transition-colors hover:bg-accent data-[separator=active]:bg-accent" />
        <Panel id="editor" defaultSize="42" minSize="20">
          <Group orientation="vertical" className="h-full">
            <Panel minSize="20">
              <div className="relative flex h-full min-h-0 flex-col">
                <Tabs
                  docs={snapshot.docs}
                  active={snapshot.active}
                  onSelect={(path) => store.activate(path)}
                  onClose={(path) => void store.close(path)}
                />
                <InlineActions
                  projectId={id}
                  getSelection={() => editor.current?.selection() ?? null}
                  onReplace={(selection, text) =>
                    editor.current?.replace(
                      selection.from,
                      selection.to,
                      selection.text,
                      text,
                    ) ?? false
                  }
                  onNotice={setNotice}
                />
                {activeDoc?.conflict && (
                  <ConflictBanner
                    kind={activeDoc.conflict.kind}
                    onKeepMine={() =>
                      void store.resolve(activeDoc.path, "mine")
                    }
                    onTakeTheirs={() =>
                      void store.resolve(activeDoc.path, "theirs")
                    }
                  />
                )}
                {activeDoc?.error && !activeDoc.conflict && (
                  <p className="border-b border-border px-3 py-1.5 text-xs text-danger">
                    {activeDoc.error}
                  </p>
                )}
                <div className="min-h-0 flex-1">
                  {!activeDoc ? (
                    <p className="mt-16 text-center text-sm text-muted">
                      Open a file from the list on the left.
                    </p>
                  ) : activeDoc.loading ? (
                    <p className="mt-16 text-center text-sm text-muted">
                      Loading…
                    </p>
                  ) : activeDoc.kind === "text" ? (
                    store.getState(activeDoc.path) && (
                      <CodeEditor
                        handle={editor}
                        path={activeDoc.path}
                        revision={activeDoc.revision}
                        getState={getState}
                        diagnostics={activeDiagnostics}
                        onChange={onChange}
                        onView={attachView}
                      />
                    )
                  ) : activeDoc.kind === "image" ? (
                    <div className="flex h-full items-center justify-center overflow-auto bg-[conic-gradient(#e4e4e7_25%,transparent_0_50%,#e4e4e7_0_75%,transparent_0)] bg-[length:16px_16px] p-6 dark:bg-[conic-gradient(#27272a_25%,transparent_0_50%,#27272a_0_75%,transparent_0)]">
                      {/* biome-ignore lint/performance/noImgElement: project files are served by the API, not optimisable. */}
                      <img
                        src={`${fileUrl(id, activeDoc.path)}?r=${activeDoc.revision}`}
                        alt={basename(activeDoc.path)}
                        className="max-h-full max-w-full shadow"
                      />
                    </div>
                  ) : (
                    <div className="mt-16 text-center text-sm text-muted">
                      <p>This file cannot be edited here.</p>
                      <a
                        href={fileUrl(id, activeDoc.path)}
                        download={basename(activeDoc.path)}
                        className="text-accent hover:underline"
                      >
                        Download {basename(activeDoc.path)}
                      </a>
                    </div>
                  )}
                </div>
              </div>
            </Panel>
            <Separator className="h-px bg-border transition-colors hover:bg-accent data-[separator=active]:bg-accent" />
            <Panel
              defaultSize="22"
              minSize="8"
              collapsible
              className="bg-panel"
            >
              <ProblemsPanel
                result={result}
                logUrl={logUrl}
                onFix={fixWithAi}
                onSelect={(diagnostic) => {
                  if (isProjectFile(diagnostic.file))
                    void openAt(diagnostic.file, diagnostic.line ?? 1);
                }}
              />
            </Panel>
          </Group>
        </Panel>
        <Separator className="w-px bg-border transition-colors hover:bg-accent data-[separator=active]:bg-accent" />
        <Panel id="pdf" defaultSize="40" minSize="15">
          <PdfViewer
            url={pdfUrl}
            downloadName={`${project.name}.pdf`}
            highlight={highlight}
            onInverseSearch={(page, x, y) => void inverseSearch(page, x, y)}
          />
        </Panel>
        {sidebar === "history" && (
          <>
            <Separator className="w-px bg-border transition-colors hover:bg-accent data-[separator=active]:bg-accent" />
            <Panel id="history" defaultSize="28" minSize="18">
              <HistoryPanel
                projectId={id}
                store={store}
                onOpenFile={(path) => void store.open(path)}
                onNotice={setNotice}
                onRestored={() => {
                  if (autoCompile) void compile();
                }}
              />
            </Panel>
          </>
        )}
        {assistantOpen && (
          <>
            <Separator className="w-px bg-border transition-colors hover:bg-accent data-[separator=active]:bg-accent" />
            <Panel id="assistant" defaultSize="28" minSize="18">
              <AssistantPanel
                handle={assistant}
                projectId={id}
                store={store}
                editorContext={editorContext}
                compileResult={result}
                compiling={running}
                onCompile={() => void compile()}
                onOpenFile={(path) => void store.open(path)}
              />
            </Panel>
          </>
        )}
      </Group>
    </div>
  );
}

type Sidebar = "assistant" | "history" | null;

const SIDEBAR_KEY = "openprism:sidebar";

/** The remembered right-hand panel; reads the older `openprism:assistant` key once. */
function loadSidebar(): Sidebar {
  try {
    const stored = window.localStorage.getItem(SIDEBAR_KEY);
    if (stored === "assistant" || stored === "history") return stored;
    if (stored === null) {
      const legacy = window.localStorage.getItem("openprism:assistant");
      window.localStorage.removeItem("openprism:assistant");
      if (legacy === "open") {
        window.localStorage.setItem(SIDEBAR_KEY, "assistant");
        return "assistant";
      }
    }
  } catch {
    // Closed by default.
  }
  return null;
}

function CompileStatus({
  running,
  result,
  counts,
}: {
  running: boolean;
  result: CompileResult | null;
  counts: { error: number; warning: number };
}) {
  if (running) return <span className="text-xs text-muted">Compiling…</span>;
  if (!result) return null;
  const seconds = (result.durationMs / 1000).toFixed(1);
  switch (result.status) {
    case "success":
      return (
        <span
          className="flex items-center gap-1 text-xs text-muted"
          title={`${counts.warning} warnings`}
        >
          <CircleCheck className="size-3.5 text-accent" />
          {seconds} s{counts.warning > 0 && ` · ${counts.warning} warnings`}
        </span>
      );
    case "failed":
      return (
        <span className="flex items-center gap-1 text-xs text-danger">
          <CircleX className="size-3.5" />
          {counts.error > 0
            ? `${counts.error} error${counts.error === 1 ? "" : "s"}`
            : "Failed"}
        </span>
      );
    case "timedOut":
      return <span className="text-xs text-danger">Timed out</span>;
    case "cancelled":
      return <span className="text-xs text-muted">Cancelled</span>;
    case "error":
      return (
        <span
          className="text-xs text-danger"
          title={result.message ?? undefined}
        >
          Cannot compile
        </span>
      );
  }
}

function Tabs({
  docs,
  active,
  onSelect,
  onClose,
}: {
  docs: Array<{
    path: string;
    dirty: boolean;
    saving: boolean;
    conflict: unknown;
  }>;
  active: string | null;
  onSelect: (path: string) => void;
  onClose: (path: string) => void;
}) {
  return (
    <div
      role="tablist"
      className="flex h-9 shrink-0 items-end gap-px overflow-x-auto border-b border-border bg-panel pr-16 pl-1"
    >
      {docs.map((doc) => {
        const selected = doc.path === active;
        return (
          <div
            key={doc.path}
            className={`group flex h-8 max-w-56 items-center gap-1 rounded-t-md border border-b-0 px-2 text-xs ${
              selected
                ? "border-border bg-background text-foreground"
                : "border-transparent text-muted hover:text-foreground"
            }`}
          >
            <button
              type="button"
              role="tab"
              aria-selected={selected}
              title={doc.path}
              onClick={() => onSelect(doc.path)}
              className="truncate"
            >
              {basename(doc.path)}
            </button>
            {doc.saving ? (
              <LoaderCircle
                className="size-3 shrink-0 animate-spin"
                aria-label="Saving"
              />
            ) : doc.conflict ? (
              <CircleX
                className="size-3 shrink-0 text-danger"
                aria-label="Conflict"
              />
            ) : doc.dirty ? (
              <span
                className="size-1.5 shrink-0 rounded-full bg-foreground/60"
                title="Unsaved changes"
              >
                <span className="sr-only">Unsaved changes</span>
              </span>
            ) : null}
            <button
              type="button"
              aria-label={`Close ${basename(doc.path)}`}
              onClick={() => onClose(doc.path)}
              className="rounded p-0.5 opacity-0 hover:bg-zinc-200/60 group-hover:opacity-100 dark:hover:bg-zinc-800/60"
            >
              <X className="size-3" />
            </button>
          </div>
        );
      })}
    </div>
  );
}

function ConflictBanner({
  kind,
  onKeepMine,
  onTakeTheirs,
}: {
  kind: "external" | "rejected" | "deleted";
  onKeepMine: () => void;
  onTakeTheirs: () => void;
}) {
  const message =
    kind === "deleted"
      ? "This file was deleted on disk while you had unsaved changes."
      : "This file changed on disk while you had unsaved changes.";
  return (
    <div
      role="alert"
      className="flex flex-wrap items-center gap-2 border-b border-border bg-amber-50 px-3 py-1.5 text-xs dark:bg-amber-950/40"
    >
      <span className="flex-1">{message}</span>
      <Button onClick={onTakeTheirs} className="py-1 text-xs">
        {kind === "deleted" ? "Discard my changes" : "Load the disk version"}
      </Button>
      <Button variant="primary" onClick={onKeepMine} className="py-1 text-xs">
        Keep my version
      </Button>
    </div>
  );
}
