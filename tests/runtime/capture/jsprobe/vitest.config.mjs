// vitest configuration of a capture run. Copied into <copy>/.callix/ by
// tests/runtime/capture/typescript.sh, so `vitest/config` and @babel/parser
// resolve from the copy. Environment:
//   CALLIX_PROJ          the copy of the checkout (paths are relative to it)
//   CALLIX_PACKAGE       the package whose tests run (packages/zod)
//   CALLIX_INSTRUMENT    the directory whose .ts files are instrumented (packages/zod/src)
//   CALLIX_TRACE_DIR     where the per-test-file edge lists go
//   CALLIX_SETUP         extra setup files of the project, comma separated
import { dirname, relative, resolve, sep } from "node:path";
import { fileURLToPath } from "node:url";
import { defineConfig } from "vitest/config";
import { instrument } from "./instrument.mjs";

const HERE = dirname(fileURLToPath(import.meta.url));
const PROJ = resolve(process.env.CALLIX_PROJ);
const INSTRUMENT = resolve(PROJ, process.env.CALLIX_INSTRUMENT) + sep;
const SETUP = (process.env.CALLIX_SETUP || "").split(",").filter(Boolean).map((f) => resolve(PROJ, f));

const probe = {
  name: "callix-probe",
  enforce: "pre",
  transform(code, id) {
    const file = id.split("?")[0];
    if (!file.startsWith(INSTRUMENT) || !file.endsWith(".ts") || file.endsWith(".d.ts")) return null;
    const out = instrument(code, relative(PROJ, file).split(sep).join("/"));
    return out ? { code: out.code, map: null } : null;
  },
};

export default defineConfig({
  root: resolve(PROJ, process.env.CALLIX_PACKAGE),
  plugins: [probe],
  resolve: { conditions: ["@zod/source", "default"] },
  ssr: {
    resolve: {
      conditions: ["@zod/source", "default"],
      externalConditions: ["@zod/source", "default"],
    },
  },
  test: {
    watch: false,
    isolate: true,
    setupFiles: [resolve(HERE, "setup.mjs"), ...SETUP],
    typecheck: { enabled: false },
    silent: true,
    pool: "forks",
    maxWorkers: 2,
    testTimeout: 120000,
  },
});
