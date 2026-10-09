/**
 * i18n 진입점 — 모든 섹션 사전을 등록하고 코어 API를 재수출한다.
 *
 * 규칙: 컴포넌트/모듈은 반드시 이 index에서 { t, useI18n }을 가져온다
 * (core를 직접 import하면 사전이 등록되지 않아 키가 그대로 노출된다).
 * 새 섹션은 sections/에 추가하고 아래 import·register를 함께 늘린다.
 */

import { registerMessages } from "./core";
import { appSection } from "./sections/app";
import { settingsSection } from "./sections/settings";
import { terminalSection } from "./sections/terminal";
import { monitorSection } from "./sections/monitor";
import { workloadsSection } from "./sections/workloads";
import { profilesSection } from "./sections/profiles";
import { missionsSection } from "./sections/missions";

registerMessages(appSection);
// settings는 app 뒤에 등록한다 — 설정 페이지 문구의 정본은 이 섹션이다.
registerMessages(settingsSection);
registerMessages(terminalSection);
registerMessages(monitorSection);
registerMessages(workloadsSection);
registerMessages(profilesSection);
registerMessages(missionsSection);

export {
  detectLanguage,
  formatMessage,
  resolveLanguage,
  t,
  translate,
  useI18n,
  useI18nStore,
  createI18nStore,
  messagesSnapshot,
  DEFAULT_LANGUAGE,
  LANGUAGES,
} from "./core";
export type { Language, MessageParams, Messages, SectionMessages } from "./core";
