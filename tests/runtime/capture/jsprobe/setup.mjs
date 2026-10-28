// First setup file: installs the probe runtime before any project module is
// imported, and writes the edges of each test file when it ends.
import { afterAll } from "vitest";
import { install } from "./rt.mjs";

const rt = install(process.env.CALLIX_TRACE_DIR);
afterAll(() => rt.flush());
