import type { ReasoningLevel } from "@/lib/api/types/ReasoningLevel";

/** Choices for the reasoning ("thinking") effort; "" keeps the provider's default. */
export const REASONING_OPTIONS: Array<{
  value: ReasoningLevel | "";
  label: string;
}> = [
  { value: "", label: "Default" },
  { value: "off", label: "Off" },
  { value: "low", label: "Low" },
  { value: "medium", label: "Medium" },
  { value: "high", label: "High" },
  { value: "max", label: "Max" },
];

export function parseReasoning(value: string): ReasoningLevel | undefined {
  return REASONING_OPTIONS.some((o) => o.value === value && value !== "")
    ? (value as ReasoningLevel)
    : undefined;
}
