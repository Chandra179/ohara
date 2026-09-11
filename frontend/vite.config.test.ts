import { describe, expect, it } from "vitest";
import { DEV_SERVER_STRICT_PORT, apiProxyTarget } from "./vite.config.helpers";

describe("Vite development server", () => {
  it("does not silently move to another frontend port", () => {
    expect(DEV_SERVER_STRICT_PORT).toBe(true);
  });

  it("uses the configured Rust API bind as the proxy target", () => {
    expect(apiProxyTarget({ OHARA_API_PROXY_TARGET: "http://127.0.0.1:3100" })).toBe(
      "http://127.0.0.1:3100",
    );
  });
});
