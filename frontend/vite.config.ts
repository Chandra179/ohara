import tailwindcss from "@tailwindcss/vite";
import react from "@vitejs/plugin-react";
import { defineConfig } from "vitest/config";
import { DEV_SERVER_STRICT_PORT, apiProxyTarget } from "./vite.config.helpers";

export default defineConfig({
  plugins: [react(), tailwindcss()],
  server: {
    strictPort: DEV_SERVER_STRICT_PORT,
    proxy: {
      "/api": apiProxyTarget(),
    },
  },
  test: {
    css: true,
    environment: "jsdom",
    exclude: ["node_modules", "dist", "e2e"],
    globals: true,
    setupFiles: "./src/test/setup.ts",
  },
});
