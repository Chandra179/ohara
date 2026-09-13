import { defineConfig, devices } from "@playwright/test";

const frontendPort = 4174;

export default defineConfig({
  testDir: "./e2e",
  testMatch: "**/live.spec.ts",
  fullyParallel: false,
  workers: 1,
  reporter: "list",
  use: {
    baseURL: `http://127.0.0.1:${frontendPort}`,
    launchOptions: process.env.PLAYWRIGHT_EXECUTABLE_PATH
      ? { executablePath: process.env.PLAYWRIGHT_EXECUTABLE_PATH }
      : undefined,
    trace: "on-first-retry",
  },
  // The live suite exercises the five Rust processes and their providers.
  // Start them with `make dev` or Compose before running this command.
  webServer: {
    command: `npm run dev -- --host 127.0.0.1 --port ${frontendPort}`,
    env: {
      ...process.env,
      OHARA_API_PROXY_TARGET: "http://127.0.0.1:3000",
      VITE_OHARA_API_MODE: "http",
    },
    url: `http://127.0.0.1:${frontendPort}`,
    reuseExistingServer: true,
  },
  projects: [
    {
      name: "chromium",
      use: { ...devices["Desktop Chrome"] },
    },
  ],
});
