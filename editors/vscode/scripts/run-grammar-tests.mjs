// Runs vscode-tmgrammar-test and maps its result to a reliable exit code.
//
// On Windows with Node 23-25, the runner calls `process.exit()` and libuv
// aborts during teardown (nodejs/node#56645) *after* the runner has already
// printed the real result, so the child's exit status is unreliable there.
// This wrapper judges by the printed result instead. The runner prints
// "run successfuly." for each file that passes, "ERROR" lines for every
// failed assertion, parse error, or empty run, and
// "aborted due to an error" (with an `Error:` on stderr) when a grammar
// fails to load. A run passes only if at least one file passed and none of
// the failure markers appear.

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

const passed =
  output.includes("run successfuly") &&
  !output.includes("ERROR") &&
  !output.includes("aborted due to an error");
process.exitCode = passed ? 0 : 1;
