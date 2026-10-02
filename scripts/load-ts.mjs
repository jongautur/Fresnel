// Load the app's TypeScript modules in node, for scripts/bench-heatmap.mjs
// and scripts/test-report.mjs. Uses esbuild from node_modules (installed with
// Vite) when present, otherwise compiles with tsc to CommonJS in a temp dir.
//
//   const { buildGrid, reportHtml } = await loadTs(["src/lib/heatmap.ts", "src/lib/report.ts"]);
//
// All entries share one module graph (one copy of each module).

import { execFileSync } from "node:child_process";
import { mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { createRequire } from "node:module";
import { tmpdir } from "node:os";
import { dirname, join, relative, resolve } from "node:path";
import { fileURLToPath } from "node:url";

export const root = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const require = createRequire(import.meta.url);

export async function loadTs(entries) {
  let esbuild = null;
  try {
    esbuild = require("esbuild");
  } catch {
    /* fall back to tsc */
  }
  if (esbuild) {
    const out = await esbuild.build({
      stdin: {
        contents: entries.map((e) => `export * from ${JSON.stringify("./" + e)};`).join("\n"),
        resolveDir: root,
        loader: "ts",
      },
      bundle: true,
      write: false,
      format: "esm",
      platform: "node",
      target: "node20",
      // Same transform settings as the app build.
      tsconfig: join(root, "tsconfig.json"),
      logLevel: "silent",
    });
    const code = out.outputFiles[0].text;
    return import(`data:text/javascript;base64,${Buffer.from(code).toString("base64")}`);
  }

  const dir = mkdtempSync(join(tmpdir(), "fresnel-ts-"));
  try {
    const tsc = join(root, "node_modules", "typescript", "bin", "tsc");
    execFileSync(
      process.execPath,
      [
        tsc,
        "--outDir", dir,
        "--rootDir", join(root, "src"),
        "--module", "commonjs",
        "--moduleResolution", "node10",
        "--target", "es2022",
        "--lib", "es2022,dom",
        "--jsx", "react-jsx",
        "--skipLibCheck",
        "--esModuleInterop",
        ...entries.map((e) => join(root, e)),
      ],
      { stdio: "inherit" },
    );
    // The package is "type": "module"; mark the output as CommonJS.
    writeFileSync(join(dir, "package.json"), '{ "type": "commonjs" }');
    const mods = entries.map((e) => require(join(dir, relative(join(root, "src"), join(root, e))).replace(/\.ts$/, ".js")));
    return Object.assign({}, ...mods);
  } finally {
    // require() loaded the whole module graph synchronously.
    rmSync(dir, { recursive: true, force: true });
  }
}
