/** LaTeX written by the AI from an image pasted or dropped into the editor. */
import { postEventStream } from "./sse";
import type { ImageLatexEvent } from "./types/ImageLatexEvent";
import type { ImageLatexRequest } from "./types/ImageLatexRequest";

export type ImageLatexResult = Omit<
  Extract<ImageLatexEvent, { type: "done" }>,
  "type"
>;

/**
 * Asks for the LaTeX of an image: `onText` gets the answer so far while the model writes it,
 * and the promise resolves with the cleaned result. A failure, before or while it is written,
 * rejects the promise.
 */
export async function streamImageLatex(
  request: ImageLatexRequest,
  signal: AbortSignal,
  onText: (text: string) => void,
): Promise<ImageLatexResult> {
  const outcome: { result?: ImageLatexResult; error?: string } = {};
  await postEventStream<ImageLatexEvent>(
    "/api/ai/image-to-latex",
    request,
    (event) => {
      if (event.type === "text") onText(event.text);
      else if (event.type === "done") {
        outcome.result = {
          latex: event.latex,
          packages: event.packages,
          citationsRemoved: event.citationsRemoved,
        };
      } else outcome.error = event.message;
    },
    signal,
  );
  if (outcome.error !== undefined) throw new Error(outcome.error);
  if (!outcome.result) {
    throw new Error("The answer stopped before it was complete");
  }
  return outcome.result;
}
