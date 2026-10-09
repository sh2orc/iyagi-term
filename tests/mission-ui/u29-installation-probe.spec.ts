import { expect, test } from "@playwright/test";
import type { MockDaemonClient } from "../../src/features/daemon/mockClient";
import { prepare } from "./helpers";

for (const layout of ["700", "zoom200"] as const) {
  test(`설치 확인의 실패와 미저장 변경을 ${layout} 화면에서 구분한다`, async ({ page }, testInfo) => {
    await prepare(page);
    await page.setViewportSize({ width: layout === "700" ? 700 : 1440, height: 1000 });
    await page.evaluate(() => {
      const client=window.__mockDaemon!.client as MockDaemonClient;
      const probe=client.bindingProbe.bind(client);let count=0;
      client.bindingProbe=async params=>{
        const result=await probe(params);count+=1;
        return {...result,binding:{...result.binding,runtime_version:count===1?"0.154.0":null},installation:count===1?"verified":"timed_out"};
      };
    });
    await page.getByRole("button",{name:"설정",exact:true}).click();
    await page.locator(".settings-nav button").filter({hasText:/^AI 작업/}).click();
    if (layout === "zoom200") await page.evaluate(()=>{document.documentElement.style.zoom="2";});
    const settings=page.locator(".mission-settings");
    await settings.getByTestId("mission-model-advanced").locator("summary").click();
    await settings.getByLabel("표시 이름",{exact:true}).fill("Probe model");
    await settings.getByLabel("CLI 실행 파일의 전체 경로",{exact:true}).fill("/fixture/codex");
    await settings.getByLabel("모델 ID",{exact:true}).selectOption({label:"직접 입력…"});
    await settings.getByLabel("모델 ID 직접 입력",{exact:true}).fill("fixture-model");
    await settings.getByRole("button",{name:"모델 저장",exact:true}).click();
    const probe=settings.getByRole("button",{name:"설치 확인",exact:true});
    await expect(probe).toBeEnabled();await probe.click();
    await expect(settings.getByTestId("mission-settings-notice")).toContainText("설치된 CLI 파일과 버전을 확인하고 저장했습니다");
    await expect(settings.locator(".mission-settings-card").first()).toContainText("0.154.0");
    await settings.getByLabel("표시 이름",{exact:true}).fill("Unsaved change");
    await expect(probe).toBeDisabled();
    await expect(settings).toContainText("변경한 모델 설정을 먼저 저장한 뒤");
    await settings.getByLabel("표시 이름",{exact:true}).fill("Probe model");
    await expect(probe).toBeEnabled();await probe.click();
    await expect(settings.getByTestId("mission-settings-notice")).toContainText("버전 조회가 3초 안에 끝나지 않아 중단했습니다");
    await expect(settings.locator(".mission-settings-card").first()).not.toContainText("0.154.0");
    await settings.getByTestId("mission-settings-notice").scrollIntoViewIfNeeded();
    expect(await settings.evaluate(node=>node.scrollWidth-node.clientWidth)).toBeLessThanOrEqual(1);
    await page.screenshot({path:testInfo.outputPath(`installation-${layout}.png`)});
  });
}
