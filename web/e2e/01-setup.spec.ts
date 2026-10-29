import { expect, test } from "@playwright/test";
import { installSigner } from "./signer";

// Runs the setup wizard on a fresh stack (ALLOD_DEV_AUTO_SETUP=false), then waits for
// the managed ldk-server to receive the phrase, start, and be reachable by Allod.
test("owner sets up a new node through the wizard", async ({ page, request }) => {
  const info = await (await request.get("/api/info")).json();
  test.skip(info.setup_completed, "stack already set up (dev auto-setup)");

  await installSigner(page);
  await page.goto("/");
  await expect(page.getByText("Allod trusts the machine it runs on")).toBeVisible();
  await page.getByRole("button", { name: "Get started" }).click();

  await page.getByRole("button", { name: /Browser extension/ }).click();
  await expect(page.getByText("Owner key connected")).toBeVisible();
  await page.getByRole("button", { name: "Continue" }).click();

  await page.getByRole("button", { name: "Create my node" }).click();
  await expect(page.getByRole("heading", { name: "Write down your recovery phrase" })).toBeVisible();
  await page.getByRole("button", { name: "Show words" }).click();
  const words = (await page.locator("ol li").allInnerTexts()).map((t) => t.split(/\s+/).pop()!.trim());
  expect(words).toHaveLength(24);
  await page.getByRole("checkbox").check();
  await page.getByRole("button", { name: "Continue" }).click();

  await expect(page.getByRole("heading", { name: "Check your backup" })).toBeVisible();
  for (const label of await page.getByText(/^Word #\d+$/).allInnerTexts()) {
    const n = Number(label.replace("Word #", ""));
    await page.getByLabel(label).fill(words[n - 1]!);
  }
  await page.getByRole("button", { name: "Confirm backup" }).click();

  // Regtest has no fast sync, so the wizard goes straight to the end; USD is the default
  // display currency (changed later in Settings).
  await expect(page.getByRole("heading", { name: "Your node is set up" })).toBeVisible();
  // ldk-server receives the phrase, starts, and Allod provisions its own macaroon.
  await expect(page.getByText(/running at \d+/)).toBeVisible({ timeout: 180_000 });
  await page.getByRole("button", { name: "Go to Allod" }).click();
  await expect(page.getByText("Running")).toBeVisible({ timeout: 60_000 });
});
