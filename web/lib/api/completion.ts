/** Suggestions to continue the text at the editor cursor ("ghost text"). */
import type { SuggestionRequest } from "@/lib/editor/ghost-text";
import { postEventStream } from "./sse";
import type { CompletionEvent } from "./types/CompletionEvent";

/**
 * Asks for a suggestion: `onText` gets the suggestion so far while the model writes it, and
 * the promise resolves with the whole suggestion. A failure, before or while it is written,
 * rejects the promise.
 */
export async function streamSuggestion(
  projectId: string,
  request: SuggestionRequest,
  signal: AbortSignal,
  onText: (text: string) => void,
): Promise<string> {
  const result: { text?: string; error?: string } = {};
  await postEventStream<CompletionEvent>(
    "/api/ai/complete",
    { projectId, ...request },
    (event) => {
      if (event.type === "text") onText(event.text);
      else if (event.type === "done") result.text = event.text;
      else result.error = event.message;
    },
    signal,
  );
  if (result.error !== undefined) throw new Error(result.error);
  if (result.text === undefined) {
    throw new Error("The suggestion stopped before it was complete");
  }
  return result.text;
}
