/** Offline fixture only. Fake runtime, no actual model or credential access. */
import type { Binding, Mission, Policy, Rpc, TeamTemplate, ProviderResult } from './contracts';

const id = {
  mission: '10000000-0000-4000-8000-000000000001',
  repository: '10000000-0000-4000-8000-000000000002',
  binding: '10000000-0000-4000-8000-000000000003',
  goal: '10000000-0000-4000-8000-000000000004',
  requirement: '10000000-0000-4000-8000-000000000005',
  command: '10000000-0000-4000-8000-000000000006',
  request: '10000000-0000-4000-8000-000000000007',
  template: '10000000-0000-4000-8000-000000000008',
};
const yes = { supported: true, reason_code: null };
const no = { supported: false, reason_code: 'fake_no_native_terminal' };

export const binding: Binding = {
  id: id.binding, revision: '1', label: 'Offline fixture', runtime: 'fake',
  program: '/fixture/iyagi-agent', runtime_version: 'fixture-v1',
  provider_id: 'fake', model_id: 'fixture-model', effort: null,
  auth_route: 'local', credential_ref: null, endpoint_ref: null,
  capabilities: {
    structured_result: yes, events: yes, cancel: yes, resume: yes, steer: yes,
    approval_reply: yes, read_only: yes, scoped_write: yes, model_listing: yes,
    usage: yes, native_terminal_attach: no,
  },
  checked_at: '2026-09-13T00:00:00Z', enabled: true,
  resource_policy: { reservation_bytes: '2147483648', cpu_slots: 1,
    enforcement: 'observe', memory_max_bytes: null, cpu_max_cores: null, pids_max: null },
};

export const policy: Policy = {
  max_parallel_runs: 4, max_attempts_per_task: 3, max_repair_cycles: 3,
  max_automatic_starts: 64, active_time_limit_ms: 14400000, run_time_limit_ms: 2700000,
  max_cost_usd_micros: null, unknown_cost: 'allow_with_notice', allow_network: false,
  allow_automatic_plan_apply: true, allow_recovery_of_unsent: true,
  allowed_binding_ids: [id.binding], allowed_roles: ['lead', 'builder', 'reviewer', 'integrator'],
  allowed_verification_ids: [id.command], require_independent_review: true,
  require_enforced_verification: false,
};

export const template: TeamTemplate = {
  id: id.template, revision: '1', label: 'Offline balanced', repository_id: null,
  role_bindings: [
    { role: 'lead', primary_binding_id: id.binding, fallback_binding_ids: [] },
    { role: 'builder', primary_binding_id: id.binding, fallback_binding_ids: [] },
    { role: 'reviewer', primary_binding_id: id.binding, fallback_binding_ids: [] },
    { role: 'integrator', primary_binding_id: id.binding, fallback_binding_ids: [] },
  ], policy,
};

export const mission: Mission = {
  id: id.mission, revision: '1', state: 'draft', phase: 'planning',
  title: '로그인 기능', repository_path: '/fixture/repo', repository_id: id.repository,
  base_oid: 'a'.repeat(40),
  goal_ref: { id: id.goal, sha256: 'b'.repeat(64), bytes: '120', media_type: 'text/plain' },
  requirements: [{ id: id.requirement, text: '로그인 성공과 실패 경로를 검증한다.',
    verification_ids: [id.command], human_check: false }],
  policy, role_bindings: template.role_bindings, plan_revision: 0,
  candidate_id: null, open_decision_count: 0, active_time_ms: '0', automatic_start_count: 0,
  created_at: '2026-09-13T00:00:00Z', updated_at: '2026-09-13T00:00:00Z',
  archived_at: null, accepted_at: null, failure_code: null,
};

export const startRequest: Rpc['mission.control']['params'] = {
  request_id: id.request, mission_id: id.mission, expected_revision: '1', action: 'start',
};

export const parallelPlan: ProviderResult = {
  kind: 'plan', based_on_plan_revision: 0, retire_task_ids: [],
  rationale_text: 'API 응답 계약을 공유하고 API와 화면을 독립 작업 공간에서 구현합니다.',
  tasks: [
    { local_key: 'api', title: 'API 구현', kind: 'implement', role: 'builder', required: true,
      parent_key: null, depends_on_keys: [], objective_text: '로그인 API와 관련 테스트를 구현한다.',
      requirement_ids: [id.requirement], input_artifact_ids: [], allowed_paths: ['src/api/'],
      expected_outputs: ['patch'], verification_ids: [id.command], specialty: null,
      binding_id: id.binding, replacement_of: null },
    { local_key: 'ui', title: '화면 구현', kind: 'implement', role: 'builder', required: true,
      parent_key: null, depends_on_keys: [], objective_text: '공유 API 계약에 맞춰 로그인 화면을 구현한다.',
      requirement_ids: [id.requirement], input_artifact_ids: [], allowed_paths: ['src/ui/'],
      expected_outputs: ['patch'], verification_ids: [id.command], specialty: null,
      binding_id: id.binding, replacement_of: null },
  ],
};
