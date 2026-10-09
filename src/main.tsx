import React from "react";
import ReactDOM from "react-dom/client";
import { App } from "./app/App";

// 내장 한글 코딩 폰트(Nanum Gothic Coding, OFL)를 xterm이 셀 메트릭을
// 재기 전에 준비시킨다 — 이 폰트의 "한글 = 라틴×2" 계약이 커서 정렬의
// 기반이므로, 늦게 로드되면 첫 터미널의 격자가 폴백 메트릭으로 굳는다.
const bundledFonts = [
  document.fonts?.load('13px "Nanum Gothic Coding"'),
  document.fonts?.load('bold 13px "Nanum Gothic Coding"'),
];
void Promise.allSettled(bundledFonts).then(() => {
  ReactDOM.createRoot(document.getElementById("root") as HTMLElement).render(
    <React.StrictMode>
      <App />
    </React.StrictMode>,
  );
});
