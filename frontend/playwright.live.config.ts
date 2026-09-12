import { defineConfig, devices } from "@playwright/test";

const backendPort = 4313;
const invalidKnowledgePort = 4314;
const frontendPort = 4174;

const backendCommand = [
  "cd ..",
  "node frontend/e2e/prepare-live-fixture.mjs --live",
  `make backend API_BIND=127.0.0.1:${backendPort} BACKEND_ARGS='serve --bind 127.0.0.1:${backendPort} --config frontend/e2e/live-config.toml'`,
].join(" && ");

const invalidKnowledgeCommand = [
  "cd ..",
  "node frontend/e2e/prepare-live-fixture.mjs",
  `make backend API_BIND=127.0.0.1:${invalidKnowledgePort} BACKEND_ARGS='serve --bind 127.0.0.1:${invalidKnowledgePort} --config frontend/e2e/invalid-knowledge-config.toml'`,
].join(" && ");

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
  webServer: [
    {
      command: backendCommand,
      url: `http://127.0.0.1:${backendPort}/api/health`,
      reuseExistingServer: true,
    },
    {
      command: invalidKnowledgeCommand,
      url: `http://127.0.0.1:${invalidKnowledgePort}/api/health`,
      reuseExistingServer: true,
    },
    {
      command: `npm run dev -- --host 127.0.0.1 --port ${frontendPort}`,
      env: {
        ...process.env,
        OHARA_API_PROXY_TARGET: `http://127.0.0.1:${backendPort}`,
        VITE_OHARA_API_MODE: "http",
      },
      url: `http://127.0.0.1:${frontendPort}`,
      reuseExistingServer: true,
    },
  ],
  projects: [
    {
      name: "chromium",
      use: { ...devices["Desktop Chrome"] },
    },
  ],
});
