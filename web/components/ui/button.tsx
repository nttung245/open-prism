import type { ButtonHTMLAttributes } from "react";

type Variant = "primary" | "secondary" | "ghost" | "danger";

const VARIANTS: Record<Variant, string> = {
  primary:
    "bg-accent text-white hover:opacity-90 dark:text-zinc-950 disabled:opacity-50",
  secondary:
    "border border-border bg-background hover:bg-panel disabled:opacity-50",
  ghost: "hover:bg-zinc-200/60 dark:hover:bg-zinc-800/60 disabled:opacity-40",
  danger: "text-danger hover:bg-red-50 dark:hover:bg-red-950/40",
};

export function Button({
  variant = "secondary",
  className = "",
  type = "button",
  ...props
}: ButtonHTMLAttributes<HTMLButtonElement> & { variant?: Variant }) {
  return (
    <button
      type={type}
      className={`inline-flex items-center justify-center gap-1.5 rounded-md px-2.5 py-1.5 text-sm font-medium transition-colors focus-visible:outline-2 focus-visible:outline-accent disabled:cursor-not-allowed ${VARIANTS[variant]} ${className}`}
      {...props}
    />
  );
}

/** A square button holding one icon; `label` is announced and shown as a tooltip. */
export function IconButton({
  label,
  className = "",
  type = "button",
  ...props
}: ButtonHTMLAttributes<HTMLButtonElement> & { label: string }) {
  return (
    <button
      type={type}
      aria-label={label}
      title={label}
      className={`inline-flex size-7 items-center justify-center rounded-md text-muted transition-colors hover:bg-zinc-200/60 hover:text-foreground focus-visible:outline-2 focus-visible:outline-accent disabled:opacity-40 dark:hover:bg-zinc-800/60 ${className}`}
      {...props}
    />
  );
}
