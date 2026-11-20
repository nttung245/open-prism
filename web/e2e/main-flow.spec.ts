import { readFile } from "node:fs/promises";
import path from "node:path";
import {
  type APIRequestContext,
  expect,
  type Page,
  request,
  test,
} from "@playwright/test";
import { ACCESS_TOKEN, API_URL, MODEL_PORT } from "./env";
import {
  CLOSING,
  type FakeModel,
  FIX,
  MODEL,
  startFakeModel,
  TYPO,
  toolResult,
} from "./fake-model";

const PAPER_ZIP = path.join(__dirname, "fixtures", "paper.zip");
const TYPED = "This sentence was typed by the end-to-end test.";

let model: FakeModel;
let api: APIRequestContext;

test.beforeAll(async () => {
  model = await startFakeModel(MODEL_PORT);
  api = await request.newContext({
    baseURL: `${API_URL}/`,
    extraHTTPHeaders: { authorization: `Bearer ${ACCESS_TOKEN}` },
  });
});

test.afterAll(async () => {
  await api?.dispose();
  await model?.close();
});

async function fileText(projectId: string, path: string): Promise<string> {
  const response = await api.get(`projects/${projectId}/files/${path}`);
  expect(response.ok()).toBe(true);
  return response.text();
}

/**
 * Waits for `text` in the assistant. While a turn ends, the live view and the saved messages can
 * both show it for a moment, so the first match is enough.
 */
async function assistantShows(page: Page, text: string, timeout = 15_000) {
  await expect(page.getByText(text).first()).toBeVisible({ timeout });
}

/**
 * Starts a compilation with `trigger` and waits for one that started since to succeed with a
 * PDF. That may be a later run than the one `trigger` started: compiling after every save is
 * on, and a newer run cancels the one in progress.
 */
async function compile(
  page: Page,
  projectId: string,
  trigger: () => Promise<void>,
) {
  const since = Date.now();
  const started = page.waitForResponse(
    (response) =>
      response.request().method() === "POST" &&
      new URL(response.url()).pathname === `/api/projects/${projectId}/compile`,
  );
  await trigger();
  expect((await started).status()).toBe(202);
  await expect
    .poll(
      async () => {
        const response = await api.get(`projects/${projectId}/compile/latest`);
        const run = (await response.json()) as {
          status: string;
          hasPdf: boolean;
          startedAt: number;
        } | null;
        if (!run || run.startedAt < since) return "waiting";
        return `${run.status} pdf=${run.hasPdf}`;
      },
      { timeout: 90_000 },
    )
    .toBe("success pdf=true");
  await expect(page.getByText("No problems reported.")).toBeVisible();
  await expect(page.getByLabel("Page 1")).toBeVisible();
}

test("import, edit, compile, and accept an edit from the assistant", async ({
  page,
}) => {
  // Anything the app logs as an error fails the test. Chromium also logs every failed
  // request as "Failed to load resource"; the responses that matter are checked below.
  const errors: string[] = [];
  page.on("pageerror", (error) => errors.push(`pageerror: ${error.message}`));
  page.on("console", (message) => {
    if (
      message.type() === "error" &&
      !message.text().startsWith("Failed to load resource")
    ) {
      errors.push(`console: ${message.text()}`);
    }
  });

  await test.step("sign in with the link", async () => {
    await page.goto(`/login?token=${ACCESS_TOKEN}`);
    await expect(
      page.getByRole("heading", { name: "Open Prism" }),
    ).toBeVisible();
  });

  await test.step("add a provider and choose the agent model", async () => {
    // Start from no providers, so the test can run again against the same app.
    const existing = (await (await api.get("settings/providers")).json()) as {
      id: string;
    }[];
    for (const provider of existing) {
      expect((await api.delete(`settings/providers/${provider.id}`)).ok()).toBe(
        true,
      );
    }
    await page.getByRole("link", { name: "Settings" }).click();
    await expect(page.getByRole("heading", { name: "Settings" })).toBeVisible();
    await page
      .getByLabel("Type")
      .selectOption({ label: "OpenAI-compatible (vLLM, LM Studio, …)" });
    await page.getByLabel("Name", { exact: true }).fill("Fake model");
    await page
      .getByLabel("Base URL (required)", { exact: true })
      .fill(model.url);
    await page.getByRole("button", { name: "Add provider" }).click();

    await page.getByRole("button", { name: "Test connection" }).click();
    await expect(
      page.getByText("Connected. 1 models available."),
    ).toBeVisible();

    await page
      .getByLabel("Provider")
      .first()
      .selectOption({ label: "Fake model" });
    await page
      .getByRole("combobox", { name: "Model", exact: true })
      .first()
      .fill(MODEL);
    await page.getByRole("button", { name: "Save models" }).click();
    await expect(page.getByText("Saved.")).toBeVisible();
  });

  let projectId = "";
  await test.step("import a ZIP", async () => {
    await page.getByRole("link", { name: "All projects" }).click();
    await page.locator('input[type="file"]').setInputFiles(PAPER_ZIP);
    await page.waitForURL(/\/project\/[^/]+$/);
    projectId = new URL(page.url()).pathname.split("/").at(-1) ?? "";
    await expect(page.getByText("main: main.tex")).toBeVisible();
    await expect(
      page.getByRole("button", { name: "sections", exact: false }),
    ).toBeVisible();
  });

  await test.step("edit main.tex and wait for autosave", async () => {
    await page.locator(".cm-line", { hasText: "\\maketitle" }).click();
    await page.keyboard.press("End");
    await page.keyboard.press("Enter");
    await page.keyboard.type(TYPED);
    await expect.poll(() => fileText(projectId, "main.tex")).toContain(TYPED);
  });

  await test.step("compile with Ctrl+Enter", async () => {
    await compile(page, projectId, () => page.keyboard.press("Control+Enter"));
  });

  await test.step("accept the edit the assistant proposes", async () => {
    await page.getByRole("button", { name: "Show the assistant" }).click();
    const message = page.getByRole("textbox", {
      name: /Ask or describe a change/,
    });
    await message.click();
    await page.keyboard.type("Please fix the typos in my paper.");
    await page.keyboard.press("Enter");

    await assistantShows(page, "Read main.tex");
    await assistantShows(page, "Read sections/intro.tex");
    await expect(page.getByLabel("Apply change 1")).toBeChecked();
    await page.getByRole("button", { name: "Apply selected" }).click();

    await assistantShows(page, CLOSING, 30_000);
    await expect
      .poll(() => fileText(projectId, "sections/intro.tex"))
      .toContain(FIX);
    expect(await fileText(projectId, "sections/intro.tex")).not.toContain(TYPO);
    expect(await fileText(projectId, "main.tex")).toContain(TYPED);

    // The agent read main.tex as saved, with the sentence typed a moment before.
    const results = model.requests.flatMap((chat) =>
      chat.messages.map(toolResult),
    );
    expect(results.some((text) => text.includes(TYPED))).toBe(true);
  });

  await test.step("compile again", async () => {
    await compile(page, projectId, () =>
      page.getByRole("button", { name: "Compile", exact: true }).click(),
    );
  });

  await test.step("download the project as 7z", async () => {
    await page.getByRole("button", { name: "Download project" }).click();
    const downloading = page.waitForEvent("download");
    await page.getByRole("menuitem", { name: "7z" }).click();
    const download = await downloading;
    expect(download.suggestedFilename()).toBe("paper.7z");
    const bytes = await readFile(await download.path());
    expect([...bytes.subarray(0, 6)]).toEqual([
      0x37, 0x7a, 0xbc, 0xaf, 0x27, 0x1c,
    ]);
  });

  await test.step("download the page being read", async () => {
    const form = page.getByRole("form", { name: "Download pages" });
    await page.getByRole("button", { name: "Download pages…" }).click();
    await expect(form.getByLabel("Pages")).toHaveValue("1");
    const downloading = page.waitForEvent("download");
    await form.getByRole("button", { name: "Download", exact: true }).click();
    const download = await downloading;
    expect(download.suggestedFilename()).toBe("paper (pages 1).pdf");
    const bytes = await readFile(await download.path());
    expect(bytes.subarray(0, 5).toString()).toBe("%PDF-");

    // A bad list shows the server's message and downloads nothing.
    await page.getByRole("button", { name: "Download pages…" }).click();
    await form.getByLabel("Pages").fill("3-x");
    await form.getByRole("button", { name: "Download", exact: true }).click();
    await expect(form.getByRole("alert")).toContainText("3-x");
    await page.keyboard.press("Escape");
  });

  expect(errors).toEqual([]);
});
