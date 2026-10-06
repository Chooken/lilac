// Runs vscode-tmgrammar-test and maps its result to a reliable exit code.
//
// On Windows with Node 23-25, the runner calls `process.exit()` and libuv
// aborts during teardown (nodejs/node#56645) *after* the runner has already
// printed the real result, so the child's exit status is unreliable there.
// This wrapper judges by the printed result instead: the runner prints
// "run successfuly." per passing file and "ERROR" lines for every failure,
// parse error, or grammar-load error.

import { spawnSync } from "node:child_process";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const here = dirname(fileURLToPath(import.meta.url));
const runner = join(here, "..", "node_modules", "vscode-tmgrammar-test", "dist", "unit.js");
const patterns = process.argv.slice(2);

const result = spawnSync(process.execPath, [runner, "-c", ...patterns], {
  encoding: "utf8",
});
const output = `${result.stdout ?? ""}${result.stderr ?? ""}`;
process.stdout.write(output);

const passed = output.includes("run successfuly") && !output.includes("ERROR");
process.exitCode = passed ? 0 : 1;
