import { defineConfig, devices } from "@playwright/test";

// End-to-end tests against a running Allod (default: the regtest stack, `just e2e-ui`).
export default defineConfig({
  testDir: "./e2e",
  fullyParallel: false,
  workers: 1,
  timeout: 240_000,
  expect: { timeout: 20_000 },
  reporter: [["list"]],
  use: {
    baseURL: process.env.ALLOD_E2E_URL ?? "http://127.0.0.1:18029",
    trace: "retain-on-failure",
  },
  projects: [{ name: "chromium", use: { ...devices["Desktop Chrome"] } }],
});
