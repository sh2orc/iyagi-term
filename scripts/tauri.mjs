#!/usr/bin/env node
/**
 * tauri.mjs — `npm run tauri …` 진입점. 인자를 그대로 Tauri CLI에 넘기되,
 * macOS에서 안정 코드 서명 인증서가 키체인에 있으면 로컬 빌드를 그 인증서로
 * 서명한다(기본 이름 "iyagi-dev", IYAGI_SIGNING_IDENTITY로 바꾸고 "-"로 끈다).
 *
 * 왜 필요한가: ad-hoc 서명은 빌드마다 코드 해시(cdhash)가 바뀌어, macOS 개인정보
 * 보호(TCC)가 다시 빌드한 앱을 처음 보는 앱으로 여기고 "다른 앱의 데이터에 접근"
 * 같은 허용을 또 묻는다. 인증서로 서명하면 지정 요구 조건이 `identifier +
 * certificate leaf`가 되어 다시 빌드해도 같은 앱으로 남는다.
 *
 * - build: APPLE_SIGNING_IDENTITY를 넘겨 번들러가 .app과 사이드카(iyagi-termd)를 그
 *   인증서로 서명하게 한다. build는 Developer ID 인증서가 하나 있으면 우선 사용하고
 *   hardened runtime을 켠다. 로컬 개발 인증서와 ad-hoc 빌드는 기존 설정을 유지한다.
 * - dev: `tauri dev`는 번들 없이 target/<profile>의 실행 파일을 곧바로 띄우므로
 *   runner(scripts/macos-sign-runner.sh)가 cargo 빌드 직후 서명한다.
 * - macOS dev는 인증서가 없어도 runner를 거친다 — runner가 실행 파일을
 *   `<productName>.app`으로 감싸야 Dock에 `iyagi-app`이 아니라 제품 이름이 뜬다.
 * - macOS에서 인증서가 없으면 build만 ad-hoc("-")으로 번들 전체를 서명한다 — 서명을 건너뛰면
 *   번들 서명이 깨져 Gatekeeper가 "손상됨"으로 막는다. macOS가 아니면 아무것도 바꾸지 않는다.
 *   APPLE_SIGNING_IDENTITY나 --runner를 직접 지정했으면 그 값을 따른다.
 */

import { execFileSync, spawn } from "node:child_process";
import { readFileSync } from "node:fs";
import { join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const repoRoot = resolve(fileURLToPath(import.meta.url), "..", "..");
const tauriCli = join(repoRoot, "node_modules", "@tauri-apps", "cli", "tauri.js");
const DEFAULT_IDENTITY = "iyagi-dev";

const args = process.argv.slice(2);
const command = args[0];
const env = { ...process.env };
const injected = [];

const identity = command === "dev" || command === "build" ? localSigningIdentity() : null;
const devApp = command === "dev" && process.platform === "darwin" ? devAppIdentity() : null;
if (devApp) {
  env.IYAGI_DEV_APP_NAME = devApp.name;
  env.IYAGI_DEV_APP_ID = devApp.id;
}
if ((identity || devApp) && !hasOption(args, "--runner", "-r")) {
  injected.push("--runner", join(repoRoot, "scripts", "macos-sign-runner.sh"));
}
if (identity) {
  env.IYAGI_SIGNING_IDENTITY = identity;
  if (command === "build" && !process.env.APPLE_SIGNING_IDENTITY) {
    env.APPLE_SIGNING_IDENTITY = identity;
    injected.push("--config", JSON.stringify({ bundle: { macOS: { hardenedRuntime: identity.startsWith("Developer ID Application:") } } }));
  }
  console.error(`tauri.mjs: signing this ${command} with the local identity "${identity}" (stable macOS permissions)`);
} else if (command === "build" && process.platform === "darwin" && !process.env.APPLE_SIGNING_IDENTITY) {
  // 인증서가 없어도 번들 전체를 ad-hoc("-")으로 봉인한다. 서명을 건너뛰면 실행 파일에는
  // 링커의 ad-hoc 서명만 남는데, 그 서명은 리소스 봉인(CodeResources)을 요구하므로
  // 번들 안에서 깨진 서명이 되고 Gatekeeper가 "손상되었기 때문에 열 수 없습니다"로 막는다.
  env.APPLE_SIGNING_IDENTITY = "-";
  injected.push("--config", JSON.stringify({ bundle: { macOS: { hardenedRuntime: false } } }));
  console.error(`tauri.mjs: no local signing identity — ad-hoc signing this build`);
}

const child = spawn(
  process.execPath,
  [tauriCli, ...(command === undefined ? [] : [command]), ...injected, ...args.slice(1)],
  { stdio: "inherit", env },
);

// 터미널의 Ctrl+C는 같은 프로세스 그룹의 tauri에도 이미 간다 — 여기서는 기다리기만 한다.
process.on("SIGINT", () => {});
for (const signal of ["SIGTERM", "SIGHUP"]) process.on(signal, () => child.kill(signal));
child.on("error", (error) => {
  console.error(`tauri.mjs: cannot start the Tauri CLI (${tauriCli}): ${error.message}`);
  process.exit(1);
});
child.on("exit", (code, signal) => process.exit(code ?? (signal === "SIGINT" ? 130 : 1)));

/** 키체인에 쓸 수 있는 코드 서명 인증서가 있으면 그 이름(없거나 끄면 null). */
function localSigningIdentity() {
  if (process.platform !== "darwin") return null;
  const wanted = process.env.IYAGI_SIGNING_IDENTITY ?? DEFAULT_IDENTITY;
  if (wanted === "" || wanted === "-") return null;
  try {
    const listing = execFileSync("security", ["find-identity", "-v", "-p", "codesigning"], {
      encoding: "utf8",
      stdio: ["ignore", "pipe", "ignore"],
    });
    if (command === "build" && process.env.IYAGI_SIGNING_IDENTITY === undefined && !process.env.APPLE_SIGNING_IDENTITY) {
      const developerIds = [...listing.matchAll(/"(Developer ID Application:[^"]+)"/g)].map((match) => match[1]);
      if (developerIds.length > 1) {
        console.error("tauri.mjs: multiple Developer ID certificates found; set APPLE_SIGNING_IDENTITY explicitly");
        process.exit(1);
      }
      if (developerIds.length === 1) return developerIds[0];
    }
    // 행 형식: `  1) 34367B57…C65 "iyagi-dev"` — 이름(따옴표) 또는 SHA-1로 지정할 수 있다.
    const found = listing
      .split("\n")
      .some((line) => line.includes(`"${wanted}"`) || line.split(/\s+/).includes(wanted.toUpperCase()));
    return found ? wanted : null;
  } catch {
    return null;
  }
}

/** dev 실행 파일을 감쌀 .app의 이름·식별자(tauri.conf.json). 읽지 못하면 null — 감싸지 않는다. */
function devAppIdentity() {
  try {
    const conf = JSON.parse(readFileSync(join(repoRoot, "src-tauri", "tauri.conf.json"), "utf8"));
    const name = typeof conf.productName === "string" ? conf.productName.trim() : "";
    // 번들 디렉터리 이름이 되므로 경로 구분자는 받지 않는다.
    if (!name || name.includes("/")) return null;
    return { name, id: typeof conf.identifier === "string" ? conf.identifier : "ai.iyagi.term" };
  } catch {
    return null;
  }
}

/** `--` 앞에 그 옵션이 이미 있는가. */
function hasOption(list, long, short) {
  for (const arg of list) {
    if (arg === "--") return false;
    if (arg === long || arg === short || arg.startsWith(`${long}=`)) return true;
  }
  return false;
}
