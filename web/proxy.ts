/**
 * Optimistic sign-in check for pages: without a session cookie, go to /login.
 * The Rust API is the real authority and validates the cookie on every request.
 *
 * `/api` is deliberately excluded: Proxy buffers request bodies and truncates them at 10 MB.
 */
import { type NextRequest, NextResponse } from "next/server";
import { SESSION_COOKIE } from "@/lib/api/config";

export function proxy(request: NextRequest) {
  if (request.cookies.has(SESSION_COOKIE)) return NextResponse.next();

  const login = new URL("/login", request.url);
  const target = request.nextUrl.pathname + request.nextUrl.search;
  if (target !== "/") login.searchParams.set("next", target);
  return NextResponse.redirect(login);
}

export const config = {
  matcher: ["/((?!api|_next/static|_next/image|favicon.ico|login).*)"],
};
