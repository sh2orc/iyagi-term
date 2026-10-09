import { useCallback, useEffect, useId, useRef, useState } from "react";
import type { DaemonClient } from "../daemon/client";
import type { Binding } from "../../generated/Binding";
import type { CompatibilityGrade } from "../../generated/CompatibilityGrade";
import type { DetectedRuntime } from "../../generated/DetectedRuntime";
import type { Role } from "../../generated/Role";
import type { RuntimeDetectResult } from "../../generated/RuntimeDetectResult";
import type { RuntimeKind } from "../../generated/RuntimeKind";
import type { TeamTemplate } from "../../generated/TeamTemplate";
import { useI18n, type MessageParams } from "../../i18n";
import { ZAI_MAIN_MODELS } from "../../store/preferences";
import { isTauri } from "../bridge/ipc";
import { SUBSCRIPTION_REFRESH_EVENT, getZaiKeyStatus } from "../subscriptions/client";
import { bindingSupportsRole } from "./bindingSupport";
import { CUSTOM_MODEL_OPTION, missionPolicy, newBinding, REVIEWED_TEAM_ROLES } from "./configuration";
import { errorText, newRequestId } from "./viewUtils";
import "./missions.css";
import "./createFlow.css";

const teamRoles: Role[]=[...REVIEWED_TEAM_ROLES];
const roleNames: Partial<Record<Role, string>>={lead:"Lead",builder:"Builder",reviewer:"Reviewer",integrator:"Integrator"};
export const runtimeNames: Record<RuntimeKind, string>={codex:"Codex",claude:"Claude Code",opencode:"OpenCode",fake:"Fake"};

/**
 * Claude Code's second provider route: the same executable pointed at Z.ai Coding Plan's
 * Anthropic-compatible endpoint and launched from the app's own key store — the mission
 * equivalent of `claude-exec --provider zai` (the `ccg` launch profile). Z.ai ships no CLI of
 * its own, so `runtime.detect` has no row to report for it and never will; this card is derived
 * from the Claude Code row, keeping its program, version and grade while swapping the provider —
 * and with it the model ids, which the Anthropic route would not accept.
 */
export const ZAI_PROVIDER_ID="zai-coding-plan";
const ZAI_PROVIDER_NAME="Z.ai Coding Plan";

/** One card: a detected CLI on one of the provider routes it serves. */
export interface SetupRoute {
  /** State key for the model pick, the consent, the busy flag and the status line. The route
   * detect itself suggested keeps the bare runtime name, so a re-detect leaves it untouched. */
  key: string;
  label: string;
  detected: DetectedRuntime;
  provider: string;
  /** The derived Z.ai route rather than the one detect suggested. */
  zai: boolean;
}

/**
 * Cards to render: detect's own order, each CLI followed by the extra routes it serves. Only an
 * installed Claude Code has one — an executable that was not found cannot run GLM either.
 */
export function setupRoutes(runtimes: DetectedRuntime[]): SetupRoute[] {
  return runtimes.flatMap(detected=>{
    const fallback=detected.runtime==="claude" ? "anthropic" : detected.runtime==="codex" ? "openai" : "";
    const own: SetupRoute={key:detected.runtime,label:runtimeNames[detected.runtime],detected,
      provider:detected.suggested_provider_id || fallback,zai:false};
    if (detected.runtime!=="claude" || detected.installation!=="verified" || !detected.program) return [own];
    return [own,{key:`claude:${ZAI_PROVIDER_ID}`,label:`${runtimeNames.claude} · ${ZAI_PROVIDER_NAME}`,
      detected,provider:ZAI_PROVIDER_ID,zai:true}];
  });
}

type Tone="progress" | "success" | "blocked" | "error";
/** Keys (not rendered text) survive a language switch while the message stays on screen. */
type Message={key: string; params?: MessageParams} | {text: string};
interface ActionStatus { route: string; tone: Tone; message: Message }

export interface QuickSetupProps {
  client: DaemonClient;
  /** Embedded use (e.g. the new-mission dialog): cards only, no heading or intro. */
  compact?: boolean;
  /** Marks this block as a settings row so 설정 검색 can scroll to it. */
  settingId?: string;
  onTeamCreated?: (template: TeamTemplate) => void;
  /**
   * The daemon stored this connection (right after saving, and again after the probe
   * records its result) — even when setup stops before a team is created.
   */
  onBindingSaved?: (binding: Binding) => void;
  /**
   * What `runtime.detect` reported, handed up after every successful (re-)detect so an
   * embedder can reuse it instead of running detection a second time — each run spawns
   * the installed CLIs. Not called when detection fails: this component owns that error.
   */
  onDetected?: (runtimes: DetectedRuntime[]) => void;
}

function defaultModel(route: SetupRoute): string {
  // configured_model_id is what the user actually set in the CLI — that's the model they mean to
  // use. proven_model_id is only a sort/default hint (11 §8): picking another model needs no
  // consent. With neither, the first advertised model keeps the picker on a real choice instead
  // of an empty manual entry. Both hints are read off the Anthropic route's own settings and
  // transcripts, so a card whose route cannot launch them falls through to its catalog rather
  // than opening on an id that fails at start.
  const choices=routeModelChoices(route);
  const hinted=[route.detected.configured_model_id,route.detected.proven_model_id]
    .find((id): id is string=>!!id && choices.includes(id));
  return hinted ?? choices[0] ?? "";
}

/**
 * Every model the card offers, in one order: the CLI's configured model, the one live evidence
 * pinned, then the runtime's advertised catalog — deduped. A daemon that predates `models`
 * advertises nothing, so the first two still stand on their own.
 */
export function modelChoices(detected: DetectedRuntime): string[] {
  const advertised=((detected as Partial<DetectedRuntime>).models ?? []).map(entry=>entry.id);
  return [...new Set([detected.configured_model_id,detected.proven_model_id,...advertised].filter((id): id is string=>!!id))];
}

/** Model ids Anthropic itself serves (`claude-*`, the CLI's aliases). Any other id a Claude Code
 * transcript shows answering belongs to another route — Z.ai Coding Plan in practice. Pickers that
 * build an Anthropic-route binding filter the observed list through this; the team template splits
 * the same list the other way to fill its Z.ai group. */
export const ANTHROPIC_ALIAS_MODELS=["opus","sonnet","haiku"];
/** Claude Code's context-window suffix (`opus[1m]`, `claude-fable-5-1[1m]`): the same model at a
 * larger context, never a different one — and `~/.claude/settings.json` stores it that way, so it
 * arrives as `configured_model_id`. Dropped before the alias compare, or a suffixed alias reads as
 * foreign and the shape split files it under Z.ai while the Anthropic group loses it. */
function withoutContextSuffix(id: string): string {
  return id.replace(/\[[^\]]*\]$/,"");
}
export function anthropicModelId(id: string): boolean {
  const base=withoutContextSuffix(id.trim());
  return base.startsWith("claude-")||ANTHROPIC_ALIAS_MODELS.includes(base);
}

/**
 * Models the Z.ai route offers: the Coding Plan's own ids first — a fixed product set, so the card
 * has real choices before this CLI has ever been run on that route — then the ids the daemon
 * observed for the route, then any foreign id an older daemon could only report inside `models`
 * (the same shape split the team-template picker does).
 */
export function zaiModelChoices(detected: DetectedRuntime): string[] {
  const advertised=(detected.alt_models ?? []).filter(alt=>alt.provider_id===ZAI_PROVIDER_ID)
    .flatMap(alt=>alt.models.map(entry=>entry.id));
  const observed=modelChoices(detected).filter(id=>!anthropicModelId(id));
  return [...new Set<string>([...ZAI_MAIN_MODELS,...advertised,...observed])];
}

/** Every model one card offers, narrowed to what that card's own route could actually launch. */
export function routeModelChoices(route: SetupRoute): string[] {
  if (route.zai) return zaiModelChoices(route.detected);
  // Only ids the Anthropic route serves: a foreign id (GLM through Z.ai) belongs to the card beside
  // this one, and here it would save a binding that cannot run.
  return route.detected.runtime==="claude" ? modelChoices(route.detected).filter(anthropicModelId)
    : modelChoices(route.detected);
}

/**
 * Compatibility grade from runtime.detect (contract A). A daemon that predates the
 * field is graded from the installation and the verified roles it reports.
 */
export function detectedGrade(detected: DetectedRuntime): CompatibilityGrade {
  const reported=(detected as Partial<DetectedRuntime>).grade;
  if (reported) return reported;
  if (detected.installation==="not_found") return "not_installed";
  return teamRoles.every(role=>detected.verified_roles.includes(role)) ? "verified" : "unverified";
}

/** Roles that pass the start gate once the user accepts this version experimentally. */
function experimentalRoles(detected: DetectedRuntime): Role[] {
  return (detected as Partial<DetectedRuntime>).experimental_roles ?? [];
}

export function isUnverifiedGrade(grade: CompatibilityGrade): boolean {
  return grade==="same_line_unverified" || grade==="unverified";
}

/**
 * The grade of one card. Shipped evidence — and the version line it carries — is keyed by provider
 * and auth route (`capability_evidence::shipped_route_matches`), so what detect recorded for the
 * Anthropic login says nothing about the Z.ai route: nobody ran that one live. The derived card
 * therefore stays unverified however well the CLI itself is graded, and is opened by this machine's
 * own self-check or the user's experimental consent — the same judgement the team-template picker
 * already makes for a Z.ai pick.
 */
export function routeGrade(route: SetupRoute): CompatibilityGrade {
  const grade=detectedGrade(route.detected);
  return route.zai && grade!=="not_installed" ? "unverified" : grade;
}

export interface QuickTeamShape {
  roles: Role[];
  /** false = quick mode: no reviewer, `require_independent_review=false` (contract B). */
  requireReview: boolean;
}

/**
 * The team a set of usable roles can form: all four roles → a reviewed team;
 * Lead and Builder → a quick-mode team without review (Integrator kept when usable);
 * otherwise none.
 */
export function quickTeamShape(available: readonly Role[]): QuickTeamShape | null {
  if (teamRoles.every(role=>available.includes(role))) return {roles:[...teamRoles],requireReview:true};
  if (available.includes("lead") && available.includes("builder")) {
    return {roles:available.includes("integrator") ? ["lead","builder","integrator"] : ["lead","builder"],requireReview:false};
  }
  return null;
}

/**
 * One-click team setup: detect installed CLIs, save (or reuse) one subscription
 * connection, probe it, and create (or reuse) a team only for roles whose automated
 * execution is verified — or, for an unverified CLI version, explicitly accepted as
 * experimental by the user. Nothing unverified is presented as ready.
 */
export function QuickSetup({client, compact=false, settingId, onTeamCreated, onBindingSaved, onDetected}: QuickSetupProps): JSX.Element {
  const {t}=useI18n();
  const [runtimes,setRuntimes]=useState<DetectedRuntime[]>([]);
  const [loading,setLoading]=useState(true),[detectError,setDetectError]=useState<string|null>(null);
  const [models,setModels]=useState<Partial<Record<string, string>>>({});
  /** Cards the user accepted for experimental use, holding the version observed at consent time
   * (display only — consent is per connection and does not expire when the CLI updates, 11 §3.4). */
  const [consents,setConsents]=useState<Partial<Record<string, string>>>({});
  const [busy,setBusy]=useState<string|null>(null),[status,setStatus]=useState<ActionStatus|null>(null);
  /** Whether the app key store holds a Z.ai key — the only credential the Z.ai route launches on
   * (claude/auth.rs reads it at start time, per connection nothing is stored). `null` means unasked,
   * which is also what a build without the desktop bridge stays on: the card then offers the route
   * without asserting a key state it cannot check. */
  const [zaiKey,setZaiKey]=useState<boolean|null>(null);
  const mounted=useRef(true),generation=useRef(0),inFlight=useRef(false);
  useEffect(()=>{mounted.current=true;return()=>{mounted.current=false;};},[]);
  // Kept in a ref, and synced by an effect declared before the detect effect below: an inline
  // callback from the parent would otherwise change `detect`'s identity every render and make
  // its mount effect re-detect in a loop.
  const reportDetected=useRef(onDetected);
  useEffect(()=>{reportDetected.current=onDetected;});

  const detect=useCallback(async()=>{
    const current=++generation.current;
    const live=()=>mounted.current && current===generation.current;
    setLoading(true);setDetectError(null);setStatus(null);
    try {
      // A client without the method rejects here instead of throwing during render.
      const result: RuntimeDetectResult=await Promise.resolve().then(()=>client.runtimeDetect());
      if (!live()) return;
      setRuntimes(result.runtimes);
      setModels(previous=>{
        const next: Partial<Record<string, string>>={};
        for (const route of setupRoutes(result.runtimes)) next[route.key]=previous[route.key] ?? defaultModel(route);
        return next;
      });
      reportDetected.current?.(result.runtimes);
    } catch (error) {
      if (!live()) return;
      setRuntimes([]);setDetectError(errorText(error));
    } finally {
      if (live()) setLoading(false);
    }
  },[client]);
  useEffect(()=>{void detect();},[detect]);
  // The key is registered in Settings → Integrations, outside this component and possibly after it
  // mounted, so the Z.ai card re-reads the store on every detect as well as on mount. A failure
  // leaves the state unknown rather than claiming the key is gone.
  const readZaiKey=useCallback(()=>{
    if (!isTauri()) return;
    void getZaiKeyStatus().then(
      status=>{if (mounted.current) setZaiKey(status.configured);},
      ()=>{if (mounted.current) setZaiKey(null);},
    );
  },[]);
  useEffect(()=>{readZaiKey();},[readZaiKey]);
  // The key is registered one card away, in the same settings screen: Z.ai Coding Plan announces
  // every save and removal on this event, so the card unlocks (or locks) where the user is looking
  // instead of waiting for a re-detect.
  useEffect(()=>{
    window.addEventListener(SUBSCRIPTION_REFRESH_EVENT,readZaiKey);
    return()=>window.removeEventListener(SUBSCRIPTION_REFRESH_EVENT,readZaiKey);
  },[readZaiKey]);

  const createTeam=async(route: SetupRoute)=>{
    const detected=route.detected,runtime=detected.runtime,model=(models[route.key] ?? "").trim();
    if (inFlight.current || loading || !model || detected.installation!=="verified" || runtime==="opencode") return;
    // The Z.ai route launches on the stored key alone: without one the daemon rejects the run with
    // `zai_key_missing`, so the card stops here instead of saving a connection that cannot run.
    if (route.zai && zaiKey===false) return;
    const grade=routeGrade(route);
    if (grade==="not_installed") return;
    // An unverified version runs only with the user's consent. A model other than the pinned one
    // needs none: the runtime's own RESULT_INVALID and plan repair judge model fitness (11 §8).
    const experimental=isUnverifiedGrade(grade);
    const experimentalVersion=experimental && detected.version && consents[route.key]===detected.version ? detected.version : null;
    if (experimental && experimentalVersion===null) return;
    const report=(tone: Tone, message: Message)=>{if (mounted.current) setStatus({route:route.key,tone,message});};
    inFlight.current=true;setBusy(route.key);
    try {
      const program=detected.program,provider=route.provider;
      const {bindings}=await client.bindingList();
      const existing=bindings.find(b=>b.runtime===runtime && b.program===program && b.provider_id===provider && b.model_id===model
        && b.auth_route==="subscription" && b.credential_ref===null && b.endpoint_ref===null && b.enabled);
      // Consent is per connection, once (11 §3.4): a stored consent stands whatever version it
      // names, so an updated CLI reuses the connection instead of asking again.
      const consented=!!existing?.experimental_version;
      let binding: Binding;
      if (existing && (experimentalVersion===null || consented)) {
        report("progress",{key:"missions.quickSetup.step.reuseBinding"});
        binding=existing;
      } else if (existing) {
        // Same connection, first consent: record it.
        report("progress",{key:"missions.quickSetup.step.saveBinding"});
        binding=(await client.bindingSave({request_id:newRequestId(),expected_revision:existing.revision,binding:{
          ...existing,experimental_version:experimentalVersion,
        }})).binding;
        if (mounted.current) onBindingSaved?.(binding);
      } else {
        report("progress",{key:"missions.quickSetup.step.saveBinding"});
        binding=(await client.bindingSave({request_id:newRequestId(),expected_revision:"0",binding:{
          ...newBinding(runtime),label:`${route.label} · ${model}`,program,provider_id:provider,model_id:model,
          auth_route:"subscription",credential_ref:null,endpoint_ref:null,enabled:true,experimental_version:experimentalVersion,
        }})).binding;
        if (mounted.current) onBindingSaved?.(binding);
      }
      report("progress",{key:"missions.quickSetup.step.probe"});
      const probe=await client.bindingProbe({binding_id:binding.id});
      // The probe stores its result (new revision) whether or not setup continues.
      if (mounted.current) onBindingSaved?.(probe.binding);
      if (probe.installation!=="verified") {
        report("blocked",{key:`missions.settings.probeStatus.${probe.installation}`});
        return;
      }
      // `supported: true` counts whether it comes from evidence or from the experimental opt-in.
      const supported=teamRoles.filter(role=>bindingSupportsRole(probe.binding,role));
      let shape: QuickTeamShape | null;
      if (experimentalVersion!==null) {
        const allowed=experimentalRoles(detected);
        const usable=supported.filter(role=>allowed.includes(role));
        shape=quickTeamShape(usable);
        if (!shape) {
          const blocked=teamRoles.filter(role=>!usable.includes(role));
          report("blocked",{key:"missions.quickSetup.experimental.rolesBlocked",params:{roles:blocked.map(role=>roleNames[role] ?? role).join(", ")}});
          return;
        }
      } else {
        const unverified=teamRoles.filter(role=>!supported.includes(role));
        if (unverified.length>0) {
          report("blocked",{key:"missions.quickSetup.rolesBlocked",params:{roles:unverified.map(role=>roleNames[role] ?? role).join(", ")}});
          return;
        }
        shape={roles:[...teamRoles],requireReview:true};
      }
      report("progress",{key:"missions.quickSetup.step.team"});
      const id=probe.binding.id;
      const plan=shape;
      const {templates}=await client.templateList({repository_id:null});
      let template=templates.find(candidate=>candidate.repository_id===null
        && plan.roles.every(role=>candidate.role_bindings.some(entry=>entry.role===role && entry.primary_binding_id===id))
        && (plan.requireReview || (candidate.policy.require_independent_review===false && !candidate.role_bindings.some(entry=>entry.role==="reviewer"))));
      if (!template) {
        const policy=missionPolicy([id]);
        template=(await client.templateSave({request_id:newRequestId(),expected_revision:"0",template:{
          id:newRequestId(),revision:"0",
          label:plan.requireReview ? probe.binding.label : t("missions.quickSetup.reviewSkippedLabel",{label:probe.binding.label}),
          repository_id:null,
          role_bindings:plan.roles.map(role=>({role,primary_binding_id:id,fallback_binding_ids:[]})),
          policy:plan.requireReview ? policy : {...policy,allowed_roles:[...plan.roles],require_independent_review:false},
        }})).template;
      }
      report("success",{key:"missions.quickSetup.done",params:{label:template.label}});
      if (mounted.current) onTeamCreated?.(template);
    } catch (error) {
      report("error",{text:errorText(error)});
    } finally {
      inFlight.current=false;
      if (mounted.current) setBusy(null);
    }
  };

  const render=(message: Message)=>"text" in message ? message.text : t(message.key,message.params);
  const routes=setupRoutes(runtimes);
  return <section className={`mission-quick-setup${compact?" is-compact":""}`} data-testid="mission-quick-setup" data-setting-id={settingId} aria-busy={loading || busy!==null}>
    {compact?null:<>
      <h3>{t("missions.quickSetup.title")}</h3>
      <p className="muted">{t("missions.quickSetup.intro")}</p>
    </>}
    <div className="mission-quick-setup-toolbar">
      {loading?<span className="muted">{t("missions.quickSetup.detecting")}</span>:null}
      <button type="button" disabled={loading || busy!==null} onClick={()=>{readZaiKey();void detect();}}>{t("missions.quickSetup.redetect")}</button>
    </div>
    {detectError?<p role="alert" className="mission-area-error">{t("missions.quickSetup.detectFailed",{error:detectError})}</p>:null}
    {routes.length>0?<div className="mission-quick-setup-cards">
      {routes.map(route=><RuntimeCard key={route.key} route={route} compact={compact} zaiKey={zaiKey}
        model={models[route.key] ?? ""} onModel={value=>setModels(previous=>({...previous,[route.key]:value}))}
        consented={!!route.detected.version && consents[route.key]===route.detected.version}
        onConsent={accepted=>setConsents(previous=>({...previous,[route.key]:accepted && route.detected.version ? route.detected.version : undefined}))}
        busy={busy!==null || loading} running={busy===route.key} onCreate={()=>void createTeam(route)}
        status={status?.route===route.key ? {tone:status.tone,text:render(status.message)} : null}/>)}
    </div>:null}
  </section>;
}

const gradeBadge: Record<CompatibilityGrade, {key: string; tone: "is-verified" | "is-partial" | "is-blocked"}>={
  verified:{key:"missions.quickSetup.grade.verified",tone:"is-verified"},
  // 이 PC의 자가 진단·성공 실행이 네 역할을 열었다 — 출시 증거와 같은 무게로 보여 준다(11 §8).
  verified_locally:{key:"missions.quickSetup.grade.verified_locally",tone:"is-verified"},
  same_line_unverified:{key:"missions.quickSetup.grade.same_line_unverified",tone:"is-partial"},
  unverified:{key:"missions.quickSetup.grade.unverified",tone:"is-blocked"},
  not_installed:{key:"missions.quickSetup.grade.not_installed",tone:"is-blocked"},
};

function RuntimeCard({route, compact, zaiKey, model, onModel, consented, onConsent, busy, running, onCreate, status}: {
  route: SetupRoute; compact: boolean; zaiKey: boolean | null; model: string; onModel: (value: string) => void;
  consented: boolean; onConsent: (accepted: boolean) => void;
  busy: boolean; running: boolean; onCreate: () => void; status: {tone: Tone; text: string} | null;
}): JSX.Element {
  const {t}=useI18n();
  const reasonId=useId(),riskId=useId();
  const detected=route.detected;
  const name=route.label;
  const grade=routeGrade(route);
  const missing=detected.installation==="not_found" || grade==="not_installed";
  const opencode=detected.runtime==="opencode";
  const verifiedRoles=teamRoles.filter(role=>detected.verified_roles.includes(role));
  const choices=routeModelChoices(route);
  // Manual entry is a mode, not a value: while it is on, the select stays on "enter manually" even
  // when the typed id happens to match a choice, so the text field does not vanish mid-word.
  const [manual,setManual]=useState(false);
  const manualEntry=choices.length===0 || manual || !choices.includes(model);
  const trimmed=model.trim();
  const installed=!missing && detected.installation==="verified";
  // Consent is needed only where this version has no evidence at all — a model other than the
  // pinned one is not a reason to ask (11 §8).
  const unverified=installed && !opencode && isUnverifiedGrade(grade);
  const experimentalShape=quickTeamShape(experimentalRoles(detected));
  const canConsent=unverified && experimentalShape!==null && !!detected.version;
  const needsConsent=unverified && !consented;
  // A key the store does not have is the one thing this card cannot fix on its own.
  const zaiBlocked=route.zai && zaiKey===false;
  const disabled=busy || missing || opencode || !installed || !trimmed || needsConsent || zaiBlocked || (unverified && !canConsent);
  // Installed and usable except for the model field: say so next to the disabled button.
  const needsModel=installed && !opencode && !trimmed && !needsConsent && !zaiBlocked;
  const consentMissing=canConsent && needsConsent;
  const withCommand=(key: string, command: string)=>{
    const [before,after=""]=t(key).split("{command}");
    return <>{before}<code>{command}</code>{after}</>;
  };
  // The Z.ai route ignores the CLI's Anthropic sign-in entirely — what decides whether it can run
  // is the stored key, so that is what the card reports in the same place.
  const login=route.zai ? (zaiKey===null ? t("missions.quickSetup.zaiKey.unknown")
      : zaiKey ? t("missions.quickSetup.zaiKey.found") : t("missions.quickSetup.zaiKey.missing"))
    : detected.login==="found" ? t("missions.quickSetup.login.found")
    : detected.login==="unknown" ? t("missions.quickSetup.login.unknown")
    : detected.runtime==="codex" ? withCommand("missions.quickSetup.login.not_found.codex","codex login")
    : detected.runtime==="claude" ? withCommand("missions.quickSetup.login.not_found.claude","claude")
    : t("missions.quickSetup.login.not_found.other");
  let badge: JSX.Element;
  if (missing) {
    badge=<p className="mission-quick-setup-badge is-blocked" data-grade="not_installed">{t(gradeBadge.not_installed.key)}</p>;
  } else if (detected.installation!=="verified") {
    badge=<p className="mission-quick-setup-badge is-blocked">{t(`missions.settings.probeStatus.${detected.installation}`)}</p>;
  } else {
    const {key,tone}=gradeBadge[grade];
    const partial=grade==="verified" && verifiedRoles.length>0 && verifiedRoles.length<teamRoles.length;
    badge=<div className={`mission-quick-setup-badge ${tone}`} data-grade={grade}>
      <p>{t(key)}</p>
      {partial?<>
        <p>{t("missions.quickSetup.verified.partial",{roles:verifiedRoles.map(role=>roleNames[role] ?? role).join(", ")})}</p>
        <p>{t("missions.quickSetup.verified.partialNote")}</p>
      </>:null}
    </div>;
  }
  const version=detected.version ?? t("missions.quickSetup.versionUnknown");
  return <div role="group" aria-label={name} data-runtime={detected.runtime} data-provider={route.provider}
    className={`mission-quick-setup-runtime${missing?" is-missing":""}`}>
    <div className="mission-quick-setup-head">
      <strong>{name}</strong>
      {missing?null:<span className="muted">{detected.version ? t("missions.quickSetup.version",{version:detected.version}) : t("missions.quickSetup.versionUnknown")}</span>}
    </div>
    {detected.program?<code className="mission-quick-setup-path">{detected.program}</code>:null}
    {route.zai?<p className="muted mission-quick-setup-route">{t("missions.quickSetup.zaiRoute")}</p>:null}
    {missing?null:<p className="mission-quick-setup-login">{login}</p>}
    {badge}
    {opencode ? <p className="muted">{t(compact ? "missions.quickSetup.opencodeManualSettings" : "missions.quickSetup.opencodeManual")}</p>
      : <label className="mission-quick-setup-model">{t("missions.quickSetup.model")}
        {choices.length>0 ? <select value={manualEntry ? CUSTOM_MODEL_OPTION : model} disabled={busy || missing} data-testid="quick-setup-model"
          onChange={e=>{const picked=e.target.value;if (picked===CUSTOM_MODEL_OPTION) {setManual(true);onModel("");} else {setManual(false);onModel(picked);}}}>
          {choices.map(id=><option key={id} value={id}>{id===detected.proven_model_id ? t("missions.quickSetup.modelVerified",{model:id}) : id}</option>)}
          <option value={CUSTOM_MODEL_OPTION}>{t("missions.quickSetup.modelCustom")}</option>
        </select> : null}
        {manualEntry ? <input value={model} disabled={busy || missing} autoComplete="off" spellCheck={false} data-testid="quick-setup-model-manual"
          aria-label={t("missions.quickSetup.modelCustomInput")} onChange={e=>onModel(e.target.value)}/> : null}
      </label>}
    {unverified ? (canConsent && experimentalShape ? <div className="mission-quick-setup-experimental" data-testid="quick-setup-experimental">
      <label className="mission-quick-setup-experimental-toggle">
        <input type="checkbox" checked={consented} disabled={busy} aria-describedby={riskId}
          data-testid="quick-setup-experimental-consent" onChange={e=>onConsent(e.target.checked)}/>
        {t("missions.quickSetup.experimental.use")}
      </label>
      <p id={riskId} className="mission-quick-setup-experimental-risk">
        {route.zai ? t("missions.quickSetup.experimental.zaiRisk") : t("missions.quickSetup.experimental.risk",{version})}</p>
      {experimentalShape.requireReview ? null
        : <p className="mission-quick-setup-experimental-note" data-testid="quick-setup-experimental-quick-only">{t("missions.quickSetup.experimental.quickOnly")}</p>}
    </div> : <p className="mission-quick-setup-reason" data-testid="quick-setup-experimental-unavailable">{t("missions.quickSetup.experimental.unavailable")}</p>) : null}
    <button type="button" disabled={disabled} aria-busy={running} aria-describedby={needsModel || consentMissing || zaiBlocked?reasonId:undefined} onClick={onCreate}>{t("missions.quickSetup.create")}</button>
    {zaiBlocked?<p id={reasonId} className="mission-quick-setup-reason">{t("missions.quickSetup.zaiKeyRequired")}</p>
      :needsModel?<p id={reasonId} className="mission-quick-setup-reason">{t("missions.quickSetup.modelRequired")}</p>
      :consentMissing?<p id={reasonId} className="mission-quick-setup-reason">{t("missions.quickSetup.experimental.consentRequired")}</p>:null}
    {status?<p role={status.tone==="blocked" || status.tone==="error" ? "alert" : "status"}
      className={`mission-quick-setup-status is-${status.tone}`}>{status.text}</p>:null}
  </div>;
}
