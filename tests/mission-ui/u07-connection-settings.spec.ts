import { expect, test } from "@playwright/test";
import { prepare } from "./helpers";

for (const [label,width,zoom] of [["700",700,1],["zoom200",1440,2]] as const) {
  for (const runtime of ["opencode","codex","claude"] as const) {
  test(`${runtime} connection references fit the ${label} settings view`,async({page})=>{
    await prepare(page);
    await page.setViewportSize({width,height:1000});
    await page.getByRole("button",{name:"설정",exact:true}).click();
    await page.locator(".settings-nav button").filter({hasText:/^AI 작업/}).click();
    await page.evaluate(value=>{document.documentElement.style.zoom=String(value);},zoom);
    const settings=page.locator(".mission-settings");
    await settings.getByTestId("mission-model-advanced").locator("summary").click();
    await settings.getByLabel("CLI 종류",{exact:true}).selectOption(runtime);
    await settings.getByLabel("표시 이름",{exact:true}).fill(runtime==="opencode"?"Z.ai Coding":`${runtime} API`);
    await settings.getByLabel("CLI 실행 파일의 전체 경로",{exact:true}).fill(`/fixture/${runtime}`);
    // The catalog select offers what the mock detected; an id outside it goes through manual entry.
    await settings.getByLabel("모델 ID",{exact:true}).selectOption({label:"직접 입력…"});
    await settings.getByLabel("모델 ID 직접 입력",{exact:true}).fill(runtime==="opencode"?"glm-5.3":"exact-model-id");
    await settings.getByLabel("제공자 ID",{exact:true}).fill(runtime==="codex"?"openai":runtime==="claude"?"anthropic":"zai-coding-plan");
    await settings.getByLabel("인증 방식",{exact:true}).selectOption(runtime==="opencode"?"subscription":"api_key");
    await settings.getByLabel("저장된 인증 참조",{exact:true}).fill("keyring:11111111-1111-4111-8111-111111111111");
    await settings.getByLabel("저장된 엔드포인트 ID",{exact:true}).fill("11111111-1111-4111-8111-111111111111");
    await expect(settings).toContainText("iyagi-termd connection add");
    const overflow=await settings.evaluate(node=>node.scrollWidth-node.clientWidth);
    expect(overflow).toBeLessThanOrEqual(1);
    await settings.getByRole("button",{name:"모델 저장",exact:true}).scrollIntoViewIfNeeded();
    await page.screenshot({path:`/tmp/iyagi-${runtime}-connection-${label}.png`});
    await settings.getByRole("button",{name:"모델 저장",exact:true}).click();
    await expect(settings.getByTestId("mission-settings-notice")).toContainText("저장했습니다");
  });
  }
}
