import path from "node:path";
import { expect, type Locator, type Page, test } from "@playwright/test";
import { ACCESS_TOKEN } from "./env";

const PAPER_ZIP = path.join(__dirname, "fixtures", "paper.zip");

type Rgb = [number, number, number];

/** The colour most pixels of the text of `line` have on screen: its background. */
async function lineBackground(page: Page, line: Locator): Promise<Rgb> {
  const box = await line.evaluate((element) => {
    const range = document.createRange();
    range.selectNodeContents(element);
    const rect = range.getBoundingClientRect();
    return {
      x: rect.x + 1,
      y: rect.y + 1,
      width: rect.width - 2,
      height: rect.height - 2,
    };
  });
  const png = await page.screenshot({ clip: box });
  return page.evaluate(async (base64) => {
    const bytes = Uint8Array.from(atob(base64), (char) => char.charCodeAt(0));
    const image = await createImageBitmap(
      new Blob([bytes], { type: "image/png" }),
    );
    const canvas = new OffscreenCanvas(image.width, image.height);
    const context = canvas.getContext("2d");
    if (!context) throw new Error("no 2D canvas");
    context.drawImage(image, 0, 0);
    const { data } = context.getImageData(0, 0, image.width, image.height);
    const counts = new Map<string, number>();
    for (let i = 0; i < data.length; i += 4) {
      const key = `${data[i]},${data[i + 1]},${data[i + 2]}`;
      counts.set(key, (counts.get(key) ?? 0) + 1);
    }
    const [mostCommon] = [...counts].sort((a, b) => b[1] - a[1])[0];
    return mostCommon.split(",").map(Number) as Rgb;
  }, png.toString("base64"));
}

function distance(a: Rgb, b: Rgb): number {
  return Math.hypot(a[0] - b[0], a[1] - b[1], a[2] - b[2]);
}

// The selection is drawn under the text. On the line with the cursor, the highlight of the
// active line used to cover it, so text selected there looked unselected.
test("selected text shows on the line with the cursor", async ({ page }) => {
  await page.goto(`/login?token=${ACCESS_TOKEN}`);
  await expect(page.getByRole("heading", { name: "Open Prism" })).toBeVisible();
  await page
    .locator('input[type="file"][accept*="zip"]')
    .setInputFiles(PAPER_ZIP);
  await page.waitForURL(/\/project\/[^/]+$/);
  await expect(page.getByText("main: main.tex")).toBeVisible();
  const projectUrl = page.url();

  for (const colorScheme of ["light", "dark"] as const) {
    await test.step(`${colorScheme} colours`, async () => {
      await page.emulateMedia({ colorScheme });
      await page.goto(projectUrl);
      const above = page.locator(".cm-line", { hasText: "\\begin{document}" });
      const cursorLine = page.locator(".cm-line", { hasText: "\\maketitle" });

      // From the start of `\begin{document}` to the end of `\maketitle`, the next line.
      await above.click();
      await page.keyboard.press("Home");
      await page.keyboard.press("Shift+ArrowDown");
      await page.keyboard.press("Shift+End");
      await expect(cursorLine).toHaveClass(/cm-activeLine/);
      await expect(above).not.toHaveClass(/cm-activeLine/);

      const unselected = await lineBackground(
        page,
        page.locator(".cm-line").first(),
      );
      await expect
        .poll(async () =>
          distance(await lineBackground(page, above), unselected),
        )
        .toBeGreaterThan(30);
      const selected = await lineBackground(page, above);
      await expect
        .poll(async () =>
          distance(await lineBackground(page, cursorLine), selected),
        )
        .toBeLessThan(30);
    });
  }
});
