/** Ports and the access token of the end-to-end run, shared by the config and the tests. */

function port(name: string, fallback: number): number {
  const value = Number(process.env[name]);
  return Number.isInteger(value) && value > 0 ? value : fallback;
}

/** The web app started by `serve --web`. */
export const WEB_PORT = port("E2E_WEB_PORT", 3798);
/** The API started by `serve --web`. */
export const API_PORT = port("E2E_API_PORT", 8798);
/** The fake OpenAI-compatible model the tests start (`fake-model.ts`). */
export const MODEL_PORT = port("E2E_MODEL_PORT", 8797);

export const WEB_URL = `http://127.0.0.1:${WEB_PORT}`;
export const API_URL = `http://127.0.0.1:${API_PORT}/api`;

/** Fixed so the tests can sign in and call the API; only ever used for this throwaway run. */
export const ACCESS_TOKEN = "e2e-access-token-0123456789abcdef0123456789";

/** Server mode (`server-mode.spec.ts`): a second app with accounts, its own ports and data dir. */
export const SERVER_WEB_PORT = port("E2E_SERVER_WEB_PORT", 3799);
export const SERVER_API_PORT = port("E2E_SERVER_API_PORT", 8799);
export const SERVER_WEB_URL = `http://127.0.0.1:${SERVER_WEB_PORT}`;
export const SERVER_API_URL = `http://127.0.0.1:${SERVER_API_PORT}/api`;
/** Its log, relative to `web/`: it prints the link that creates the first admin. */
export const SERVER_LOG = "test-results/server-mode.log";
