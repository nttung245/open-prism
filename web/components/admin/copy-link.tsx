"use client";

import { Check, Copy } from "lucide-react";
import { useRef, useState } from "react";
import { Button } from "@/components/ui/button";

/** A one-time link to send to someone, with a button to copy it. */
export function CopyLink({ link, note }: { link: string; note: string }) {
  const field = useRef<HTMLInputElement>(null);
  const [copied, setCopied] = useState(false);
  const copy = async () => {
    try {
      await navigator.clipboard.writeText(link);
      setCopied(true);
    } catch {
      // The clipboard needs HTTPS: select the link so it can be copied by hand.
      field.current?.select();
    }
  };
  return (
    <div className="flex flex-col gap-1 rounded-md border border-accent/40 bg-accent/5 p-2">
      <p className="text-xs">{note}</p>
      <div className="flex gap-2">
        <input
          ref={field}
          readOnly
          value={link}
          aria-label="One-time link"
          onFocus={(event) => event.target.select()}
          className="min-w-0 flex-1 rounded-md border border-border bg-background px-2 py-1 font-mono text-xs"
        />
        <Button onClick={() => void copy()}>
          {copied ? <Check className="size-4" /> : <Copy className="size-4" />}
          {copied ? "Copied" : "Copy"}
        </Button>
      </div>
    </div>
  );
}
