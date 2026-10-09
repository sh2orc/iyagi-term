#!/usr/bin/env node
/**
 * 브리지 IME 트레이스(node_modules/.cache/iyagi-ime-trace.log — vite 플러그인
 * iyagi-ime-trace가 기록)의 한 구간을 리플레이 fixture(TSV)로 변환한다.
 * docs/implementation/07-korean-ime.md §5.
 *
 *   node scripts/ime-trace-to-fixture.mjs <log> <fromLine> <toLine> > src/features/terminal/__fixtures__/webkit-ime-trace-<name>.tsv
 *
 * 형식(탭 구분, ts는 첫 이벤트 기준 ms):
 *   ts  kd   keyCode  key(JSON)  shift(0/1)  mod(0/1)
 *   ts  ku   keyCode  key(JSON)
 *   ts  kp   keyCode  key(JSON)
 *   ts  bi   inputType  data(JSON|null)
 *   ts  inp  inputType  data(JSON|null)  tailDeletes  tailInsert(JSON)
 * inp의 꼬리 편집은 로그의 value=로 계산하며, `resync value=""`는 textarea가
 * 비워진 것으로 반영한다. "-> …" 판정 줄과 타이머 줄은 버린다(리플레이가 다시 만든다).
 */
import { readFileSync } from "node:fs";

const [file, from, to] = process.argv.slice(2);
if (!file) {
  console.error("usage: ime-trace-to-fixture.mjs <log> [fromLine] [toLine]");
  process.exit(2);
}
const all = readFileSync(file, "utf8").split("\n").filter(Boolean);
const lines = all.slice(from ? Number(from) - 1 : 0, to ? Number(to) : all.length);

const tail = (prev, next) => {
  const a = Array.from(prev);
  const b = Array.from(next);
  let c = 0;
  while (c < a.length && c < b.length && a[c] === b[c]) c++;
  return [a.length - c, b.slice(c).join("")];
};

const STR = '("(?:[^"\\\\]|\\\\.)*")';
let value = "";
let base = null;
const out = [];
for (const l of lines) {
  const ts0 = Number(l.split(" ")[0]);
  if (!Number.isFinite(ts0)) continue;
  if (base === null) base = ts0;
  const ts = ts0 - base;
  let m;
  if ((m = l.match(new RegExp(`^\\d+ kd (\\d+) key=${STR} composing=\\w+(?: shift=(\\w+))? mod=(\\w+)`)))) {
    out.push([ts, "kd", m[1], m[2], m[3] === "true" ? 1 : 0, m[4] === "true" ? 1 : 0].join("\t"));
  } else if ((m = l.match(new RegExp(`^\\d+ (ku|kp) (\\d+)(?: key=${STR})?`)))) {
    out.push([ts, m[1], m[2], m[3] ?? '""'].join("\t"));
  } else if ((m = l.match(/^\d+ bi (\S+) data=(.*)$/))) {
    out.push([ts, "bi", m[1], m[2]].join("\t"));
  } else if ((m = l.match(new RegExp(`^\\d+ inp (\\S+) data=(.*?) value=${STR}$`)))) {
    const next = JSON.parse(m[3]);
    const [deletes, insert] = tail(value, next);
    value = next;
    out.push([ts, "inp", m[1], m[2], deletes, JSON.stringify(insert)].join("\t"));
  } else if (/^\d+ resync(\(blur\))? value=""/.test(l)) {
    value = "";
  }
}
process.stdout.write(out.join("\n") + "\n");
