/**
 * ShellSelectDialog 렌더링: 감지된 프로필 목록/빈 상태 안내/기본 저장
 * 체크박스(기본 켬). SSR은 effect를 돌지 않으므로 detectShellProfiles로
 * 캐시를 예열해 두면 첫 렌더부터 목록이 보인다(실제 앱도 탐지 완료 후
 * 대화상자를 연다).
 */

import { describe, expect, it } from "vitest";
import { renderToString } from "react-dom/server";
import { t } from "../../i18n";
import { ShellSelectDialog } from "./ShellSelectDialog";
import { detectShellProfiles, setShellProbe } from "./shellDeps";
import type { SystemProbe } from "../bridge/systemProbe";
import type { DetectedShell } from "./shellProfiles";

function probeWith(...shells: DetectedShell[]): SystemProbe {
  return { listShells: async () => shells } as unknown as SystemProbe;
}

const pick = () => undefined;

describe("ShellSelectDialog (시작 터미널 선택)", () => {
  it("shows the detected system shell first and focuses it", async () => {
    setShellProbe(probeWith(
      { program: "/bin/zsh", kind: "unix", distro: null },
      { program: "/bin/bash", kind: "unix", distro: null, isDefault: true },
    ));
    await detectShellProfiles("darwin");
    const html = renderToString(<ShellSelectDialog platform="darwin" onPick={pick} />);
    expect(html).toContain(t("terminal.select.systemDefault"));
    expect(html.indexOf('title="/bin/bash -l -i"')).toBeLessThan(html.indexOf('title="/bin/zsh -l -i"'));
    expect(html).toMatch(/autofocus=""[^>]*title="\/bin\/bash -l -i"/);
  });
  it("감지된 프로필과 기본 저장 체크박스(기본 켬)를 렌더링한다", async () => {
    setShellProbe(
      probeWith(
        { program: "C:\\Windows\\System32\\WindowsPowerShell\\v1.0\\powershell.exe", kind: "powershell", distro: null },
        { program: "C:\\Windows\\System32\\cmd.exe", kind: "cmd", distro: null },
      ),
    );
    await detectShellProfiles("windows");

    const html = renderToString(<ShellSelectDialog platform="windows" onPick={pick} />);
    expect(html).toContain("시작 터미널 선택");
    expect(html).toContain("PowerShell");
    expect(html).toContain("명령 프롬프트");
    expect(html).toContain("선택한 셸을 기본으로 저장");
    expect(html).toContain("checked"); // 체크박스 기본 켬
    expect(html).toContain("shell-select-list");
    expect(html).toContain("shell-select-item");
  });

  it("Windows에서 탐지가 비어도 PowerShell·cmd를 합성해 고를 것이 있다(빈 상태로 멈추지 않는다)", async () => {
    setShellProbe(probeWith());
    await detectShellProfiles("windows");

    const html = renderToString(<ShellSelectDialog platform="windows" onPick={pick} />);
    expect(html).not.toContain("shell-select-empty");
    expect(html).toContain("shell-select-item");
    expect(html).toContain("PowerShell");
    expect(html).toContain("명령 프롬프트");
  });

  it("WSL 배포판 프로필도 목록에 나온다", async () => {
    setShellProbe(
      probeWith(
        { program: "C:\\Windows\\System32\\WindowsPowerShell\\v1.0\\powershell.exe", kind: "powershell", distro: null },
        { program: "C:\\Windows\\System32\\wsl.exe", kind: "wsl", distro: "Ubuntu" },
      ),
    );
    await detectShellProfiles("windows");

    const html = renderToString(<ShellSelectDialog platform="windows" onPick={pick} />);
    expect(html).toContain("WSL — Ubuntu");
  });
});
