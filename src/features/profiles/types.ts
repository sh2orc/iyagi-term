/**
 * CLI 프로필 형식 (04-ui.md §5, 03-resources.md §8, 01 §7).
 *
 * - CliDescriptor는 04 §5의 계약 그대로다(src/generated에 없는 프론트
 *   소유 형식). R1에서 심화 capabilities(turn_events/resume/
 *   concurrency_control)는 모두 false가 기본이다.
 * - 환경 변수는 값 또는 OS 보안 저장소 참조(secretRef) 중 하나만 담는다.
 *   raw 비밀 값을 입력받는 필드는 존재하지 않는다(01 §7).
 */

import type { Enforcement } from "../../generated/Enforcement";
import { t } from "../../i18n";

export type CliKind = "codex" | "claude" | "opencode" | "custom";

export interface CliDescriptorCapabilities {
  turn_events: boolean;
  resume: boolean;
  concurrency_control: boolean;
}

/** 04 §5 그대로. R1 기본 capabilities는 전부 false. */
export interface CliDescriptor {
  kind: CliKind;
  /** 관리 실행에 등록한 실행 파일 경로(절대). interpreter 형태에서는 신원 표시용. */
  program: string;
  /** 실행에 항상 붙는 고정 인수(프로그램 자신 제외). */
  argv_prefix: string[];
  /** 검증된 version query 결과만. 검증 못했으면 null — 값을 만들지 않는다. */
  detected_version: string | null;
  transport: "pty";
  capabilities: CliDescriptorCapabilities;
}

/** R1 기본값: 심화 capabilities는 지원 검증 전까지 모두 false. */
export function defaultCliCapabilities(): CliDescriptorCapabilities {
  return { turn_events: false, resume: false, concurrency_control: false };
}

/**
 * Windows interpreter 형태 (02 §3 마지막 문단).
 * `.cmd`/`.bat`/`.ps1` shim은 관리 프로필에서 직접 실행하지 않는다.
 * native 실행 파일(예: node.exe) + argv prefix로 script를 실행한다.
 */
export interface ProfileInterpreter {
  /** native 실행 파일 절대 경로(예: C:\Program Files\nodejs\node.exe). */
  executable: string;
  /** 스크립트 경로 등 interpreter 뒤에 항상 붙는 인수(최소 1개). */
  scriptArgvPrefix: string[];
}

/** env 항목: 값(일반) 또는 OS 보안 저장소 참조 중 정확히 하나. */
export interface ProfileEnvEntry {
  key: string;
  /** 일반 값. secretRef와 동시에 설정하지 않는다. */
  value: string | null;
  /**
   * OS 보안 저장소(자격 증명 관리자/Keychain) 항목 label 참조.
   * 비밀 값 자체는 절대 저장/입력받지 않는다(01 §7). R1 placeholder.
   */
  secretRef: string | null;
}

/** 기본 프로필 정책 (03 §8): observe + 2 GiB + cpu_slots 1 + cap 전부 null. */
export interface ProfilePolicy {
  enforcement: Enforcement;
  /** U64String(바이트). */
  reservation_bytes: string;
  cpu_slots: number;
  memory_max_bytes: string | null;
  cpu_max_cores: number | null;
  pids_max: number | null;
}

export const DEFAULT_RESERVATION_BYTES = "2147483648"; // 2 GiB (03 §8)
export const DEFAULT_CPU_SLOTS = 1; // 03 §8
export const MIN_RESERVATION_BYTES = 256 * 1024 * 1024; // 256 MiB — UI 하한

export function defaultProfilePolicy(): ProfilePolicy {
  return {
    enforcement: "observe",
    reservation_bytes: DEFAULT_RESERVATION_BYTES,
    cpu_slots: DEFAULT_CPU_SLOTS,
    memory_max_bytes: null,
    cpu_max_cores: null,
    pids_max: null,
  };
}

export interface LaunchProfile {
  id: string;
  /** 프로필 이름 — 스토어 전체에서 유일해야 한다. */
  label: string;
  descriptor: CliDescriptor;
  /** 기본 작업 디렉터리(절대 경로). 비어 있으면 실행 시 프로젝트 root 기본값. */
  cwd: string;
  policy: ProfilePolicy;
  env: ProfileEnvEntry[];
  notes: string;
  /** Windows interpreter 형태일 때만 설정. null이면 program을 직접 실행. */
  interpreter: ProfileInterpreter | null;
}

/** 표시 라벨 — "custom"만 번역한다(접근 시점에 t() 평가, 나머지는 고유명칭). */
export const CLI_KIND_LABELS: Record<CliKind, string> = {
  codex: "Codex CLI",
  claude: "Claude Code",
  opencode: "OpenCode",
  get custom(): string {
    return t("profile.kind.custom");
  },
};

/**
 * 이 프로필이 실제로 실행할 명령 형태(04 §5 "실행 파일 경로·설치 형태를
 * 보여 주고"). interpreter 형태면 interpreter.executable이 program이 되고
 * scriptArgvPrefix가 argv 앞에 붙는다. UI는 이 결과를 "그대로 실행되는
 * 형태"로 표시한다.
 */
export function effectiveCommand(
  profile: Pick<LaunchProfile, "descriptor" | "interpreter">,
  extraArgv: readonly string[] = [],
): { program: string; argv: string[] } {
  if (profile.interpreter) {
    return {
      program: profile.interpreter.executable,
      argv: [...profile.interpreter.scriptArgvPrefix, ...profile.descriptor.argv_prefix, ...extraArgv],
    };
  }
  return {
    program: profile.descriptor.program,
    argv: [...profile.descriptor.argv_prefix, ...extraArgv],
  };
}
