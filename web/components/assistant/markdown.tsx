/**
 * Just enough Markdown for assistant answers: paragraphs, fenced code, inline code, bold and
 * lists. Everything is rendered as text nodes (never as HTML), so model output cannot inject
 * markup.
 */
import type { ReactNode } from "react";

function inline(text: string, keyPrefix: string): ReactNode[] {
  const parts: ReactNode[] = [];
  const pattern = /(`[^`\n]+`|\*\*[^*\n]+\*\*)/g;
  let last = 0;
  let index = 0;
  for (const match of text.matchAll(pattern)) {
    const at = match.index ?? 0;
    if (at > last) parts.push(text.slice(last, at));
    const token = match[0];
    if (token.startsWith("`")) {
      parts.push(
        <code
          key={`${keyPrefix}-${index}`}
          className="rounded bg-zinc-200/70 px-1 py-px font-mono text-[0.85em] dark:bg-zinc-800"
        >
          {token.slice(1, -1)}
        </code>,
      );
    } else {
      parts.push(
        <strong key={`${keyPrefix}-${index}`}>{token.slice(2, -2)}</strong>,
      );
    }
    last = at + token.length;
    index += 1;
  }
  if (last < text.length) parts.push(text.slice(last));
  return parts;
}

export function Markdown({ text }: { text: string }) {
  const blocks: ReactNode[] = [];
  const lines = text.split("\n");
  let i = 0;
  let key = 0;
  while (i < lines.length) {
    const line = lines[i];
    if (line.trimStart().startsWith("```")) {
      const code: string[] = [];
      i += 1;
      while (i < lines.length && !lines[i].trimStart().startsWith("```")) {
        code.push(lines[i]);
        i += 1;
      }
      i += 1;
      blocks.push(
        <pre
          key={key++}
          className="my-2 overflow-x-auto rounded-md bg-zinc-100 p-2 font-mono text-xs dark:bg-zinc-900"
        >
          {code.join("\n")}
        </pre>,
      );
      continue;
    }
    if (line.startsWith(">")) {
      const quoted: string[] = [];
      while (i < lines.length && lines[i].startsWith(">")) {
        quoted.push(lines[i].replace(/^>\s?/, ""));
        i += 1;
      }
      blocks.push(
        <blockquote
          key={key++}
          className="my-1 border-l-2 border-border pl-3 whitespace-pre-wrap text-muted"
        >
          {inline(quoted.join("\n"), `q${key}`)}
        </blockquote>,
      );
      continue;
    }
    if (/^\s*([-*]|\d+\.)\s+/.test(line)) {
      const items: string[] = [];
      while (i < lines.length && /^\s*([-*]|\d+\.)\s+/.test(lines[i])) {
        items.push(lines[i].replace(/^\s*([-*]|\d+\.)\s+/, ""));
        i += 1;
      }
      blocks.push(
        <ul key={key++} className="my-1 list-disc pl-5">
          {items.map((item, n) => (
            // biome-ignore lint/suspicious/noArrayIndexKey: static text.
            <li key={n}>{inline(item, `${key}-${n}`)}</li>
          ))}
        </ul>,
      );
      continue;
    }
    if (!line.trim()) {
      i += 1;
      continue;
    }
    const paragraph: string[] = [];
    while (
      i < lines.length &&
      lines[i].trim() &&
      !lines[i].trimStart().startsWith("```") &&
      !/^\s*([-*]|\d+\.)\s+/.test(lines[i]) &&
      !lines[i].startsWith(">")
    ) {
      paragraph.push(lines[i].replace(/^#+\s*/, ""));
      i += 1;
    }
    blocks.push(
      <p key={key++} className="my-1 whitespace-pre-wrap">
        {inline(paragraph.join("\n"), String(key))}
      </p>,
    );
  }
  return <div className="text-sm leading-relaxed">{blocks}</div>;
}
