import { defineConfig, devices } from "@playwright/test";
import { ACCESS_TOKEN, API_PORT, WEB_PORT, WEB_URL } from "./e2e/env";

// End-to-end tests run against the real build: `open-prism-api serve --web` with the debug API
// (`cargo build -p openprism-server`) and the standalone web app (`just build-web`).
// Playwright empties `test-results/` before starting the server, so every run gets a new data
// dir there; it and the server log stay after a failed run to look at.
const server = [
  "mkdir -p test-results",
  `&& exec ../target/debug/open-prism-api --data-dir test-results/data serve`,
  `--web .next/standalone --port ${API_PORT} --web-port ${WEB_PORT}`,
  "> test-results/server.log 2>&1",
].join(" ");

export default defineConfig({
  testDir: "e2e",
  // `*.test.ts` files there are Vitest unit tests of the test helpers.
  testMatch: "**/*.spec.ts",
  // One app with one data dir: the tests run one after the other.
  fullyParallel: false,
  workers: 1,
  retries: 0,
  forbidOnly: Boolean(process.env.CI),
  timeout: 120_000,
  expect: { timeout: 15_000 },
  reporter: [["list"], ["html", { open: "never" }]],
  use: {
    baseURL: WEB_URL,
    trace: "retain-on-failure",
    screenshot: "only-on-failure",
  },
  projects: [{ name: "chromium", use: { ...devices["Desktop Chrome"] } }],
  webServer: {
    command: server,
    url: `${WEB_URL}/login`,
    timeout: 60_000,
    // Never test against an app that is already running with someone's data.
    reuseExistingServer: false,
    // SIGTERM lets the API stop the web app, which runs in its own process group.
    gracefulShutdown: { signal: "SIGTERM", timeout: 10_000 },
    env: { ...process.env, OPENPRISM_ACCESS_TOKEN: ACCESS_TOKEN } as Record<
      string,
      string
    >,
  },
});
