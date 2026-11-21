import path from "node:path";
import {
  type APIRequestContext,
  expect,
  request,
  test,
} from "@playwright/test";
import type { JobView } from "@/lib/api/types/JobView";
import { ACCESS_TOKEN, API_URL, MODEL_PORT } from "./env";
import {
  type FakeModel,
  MODEL,
  PAGE_PROMPT_START,
  PREAMBLE_PROMPT_START,
  startFakeModel,
} from "./fake-model";

const PDF = path.join(__dirname, "fixtures", "two-pages.pdf");
/** Text of the PDF's pages, which their text layers give the fake model to copy. */
const PAGE_ONE = "Open Prism turns this page into LaTeX.";
const PAGE_TWO = "The second page of the test document ends here.";

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

test("import a PDF as a new project", async ({ page }) => {
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

  await test.step("without an Agent model, the dialog links to a working Settings page", async () => {
    expect(
      (await api.put("settings/roles", { data: { agent: null } })).ok(),
    ).toBe(true);
    await page.getByRole("button", { name: "Import PDF" }).click();
    const dialog = page.getByRole("dialog", { name: "Import PDF" });
    await dialog.getByLabel("PDF file").setInputFiles(PDF);
    await dialog.getByRole("button", { name: "Import", exact: true }).click();
    await expect(dialog.getByRole("alert")).toContainText(
      "choose a model for Agent in Settings first",
    );
    await dialog.getByRole("link", { name: "Open Settings" }).click();
    await expect(page.getByRole("heading", { name: "Settings" })).toBeVisible();
    // The dialog closed on the way: a modal one left open on the hidden page would block the
    // whole Settings page.
    const name = page.getByLabel("Name", { exact: true });
    await name.fill("Typed on the Settings page");
    await expect(name).toHaveValue("Typed on the Settings page");
    await page.getByRole("link", { name: "All projects" }).click();
    await expect(page.getByRole("dialog", { name: "Import PDF" })).toBeHidden();
  });

  await test.step("choose the fake model for Agent", async () => {
    const providers = (await (await api.get("settings/providers")).json()) as {
      id: string;
      name: string;
    }[];
    let providerId = providers.find(
      (provider) => provider.name === "Fake model",
    )?.id;
    if (!providerId) {
      const created = await api.post("settings/providers", {
        data: {
          name: "Fake model",
          kind: "openai-compatible",
          baseUrl: model.url,
        },
      });
      expect(created.status()).toBe(201);
      providerId = ((await created.json()) as { id: string }).id;
    }
    const roles = await api.put("settings/roles", {
      data: { agent: { providerId, model: MODEL } },
    });
    expect(roles.ok()).toBe(true);
  });

  const row = page
    .getByRole("region", { name: "Background jobs" })
    .getByRole("listitem")
    .filter({ hasText: "Import two-pages.pdf" })
    .first();
  let job: JobView | undefined;
  await test.step("import the PDF from the project list", async () => {
    await page.getByRole("button", { name: "Import PDF" }).click();
    const dialog = page.getByRole("dialog", { name: "Import PDF" });
    await dialog.getByLabel("PDF file").setInputFiles(PDF);
    await expect(dialog.getByLabel("Project name")).toHaveAttribute(
      "placeholder",
      "two-pages",
    );
    await dialog.getByRole("button", { name: "Import", exact: true }).click();
    await expect(dialog).toBeHidden();

    // The row shows up at once, then follows the job without reloading the page.
    await expect(row).toBeVisible();
    await expect(row.getByText("Done")).toBeVisible({ timeout: 90_000 });
    const jobs = (await (await api.get("jobs")).json()) as JobView[];
    job = jobs.find((one) => one.title === "Import two-pages.pdf");
    expect(job).toMatchObject({
      kind: "pdfImport",
      state: "succeeded",
      stepsDone: 2,
      stepsTotal: 2,
    });
    // The new project is in the list.
    await expect(
      page.getByRole("link", { name: /^two-pages main\.tex/ }),
    ).toBeVisible();
  });

  await test.step("open the project the job made", async () => {
    const projectId = job?.projectId ?? "";
    await row.getByRole("link", { name: "Open project" }).click();
    await page.waitForURL(`**/project/${projectId}`);
    await expect(page.getByText("main: main.tex")).toBeVisible();
    await expect(page.locator(".cm-content")).toContainText(PAGE_ONE);
    await expect(page.getByRole("button", { name: "p1-1.png" })).toBeVisible();
    await expect(page.getByLabel("Page 1")).toBeVisible();

    const main = await (
      await api.get(`projects/${projectId}/files/main.tex`)
    ).text();
    expect(main).toContain(PAGE_ONE);
    expect(main).toContain(PAGE_TWO);
    expect(main).toContain(
      "\\includegraphics[width=0.4\\linewidth]{figures/p1-1.png}",
    );
    const figure = await api.get(
      `projects/${projectId}/files/figures/p1-1.png`,
    );
    expect(figure.ok()).toBe(true);
    expect([...(await figure.body()).subarray(0, 4)]).toEqual([
      0x89, 0x50, 0x4e, 0x47,
    ]);
    const latest = (await (
      await api.get(`projects/${projectId}/compile/latest`)
    ).json()) as { status: string; hasPdf: boolean };
    expect(latest).toMatchObject({ status: "success", hasPdf: true });
  });

  // One request for the preamble and one per page, each with the page image.
  const sent = model.requests.map((chat) => JSON.stringify(chat.messages));
  expect(
    sent.filter((text) => text.includes(PREAMBLE_PROMPT_START)),
  ).toHaveLength(1);
  const pages = sent.filter((text) => text.includes(PAGE_PROMPT_START));
  expect(pages).toHaveLength(2);
  expect(pages.every((text) => text.includes("image_url"))).toBe(true);

  expect(errors).toEqual([]);
});
