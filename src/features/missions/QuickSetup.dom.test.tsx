/**
 * 빠른 설정: CLI 감지 카드의 정직한 표시(호환 등급 배지)와 원클릭 팀 생성(연결 저장/재사용 →
 * 설치 확인 → 네 역할 검증 → 팀 저장/재사용), 미검증 버전의 실험적 사용 동의(계약 A).
 * 클라이언트는 필요한 메서드만 가진 스텁.
 */

import { beforeEach, expect, it, vi } from "vitest";
import type { DaemonClient } from "../daemon/client";
import type { Binding } from "../../generated/Binding";
import type { BindingProbeParams } from "../../generated/BindingProbeParams";
import type { BindingSaveParams } from "../../generated/BindingSaveParams";
import type { DetectedRuntime } from "../../generated/DetectedRuntime";
import type { InstallationStatus } from "../../generated/InstallationStatus";
import type { Role } from "../../generated/Role";
import type { TeamTemplate } from "../../generated/TeamTemplate";
import type { TemplateSaveParams } from "../../generated/TemplateSaveParams";
import { t, useI18nStore } from "../../i18n";
import { CUSTOM_MODEL_OPTION, missionPolicy, newBinding } from "./configuration";
import { QuickSetup } from "./QuickSetup";
import { click, flushAsync, renderUi, resetAllMissionState, setValue, type RenderHandle } from "./testSupport";

beforeEach(resetAllMissionState);

/**
 * The Z.ai key lives in the desktop key store behind the Tauri bridge, and the card reads it
 * through this module. `isTauri()` stays real: only the case that cares about the key installs the
 * bridge marker, so every other case keeps the "status unknown" path a browser build would take.
 */
const zaiKeyStore = vi.hoisted(() => ({ configured: false, reads: 0 }));
vi.mock("../subscriptions/client", () => ({
  SUBSCRIPTION_REFRESH_EVENT: "iyagi-subscription-refresh",
  getZaiKeyStatus: async () => { zaiKeyStore.reads += 1; return { configured: zaiKeyStore.configured }; },
}));
function installTauriBridge(): () => void {
  const host = window as unknown as Record<string, unknown>;
  host.__TAURI_INTERNALS__ = {};
  return () => { delete host.__TAURI_INTERNALS__; };
}

const allRoles: Role[] = ["lead", "builder", "reviewer", "integrator"];

function detected(overrides: Partial<DetectedRuntime> & Pick<DetectedRuntime, "runtime">): DetectedRuntime {
  return {
    program: `/usr/local/bin/${overrides.runtime}`, version: "0.154.0", installation: "verified", login: "found",
    configured_model_id: null, suggested_provider_id: overrides.runtime === "claude" ? "anthropic" : "openai",
    proven_model_id: null, models: [], verified_roles: [],
    grade: overrides.installation === "not_found" ? "not_installed" : "verified", experimental_roles: [], ...overrides,
  };
}

function withCapabilities(binding: Binding, reason: string | null): Binding {
  const capabilities = { ...binding.capabilities };
  for (const key of Object.keys(capabilities) as Array<keyof Binding["capabilities"]>) {
    capabilities[key] = { supported: true, reason_code: reason };
  }
  return { ...binding, capabilities, runtime_version: binding.experimental_version ?? "0.154.0", checked_at: "2026-09-17T00:00:00Z" };
}

function withVerifiedCapabilities(binding: Binding): Binding {
  const capabilities = { ...binding.capabilities };
  for (const key of Object.keys(capabilities) as Array<keyof Binding["capabilities"]>) {
    capabilities[key] = { supported: true, reason_code: null };
  }
  return { ...binding, capabilities, runtime_version: "0.154.0", checked_at: "2026-09-17T00:00:00Z" };
}

function stubClient(options: {
  runtimes: DetectedRuntime[]; bindings?: Binding[]; templates?: TeamTemplate[];
  probe?: (binding: Binding) => Binding; installation?: InstallationStatus;
}) {
  const bindings = [...(options.bindings ?? [])];
  const templates = [...(options.templates ?? [])];
  const stub = {
    runtimeDetect: vi.fn(async () => ({ runtimes: options.runtimes })),
    bindingList: vi.fn(async () => ({ bindings: bindings.map(binding => ({ ...binding })) })),
    bindingSave: vi.fn(async (params: BindingSaveParams) => {
      const saved = { ...params.binding, revision: "1" };
      bindings.push(saved);
      return { binding: saved };
    }),
    bindingProbe: vi.fn(async (params: BindingProbeParams) => {
      const stored = bindings.find(binding => binding.id === params.binding_id);
      if (!stored) throw new Error("unknown binding");
      const probed = { ...(options.probe ?? withVerifiedCapabilities)(stored), revision: String(Number(stored.revision) + 1) };
      return { binding: probed, models: [], installation: options.installation ?? "verified" };
    }),
    templateList: vi.fn(async () => ({ templates: templates.map(template => ({ ...template })) })),
    templateSave: vi.fn(async (params: TemplateSaveParams) => {
      const saved = { ...params.template, revision: "1" };
      templates.push(saved);
      return { template: saved };
    }),
  };
  return { stub, client: stub as unknown as DaemonClient };
}

function card(handle: RenderHandle, name: string): HTMLElement {
  const node = handle.container.querySelector<HTMLElement>(`[role=group][aria-label="${name}"]`);
  if (!node) throw new Error(`Missing card: ${name}`);
  return node;
}
function button(root: Element, key: string): HTMLButtonElement {
  const node = [...root.querySelectorAll("button")].find(candidate => candidate.textContent === t(key));
  if (!node) throw new Error(`Missing button: ${key}`);
  return node;
}

it.each(["ko", "en"] as const)("%s 검증된 Codex로 연결을 저장·확인하고 네 역할 팀을 만든다", async language => {
  useI18nStore.setState({ language });
  const { stub, client } = stubClient({ runtimes: [
    detected({ runtime: "codex", proven_model_id: "gpt-5.6-luna", configured_model_id: "gpt-5.5",
      models: [{ id: "gpt-5.6-luna", efforts: [] }, { id: "gpt-6-astra", efforts: [] }], verified_roles: [...allRoles], experimental_roles: [...allRoles] }),
    detected({ runtime: "claude", installation: "not_found", program: "", version: null, login: "unknown" }),
    detected({ runtime: "opencode", login: "unknown" }),
  ] });
  const onTeamCreated = vi.fn();
  const onBindingSaved = vi.fn();
  const handle = renderUi(<QuickSetup client={client} onTeamCreated={onTeamCreated} onBindingSaved={onBindingSaved} />);
  try {
    await flushAsync();
    expect(stub.runtimeDetect).toHaveBeenCalledTimes(1);
    expect(handle.container.querySelector("h3")?.textContent).toBe(t("missions.quickSetup.title"));
    expect(handle.container.textContent).toContain(t("missions.quickSetup.intro"));
    const codex = card(handle, "Codex");
    expect(codex.textContent).toContain(t("missions.quickSetup.grade.verified"));
    expect(codex.querySelector("[data-grade]")?.getAttribute("data-grade")).toBe("verified");
    expect(codex.textContent).toContain(t("missions.quickSetup.version", { version: "0.154.0" }));
    expect(codex.textContent).toContain("/usr/local/bin/codex");
    expect(codex.textContent).toContain(t("missions.quickSetup.login.found"));
    const select = codex.querySelector<HTMLSelectElement>("[data-testid=quick-setup-model]")!;
    // configured_model_id wins as the default — it's what the user set up in the CLI, not just
    // the one live evidence happened to pin.
    expect(select.value).toBe("gpt-5.5");
    // Every model is a choice, in one order: configured model, then proven model, then the runtime's
    // advertised list — deduped (gpt-5.6-luna is both proven_model_id and advertised, but appears
    // once) — and manual entry last. The proven one is labelled as such.
    expect([...select.options].map(option => option.value)).toEqual(["gpt-5.5", "gpt-5.6-luna", "gpt-6-astra", CUSTOM_MODEL_OPTION]);
    expect([...select.options].map(option => option.textContent)).toEqual(["gpt-5.5", t("missions.quickSetup.modelVerified", { model: "gpt-5.6-luna" }), "gpt-6-astra", t("missions.quickSetup.modelCustom")]);
    expect(codex.querySelector("[data-testid=quick-setup-model-manual]")).toBeNull();
    // A model other than the pinned one is a plain choice now (11 §8): the pin is a sort hint,
    // not a reason to warn or to ask for consent.
    expect(codex.querySelector(".mission-quick-setup-warning")).toBeNull();
    expect(codex.querySelector("[data-testid=quick-setup-experimental]")).toBeNull();
    setValue(select, "gpt-5.6-luna");
    expect(codex.querySelector("[data-testid=quick-setup-experimental]")).toBeNull();
    // No progress or outcome is announced before the user acts.
    expect(handle.container.querySelector("[role=status]")).toBeNull();

    click(button(codex, "missions.quickSetup.create"));
    await flushAsync();
    expect(stub.bindingSave).toHaveBeenCalledTimes(1);
    const save = stub.bindingSave.mock.calls[0][0];
    expect(save.expected_revision).toBe("0");
    expect(save.binding).toMatchObject({ runtime: "codex", label: "Codex · gpt-5.6-luna", program: "/usr/local/bin/codex", provider_id: "openai",
      model_id: "gpt-5.6-luna", auth_route: "subscription", credential_ref: null, endpoint_ref: null, enabled: true, experimental_version: null });
    expect(stub.bindingProbe).toHaveBeenCalledWith({ binding_id: save.binding.id });
    // The stored connection is reported right after saving and again with the probe result.
    expect(onBindingSaved).toHaveBeenCalledTimes(2);
    expect(onBindingSaved.mock.calls[0][0]).toMatchObject({ id: save.binding.id, revision: "1", model_id: "gpt-5.6-luna", runtime_version: null });
    expect(onBindingSaved.mock.calls[1][0]).toMatchObject({ id: save.binding.id, revision: "2", runtime_version: "0.154.0" });
    expect(onBindingSaved.mock.invocationCallOrder[1]).toBeLessThan(onTeamCreated.mock.invocationCallOrder[0]);
    expect(stub.templateList).toHaveBeenCalledWith({ repository_id: null });
    expect(stub.templateSave).toHaveBeenCalledTimes(1);
    const template = stub.templateSave.mock.calls[0][0].template;
    expect(template).toMatchObject({ label: "Codex · gpt-5.6-luna", repository_id: null, revision: "0" });
    expect(template.role_bindings).toEqual(allRoles.map(role => ({ role, primary_binding_id: save.binding.id, fallback_binding_ids: [] })));
    expect(template.policy.allowed_binding_ids).toEqual([save.binding.id]);
    expect(template.policy.require_independent_review).toBe(true);
    expect(onTeamCreated).toHaveBeenCalledTimes(1);
    expect(onTeamCreated.mock.calls[0][0]).toMatchObject({ id: template.id, label: "Codex · gpt-5.6-luna" });
    expect(codex.querySelector("[role=status]")?.textContent).toBe(t("missions.quickSetup.done", { label: "Codex · gpt-5.6-luna" }));
    expect(handle.container.querySelector("[role=alert]")).toBeNull();
    expect(button(codex, "missions.quickSetup.create").disabled).toBe(false);
  } finally { handle.unmount(); }
});

it.each(["ko", "en"] as const)("%s 검증된 Codex에서 핀과 다른 모델을 골라도 동의 없이 그 모델로 팀을 만든다", async language => {
  useI18nStore.setState({ language });
  const { stub, client } = stubClient({
    runtimes: [detected({ runtime: "codex", proven_model_id: "gpt-5.6-luna", configured_model_id: "gpt-6-astra",
      models: [{ id: "gpt-6-astra", efforts: [] }, { id: "gpt-5.6-sol", efforts: [] }, { id: "gpt-5.6-luna", efforts: [] }],
      verified_roles: [...allRoles], experimental_roles: [...allRoles] })],
    // 모델 적합성은 런타임이 판단한다(11 §8): 핀과 다른 모델도 이 PC의 자가 진단으로 열린다.
    probe: binding => withCapabilities(binding, "local_probe"),
  });
  const onTeamCreated = vi.fn();
  const handle = renderUi(<QuickSetup client={client} onTeamCreated={onTeamCreated} />);
  try {
    await flushAsync();
    const codex = card(handle, "Codex");
    const select = codex.querySelector<HTMLSelectElement>("[data-testid=quick-setup-model]")!;
    // The whole catalog is on offer, and the CLI's configured model is what the card starts on.
    expect(select.value).toBe("gpt-6-astra");
    expect([...select.options].map(option => option.value)).toEqual(["gpt-6-astra", "gpt-5.6-luna", "gpt-5.6-sol", CUSTOM_MODEL_OPTION]);
    // Nothing about the pinned model blocks or warns any more.
    expect(codex.querySelector("[data-grade]")?.getAttribute("data-grade")).toBe("verified");
    expect(codex.querySelector(".mission-quick-setup-warning")).toBeNull();
    expect(codex.querySelector("[data-testid=quick-setup-experimental]")).toBeNull();
    expect(codex.querySelector(".mission-quick-setup-reason")).toBeNull();
    expect(button(codex, "missions.quickSetup.create").disabled).toBe(false);

    click(button(codex, "missions.quickSetup.create"));
    await flushAsync();
    expect(stub.bindingSave).toHaveBeenCalledTimes(1);
    expect(stub.bindingSave.mock.calls[0][0].binding).toMatchObject({ runtime: "codex", label: "Codex · gpt-6-astra", model_id: "gpt-6-astra", experimental_version: null });
    expect(stub.templateSave).toHaveBeenCalledTimes(1);
    const template = stub.templateSave.mock.calls[0][0].template;
    expect(template.label).toBe("Codex · gpt-6-astra");
    expect(template.role_bindings.map(entry => entry.role)).toEqual(allRoles);
    expect(template.policy.require_independent_review).toBe(true);
    expect(onTeamCreated).toHaveBeenCalledTimes(1);

    // Back on the pinned model nothing changes either.
    setValue(select, "gpt-5.6-luna");
    expect(codex.querySelector("[data-testid=quick-setup-experimental]")).toBeNull();
    expect(button(codex, "missions.quickSetup.create").disabled).toBe(false);

    // Manual entry is a mode: the select stays on it while an id is typed, even one the catalog names.
    setValue(select, CUSTOM_MODEL_OPTION);
    const manual = codex.querySelector<HTMLInputElement>("[data-testid=quick-setup-model-manual]")!;
    expect(manual.value).toBe("");
    expect(button(codex, "missions.quickSetup.create").disabled).toBe(true);
    setValue(manual, "gpt-5.6-luna");
    expect(select.value).toBe(CUSTOM_MODEL_OPTION);
    expect(codex.querySelector("[data-testid=quick-setup-model-manual]")).not.toBeNull();
    expect(button(codex, "missions.quickSetup.create").disabled).toBe(false);
  } finally { handle.unmount(); }
});

it("reuses a matching saved connection and team without saving either again", async () => {
  const existing = withVerifiedCapabilities({ ...newBinding("codex"), id: "binding-match", revision: "3", label: "My Codex",
    program: "/usr/local/bin/codex", provider_id: "openai", model_id: "gpt-5.6-luna" });
  // Same model through an API key is a different connection and must not be picked.
  const apiKey = { ...existing, id: "binding-api", auth_route: "api_key" as const,
    credential_ref: "keyring:11111111-1111-4111-8111-111111111111", endpoint_ref: "11111111-1111-4111-8111-111111111111" };
  const partialTeam: TeamTemplate = { id: "team-partial", revision: "1", label: "Mixed team", repository_id: null,
    role_bindings: allRoles.map(role => ({ role, primary_binding_id: role === "integrator" ? apiKey.id : existing.id, fallback_binding_ids: [] })),
    policy: missionPolicy([existing.id, apiKey.id]) };
  const team: TeamTemplate = { id: "team-match", revision: "2", label: "Existing team", repository_id: null,
    role_bindings: allRoles.map(role => ({ role, primary_binding_id: existing.id, fallback_binding_ids: [] })), policy: missionPolicy([existing.id]) };
  const { stub, client } = stubClient({
    runtimes: [detected({ runtime: "codex", proven_model_id: "gpt-5.6-luna", verified_roles: [...allRoles] })],
    bindings: [apiKey, existing], templates: [partialTeam, team],
  });
  const onTeamCreated = vi.fn();
  const onBindingSaved = vi.fn();
  const handle = renderUi(<QuickSetup client={client} onTeamCreated={onTeamCreated} onBindingSaved={onBindingSaved} />);
  try {
    await flushAsync();
    const codex = card(handle, "Codex");
    click(button(codex, "missions.quickSetup.create"));
    await flushAsync();
    expect(stub.bindingSave).not.toHaveBeenCalled();
    expect(stub.bindingProbe).toHaveBeenCalledWith({ binding_id: existing.id });
    // Reuse saves nothing, but the probe still stores a new revision.
    expect(onBindingSaved).toHaveBeenCalledTimes(1);
    expect(onBindingSaved.mock.calls[0][0]).toMatchObject({ id: existing.id, revision: "4" });
    expect(stub.templateSave).not.toHaveBeenCalled();
    expect(onTeamCreated).toHaveBeenCalledWith(team);
    expect(codex.querySelector("[role=status]")?.textContent).toBe(t("missions.quickSetup.done", { label: "Existing team" }));
  } finally { handle.unmount(); }
});

it.each(["ko", "en"] as const)("%s 설치 확인 후 역할 검증이 부족하면 팀을 만들지 않고 이유를 남긴다", async language => {
  useI18nStore.setState({ language });
  const { stub, client } = stubClient({
    runtimes: [detected({ runtime: "claude", version: "2.1.0", configured_model_id: "claude-opus-5", verified_roles: ["lead", "reviewer"] })],
    probe: binding => {
      const probed = withVerifiedCapabilities(binding);
      return { ...probed, capabilities: { ...probed.capabilities, scoped_write: { supported: false, reason_code: "capability_unverified" } } };
    },
  });
  const onTeamCreated = vi.fn();
  const onBindingSaved = vi.fn();
  const handle = renderUi(<QuickSetup client={client} onTeamCreated={onTeamCreated} onBindingSaved={onBindingSaved} />);
  try {
    await flushAsync();
    const claude = card(handle, "Claude Code");
    expect(claude.textContent).toContain(t("missions.quickSetup.grade.verified"));
    expect(claude.textContent).toContain(t("missions.quickSetup.verified.partial", { roles: "Lead, Reviewer" }));
    expect(claude.textContent).toContain(t("missions.quickSetup.verified.partialNote"));
    expect(claude.querySelector<HTMLSelectElement>("[data-testid=quick-setup-model]")!.value).toBe("claude-opus-5");
    // Without a proven model there is nothing to contradict.
    expect(claude.querySelector(".mission-quick-setup-warning")).toBeNull();
    click(button(claude, "missions.quickSetup.create"));
    await flushAsync();
    expect(stub.bindingSave).toHaveBeenCalledTimes(1);
    expect(stub.bindingSave.mock.calls[0][0].binding).toMatchObject({ runtime: "claude", provider_id: "anthropic", label: "Claude Code · claude-opus-5" });
    expect(stub.bindingProbe).toHaveBeenCalledTimes(1);
    expect(stub.templateList).not.toHaveBeenCalled();
    expect(stub.templateSave).not.toHaveBeenCalled();
    expect(onTeamCreated).not.toHaveBeenCalled();
    // Stopping before the team still reports the saved (and probed) connection.
    const savedId = stub.bindingSave.mock.calls[0][0].binding.id;
    expect(onBindingSaved.mock.calls.map(([binding]) => [binding.id, binding.revision])).toEqual([[savedId, "1"], [savedId, "2"]]);
    expect(claude.querySelector("[role=alert]")?.textContent).toBe(t("missions.quickSetup.rolesBlocked", { roles: "Builder, Integrator" }));
    expect(claude.querySelector("[role=status]")).toBeNull();
  } finally { handle.unmount(); }
});

it("stops with the installation reason when the probe does not verify the CLI", async () => {
  const { stub, client } = stubClient({
    runtimes: [detected({ runtime: "codex", proven_model_id: "gpt-5.6-luna", verified_roles: [...allRoles] })],
    installation: "timed_out",
  });
  const handle = renderUi(<QuickSetup client={client} />);
  try {
    await flushAsync();
    const codex = card(handle, "Codex");
    click(button(codex, "missions.quickSetup.create"));
    await flushAsync();
    expect(stub.bindingProbe).toHaveBeenCalledTimes(1);
    expect(stub.templateList).not.toHaveBeenCalled();
    expect(stub.templateSave).not.toHaveBeenCalled();
    expect(codex.querySelector("[role=alert]")?.textContent).toBe(t("missions.settings.probeStatus.timed_out"));
  } finally { handle.unmount(); }
});

it("disables creation for missing CLIs, OpenCode, and empty models while stating why", async () => {
  const { stub, client } = stubClient({ runtimes: [
    detected({ runtime: "codex", version: "0.160.0", login: "not_found", verified_roles: [...allRoles] }),
    detected({ runtime: "claude", installation: "not_found", program: "", version: null, login: "not_found" }),
    detected({ runtime: "opencode", login: "unknown", configured_model_id: "glm-5.3" }),
  ] });
  const handle = renderUi(<QuickSetup client={client} />);
  try {
    await flushAsync();
    const codex = card(handle, "Codex");
    expect(codex.textContent).toContain(t("missions.quickSetup.grade.verified"));
    expect(codex.querySelector(".mission-quick-setup-login code")?.textContent).toBe("codex login");
    expect(codex.querySelector("input")!.value).toBe("");
    const codexCreate = button(codex, "missions.quickSetup.create");
    expect(codexCreate.disabled).toBe(true);
    // Installed but no model: the reason sits next to the disabled button.
    const reason = codex.querySelector<HTMLElement>(".mission-quick-setup-reason");
    expect(reason?.textContent).toBe(t("missions.quickSetup.modelRequired"));
    expect(codexCreate.getAttribute("aria-describedby")).toBe(reason?.id);
    setValue(codex.querySelector("input")!, "some-model");
    // Creation stays possible; the probe decides whether this model's roles can run.
    expect(button(codex, "missions.quickSetup.create").disabled).toBe(false);
    expect(codex.querySelector(".mission-quick-setup-reason")).toBeNull();
    expect(button(codex, "missions.quickSetup.create").hasAttribute("aria-describedby")).toBe(false);

    const claude = card(handle, "Claude Code");
    expect(claude.classList.contains("is-missing")).toBe(true);
    expect(claude.textContent).toContain(t("missions.quickSetup.grade.not_installed"));
    expect(claude.querySelector("input")!.disabled).toBe(true);
    const missing = button(claude, "missions.quickSetup.create");
    expect(missing.disabled).toBe(true);
    // Missing CLIs and OpenCode already say why; no model prompt on top.
    expect(claude.querySelector(".mission-quick-setup-reason")).toBeNull();
    click(missing);

    const opencode = card(handle, "OpenCode");
    expect(opencode.textContent).toContain(t("missions.quickSetup.opencodeManual"));
    expect(opencode.querySelector("input")).toBeNull();
    const manual = button(opencode, "missions.quickSetup.create");
    expect(manual.disabled).toBe(true);
    expect(opencode.querySelector(".mission-quick-setup-reason")).toBeNull();
    click(manual);
    await flushAsync();
    expect(stub.bindingList).not.toHaveBeenCalled();
    expect(stub.bindingSave).not.toHaveBeenCalled();
  } finally { handle.unmount(); }
});

it("renders only the cards in compact mode and points OpenCode to settings", async () => {
  const { client } = stubClient({ runtimes: [detected({ runtime: "opencode", login: "unknown" })] });
  const handle = renderUi(<QuickSetup client={client} compact />);
  try {
    await flushAsync();
    expect(handle.container.querySelector("h3")).toBeNull();
    expect(handle.container.textContent).not.toContain(t("missions.quickSetup.intro"));
    expect(card(handle, "OpenCode").textContent).toContain(t("missions.quickSetup.opencodeManualSettings"));
    expect(button(handle.container, "missions.quickSetup.redetect").disabled).toBe(false);
  } finally { handle.unmount(); }
});

it("shows a detection failure and recovers through search again", async () => {
  const { stub, client } = stubClient({ runtimes: [detected({ runtime: "codex", proven_model_id: "gpt-5.6-luna", verified_roles: [...allRoles] })] });
  stub.runtimeDetect.mockRejectedValueOnce(new Error("daemon unavailable"));
  const handle = renderUi(<QuickSetup client={client} />);
  try {
    expect(handle.container.textContent).toContain(t("missions.quickSetup.detecting"));
    expect(button(handle.container, "missions.quickSetup.redetect").disabled).toBe(true);
    await flushAsync();
    expect(handle.container.querySelector("[role=alert]")?.textContent).toBe(t("missions.quickSetup.detectFailed", { error: "daemon unavailable" }));
    expect(handle.container.querySelector("[role=group]")).toBeNull();
    expect(handle.container.textContent).not.toContain(t("missions.quickSetup.detecting"));
    const retry = button(handle.container, "missions.quickSetup.redetect");
    expect(retry.disabled).toBe(false);
    click(retry);
    await flushAsync();
    expect(stub.runtimeDetect).toHaveBeenCalledTimes(2);
    expect(handle.container.querySelector("[role=alert]")).toBeNull();
    expect(card(handle, "Codex").textContent).toContain(t("missions.quickSetup.grade.verified"));
  } finally { handle.unmount(); }
});

it.each(["ko", "en"] as const)("%s 호환 등급마다 배지를 보이고, 동의할 수 없는 미검증 버전은 팀 만들기를 막는다", async language => {
  useI18nStore.setState({ language });
  const { stub, client } = stubClient({ runtimes: [
    detected({ runtime: "codex", version: "0.161.0", grade: "same_line_unverified", proven_model_id: "gpt-5.6-luna", experimental_roles: ["lead"] }),
    detected({ runtime: "claude", version: "2.1.300", grade: "unverified", configured_model_id: "claude-opus-5" }),
  ] });
  const handle = renderUi(<QuickSetup client={client} />);
  try {
    await flushAsync();
    const codex = card(handle, "Codex");
    expect(codex.querySelector("[data-grade]")?.getAttribute("data-grade")).toBe("same_line_unverified");
    expect(codex.textContent).toContain(t("missions.quickSetup.grade.same_line_unverified"));
    // experimental_roles has no Builder: nothing to consent to.
    expect(codex.querySelector("[data-testid=quick-setup-experimental-consent]")).toBeNull();
    expect(codex.querySelector("[data-testid=quick-setup-experimental-unavailable]")?.textContent).toBe(t("missions.quickSetup.experimental.unavailable"));
    expect(button(codex, "missions.quickSetup.create").disabled).toBe(true);

    const claude = card(handle, "Claude Code");
    expect(claude.textContent).toContain(t("missions.quickSetup.grade.unverified"));
    expect(claude.querySelector("[data-testid=quick-setup-experimental-consent]")).toBeNull();
    expect(claude.querySelector("[data-testid=quick-setup-experimental-unavailable]")).not.toBeNull();
    const create = button(claude, "missions.quickSetup.create");
    expect(create.disabled).toBe(true);
    click(create);
    await flushAsync();
    expect(stub.bindingList).not.toHaveBeenCalled();
  } finally { handle.unmount(); }
});

it.each(["ko", "en"] as const)("%s 미검증 버전은 실험적 사용에 동의해야 팀을 만들고, 동의한 버전을 연결에 저장한다", async language => {
  useI18nStore.setState({ language });
  const { stub, client } = stubClient({
    runtimes: [detected({ runtime: "claude", version: "2.1.300", grade: "unverified", configured_model_id: "claude-opus-5", experimental_roles: [...allRoles] })],
    probe: binding => withCapabilities(binding, binding.experimental_version === "2.1.300" ? "experimental_opt_in" : "no_compatibility_evidence"),
  });
  const onTeamCreated = vi.fn();
  const handle = renderUi(<QuickSetup client={client} onTeamCreated={onTeamCreated} />);
  try {
    await flushAsync();
    const claude = card(handle, "Claude Code");
    const consent = claude.querySelector<HTMLInputElement>("[data-testid=quick-setup-experimental-consent]")!;
    expect(consent).not.toBeNull();
    expect(consent.checked).toBe(false);
    // The risk statement is shown up front, naming the version the consent is for.
    const risk = claude.querySelector<HTMLElement>(".mission-quick-setup-experimental-risk")!;
    expect(risk.textContent).toBe(t("missions.quickSetup.experimental.risk", { version: "2.1.300" }));
    expect(consent.getAttribute("aria-describedby")).toBe(risk.id);
    expect(claude.querySelector("[data-testid=quick-setup-experimental-quick-only]")).toBeNull();
    const create = button(claude, "missions.quickSetup.create");
    expect(create.disabled).toBe(true);
    const reason = claude.querySelector<HTMLElement>(".mission-quick-setup-reason")!;
    expect(reason.textContent).toBe(t("missions.quickSetup.experimental.consentRequired"));
    expect(create.getAttribute("aria-describedby")).toBe(reason.id);

    click(consent);
    await flushAsync(2);
    expect(consent.checked).toBe(true);
    expect(button(claude, "missions.quickSetup.create").disabled).toBe(false);
    click(button(claude, "missions.quickSetup.create"));
    await flushAsync();
    expect(stub.bindingSave).toHaveBeenCalledTimes(1);
    expect(stub.bindingSave.mock.calls[0][0].binding).toMatchObject({ runtime: "claude", model_id: "claude-opus-5", experimental_version: "2.1.300" });
    expect(stub.templateSave).toHaveBeenCalledTimes(1);
    const template = stub.templateSave.mock.calls[0][0].template;
    expect(template.role_bindings.map(entry => entry.role)).toEqual(allRoles);
    expect(template.policy.require_independent_review).toBe(true);
    expect(template.label).toBe("Claude Code · claude-opus-5");
    expect(onTeamCreated).toHaveBeenCalledTimes(1);

    // Withdrawing the consent locks creation again.
    click(consent);
    await flushAsync(2);
    expect(button(claude, "missions.quickSetup.create").disabled).toBe(true);
  } finally { handle.unmount(); }
});

it("Lead·Builder만 실험적으로 쓸 수 있으면 빠른 모드 전용임을 알리고 리뷰 생략 팀을 만든다", async () => {
  const { stub, client } = stubClient({
    runtimes: [detected({ runtime: "codex", version: "0.161.0", grade: "same_line_unverified", proven_model_id: "gpt-5.6-luna", experimental_roles: ["lead", "builder"] })],
    probe: binding => withCapabilities(binding, "experimental_opt_in"),
  });
  const onTeamCreated = vi.fn();
  const handle = renderUi(<QuickSetup client={client} onTeamCreated={onTeamCreated} />);
  try {
    await flushAsync();
    const codex = card(handle, "Codex");
    expect(codex.querySelector("[data-testid=quick-setup-experimental-quick-only]")?.textContent).toBe(t("missions.quickSetup.experimental.quickOnly"));
    click(codex.querySelector("[data-testid=quick-setup-experimental-consent]")!);
    await flushAsync(2);
    click(button(codex, "missions.quickSetup.create"));
    await flushAsync();
    expect(stub.bindingSave.mock.calls[0][0].binding.experimental_version).toBe("0.161.0");
    const template = stub.templateSave.mock.calls[0][0].template;
    // Reviewer/Integrator are not in experimental_roles even though the probe opened their capabilities.
    expect(template.role_bindings.map(entry => entry.role)).toEqual(["lead", "builder"]);
    expect(template.policy.require_independent_review).toBe(false);
    expect(template.policy.allowed_roles).toEqual(["lead", "builder"]);
    expect(template.label).toBe(t("missions.quickSetup.reviewSkippedLabel", { label: "Codex · gpt-5.6-luna" }));
    expect(onTeamCreated).toHaveBeenCalledTimes(1);
  } finally { handle.unmount(); }
});

it("이미 동의한 연결은 CLI가 업데이트돼도 다시 동의를 기록하지 않는다", async () => {
  // 동의는 연결당 한 번(11 §3.4): 저장된 값이 관측 버전과 달라도 그대로 쓴다.
  const existing: Binding = { ...newBinding("claude"), id: "binding-old", revision: "5", label: "Claude", program: "/usr/local/bin/claude",
    provider_id: "anthropic", model_id: "claude-opus-5", experimental_version: "2.1.200" };
  const { stub, client } = stubClient({
    runtimes: [detected({ runtime: "claude", version: "2.1.300", grade: "unverified", configured_model_id: "claude-opus-5", experimental_roles: [...allRoles] })],
    bindings: [existing],
    // 자가 진단도 동의도 이 기능들을 열지 못한 경우: 팀은 만들지 않는다.
    probe: binding => ({ ...binding, runtime_version: "2.1.301" }),
  });
  const handle = renderUi(<QuickSetup client={client} />);
  try {
    await flushAsync();
    const claude = card(handle, "Claude Code");
    click(claude.querySelector("[data-testid=quick-setup-experimental-consent]")!);
    await flushAsync(2);
    click(button(claude, "missions.quickSetup.create"));
    await flushAsync();
    expect(stub.bindingSave).not.toHaveBeenCalled();
    expect(stub.bindingProbe).toHaveBeenCalledWith({ binding_id: "binding-old" });
    expect(stub.templateSave).not.toHaveBeenCalled();
    expect(claude.querySelector("[role=alert]")?.textContent).toBe(t("missions.quickSetup.experimental.rolesBlocked", { roles: "Lead, Builder, Reviewer, Integrator" }));
  } finally { handle.unmount(); }
});

it("아직 동의가 없는 저장 연결에는 동의를 한 번 기록한다", async () => {
  const existing: Binding = { ...newBinding("claude"), id: "binding-fresh", revision: "5", label: "Claude", program: "/usr/local/bin/claude",
    provider_id: "anthropic", model_id: "claude-opus-5" };
  const { stub, client } = stubClient({
    runtimes: [detected({ runtime: "claude", version: "2.1.300", grade: "unverified", configured_model_id: "claude-opus-5", experimental_roles: [...allRoles] })],
    bindings: [existing],
    probe: binding => withCapabilities(binding, "experimental_opt_in"),
  });
  const onTeamCreated = vi.fn();
  const handle = renderUi(<QuickSetup client={client} onTeamCreated={onTeamCreated} />);
  try {
    await flushAsync();
    const claude = card(handle, "Claude Code");
    click(claude.querySelector("[data-testid=quick-setup-experimental-consent]")!);
    await flushAsync(2);
    click(button(claude, "missions.quickSetup.create"));
    await flushAsync();
    expect(stub.bindingSave).toHaveBeenCalledTimes(1);
    expect(stub.bindingSave.mock.calls[0][0]).toMatchObject({ expected_revision: "5", binding: { id: "binding-fresh", experimental_version: "2.1.300" } });
    expect(onTeamCreated).toHaveBeenCalledTimes(1);
  } finally { handle.unmount(); }
});

it.each(["ko", "en"] as const)("%s 이 PC에서 확인된 등급은 동의 없이 팀을 만든다", async language => {
  useI18nStore.setState({ language });
  const { stub, client } = stubClient({
    // 출시 증거는 없지만 데몬의 자가 진단이 네 역할을 열었다(11 §7 verified_locally).
    runtimes: [detected({ runtime: "codex", version: "0.161.0", grade: "verified_locally", configured_model_id: "gpt-5.6-luna",
      experimental_roles: [...allRoles] })],
    probe: binding => withCapabilities(binding, "local_probe"),
  });
  const onTeamCreated = vi.fn();
  const handle = renderUi(<QuickSetup client={client} onTeamCreated={onTeamCreated} />);
  try {
    await flushAsync();
    const codex = card(handle, "Codex");
    expect(codex.querySelector("[data-grade]")?.getAttribute("data-grade")).toBe("verified_locally");
    expect(codex.textContent).toContain(t("missions.quickSetup.grade.verified_locally"));
    expect(codex.querySelector(".mission-quick-setup-badge")?.classList.contains("is-verified")).toBe(true);
    // 이미 확인된 등급이라 동의 UI 자체가 없다.
    expect(codex.querySelector("[data-testid=quick-setup-experimental]")).toBeNull();
    expect(codex.querySelector("[data-testid=quick-setup-experimental-unavailable]")).toBeNull();
    click(button(codex, "missions.quickSetup.create"));
    await flushAsync();
    expect(stub.bindingSave.mock.calls[0][0].binding).toMatchObject({ model_id: "gpt-5.6-luna", experimental_version: null });
    expect(stub.templateSave.mock.calls[0][0].template.role_bindings.map(entry => entry.role)).toEqual(allRoles);
    expect(onTeamCreated).toHaveBeenCalledTimes(1);
  } finally { handle.unmount(); }
});

it("Z.ai 경로는 Claude Code 옆의 별도 카드로 서고, 출시 증거 없는 경로임을 밝힌 뒤 연결을 저장한다", async () => {
  // Z.ai는 CLI가 아니라 Claude Code가 서비스하는 두 번째 경로라 detect에는 줄이 없다. 카드는
  // Claude Code 줄에서 파생된다 — 같은 실행 파일·버전이고 제공자와 모델만 다르다.
  const { stub, client } = stubClient({
    runtimes: [detected({ runtime: "claude", version: "2.1.277", configured_model_id: "opus", verified_roles: [...allRoles],
      experimental_roles: [...allRoles], alt_models: [{ provider_id: "zai-coding-plan", models: [{ id: "glm-5.3", efforts: [] }] }] })],
    probe: binding => withCapabilities(binding, binding.experimental_version === "2.1.277" ? "experimental_opt_in" : "no_compatibility_evidence"),
  });
  const onTeamCreated = vi.fn();
  const handle = renderUi(<QuickSetup client={client} onTeamCreated={onTeamCreated} />);
  try {
    await flushAsync();
    // Anthropic 카드는 그 경로가 실행할 수 있는 id만 남긴다 — GLM은 옆 카드의 몫이다.
    const anthropic = card(handle, "Claude Code");
    expect([...anthropic.querySelector<HTMLSelectElement>("[data-testid=quick-setup-model]")!.options].map(option => option.value))
      .toEqual(["opus", CUSTOM_MODEL_OPTION]);
    expect(anthropic.querySelector("[data-grade]")?.getAttribute("data-grade")).toBe("verified");
    const zai = card(handle, "Claude Code · Z.ai Coding Plan");
    expect(zai.getAttribute("data-provider")).toBe("zai-coding-plan");
    expect(zai.textContent).toContain("/usr/local/bin/claude");
    expect(zai.textContent).toContain(t("missions.quickSetup.version", { version: "2.1.277" }));
    expect(zai.textContent).toContain(t("missions.quickSetup.zaiRoute"));
    // 데스크톱 브리지가 없으면 키 상태를 물을 수 없다 — 모른다고 말할 뿐 막지는 않는다.
    expect(zai.textContent).toContain(t("missions.quickSetup.zaiKey.unknown"));
    expect(zai.textContent).not.toContain(t("missions.quickSetup.login.found"));
    // 출시 증거는 provider·인증 경로로 묶인다: 같은 CLI가 Anthropic 쪽에서 검증됐어도 이 경로는 아니다.
    expect(zai.querySelector("[data-grade]")?.getAttribute("data-grade")).toBe("unverified");
    expect(zai.querySelector<HTMLElement>(".mission-quick-setup-experimental-risk")!.textContent)
      .toBe(t("missions.quickSetup.experimental.zaiRisk"));
    // Coding Plan 고유 id가 먼저, 데몬이 관측한 id가 뒤에 — 한 번도 안 돌려 본 경로도 고를 것이 있다.
    const select = zai.querySelector<HTMLSelectElement>("[data-testid=quick-setup-model]")!;
    expect([...select.options].map(option => option.value))
      .toEqual(["glm-5.3[1m]", "glm-5.3-flash[1m]", "glm-5.3", CUSTOM_MODEL_OPTION]);
    expect(select.value).toBe("glm-5.3[1m]");
    setValue(select, "glm-5.3");
    const create = button(zai, "missions.quickSetup.create");
    expect(create.disabled).toBe(true);
    click(zai.querySelector("[data-testid=quick-setup-experimental-consent]")!);
    click(create);
    await flushAsync();
    // 저장되는 연결은 키 스토어로 실행되는 Z.ai 경로 그대로이고, 동의한 버전을 달고 나간다.
    expect(stub.bindingSave.mock.calls[0][0].binding).toMatchObject({ runtime: "claude", provider_id: "zai-coding-plan",
      model_id: "glm-5.3", auth_route: "subscription", credential_ref: null, endpoint_ref: null, program: "/usr/local/bin/claude",
      label: "Claude Code · Z.ai Coding Plan · glm-5.3", experimental_version: "2.1.277" });
    expect(onTeamCreated).toHaveBeenCalledTimes(1);
    expect(zai.querySelector("[role=status]")?.textContent)
      .toBe(t("missions.quickSetup.done", { label: "Claude Code · Z.ai Coding Plan · glm-5.3" }));
    // 상태도 동의도 카드 단위다 — 옆의 Anthropic 카드는 그대로다.
    expect(anthropic.querySelector("[role=status]")).toBeNull();
    expect(anthropic.querySelector("[data-testid=quick-setup-experimental]")).toBeNull();
  } finally { handle.unmount(); }
});

it("Z.ai 키가 없으면 카드가 이유를 말하며 잠기고, 키를 등록하면 그 자리에서 풀린다", async () => {
  zaiKeyStore.configured = false;
  zaiKeyStore.reads = 0;
  const removeBridge = installTauriBridge();
  const { stub, client } = stubClient({ runtimes: [
    detected({ runtime: "claude", version: "2.1.277", configured_model_id: "opus", experimental_roles: [...allRoles],
      alt_models: [{ provider_id: "zai-coding-plan", models: [{ id: "glm-5.3", efforts: [] }] }] }),
  ] });
  const handle = renderUi(<QuickSetup client={client} />);
  try {
    await flushAsync();
    expect(zaiKeyStore.reads).toBe(1);
    const zai = card(handle, "Claude Code · Z.ai Coding Plan");
    expect(zai.textContent).toContain(t("missions.quickSetup.zaiKey.missing"));
    // 막는 이유는 키다 — 동의 여부는 그다음 문제이므로 그쪽 문구가 앞서지 않는다.
    const create = button(zai, "missions.quickSetup.create");
    expect(create.disabled).toBe(true);
    const reason = zai.querySelector<HTMLElement>(".mission-quick-setup-reason")!;
    expect(reason.textContent).toBe(t("missions.quickSetup.zaiKeyRequired"));
    expect(create.getAttribute("aria-describedby")).toBe(reason.id);
    click(create);
    await flushAsync();
    // 실행할 수 없는 연결은 저장조차 하지 않는다(데몬이 zai_key_missing으로 거절하기 전에 멈춘다).
    expect(stub.bindingList).not.toHaveBeenCalled();
    // 키는 같은 설정 화면 옆 카드에서 등록된다: 저장 알림 한 번이면 `다시 찾기` 없이 풀린다.
    zaiKeyStore.configured = true;
    window.dispatchEvent(new Event("iyagi-subscription-refresh"));
    await flushAsync();
    expect(zaiKeyStore.reads).toBe(2);
    expect(zai.textContent).toContain(t("missions.quickSetup.zaiKey.found"));
    expect(zai.querySelector<HTMLElement>(".mission-quick-setup-reason")!.textContent)
      .toBe(t("missions.quickSetup.experimental.consentRequired"));
  } finally { removeBridge(); handle.unmount(); }
});
