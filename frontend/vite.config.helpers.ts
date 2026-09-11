export const DEV_SERVER_STRICT_PORT = true;

const DEFAULT_API_PROXY_TARGET = "http://127.0.0.1:3000";

export function apiProxyTarget(
  environment: { OHARA_API_PROXY_TARGET?: string } = process.env,
): string {
  return environment.OHARA_API_PROXY_TARGET ?? DEFAULT_API_PROXY_TARGET;
}
