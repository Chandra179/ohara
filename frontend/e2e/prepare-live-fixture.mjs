import { mkdir, rm, writeFile } from "node:fs/promises";
import { dirname, resolve } from "node:path";
import process from "node:process";
import { fileURLToPath } from "node:url";

const root = dirname(fileURLToPath(import.meta.url));
if (process.argv.includes("--live")) {
  const directory = resolve(root, ".runtime");
  await rm(directory, { force: true, recursive: true });
  await mkdir(directory, { recursive: true });
} else {
  const directory = resolve(root, ".invalid-runtime");
  await mkdir(directory, { recursive: true });
  await writeFile(
    resolve(directory, "ohara.toml"),
    'data_dir = "frontend/e2e/.invalid-runtime"\n[knowledge]\nfalkordb_url = "redis://127.0.0.1:1"\n',
  );
}
