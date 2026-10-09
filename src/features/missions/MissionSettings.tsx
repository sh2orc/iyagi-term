import { useCallback, useEffect, useRef, useState, type ReactNode } from "react";
import type { DaemonClient } from "../daemon/client";
import type { Binding } from "../../generated/Binding";
import type { TeamTemplate } from "../../generated/TeamTemplate";
import type { ProbeModel } from "../../generated/ProbeModel";
import type { DetectedRuntime } from "../../generated/DetectedRuntime";
import type { Role } from "../../generated/Role";
import type { RuntimeKind } from "../../generated/RuntimeKind";
import type { VerificationCommand } from "../../generated/VerificationCommand";
import type { RepositoryInspectResult } from "../../generated/RepositoryInspectResult";
import { useI18n } from "../../i18n";
import { errorText, newRequestId } from "./viewUtils";
import { CUSTOM_MODEL_OPTION, dollarsToMicros, missionPolicy, newBinding, parseCommandArgs, validConnectionRefs } from "./configuration";
import { microsToDollars } from "./costs";
import { MissionGuide } from "./MissionGuide";
import { QuickSetup, anthropicModelId, detectedGrade, isUnverifiedGrade, modelChoices, runtimeNames, zaiModelChoices } from "./QuickSetup";
import { bindingSupportsRole, bindingTrust, localEvidenceSummary, type BindingTrust } from "./bindingSupport";
import { roleLabel } from "./labels";
import "./missions.css";
import "./createFlow.css";

const roles: Role[]=["lead","builder","reviewer","integrator"];
/** Value of the disabled row shown while a route has no stored connection to launch on. */
const BLOCKED_CHOICE="blocked:needs-connection";

/**
 * A role pick that is not a saved connection yet: a detected model, with the provider route it runs
 * on, that the team save turns into one. Routes that launch only on stored credentials — OpenCode,
 * and Claude Code on every provider but its own Anthropic login — name the connection to reuse.
 */
interface NewConnectionChoice { runtime: RuntimeKind; provider: string; model: string; from?: string }
function choiceValue(choice: NewConnectionChoice): string { return JSON.stringify(choice); }
/** Saved connections are picked by id; a new-connection pick is its JSON, which no id starts with. */
function parseChoice(value: string): NewConnectionChoice | null {
  if (!value.startsWith("{")) return null;
  try {
    const parsed=JSON.parse(value) as Partial<NewConnectionChoice>;
    return typeof parsed.runtime==="string" && typeof parsed.provider==="string" && typeof parsed.model==="string" ? parsed as NewConnectionChoice : null;
  } catch { return null; }
}
/** One route's candidates in the role pickers: a label, its models, or a blocked row with a hint. */
interface ProviderGroup { key: string; label: string; choices: NewConnectionChoice[]; blocked: boolean; hintKey: string | null }
/** The launch target quick setup and the team save share: runtime, program, provider, model, route and credentials. */
function sameTarget(a: Binding, b: Binding): boolean {
  return a.runtime===b.runtime && a.program===b.program && a.provider_id===b.provider_id && a.model_id===b.model_id
    && a.auth_route===b.auth_route && a.credential_ref===b.credential_ref && a.endpoint_ref===b.endpoint_ref;
}

const TRUST_TONE: Record<BindingTrust, "is-verified" | "is-partial" | "is-blocked">=
  {shipped:"is-verified",local:"is-verified",experimental:"is-partial",unverified:"is-blocked"};

/**
 * 연결 하나의 신뢰 칩과 그 근거 한 줄(11 §8) — 필수 기능이 어느 증거 층으로 열렸는지 하나로
 * 말하고, 데몬이 이 PC에서 측정한 조각(프로토콜·sandbox·성공 실행·확인 시각)만 그 아래 잇는다.
 * 모델이 CLI 목록에 없다는 경고는 알리기만 하고 막지 않는다.
 */
function TrustChip({binding}:{binding: Binding}): JSX.Element {
  const {t,language}=useI18n();
  const trust=bindingTrust(binding);
  const summary=localEvidenceSummary(binding);
  const when=(at: string): string=>{
    const date=new Date(at);
    return Number.isFinite(date.getTime()) ? date.toLocaleString(language) : at;
  };
  const reasons=summary ? [
    // 진단을 안 한 것(runs만 있음)과 진단이 실패한 것(local_probe_failed)은 다르게 보인다 —
    // 후자는 `지금 확인`을 다시 눌러도 바뀌지 않으므로 그 사실을 알려야 한다.
    summary.probed ? t(summary.protocolOk ? "missions.trust.protocolOk" : "missions.trust.protocolFailed") : null,
    summary.sandboxPassed!==null && summary.sandboxTotal!==null ? t("missions.trust.sandbox",{passed:summary.sandboxPassed,total:summary.sandboxTotal}) : null,
    summary.runs>0 ? t("missions.trust.runs",{count:summary.runs}) : null,
    summary.probedAt ? t("missions.trust.probedAt",{at:when(summary.probedAt)}) : null,
  ].filter((piece): piece is string=>piece!==null) : [];
  return <div className="mission-trust" data-testid="mission-binding-trust" data-trust={trust}>
    <p className={`mission-trust-chip ${TRUST_TONE[trust]}`}>{t(`missions.trust.${trust}`)}
      {binding.experimental_version
        ?<span className="muted"> · {t("missions.settings.experimentalVersion",{version:binding.experimental_version})}</span>:null}</p>
    {reasons.length>0?<p className="mission-trust-reason" data-testid="mission-binding-trust-reason">{reasons.join(" · ")}</p>:null}
    {summary?.modelListed===false
      ?<p className="mission-quick-setup-warning" data-testid="mission-binding-model-unlisted">{t("missions.trust.modelNotListed")}</p>:null}
  </div>;
}

/**
 * 설정 한 칸 — 라벨 → 보조 설명 → 컨트롤 순서로 쌓아 모든 항목이 같은 리듬을 갖게 한다.
 * 라벨은 반드시 label의 첫 자식이어야 한다(DOM 시험이 그 텍스트로 항목을 찾는다).
 *
 * span  = 한 줄을 통째로 쓴다(경로·참조처럼 값이 긴 항목).
 * short = 줄은 통째로 쓰되 입력칸만 좁게 둔다(금액·짧은 경로).
 */
function Field({label, hint, span, short, children}:{
  label: string; hint?: string; span?: boolean; short?: boolean; children: ReactNode;
}): JSX.Element {
  return <label className={`mission-field${span?" is-wide":""}${short?" is-short":""}`}>
    <span className="mission-field-label">{label}</span>
    {hint?<span className="mission-field-hint">{hint}</span>:null}
    {children}
  </label>;
}

export function MissionSettings({client}:{client:DaemonClient}): JSX.Element {
  const {t}=useI18n();
  const [bindings,setBindings]=useState<Binding[]>([]),[templates,setTemplates]=useState<TeamTemplate[]>([]);
  const [binding,setBinding]=useState<Binding>(()=>newBinding());
  const [estimatedCost,setEstimatedCost]=useState("");
  useEffect(()=>{setEstimatedCost(binding.estimated_run_cost_usd_micros ? microsToDollars(BigInt(binding.estimated_run_cost_usd_micros)) : "");},[binding.id,binding.revision]);
  const [teamName,setTeamName]=useState(""),[team,setTeam]=useState<Record<string,string>>({});
  const [repositoryPath,setRepositoryPath]=useState("");
  const [repository,setRepository]=useState<RepositoryInspectResult|null>(null),[commands,setCommands]=useState<VerificationCommand[]>([]);
  const [commandTitle,setCommandTitle]=useState(""),[program,setProgram]=useState(""),[args,setArgs]=useState(""),[cwd,setCwd]=useState("");
  const [busy,setBusy]=useState(false),[error,setError]=useState<string|null>(null),[notice,setNotice]=useState<string|null>(null);
  // Models a connection's runtime advertised to probe(), keyed by binding id — the exact list for that connection.
  const [probedModels,setProbedModels]=useState<Record<string,ProbeModel[]>>({});
  // What runtime.detect reported per runtime, by way of quick setup's detection (see
  // `runtimesDetected`): model candidates exist before (and without) a probe, and the CLI's own
  // configured and evidence-pinned models come along even when the advertised list omits them.
  const [detectedRuntimes,setDetectedRuntimes]=useState<Partial<Record<Binding["runtime"], DetectedRuntime>>>({});
  // Manual entry is a mode, not a value: the select stays on "enter manually" while it is on, so a
  // typed id that happens to match a catalog entry does not make the text field vanish mid-word.
  const [modelManual,setModelManual]=useState(false);
  // One consent for every new connection the roles name that would run experimentally.
  const [teamConsent,setTeamConsent]=useState(false);
  // Reasoning effort per distinct pick (choiceValue → effort); empty means the runtime default.
  const [pickEfforts,setPickEfforts]=useState<Record<string,string>>({});
  // OpenCode connections whose catalog the role pickers already asked a probe for (once each).
  const catalogRequested=useRef(new Set<string>());
  useEffect(()=>{let alive=true;void Promise.all([client.bindingList(),client.templateList({repository_id:null})]).then(([b,t])=>{if(alive){setBindings(b.bindings);setTemplates(t.templates);}},e=>{if(alive)setError(errorText(e));});return()=>{alive=false;};},[client]);
  const mounted=useRef(true);
  useEffect(()=>{mounted.current=true;return()=>{mounted.current=false;};},[]);
  // Quick setup writes through the daemon; re-read so the sections below show its connection and team.
  const reloadSaved=useCallback(()=>{void Promise.all([client.bindingList(),client.templateList({repository_id:null})]).then(([b,list])=>{if(mounted.current){setBindings(b.bindings);setTemplates(list.templates);}},e=>{if(mounted.current)setError(errorText(e));});},[client]);
  // A connection quick setup saved stays listed even when setup stops before creating a team.
  const bindingSaved=useCallback((saved: Binding)=>{if(mounted.current)setBindings(list=>list.some(b=>b.id===saved.id)?list.map(b=>b.id===saved.id?saved:b):[...list,saved]);},[]);
  // Quick setup above already ran runtime.detect for its own cards, so this screen reuses that
  // result rather than issuing a second one (every run spawns the installed CLIs). Candidates are
  // a convenience: a failed detect never calls back, the lists stay empty, and nothing errors here.
  const runtimesDetected=useCallback((detected: DetectedRuntime[])=>{
    if (!mounted.current) return;
    const byRuntime: Partial<Record<Binding["runtime"], DetectedRuntime>>={};
    for (const runtime of detected) byRuntime[runtime.runtime]=runtime;
    setDetectedRuntimes(byRuntime);
  },[]);
  const perform=async(action:()=>Promise<void>)=>{setBusy(true);setError(null);setNotice(null);try{await action();}catch(e){setError(errorText(e));}finally{setBusy(false);}};
  const saveBinding=()=>perform(async()=>{
    if (!binding.label.trim() || !binding.program.trim() || !binding.model_id.trim()) throw new Error(t("missions.settings.bindingRequired"));
    if (!validConnectionRefs(binding)) throw new Error(t("missions.settings.connectionInvalid"));
    let estimate: string | null;
    try { estimate=dollarsToMicros(estimatedCost); } catch { throw new Error(t("missions.create.costInvalid")); }
    const {binding:saved}=await client.bindingSave({request_id:newRequestId(),expected_revision:binding.revision,binding:{...binding,estimated_run_cost_usd_micros:estimate}});
    setBinding(saved);setBindings(list=>[...list.filter(b=>b.id!==saved.id),saved]);setNotice(t("missions.settings.saved"));
  });
  const savedBinding=bindings.find(b=>b.id===binding.id);
  const bindingDirty=savedBinding!=null && (JSON.stringify(savedBinding)!==JSON.stringify(binding)
    || estimatedCost!==(savedBinding.estimated_run_cost_usd_micros ? microsToDollars(BigInt(savedBinding.estimated_run_cost_usd_micros)) : ""));
  // Record the one consent this connection needs; its value is the version observed at the time
  // (display only — an updated CLI does not ask again, 11 §3.4).
  const recordExperimentalConsent=()=>perform(async()=>{
    if (!savedBinding?.runtime_version) return;
    const {binding:saved}=await client.bindingSave({request_id:newRequestId(),expected_revision:savedBinding.revision,
      binding:{...savedBinding,experimental_version:savedBinding.runtime_version}});
    setBinding(saved);setBindings(list=>list.map(b=>b.id===saved.id?saved:b));setNotice(t("missions.settings.saved"));
  });
  const probe=()=>perform(async()=>{
    const result=await client.bindingProbe({binding_id:binding.id});
    setBinding(result.binding);setBindings(list=>list.map(b=>b.id===result.binding.id?result.binding:b));
    setProbedModels(m=>({...m,[result.binding.id]:result.models ?? []}));
    setNotice(t(`missions.settings.probeStatus.${result.installation}`));
  });
  const enabledBindings=bindings.filter(b=>b.enabled);
  // Connections that may lend their stored credentials to a new pick. OpenCode always launches on
  // stored credentials, and so does Claude Code on every route but its own Anthropic login — the
  // mission equivalent of the terminal's `ccg` profile (claude-exec --provider zai).
  const refSources=enabledBindings.filter(b=>b.credential_ref!==null && b.endpoint_ref!==null);
  const providerName=(provider: string): string=>{
    const key=`missions.provider.${provider}`;
    const translated=t(key);
    return translated===key ? provider : translated;
  };
  /** A route that launches only on stored credentials: OpenCode. Claude Code's Z.ai route launches
   * on the daemon's own key store — the ccg key — with no stored connection. */
  const needsSource=(choice: NewConnectionChoice): boolean=>choice.runtime==="opencode";
  const draftFor=(choice: NewConnectionChoice): Binding | null=>{
    const detectedRuntime=detectedRuntimes[choice.runtime];
    const source=choice.from ? refSources.find(b=>b.id===choice.from && b.runtime===choice.runtime && b.provider_id===choice.provider) : undefined;
    const usable=needsSource(choice) ? !!source : !!detectedRuntime && detectedRuntime.installation==="verified" && !!detectedRuntime.program;
    if (!usable) return null;
    const base=newBinding(choice.runtime);
    const offLogin=choice.provider!==detectedRuntime?.suggested_provider_id;
    return {...base,
      label:offLogin ? `${runtimeNames[choice.runtime]} · ${providerName(choice.provider)} · ${choice.model}` : `${runtimeNames[choice.runtime]} · ${choice.model}`,
      program:source?.program ?? detectedRuntime?.program ?? "",provider_id:choice.provider,model_id:choice.model,
      auth_route:source?.auth_route ?? "subscription",credential_ref:source?.credential_ref ?? null,endpoint_ref:source?.endpoint_ref ?? null,enabled:true,experimental_version:null};
  };
  // Consent is needed wherever the registry claims nothing: the stored-credential OpenCode route and
  // Claude Code's Z.ai key-store route (no evidence of their own), or an unverified CLI version. A
  // model other than the evidence-pinned one is not a reason to ask (11 §8).
  const choiceNeedsConsent=(choice: NewConnectionChoice): boolean=>{
    const detectedRuntime=detectedRuntimes[choice.runtime];
    if (!detectedRuntime) return true;
    return needsSource(choice) || (choice.runtime==="claude" && choice.provider==="zai-coding-plan")
      || isUnverifiedGrade(detectedGrade(detectedRuntime));
  };
  const choiceVersion=(choice: NewConnectionChoice): string | null=>
    detectedRuntimes[choice.runtime]?.version ?? (choice.from ? refSources.find(b=>b.id===choice.from)?.runtime_version ?? null : null);
  // Effort rungs a pick's model advertises: a runtime pick reads detect's catalog, a connection
  // pick reads its probe. Nothing advertised means no select — the runtime default stands.
  const pickEffortOptions=(choice: NewConnectionChoice): string[]=>{
    const fromDetect=((detectedRuntimes[choice.runtime] as Partial<DetectedRuntime>|undefined)?.models ?? []).find(model=>model.id===choice.model)?.efforts ?? [];
    const fromProbe=choice.from ? (probedModels[choice.from] ?? []).find(model=>model.id===choice.model)?.efforts ?? [] : [];
    return [...new Set([...fromProbe,...fromDetect])];
  };
  // The pickers group candidates by the route that would run them: a runtime's own provider first,
  // then a second provider the same CLI serves (Claude Code → Z.ai Coding Plan), then OpenCode's
  // per-connection catalogs. A model that already has a connection stays listed — picking it reuses
  // that connection at save time instead of making a second one.
  const dedupeValid=(choices: NewConnectionChoice[]): NewConnectionChoice[]=>{
    const seen=new Set<string>();const out: NewConnectionChoice[]=[];
    for (const choice of choices) {
      if (seen.has(choiceValue(choice))) continue;
      if (!draftFor(choice)) continue;
      seen.add(choiceValue(choice));out.push(choice);
    }
    return out;
  };
  const groupLabel=(runtime: RuntimeKind, provider: string): string=>{
    const suggested=detectedRuntimes[runtime]?.suggested_provider_id;
    return provider===suggested ? runtimeNames[runtime] : `${runtimeNames[runtime]} · ${providerName(provider)}`;
  };
  const groups: ProviderGroup[]=[];
  for (const runtime of ["codex","claude"] as RuntimeKind[]) {
    const detectedRuntime=detectedRuntimes[runtime];
    const provider=detectedRuntime?.suggested_provider_id ?? (runtime==="claude" ? "anthropic" : "openai");
    if (!(detectedRuntime && detectedRuntime.installation==="verified" && detectedRuntime.program)) continue;
    const listed=modelChoices(detectedRuntime);
    if (runtime==="claude") {
      // Split the ids the CLI was seen using by route shape: `claude-*`/alias ids belong to the
      // Anthropic login, anything else to the Z.ai key-store route — which also carries the Coding
      // Plan's own ids, so the group is pickable before a run has put one in a transcript. The
      // daemon narrows the observed ids per provider once it reports alt_models; the shape split
      // keeps older daemons working too. Quick setup offers exactly this list on its Z.ai card.
      const zaiIds=new Set<string>(zaiModelChoices(detectedRuntime));
      // A saved Z.ai connection's probe may name ids detect missed; its credentials are not needed
      // to launch, so the pick does not borrow them.
      for (const source of refSources.filter(b=>b.runtime==="claude" && b.provider_id==="zai-coding-plan")) {
        for (const entry of probedModels[source.id] ?? []) zaiIds.add(entry.id);
      }
      const own=dedupeValid(listed.filter(anthropicModelId).map(model=>({runtime,provider,model})));
      if (own.length>0) groups.push({key:`${runtime}:${provider}`,label:groupLabel(runtime,provider),choices:own,blocked:false,hintKey:null});
      const zaiChoices=dedupeValid([...zaiIds].map(model=>({runtime,provider:"zai-coding-plan",model})));
      if (zaiChoices.length>0) groups.push({key:"claude:zai-coding-plan",label:groupLabel(runtime,"zai-coding-plan"),choices:zaiChoices,blocked:false,hintKey:"missions.settings.claudeZaiTeamHint"});
    } else {
      const own=dedupeValid(listed.map(model=>({runtime,provider,model})));
      if (own.length>0) groups.push({key:`${runtime}:${provider}`,label:groupLabel(runtime,provider),choices:own,blocked:false,hintKey:null});
    }
  }
  {
    // OpenCode has no CLI listing in detect at all: its catalogs belong to saved connections, and
    // each connection's models come from its probe (or from detect when the provider matches).
    const detectedRuntime=detectedRuntimes.opencode;
    const sources=refSources.filter(b=>b.runtime==="opencode");
    const raw=new Map<string, NewConnectionChoice>();
    for (const source of sources) {
      const fromDetect=detectedRuntime?.suggested_provider_id===source.provider_id ? ((detectedRuntime as Partial<DetectedRuntime>|undefined)?.models ?? []).map(entry=>entry.id) : [];
      for (const model of new Set([...(probedModels[source.id] ?? []).map(entry=>entry.id),...fromDetect])) {
        const choice: NewConnectionChoice={runtime:"opencode",provider:source.provider_id,model,from:source.id};
        if (!raw.has(`${source.provider_id}:${model}`)) raw.set(`${source.provider_id}:${model}`,choice);
      }
    }
    const blocked=sources.length===0 && detectedRuntime?.installation==="verified";
    const choices=dedupeValid([...raw.values()]);
    if (choices.length>0 || blocked) groups.push({key:"opencode",label:runtimeNames.opencode,choices,blocked,hintKey:blocked?"missions.settings.opencodeTeamHint":null});
  }
  const assigned=roles.map(role=>team[role]??(role==="integrator"?team.builder:undefined)??"");
  const sameModelForAll=assigned[0]&&assigned.every(id=>id===assigned[0])?assigned[0]:"";
  const selectedNew=[...new Set(assigned)].map(parseChoice).filter((choice): choice is NewConnectionChoice=>choice!==null);
  const consentChoices=selectedNew.filter(choiceNeedsConsent);
  const teamConsentNeeded=consentChoices.length>0;
  const saveTeam=()=>perform(async()=>{
    const selected: Record<string, string> = {...team, integrator: team.integrator ?? team.builder};
    if (!teamName.trim() || roles.some(role=>!selected[role])) throw new Error(t("missions.settings.teamRequired"));
    if (teamConsentNeeded && !teamConsent) throw new Error(t("missions.settings.teamConsentRequired"));
    // A picked model becomes a saved connection here — a distinct pick is created once, and a pick
    // whose connection already exists reuses it (consent and effort applied through its CAS when
    // they changed) — and is probed the way quick setup probes its own, so the team names
    // connections the start gate can judge.
    let known=bindings;
    const resolved=new Map<string, Binding>();
    const ids: Record<string, string>={};
    const unsupported: Role[]=[];
    for (const role of roles) {
      const value=selected[role];
      const choice=parseChoice(value);
      if (!choice) {
        if (!known.some(b=>b.id===value && b.enabled)) throw new Error(t("missions.settings.teamRequired"));
        ids[role]=value;
        continue;
      }
      let connection=resolved.get(value);
      if (!connection) {
        const draft=draftFor(choice);
        if (!draft) throw new Error(t("missions.settings.teamRequired"));
        const version=choiceVersion(choice);
        const wantConsent=choiceNeedsConsent(choice) ? version : null;
        const wantEffort=pickEfforts[value]?.trim() || null;
        const existing=known.find(b=>b.enabled && sameTarget(b,draft));
        let current=existing ?? null;
        if (current) {
          // 동의는 연결당 한 번(11 §3.4): 이미 기록돼 있으면 값이 달라도 다시 쓰지 않는다.
          const consentChanged=wantConsent!==null && !current.experimental_version;
          const effortChanged=wantEffort!==null && current.effort!==wantEffort;
          if (consentChanged || effortChanged) {
            const {binding:saved}=await client.bindingSave({request_id:newRequestId(),expected_revision:current.revision,
              binding:{...current,experimental_version:consentChanged?wantConsent:current.experimental_version,effort:effortChanged?wantEffort:current.effort}});
            known=[...known.filter(b=>b.id!==saved.id),saved];current=saved;
          }
        } else {
          const {binding:saved}=await client.bindingSave({request_id:newRequestId(),expected_revision:"0",
            binding:{...draft,effort:wantEffort,experimental_version:wantConsent}});
          known=[...known.filter(b=>b.id!==saved.id),saved];current=saved;
        }
        const probe=await client.bindingProbe({binding_id:current.id});
        const probed=probe.binding;
        known=[...known.filter(b=>b.id!==probed.id),probed];
        setProbedModels(m=>({...m,[probed.id]:probe.models ?? []}));
        resolved.set(value,probed);
        connection=probed;
      }
      ids[role]=connection.id;
      if (!bindingSupportsRole(connection,role)) unsupported.push(role);
    }
    setBindings(known);
    const role_bindings=roles.map(role=>({role,primary_binding_id:ids[role],fallback_binding_ids:[]}));
    const {template}=await client.templateSave({request_id:newRequestId(),expected_revision:"0",template:{id:newRequestId(),revision:"0",label:teamName,repository_id:null,role_bindings,policy:missionPolicy([...new Set(Object.values(ids))])}});
    setTemplates(list=>[...list,template]);setTeamName("");
    // The picks now name the connections they resolved to, so the selects keep showing them.
    if (resolved.size>0) setTeam(Object.fromEntries(roles.map(role=>[role,ids[role]])));
    setNotice(unsupported.length>0
      ? t("missions.settings.teamSavedUnsupported",{roles:unsupported.map(role=>roleLabel(t,role)).join(", ")})
      : t("missions.settings.saved"));
  });
  // The role pickers list an OpenCode connection's other models, which only a probe can name (detect does
  // not list OpenCode): ask once per connection, only while OpenCode is installed here, and only when
  // detect did not already list that provider.
  const opencodeInstalled=detectedRuntimes.opencode?.installation==="verified";
  useEffect(()=>{
    if (!opencodeInstalled) return;
    const listedByDetect=((detectedRuntimes.opencode as Partial<DetectedRuntime>|undefined)?.models ?? []).length>0;
    for (const source of bindings) {
      if (!source.enabled || source.runtime!=="opencode" || source.credential_ref===null || source.endpoint_ref===null) continue;
      if (probedModels[source.id] || catalogRequested.current.has(source.id)) continue;
      if (listedByDetect && detectedRuntimes.opencode?.suggested_provider_id===source.provider_id) continue;
      catalogRequested.current.add(source.id);
      void client.bindingProbe({binding_id:source.id}).then(result=>{
        if (!mounted.current) return;
        setProbedModels(m=>({...m,[result.binding.id]:result.models ?? []}));
        setBindings(list=>list.map(b=>b.id===result.binding.id?result.binding:b));
        // The form edits a copy: keep its revision current unless the user already changed something on it.
        setBinding(current=>current.id===result.binding.id && JSON.stringify(current)===JSON.stringify(source) ? result.binding : current);
      },()=>{/* a catalog is a convenience: the connection stays listed without one */});
    }
  },[bindings,probedModels,detectedRuntimes,opencodeInstalled,client]);
  const renderTeamOptions=()=><>
    <option value="">{t("missions.settings.selectModel")}</option>
    {enabledBindings.length>0?<optgroup label={t("missions.settings.savedConnections")}>
      {enabledBindings.map(b=><option key={b.id} value={b.id}>{b.label} · {b.model_id}</option>)}
    </optgroup>:null}
    {groups.filter(group=>group.choices.length>0 || group.blocked).map(group=><optgroup key={group.key} label={group.label}>
      {group.blocked
        ? <option value={BLOCKED_CHOICE} disabled>{t("missions.settings.opencodeNeedsConnection")}</option>
        : group.choices.map(choice=><option key={choiceValue(choice)} value={choiceValue(choice)}>{choice.model}</option>)}
    </optgroup>)}
  </>;
  const inspect=()=>perform(async()=>{const repo=await client.repositoryInspect({path:repositoryPath.trim()});setRepository(repo);const result=await client.verificationList({repository_id:repo.repository_id});setCommands(result.commands);});
  const saveCommand=()=>perform(async()=>{
    if (!repository || !commandTitle.trim() || !program.trim()) throw new Error(t("missions.settings.commandRequired"));
    let argv:string[];try{argv=parseCommandArgs(args);}catch{throw new Error(t("missions.settings.argsInvalid"));}
    const {command}=await client.verificationSave({request_id:newRequestId(),expected_revision:"0",command:{id:newRequestId(),revision:"0",title:commandTitle,repository_id:repository.repository_id,program:program.trim(),argv,cwd_relative:cwd.trim(),timeout_ms:300000,env_profile_ref:null,allowed_network:false}});
    setCommands(list=>[...list,command]);setCommandTitle("");setNotice(t("missions.settings.saved"));
  });
  const detected=detectedRuntimes[binding.runtime];
  // Both sources merge: probe first (exact for this connection), then what detect saw for its runtime.
  // A daemon that predates the field advertises nothing instead of breaking the list.
  const modelOptions=[...(probedModels[binding.id]??[]),...((detected as Partial<DetectedRuntime>|undefined)?.models ?? [])].filter((model,index,list)=>list.findIndex(other=>other.id===model.id)===index);
  const storedModelId=binding.model_id.trim();
  // Every id the select can name: the merged catalog plus the CLI's configured and evidence-pinned
  // models, which the advertised list may omit (Claude's `opus[1m]`, for one).
  const catalogIds=[...new Set([...modelOptions.map(model=>model.id),detected?.configured_model_id,detected?.proven_model_id].filter((id): id is string=>!!id))];
  // A stored id the catalog does not name stays selectable — the select must not drop it silently.
  const modelFieldChoices=!modelManual && storedModelId && !catalogIds.includes(storedModelId) ? [storedModelId,...catalogIds] : catalogIds;
  const manualModel=catalogIds.length===0 || modelManual;
  const modelSelectValue=modelManual ? CUSTOM_MODEL_OPTION : modelFieldChoices.includes(storedModelId) ? storedModelId : "";
  // A stored effort the runtime no longer advertises stays selectable — the select must not drop it silently.
  const effortOptions=[...new Set([...(modelOptions.find(model=>model.id===storedModelId)?.efforts ?? []),...(binding.effort?[binding.effort]:[])])];
  // Consent is a user statement per connection, asked once (11 §3.4). Ask for it after a probe has
  // recorded the version and neither the release evidence nor this machine's own self-check opened
  // a single role. An updated CLI never re-asks — the start gate sends the user back to 지금 확인.
  const consentMissing=savedBinding!=null && savedBinding.enabled && !!savedBinding.runtime_version
    && !savedBinding.experimental_version && !roles.some(role=>bindingSupportsRole(savedBinding,role));
  return <div className="mission-settings">
    <h2>{t("settings.missions")}</h2>
    {/* 설정 검색(schema.ts missionQuickSetup·missionGuide)이 스크롤할 자리 — 각 블록이 직접 표식을 단다. */}
    <QuickSetup client={client} settingId="missionQuickSetup" onTeamCreated={reloadSaved} onBindingSaved={bindingSaved} onDetected={runtimesDetected} />
    <MissionGuide settingId="missionGuide" />
    {/* 카드 아래쪽 저장 버튼을 눌러도 결과가 보이도록 스크롤 위쪽에 붙여 둔다. */}
    {error||notice?<div className="mission-settings-status">
      {error?<p role="alert" className="mission-area-error" data-testid="mission-settings-error">{error}</p>:null}
      {notice?<p role="status" className="mission-settings-ok" data-testid="mission-settings-notice">{notice}</p>:null}
    </div>:null}

    <section className="mission-settings-card" data-setting-id="missionModels">
      <header className="mission-settings-card-head">
        <h3>{t("missions.settings.models")}</h3>
        <p>{t("settings.item.missionModels.description")}</p>
      </header>
      <details className="mission-settings-advanced" data-testid="mission-model-advanced">
        <summary>{t("missions.settings.advanced")}</summary>
        <div className="mission-settings-advanced-body">
          <div className="mission-field-grid">
            <Field label={t("missions.settings.savedModels")} span>
              <select aria-label={t("missions.settings.savedModels")} value={binding.revision==="0"?"":binding.id} disabled={busy} onChange={e=>{setModelManual(false);setBinding(bindings.find(b=>b.id===e.target.value)??newBinding());}}>
                <option value="">{t("missions.settings.newModel")}</option>
                {bindings.map(b=><option key={b.id} value={b.id}>{b.label} · {b.model_id}</option>)}
              </select>
            </Field>
            <Field label={t("missions.settings.label")}>
              <input value={binding.label} disabled={busy} onChange={e=>setBinding({...binding,label:e.target.value})}/>
            </Field>
            <Field label={t("missions.settings.runtime")}>
              <select aria-label={t("missions.settings.runtime")} value={binding.runtime} disabled={busy} onChange={e=>{setModelManual(false);setBinding({...newBinding(e.target.value as Binding["runtime"]),label:binding.label});}}>
                <option value="codex">Codex</option><option value="claude">Claude Code</option><option value="opencode">OpenCode</option>
              </select>
            </Field>
            <Field label={t("missions.settings.program")} span>
              <input value={binding.program} disabled={busy} onChange={e=>setBinding({...binding,program:e.target.value})}/>
            </Field>
            <Field label={t("missions.settings.provider")}>
              <input value={binding.provider_id} disabled={busy} onChange={e=>setBinding({...binding,provider_id:e.target.value})}/>
            </Field>
            <Field label={t("missions.settings.model")}>
              {catalogIds.length>0 ? <select aria-label={t("missions.settings.model")} value={modelSelectValue} disabled={busy}
                onChange={e=>{const picked=e.target.value;if (picked===CUSTOM_MODEL_OPTION) {setModelManual(true);setBinding({...binding,model_id:""});} else {setModelManual(false);setBinding({...binding,model_id:picked});}}}>
                <option value="">{t("missions.settings.selectModel")}</option>
                {modelFieldChoices.map(id=><option key={id} value={id}>{id}</option>)}
                <option value={CUSTOM_MODEL_OPTION}>{t("missions.settings.modelCustom")}</option>
              </select> : null}
              {manualModel ? <input aria-label={t("missions.settings.modelCustomInput")} value={binding.model_id} disabled={busy} autoComplete="off" spellCheck={false} onChange={e=>setBinding({...binding,model_id:e.target.value})}/> : null}
            </Field>
            {effortOptions.length>0?<Field label={t("missions.settings.effort")}>
              <select aria-label={t("missions.settings.effort")} value={binding.effort??""} disabled={busy} onChange={e=>setBinding({...binding,effort:e.target.value||null})}>
                <option value="">{t("missions.settings.effortDefault")}</option>
                {effortOptions.map(effort=><option key={effort} value={effort}>{effort}</option>)}
              </select>
            </Field>:null}
            <Field label={t("missions.settings.auth")}>
              <select aria-label={t("missions.settings.auth")} value={binding.auth_route} disabled={busy} onChange={e=>setBinding({...binding,auth_route:e.target.value as Binding["auth_route"],...((binding.runtime==="codex"&&e.target.value!=="api_key")||binding.runtime==="claude"?{credential_ref:null,endpoint_ref:null}:{})})}>
                {["subscription","api_key","local","custom"].map(route=><option key={route} value={route}>{t(`missions.settings.auth.${route}`)}</option>)}
              </select>
            </Field>
            <Field label={t("missions.settings.estimatedCost")} hint={t("missions.settings.estimatedCostNote")} span short>
              <input inputMode="decimal" value={estimatedCost} disabled={busy} onChange={e=>setEstimatedCost(e.target.value)}/>
            </Field>
            {binding.runtime==="opencode"||binding.runtime==="claude"||(binding.runtime==="codex"&&binding.auth_route==="api_key")?<>
              <Field label={t("missions.settings.credentialRef")} span>
                <input autoComplete="off" spellCheck={false} value={binding.credential_ref??""} disabled={busy} onChange={e=>setBinding({...binding,credential_ref:e.target.value.trim()||null})}/>
              </Field>
              <Field label={t("missions.settings.endpointRef")} span>
                <input autoComplete="off" spellCheck={false} value={binding.endpoint_ref??""} disabled={busy} onChange={e=>setBinding({...binding,endpoint_ref:e.target.value.trim()||null})}/>
              </Field>
              <p className="mission-field-note">{t(binding.runtime==="codex"?"missions.settings.codexApiHint":binding.runtime==="claude"?"missions.settings.claudeAuthHint":"missions.settings.connectionHint")}</p>
            </>:<p className="mission-field-note">{t(binding.runtime==="codex"?"missions.settings.codexLoginHint":"missions.settings.authHint")}</p>}
          </div>
          <div className="mission-settings-actions">
            <button type="button" className="primary" disabled={busy} onClick={()=>void saveBinding()}>{t("missions.settings.saveModel")}</button>
            <button type="button" disabled={busy||binding.revision==="0"||bindingDirty} onClick={()=>void probe()}>{t("missions.settings.probeNow")}</button>
            {bindingDirty?<span className="mission-settings-actions-note">{t("missions.settings.probeSaveFirst")}</span>:null}
          </div>
          {binding.runtime_version||savedBinding?<div className="mission-settings-meta">
            {binding.runtime_version?<p>{t("missions.settings.version",{version:binding.runtime_version})}</p>:null}
            {savedBinding?<TrustChip binding={savedBinding}/>:null}
            {savedBinding && consentMissing?<div className="mission-settings-reconsent" role="status" data-testid="mission-binding-consent">
              <p>{t("missions.settings.experimentalConsent",{model:savedBinding.model_id,version:savedBinding.runtime_version ?? ""})}</p>
              <p className="mission-quick-setup-experimental-risk">{t("missions.quickSetup.experimental.risk",{version:savedBinding.runtime_version ?? ""})}</p>
              <button type="button" disabled={busy||bindingDirty} onClick={()=>void recordExperimentalConsent()} data-testid="mission-binding-consent-agree">{t("missions.settings.experimentalAgree")}</button>
            </div>:null}
          </div>:null}
        </div>
      </details>
    </section>

    <section className="mission-settings-card" data-setting-id="missionTeams">
      <header className="mission-settings-card-head">
        <h3>{t("missions.settings.teams")}</h3>
        <p>{t("settings.item.missionTeams.description")}</p>
      </header>
      <Field label={t("missions.settings.allRoles")} hint={t("missions.settings.allRolesHint")} span>
        <select aria-label={t("missions.settings.allRoles")} value={sameModelForAll} disabled={busy} onChange={e=>setTeam(e.target.value?Object.fromEntries(roles.map(role=>[role,e.target.value])):{})}>
          {renderTeamOptions()}
        </select>
      </Field>
      <Field label={t("missions.settings.teamName")} span short>
        <input value={teamName} disabled={busy} onChange={e=>setTeamName(e.target.value)}/>
      </Field>
      <div className="mission-settings-block">
        <h4>{t("missions.settings.rolesTitle")}</h4>
        <div className="mission-field-grid">
          {roles.map(role=><Field key={role} label={roleLabel(t,role)}>
            <select aria-label={roleLabel(t,role)} value={team[role]??(role==="integrator"?team.builder:"")??""} disabled={busy} onChange={e=>setTeam({...team,[role]:e.target.value})}>
              {renderTeamOptions()}
            </select>
          </Field>)}
        </div>
        <p className="mission-field-note">{t("missions.settings.rolesNote")}</p>
        {groups.flatMap(group=>group.hintKey?[<p key={group.key} className="mission-field-note" data-testid={`team-hint-${group.key}`}>{t(group.hintKey)}</p>]:[])}
      </div>
      {selectedNew.some(choice=>pickEffortOptions(choice).length>0)?<div className="mission-settings-block">
        <h4>{t("missions.settings.pickEffortsTitle")}</h4>
        <div className="mission-field-grid">
          {selectedNew.filter(choice=>pickEffortOptions(choice).length>0).map(choice=>{
            const value=choiceValue(choice);
            const label=`${runtimeNames[choice.runtime]} · ${choice.model}`;
            return <Field key={value} label={label} short>
              <select aria-label={`${label} ${t("missions.settings.effort")}`} value={pickEfforts[value]??""} disabled={busy}
                onChange={e=>setPickEfforts(previous=>({...previous,[value]:e.target.value}))}>
                <option value="">{t("missions.settings.effortDefault")}</option>
                {pickEffortOptions(choice).map(effort=><option key={effort} value={effort}>{effort}</option>)}
              </select>
            </Field>;
          })}
        </div>
      </div>:null}
      {teamConsentNeeded?<div className="mission-settings-consent" data-testid="team-experimental">
        <label className="mission-settings-consent-toggle">
          <input type="checkbox" checked={teamConsent} disabled={busy} data-testid="team-experimental-consent" onChange={e=>setTeamConsent(e.target.checked)}/>
          {t("missions.quickSetup.experimental.use")}
        </label>
        <p className="mission-quick-setup-experimental-risk">{t("missions.settings.teamExperimentalRisk",{models:consentChoices.map(choice=>`${runtimeNames[choice.runtime]} · ${choice.model}`).join(", ")})}</p>
      </div>:null}
      <div className="mission-settings-actions">
        <button type="button" className="primary" disabled={busy || (teamConsentNeeded && !teamConsent)} onClick={()=>void saveTeam()}>{t("missions.settings.saveTeam")}</button>
        {teamConsentNeeded && !teamConsent?<span className="mission-settings-actions-note">{t("missions.settings.teamConsentRequired")}</span>:null}
      </div>
      <div className="mission-settings-block">
        <h4>{t("missions.settings.savedTeams")}</h4>
        {templates.length===0?<p className="muted">{t("missions.settings.teamsEmpty")}</p>
          :<ul className="mission-settings-list">{templates.map(template=><li key={template.id}>
            <strong>{template.label}</strong>
            <ul className="mission-settings-roles">{template.role_bindings.map(role=><li key={role.role}>
              <span className="mission-settings-role">{roleLabel(t,role.role)}</span>{": "}{bindings.find(b=>b.id===role.primary_binding_id)?.label ?? role.primary_binding_id}
            </li>)}</ul>
          </li>)}</ul>}
      </div>
    </section>

    <section className="mission-settings-card" data-setting-id="missionVerification">
      <header className="mission-settings-card-head">
        <h3>{t("missions.settings.verification")}</h3>
        <p>{t("settings.item.missionVerification.description")}</p>
      </header>
      <div className="mission-settings-block">
        <h4>{t("missions.settings.repoSection")}</h4>
        <div className="mission-field-row">
          <Field label={t("missions.create.repo")}>
            <input value={repositoryPath} disabled={busy} onChange={e=>{setRepositoryPath(e.target.value);setRepository(null);setCommands([]);}}/>
          </Field>
          <button type="button" disabled={busy||!repositoryPath.trim()} onClick={()=>void inspect()}>{t("missions.create.inspect")}</button>
        </div>
        {repository
          ?<p className="mission-settings-repo"><code>{repository.canonical_path}</code> · {repository.head_oid.slice(0,12)}</p>
          :<p className="mission-field-note">{t("missions.settings.repoFirst")}</p>}
      </div>
      <div className="mission-settings-block">
        <h4>{t("missions.settings.commandSection")}</h4>
        <div className="mission-field-grid">
          <Field label={t("missions.settings.commandTitle")}>
            <input value={commandTitle} disabled={busy} onChange={e=>setCommandTitle(e.target.value)}/>
          </Field>
          <Field label={t("missions.settings.commandProgram")}>
            <input placeholder="npm" value={program} disabled={busy} onChange={e=>setProgram(e.target.value)}/>
          </Field>
          <Field label={t("missions.settings.commandArgs")} span>
            <input placeholder="test -- --run" spellCheck={false} value={args} disabled={busy} onChange={e=>setArgs(e.target.value)}/>
          </Field>
          <Field label={t("missions.settings.commandCwd")} span short>
            <input value={cwd} disabled={busy} onChange={e=>setCwd(e.target.value)}/>
          </Field>
        </div>
      </div>
      <div className="mission-settings-actions">
        <button type="button" className="primary" disabled={busy||!repository} onClick={()=>void saveCommand()}>{t("missions.settings.saveCommand")}</button>
      </div>
      {repository?<div className="mission-settings-block">
        <h4>{t("missions.settings.savedCommands")}</h4>
        {commands.length===0?<p className="muted">{t("missions.settings.commandsEmpty")}</p>
          :<ul className="mission-settings-list">{commands.map(command=><li key={command.id}>
            <strong>{command.title}</strong><code>{command.program} {command.argv.join(" ")}</code>
          </li>)}</ul>}
      </div>:null}
    </section>
  </div>;
}
