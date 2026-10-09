#!/usr/bin/env node
/**
 * prepare-bundle.mjs — stage the iyagi-termd daemon for Tauri's externalBin
 * (npm script "prebuild:bundle", spec 06-verification.md §6: "installer에는
 * daemon binary가 함께 포함되어야 한다").
 *
 * Tauri 2 requires the on-disk file to carry a target-triple suffix
 * (`src-tauri/binaries/iyagi-termd-<triple>.exe`) while tauri.conf.json
 * lists the bare base path ("binaries/iyagi-termd"). The bundler then ships
 * it next to the app executable WITHOUT the suffix — exactly where
 * src-tauri/src/bridge/daemon_manager.rs looks for it
 * (`daemon_binary_candidates`, adjacent to the app exe).
 *
 * Windows GNU hosts (this repo's dev hosts) are validated by TWO different
 * consumers with DIFFERENT expected triples (verified empirically):
 *   1. tauri-build (compile time) checks the CARGO target triple, e.g.
 *      `...-x86_64-pc-windows-gnu` (TAURI_ENV_TARGET_TRIPLE).
 *   2. tauri-bundler (bundle time) hardcodes `...-x86_64-pc-windows-msvc`
 *      via tauri-utils::platform::target_triple().
 * On an MSVC CI runner both coincide. On a GNU host we stage BOTH names so
 * `cargo build -p iyagi-app` and `tauri build` both find their file.
 *
 * Usage:
 *   node scripts/prepare-bundle.mjs [--triple x86_64-pc-windows-msvc] [--profile release]
 *
 * No dependencies beyond node stdlib.
 */
import { execFileSync } from "node:child_process";
import { cpSync, mkdirSync, readdirSync, rmSync } from "node:fs";
import { basename, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const repoRoot = resolve(fileURLToPath(import.meta.url), "..", "..");
const args = process.argv.slice(2);

function argValue(flag) {
  const i = args.indexOf(flag);
  return i !== -1 ? args[i + 1] : undefined;
}

const profile = argValue("--profile") ?? "release";
const triple = argValue("--triple") ?? hostTriple();

function hostTriple() {
  try {
    const out = execFileSync("rustc", ["-vV"], { encoding: "utf8", stdio: ["ignore", "pipe", "inherit"] });
    const line = out.split(/\r?\n/).find((l) => l.startsWith("host:"));
    if (line) return line.slice("host:".length).trim();
    console.error("prepare-bundle: cannot parse `rustc -vV` host:\n" + out);
  } catch (err) {
    console.error("prepare-bundle: `rustc -vV` failed: " + err.message);
  }
  // Last-resort defaults mirroring tauri-utils::platform::target_triple.
  if (process.platform === "win32") return `${rustArch()}-pc-windows-msvc`;
  return process.platform === "darwin" ? "aarch64-apple-darwin" : `${rustArch()}-unknown-linux-gnu`;
}

/** Rust target arch for this Node arch (`x64` → `x86_64`, `arm64` → `aarch64`). */
function rustArch() {
  if (process.arch === "x64") return "x86_64";
  if (process.arch === "arm64") return "aarch64";
  return process.arch;
}

/** tauri-bundler's hardcoded msvc name for this Windows arch. */
function bundlerTriple() {
  return `${rustArch()}-pc-windows-msvc`;
}

const exeSuffix = triple.includes("windows") ? ".exe" : "";
const source = join(repoRoot, "target", profile, `iyagi-termd${exeSuffix}`);
const outDir = join(repoRoot, "src-tauri", "binaries");

// Names to stage: the cargo-target triple plus, on Windows, the bundler's
// msvc name (deduplicated — identical on MSVC hosts).
const triples = [triple];
if (process.platform === "win32" && !triple.includes("windows-msvc")) {
  triples.push(bundlerTriple());
}

mkdirSync(outDir, { recursive: true });

try {
  for (const t of triples) {
    cpSync(source, join(outDir, `iyagi-termd-${t}${exeSuffix}`), { force: true });
  }
} catch (err) {
  console.error(
    `prepare-bundle: cannot stage the daemon binary.\n` +
      `  looked for: ${source}\n` +
      `  hint: build it first, e.g. cargo build -p iyagi-termd --${profile}\n` +
      `  (or pass --profile debug while developing)\n` +
      `  cause: ${err.message}`,
  );
  process.exit(1);
}

// Remove stale daemon copies from other triples/profiles.
const keep = new Set(triples.map((t) => `iyagi-termd-${t}${exeSuffix}`));
for (const entry of readdirSync(outDir)) {
  if (entry.startsWith("iyagi-termd-") && !keep.has(entry)) {
    rmSync(join(outDir, entry), { force: true });
    console.log(`prepare-bundle: removed stale ${entry}`);
  }
}

console.log(
  `prepare-bundle: staged ${source} -> ${triples.map((t) => `iyagi-termd-${t}${exeSuffix}`).join(", ")} (profile=${profile})`,
);
