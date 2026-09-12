import { mkdir, writeFile } from "node:fs/promises";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const directory = resolve(dirname(fileURLToPath(import.meta.url)), ".invalid-runtime");
await mkdir(directory, { recursive: true });
await writeFile(resolve(directory, "ladybug"), "invalid knowledge artifact\n");
