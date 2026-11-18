"use client";

import {
  ChevronDown,
  ChevronRight,
  File,
  FileCode2,
  FilePlus2,
  FileText,
  Folder,
  FolderPlus,
  Image as ImageIcon,
  Pencil,
  Star,
  Trash2,
  Upload,
} from "lucide-react";
import { useRef, useState } from "react";
import { IconButton } from "@/components/ui/button";
import { extension, fileKind, type TreeNode } from "@/lib/files/paths";

export type TreeActions = {
  open: (path: string) => void;
  createFile: (dir: string) => void;
  createFolder: (dir: string) => void;
  upload: (dir: string, files: FileList) => void;
  rename: (path: string) => void;
  remove: (path: string, kind: "file" | "dir") => void;
  setMain: (path: string) => void;
};

function FileIcon({ path }: { path: string }) {
  const kind = fileKind(path);
  const ext = extension(path);
  if (kind === "image") return <ImageIcon className="size-4 shrink-0" />;
  if (ext === "tex" || ext === "sty" || ext === "cls")
    return <FileCode2 className="size-4 shrink-0" />;
  if (kind === "text") return <FileText className="size-4 shrink-0" />;
  return <File className="size-4 shrink-0" />;
}

export function FileTree({
  nodes,
  active,
  mainFile,
  dirty,
  actions,
}: {
  nodes: TreeNode[];
  active: string | null;
  mainFile: string;
  dirty: Set<string>;
  actions: TreeActions;
}) {
  const [collapsed, setCollapsed] = useState<Set<string>>(new Set());
  const upload = useRef<HTMLInputElement>(null);
  const uploadDir = useRef("");

  const toggle = (path: string) =>
    setCollapsed((current) => {
      const next = new Set(current);
      if (next.has(path)) next.delete(path);
      else next.add(path);
      return next;
    });

  const startUpload = (dir: string) => {
    uploadDir.current = dir;
    upload.current?.click();
  };

  const renderNodes = (list: TreeNode[], depth: number) =>
    list.map((node) => {
      const isDir = node.kind === "dir";
      const isOpen = isDir && !collapsed.has(node.path);
      const isActive = node.path === active;
      return (
        <li key={node.path}>
          <div
            className={`group flex h-7 items-center gap-1 rounded-md pr-1 text-sm ${
              isActive
                ? "bg-accent-soft text-foreground"
                : "hover:bg-zinc-200/50 dark:hover:bg-zinc-800/50"
            }`}
            style={{ paddingLeft: 6 + depth * 14 }}
          >
            <button
              type="button"
              className="flex min-w-0 flex-1 items-center gap-1.5 text-left"
              onClick={() =>
                isDir ? toggle(node.path) : actions.open(node.path)
              }
              title={node.path}
            >
              {isDir ? (
                <>
                  {isOpen ? (
                    <ChevronDown className="size-3.5 shrink-0 text-muted" />
                  ) : (
                    <ChevronRight className="size-3.5 shrink-0 text-muted" />
                  )}
                  <Folder className="size-4 shrink-0 text-muted" />
                </>
              ) : (
                <span className="flex items-center pl-[18px] text-muted">
                  <FileIcon path={node.path} />
                </span>
              )}
              <span className="truncate">{node.name}</span>
              {node.path === mainFile && (
                <Star
                  className="size-3 shrink-0 fill-current text-warning"
                  aria-label="main file"
                />
              )}
              {dirty.has(node.path) && (
                <span
                  className="size-1.5 shrink-0 rounded-full bg-foreground/60"
                  title="Unsaved changes"
                />
              )}
            </button>
            <span className="hidden shrink-0 items-center group-hover:flex">
              {isDir && (
                <>
                  <IconButton
                    label="New file here"
                    className="size-6"
                    onClick={() => actions.createFile(node.path)}
                  >
                    <FilePlus2 className="size-3.5" />
                  </IconButton>
                  <IconButton
                    label="Upload here"
                    className="size-6"
                    onClick={() => startUpload(node.path)}
                  >
                    <Upload className="size-3.5" />
                  </IconButton>
                </>
              )}
              {!isDir &&
                extension(node.path) === "tex" &&
                node.path !== mainFile && (
                  <IconButton
                    label="Set as main file"
                    className="size-6"
                    onClick={() => actions.setMain(node.path)}
                  >
                    <Star className="size-3.5" />
                  </IconButton>
                )}
              <IconButton
                label="Rename or move"
                className="size-6"
                onClick={() => actions.rename(node.path)}
              >
                <Pencil className="size-3.5" />
              </IconButton>
              <IconButton
                label="Delete"
                className="size-6 hover:text-danger"
                onClick={() => actions.remove(node.path, node.kind)}
              >
                <Trash2 className="size-3.5" />
              </IconButton>
            </span>
          </div>
          {isOpen && node.children.length > 0 && (
            <ul>{renderNodes(node.children, depth + 1)}</ul>
          )}
        </li>
      );
    });

  return (
    <div className="flex h-full min-h-0 flex-col">
      <div className="flex h-9 shrink-0 items-center gap-1 border-b border-border px-2">
        <span className="flex-1 text-xs font-medium uppercase tracking-wide text-muted">
          Files
        </span>
        <IconButton label="New file" onClick={() => actions.createFile("")}>
          <FilePlus2 className="size-4" />
        </IconButton>
        <IconButton label="New folder" onClick={() => actions.createFolder("")}>
          <FolderPlus className="size-4" />
        </IconButton>
        <IconButton label="Upload files" onClick={() => startUpload("")}>
          <Upload className="size-4" />
        </IconButton>
      </div>
      <nav
        className="min-h-0 flex-1 overflow-auto p-1"
        aria-label="Project files"
      >
        <ul>{renderNodes(nodes, 0)}</ul>
      </nav>
      <input
        ref={upload}
        type="file"
        multiple
        className="hidden"
        onChange={(event) => {
          const files = event.target.files;
          if (files && files.length > 0)
            actions.upload(uploadDir.current, files);
          event.target.value = "";
        }}
      />
    </div>
  );
}
