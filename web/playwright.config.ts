import { defineConfig, devices } from "@playwright/test";
import {
  ACCESS_TOKEN,
  API_PORT,
  SERVER_API_PORT,
  SERVER_LOG,
  SERVER_WEB_PORT,
  SERVER_WEB_URL,
  WEB_PORT,
  WEB_URL,
} from "./e2e/env";

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

// Server mode (design D13): accounts and passwords, over plain HTTP on this machine only.
const serverMode = [
  "mkdir -p test-results",
  `&& exec ../target/debug/open-prism-api --data-dir test-results/server-data`,
  `--mode server --public-url ${SERVER_WEB_URL} --insecure-http serve`,
  `--web .next/standalone --port ${SERVER_API_PORT} --web-port ${SERVER_WEB_PORT}`,
  `> ${SERVER_LOG} 2>&1`,
].join(" ");

const env = {
  ...process.env,
  OPENPRISM_ACCESS_TOKEN: ACCESS_TOKEN,
} as Record<string, string>;

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
  projects: [
    {
      name: "chromium",
      testIgnore: "**/server-mode.spec.ts",
      use: { ...devices["Desktop Chrome"] },
    },
    {
      name: "server-mode",
      testMatch: "**/server-mode.spec.ts",
      use: { ...devices["Desktop Chrome"], baseURL: SERVER_WEB_URL },
    },
  ],
  webServer: [
    {
      command: server,
      url: `${WEB_URL}/login`,
      timeout: 60_000,
      // Never test against an app that is already running with someone's data.
      reuseExistingServer: false,
      // SIGTERM lets the API stop the web app, which runs in its own process group.
      gracefulShutdown: { signal: "SIGTERM", timeout: 10_000 },
      env,
    },
    {
      command: serverMode,
      url: `${SERVER_WEB_URL}/login`,
      timeout: 60_000,
      reuseExistingServer: false,
      gracefulShutdown: { signal: "SIGTERM", timeout: 10_000 },
      env,
    },
  ],
});
