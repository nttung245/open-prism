/**
 * Forwards every `/api/*` request to the Rust API (`open-prism-api`) and streams the
 * response back untouched.
 *
 * Why a route handler and not `rewrites`:
 * - `next.config` is frozen into the standalone `server.js` at build time, but the API
 *   address must be configurable at run time (`OPENPRISM_API_URL`).
 * - `rewrites` buffer request bodies and silently truncate them at 10 MB, which would
 *   corrupt project ZIP uploads. This handler streams bodies in both directions.
 * - We need control over headers (cookies, compression) and cancellation: when the browser
 *   disconnects, `request.signal` aborts the upstream request.
 *
 * Do not add `/api` to the `proxy.ts` matcher: Proxy buffers bodies with the same 10 MB cap.
 */
import type { NextRequest } from "next/server";
import { apiBaseUrl } from "@/lib/api/config";

// Hop-by-hop headers (RFC 9110 §7.6.1) and headers that `fetch` must compute itself.
const REQUEST_HEADERS_TO_DROP = new Set([
  "connection",
  "keep-alive",
  "proxy-authenticate",
  "proxy-authorization",
  "te",
  "trailer",
  "transfer-encoding",
  "upgrade",
  "host",
  "content-length",
  "accept-encoding",
  // Node's fetch (undici) rejects `Expect: 100-continue`, which curl sends for large bodies.
  "expect",
]);

const RESPONSE_HEADERS_TO_DROP = new Set([
  "connection",
  "keep-alive",
  "proxy-authenticate",
  "proxy-authorization",
  "te",
  "trailer",
  "transfer-encoding",
  "upgrade",
]);

type StreamingRequestInit = RequestInit & { duplex?: "half" };

async function proxy(request: NextRequest): Promise<Response> {
  const apiUrl = apiBaseUrl();
  const target = new URL(
    request.nextUrl.pathname + request.nextUrl.search,
    apiUrl,
  );

  const headers = new Headers();
  request.headers.forEach((value, key) => {
    if (!REQUEST_HEADERS_TO_DROP.has(key)) headers.append(key, value);
  });
  // Ask for an uncompressed body so it can be streamed through byte for byte.
  headers.set("accept-encoding", "identity");

  const hasBody = request.method !== "GET" && request.method !== "HEAD";
  const init: StreamingRequestInit = {
    method: request.method,
    headers,
    body: hasBody ? request.body : undefined,
    duplex: hasBody ? "half" : undefined,
    redirect: "manual",
    signal: request.signal,
    cache: "no-store",
  };

  let upstream: Response;
  try {
    upstream = await fetch(target, init);
  } catch (error) {
    if (request.signal.aborted) return new Response(null, { status: 499 });
    return Response.json(
      {
        error: {
          code: "api_unavailable",
          message: `Cannot reach the Open Prism API at ${apiUrl}: ${String(error)}`,
        },
      },
      { status: 502 },
    );
  }

  const responseHeaders = new Headers();
  upstream.headers.forEach((value, key) => {
    if (!RESPONSE_HEADERS_TO_DROP.has(key)) responseHeaders.append(key, value);
  });

  return new Response(upstream.body, {
    status: upstream.status,
    statusText: upstream.statusText,
    headers: responseHeaders,
  });
}

export const GET = proxy;
export const HEAD = proxy;
export const POST = proxy;
export const PUT = proxy;
export const PATCH = proxy;
export const DELETE = proxy;
