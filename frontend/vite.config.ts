import tailwindcss from "@tailwindcss/vite";
import react from "@vitejs/plugin-react";
import { defineConfig } from "vitest/config";

export default defineConfig({
  plugins: [react(), tailwindcss()],
  test: {
    css: true,
    environment: "jsdom",
    exclude: ["node_modules", "dist", "e2e"],
    globals: true,
    setupFiles: "./src/test/setup.ts",
  },
});
