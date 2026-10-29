import { expect, test } from "@playwright/test";
import { installSigner } from "./signer";

test("owner signs in, sees the node, and signs out", async ({ page }) => {
  await installSigner(page);
  await page.goto("/");
  await expect(page.getByRole("heading", { name: "Sign in" })).toBeVisible();
  await page.getByRole("button", { name: /Browser extension/ }).click();
  await page.getByRole("button", { name: "Sign in" }).click();

  await expect(page.getByRole("heading", { name: "Your node" })).toBeVisible();
  await expect(page.getByText("Running")).toBeVisible({ timeout: 60_000 });
  await expect(page.getByText(/Block \d+/)).toBeVisible();

  await page.getByTitle(/Sign out/).click();
  await expect(page.getByRole("heading", { name: "Sign in" })).toBeVisible();
});

test("a key that is not an owner is refused", async ({ page }) => {
  // Random key, not the regtest owner.
  await installSigner(page, "1".repeat(63) + "2");
  await page.goto("/");
  await page.getByRole("button", { name: /Browser extension/ }).click();
  await page.getByRole("button", { name: "Sign in" }).click();
  await expect(page.getByText("This Nostr key is not an owner of this node.")).toBeVisible();
});
