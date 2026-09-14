import { defineConfig, devices } from "@playwright/test";

const frontendPort = 4175;

export default defineConfig({
  testDir: "./e2e",
  testMatch: "**/query-contract.spec.ts",
  fullyParallel: false,
  workers: 1,
  reporter: "list",
  use: {
    ...devices["Desktop Chrome"],
    baseURL: `http://127.0.0.1:${frontendPort}`,
    launchOptions: process.env.PLAYWRIGHT_EXECUTABLE_PATH
      ? { executablePath: process.env.PLAYWRIGHT_EXECUTABLE_PATH }
      : undefined,
    trace: "on-first-retry",
  },
  webServer: {
    command: `npm run dev -- --host 127.0.0.1 --port ${frontendPort}`,
    env: {
      ...process.env,
      VITE_OHARA_API_MODE: "http",
    },
    url: `http://127.0.0.1:${frontendPort}`,
    reuseExistingServer: !process.env.CI,
  },
});
