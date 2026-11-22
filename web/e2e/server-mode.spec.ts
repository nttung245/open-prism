/**
 * Server mode (design D13): two people on one Open Prism, each in their own browser. The first
 * admin comes from the link the server prints when it starts; she adds a second person, who sets
 * a password from his own link; they share one project, and only that one.
 */
import { readFile } from "node:fs/promises";
import path from "node:path";
import {
  type APIRequestContext,
  type BrowserContext,
  expect,
  type Page,
  request,
  test,
} from "@playwright/test";
import { ACCESS_TOKEN, SERVER_API_URL, SERVER_LOG } from "./env";

const PAPER_ZIP = path.join(__dirname, "fixtures", "paper.zip");
const TYPED = "Minh typed this sentence on the shared server.";
const LAN_PASSWORD = "lan's long test password";
const MINH_PASSWORD = "minh's long test password";

let api: APIRequestContext;

test.beforeAll(async () => {
  // The access token still works in server mode, for scripts on the machine: the test reads
  // compile results with it.
  api = await request.newContext({
    baseURL: `${SERVER_API_URL}/`,
    extraHTTPHeaders: { authorization: `Bearer ${ACCESS_TOKEN}` },
  });
});

test.afterAll(async () => {
  await api?.dispose();
});

/** The link the server printed to create the first admin. */
async function firstAdminLink(): Promise<string> {
  let link = "";
  await expect
    .poll(
      async () => {
        const log = await readFile(SERVER_LOG, "utf8").catch(() => "");
        // The URL runs up to the end of the hex token, colour codes or not.
        link = log.match(/(http:\/\/\S+\/setup\/[0-9a-f]+)/)?.[1] ?? "";
        return link;
      },
      { message: `no setup link in ${SERVER_LOG}` },
    )
    .not.toBe("");
  return link;
}

/** Fails the test on anything the app logs as an error, as `main-flow.spec.ts` does. */
function watchErrors(page: Page, who: string, errors: string[]) {
  page.on("pageerror", (error) =>
    errors.push(`${who} pageerror: ${error.message}`),
  );
  page.on("console", (message) => {
    if (
      message.type() === "error" &&
      !message.text().startsWith("Failed to load resource")
    ) {
      errors.push(`${who} console: ${message.text()}`);
    }
  });
}

test("two people share a project, and only that one", async ({ browser }) => {
  const errors: string[] = [];
  const contexts: BrowserContext[] = [];
  const open = async (who: string) => {
    const context = await browser.newContext();
    contexts.push(context);
    const page = await context.newPage();
    watchErrors(page, who, errors);
    return page;
  };
  const lan = await open("lan");
  const minh = await open("minh");

  await test.step("the first admin creates her account from the link", async () => {
    await lan.goto(await firstAdminLink());
    await expect(
      lan.getByRole("heading", { name: "Create the first admin" }),
    ).toBeVisible();
    await lan.getByLabel("User name").fill("lan");
    await lan.getByLabel("Your name").fill("Lan Tran");
    await lan.getByLabel("Password", { exact: true }).fill(LAN_PASSWORD);
    await lan.getByLabel("Password again").fill(LAN_PASSWORD);
    await lan.getByRole("button", { name: "Create the account" }).click();
    await expect(
      lan.getByRole("heading", { name: "Open Prism" }),
    ).toBeVisible();
    await expect(lan.getByRole("button", { name: "Lan Tran" })).toBeVisible();
  });

  let minhLink = "";
  await test.step("she adds Minh and gets his link", async () => {
    await lan.getByRole("button", { name: "Lan Tran" }).click();
    await lan.getByRole("menuitem", { name: "Manage the server" }).click();
    await expect(lan.getByRole("heading", { name: "People" })).toBeVisible();
    const form = lan.locator("form", { hasText: "Add a person" });
    await form.getByLabel("User name").fill("minh");
    await form.getByLabel("Name", { exact: true }).fill("Minh Le");
    await form.getByRole("button", { name: "Add", exact: true }).click();
    minhLink = await form.getByLabel("One-time link").inputValue();
    expect(minhLink).toMatch(/\/setup\/[0-9a-f]+$/);
  });

  await test.step("Minh sets his password in his own browser", async () => {
    await minh.goto(minhLink);
    await expect(
      minh.getByRole("heading", { name: "Set your password" }),
    ).toBeVisible();
    await minh.getByLabel("Password", { exact: true }).fill(MINH_PASSWORD);
    await minh.getByLabel("Password again").fill(MINH_PASSWORD);
    await minh.getByRole("button", { name: "Set the password" }).click();
    await expect(minh.getByRole("button", { name: "Minh Le" })).toBeVisible();
    await expect(minh.getByText("No projects yet")).toBeVisible();
  });

  let projectId = "";
  let projectName = "";
  await test.step("Lan makes two projects and adds Minh to one", async () => {
    await lan.goto("/");
    await lan.getByLabel("New project name").fill("Private notes");
    await lan.getByRole("button", { name: "Create" }).click();
    await lan.waitForURL(/\/project\/[^/]+$/);
    await lan.getByRole("link", { name: "All projects" }).click();

    await lan
      .locator('input[type="file"][accept*="zip"]')
      .setInputFiles(PAPER_ZIP);
    await lan.waitForURL(/\/project\/[^/]+$/);
    projectId = new URL(lan.url()).pathname.split("/").at(-1) ?? "";
    projectName = (
      await lan.getByRole("heading", { level: 1 }).innerText()
    ).trim();

    await lan.getByRole("button", { name: "Members" }).click();
    const dialog = lan.getByRole("dialog", { name: /Members of/ });
    await dialog.getByLabel("Add someone by user name").fill("minh");
    await dialog.getByRole("button", { name: "Add", exact: true }).click();
    await expect(dialog.getByText("Minh Le")).toBeVisible();
    await dialog.getByRole("button", { name: "Close" }).click();
  });

  await test.step("Minh's list shows the shared project at once, and only it", async () => {
    // His page was open all along: the list follows the change without a reload.
    await expect(minh.getByRole("link", { name: projectName })).toBeVisible();
    await expect(minh.getByText("Private notes")).toHaveCount(0);
    const other = await minh.request.get("/api/projects");
    const names = ((await other.json()) as { name: string }[]).map(
      (project) => project.name,
    );
    expect(names).toEqual([projectName]);
  });

  await test.step("Minh edits and compiles it", async () => {
    await minh.getByRole("link", { name: projectName }).click();
    await expect(minh.getByText("main: main.tex")).toBeVisible();
    await minh.locator(".cm-line", { hasText: "\\maketitle" }).click();
    await minh.keyboard.press("End");
    await minh.keyboard.press("Enter");
    await minh.keyboard.type(TYPED);
    await expect
      .poll(async () =>
        (await api.get(`projects/${projectId}/files/main.tex`)).text(),
      )
      .toContain(TYPED);

    const since = Date.now();
    await minh.keyboard.press("Control+Enter");
    await expect
      .poll(
        async () => {
          const run = (await (
            await api.get(`projects/${projectId}/compile/latest`)
          ).json()) as {
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
    await expect(minh.getByLabel("Page 1")).toBeVisible();
  });

  await test.step("the history names who did what", async () => {
    await lan.goto(`/project/${projectId}`);
    await lan.getByRole("button", { name: "Show the history" }).click();
    await expect(
      lan.getByRole("button", { name: /Compiled successfully.*Minh Le/ }),
    ).toBeVisible();
    await expect(
      lan.getByRole("button", { name: /Created.*Lan Tran/ }),
    ).toBeVisible();
  });

  for (const context of contexts) await context.close();
  expect(errors).toEqual([]);
});
