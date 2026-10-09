// vendor/xterm-addon-webgl/src(패치된 @xterm/addon-webgl 소스)를 하나의 ESM 번들로 묶는다.
// 업스트림 빌드(tsc target es2021 + webpack)와 같은 의미론으로: 클래스 필드는 대입(useDefineForClassFields=false),
// 코어의 browser/·common/·vs/ 모듈은 설치된 @xterm/xterm의 src에서 가져온다(웹팩 번들이 하던 것과 같다).
// 결과 src/vendor/xterm-addon-webgl/addon-webgl.js는 저장소에 넣는다 — 앱 빌드는 이 스크립트에 기대지 않는다.
import { build } from "esbuild";
import { spawnSync } from "node:child_process";
import { readFileSync } from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";

const ROOT = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
const CORE_SRC = path.join(ROOT, "node_modules/@xterm/xterm/src");
const ENTRY = path.join(ROOT, "vendor/xterm-addon-webgl/src/WebglAddon.ts");
const OUT = path.join(ROOT, "src/vendor/xterm-addon-webgl/addon-webgl.js");
const corePkg = JSON.parse(readFileSync(path.join(ROOT, "node_modules/@xterm/xterm/package.json"), "utf8"));
const addonPkg = JSON.parse(readFileSync(path.join(ROOT, "node_modules/@xterm/addon-webgl/package.json"), "utf8"));

const coreResolver = {
  name: "xterm-core-src",
  setup(api) {
    api.onResolve({ filter: /^(browser|common|vs)\// }, (args) => {
      const base = path.join(CORE_SRC, args.path);
      for (const candidate of [`${base}.ts`, path.join(base, "index.ts")]) {
        try { readFileSync(candidate); return { path: candidate }; } catch { /* next */ }
      }
      return { errors: [{ text: `core module not found: ${args.path}` }] };
    });
  },
};

// 묶기 전에 패치된 소스를 코어 소스에 대고 타입 검사한다(esbuild는 타입을 보지 않는다). 코어의 vs/base 쪽은
// 이 설정에서 몇 개의 오류를 내므로 vendor/ 안의 오류만 실패로 본다.
const tsc = spawnSync(process.execPath, [path.join(ROOT, "node_modules/typescript/bin/tsc"), "-p", path.join(ROOT, "vendor/xterm-addon-webgl/tsconfig.json"), "--pretty", "false"], { cwd: ROOT, encoding: "utf8" });
const vendorErrors = `${tsc.stdout}${tsc.stderr}`.split("\n").filter((line) => /^vendor\//.test(line) && line.includes("error TS"));
if (vendorErrors.length > 0) {
  console.error(vendorErrors.join("\n"));
  process.exit(1);
}
console.log(`typecheck ok (${vendorErrors.length} errors in vendor/)`);

const result = await build({
  entryPoints: [ENTRY],
  bundle: true,
  format: "esm",
  platform: "browser",
  target: "es2021",
  outfile: OUT,
  external: ["@xterm/xterm", "@xterm/addon-webgl"],
  plugins: [coreResolver],
  tsconfigRaw: { compilerOptions: { useDefineForClassFields: false, target: "es2021", experimentalDecorators: true } },
  legalComments: "none",
  logLevel: "warning",
  banner: {
    js: [
      "/**",
      ` * Vendored @xterm/addon-webgl ${addonPkg.version} (upstream commit ${addonPkg.commit}) built against @xterm/xterm ${corePkg.version}.`,
      " * Source: vendor/xterm-addon-webgl/src — see vendor/xterm-addon-webgl/README.md for the applied fixes.",
      " * Regenerate with `node scripts/build-webgl-addon.mjs`. MIT License — Copyright (c) The xterm.js authors.",
      " */",
    ].join("\n"),
  },
});
if (result.errors.length) process.exit(1);
const size = readFileSync(OUT).length;
console.log(`built ${path.relative(ROOT, OUT)} (${(size / 1024).toFixed(0)} KiB)`);
