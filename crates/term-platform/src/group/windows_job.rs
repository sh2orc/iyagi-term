//! Ticket I06 — Windows Job Object backend (spec `03-resources.md` §6,
//! `02-runner.md` §3/§7). Documented Win32 APIs only — no `NtResumeProcess`-style
//! private calls (§6).
//!
//! Model: one Job Object per workload, handle owned by the daemon for the
//! daemon's lifetime. `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE` is always set and
//! breakaway is never permitted; dropping the last [`GroupHandle`] clone
//! therefore kills every remaining member — the crash-safety net.
//!
//! Limits:
//! * memory → `JOB_OBJECT_LIMIT_JOB_MEMORY` (job-wide **commit** limit — never
//!   presented as a resident cap, §6)
//! * cpu → `JOBOBJECT_CPU_RATE_CONTROL_INFORMATION` rate-based control
//!   (`rate = round(10000 * cores / logical_cpus)` clamped to 1..=10000);
//!   multi-processor-group hosts report the capability unsupported and refuse
//!   to set it (§6)
//! * pids → `JOB_OBJECT_LIMIT_ACTIVE_PROCESS`
//!
//! Sampling (§6: job API for commit/process-count/cpu accounting; per-process
//! handles for memory detail): `JobObjectBasicAndIoAccountingInformation`
//! gives IO counters, user+kernel CPU time and `ActiveProcesses`. The job API
//! only exposes a commit *peak*, so the current job commit charge is the sum
//! of per-process `PrivateUsage` over `JobObjectBasicProcessIdList` — the
//! same quantity the job limit enforces. Resident stays explicitly
//! unavailable: jobs expose no working-set sum and §2/§6 forbid labeling
//! commit as resident.

use std::io;
use std::sync::{Arc, Mutex};

use term_contracts::ids::{ProcessIdentity, U64String};
use term_contracts::metrics::{Metric, UsageCoverage, WorkloadUsage};
use term_contracts::snapshot::{Capabilities, LimitCapability, LimitSupport};
use term_contracts::workload::{GroupKind, WorkloadDescriptor};
use windows::core::{Error, BOOL, PCWSTR};
use windows::Win32::Foundation::{CloseHandle, HANDLE};
use windows::Win32::System::JobObjects::{
    AssignProcessToJobObject, CreateJobObjectW, IsProcessInJob,
    JobObjectBasicAccountingInformation, JobObjectBasicAndIoAccountingInformation,
    JobObjectBasicProcessIdList, JobObjectCpuRateControlInformation,
    JobObjectExtendedLimitInformation, QueryInformationJobObject, SetInformationJobObject,
    TerminateJobObject, JOBOBJECTINFOCLASS, JOBOBJECT_BASIC_ACCOUNTING_INFORMATION,
    JOBOBJECT_BASIC_AND_IO_ACCOUNTING_INFORMATION, JOBOBJECT_BASIC_PROCESS_ID_LIST,
    JOBOBJECT_CPU_RATE_CONTROL_INFORMATION, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
    JOB_OBJECT_CPU_RATE_CONTROL_ENABLE, JOB_OBJECT_CPU_RATE_CONTROL_HARD_CAP, JOB_OBJECT_LIMIT,
    JOB_OBJECT_LIMIT_ACTIVE_PROCESS, JOB_OBJECT_LIMIT_JOB_MEMORY,
    JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
};
use windows::Win32::System::ProcessStatus::{GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS_EX};
use windows::Win32::System::Threading::{
    GetActiveProcessorCount, GetActiveProcessorGroupCount, OpenProcess, ALL_PROCESSOR_GROUPS,
    PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_SET_QUOTA, PROCESS_TERMINATE, PROCESS_VM_READ,
};

use super::{
    bytes_metric, rate_deltas, scheduling, GroupHandle, GroupInner, RateSample, SchedulingOutcome,
    SchedulingTier, StopPhase,
};
use crate::identity;

const SOURCE: &str = "win32.job";
const SOURCE_COMMIT: &str = "win32.job.commit";

/// Job Object backend for Windows.
pub struct WindowsJobPlatform;

impl WindowsJobPlatform {
    pub fn new() -> Self {
        Self
    }
}

impl Default for WindowsJobPlatform {
    fn default() -> Self {
        Self::new()
    }
}

/// Owned job handle; `Drop` closes it, which force-kills any remaining member
/// because `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE` is always set.
struct JobHandle {
    raw: HANDLE,
}

impl std::fmt::Debug for JobHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("JobHandle")
            .field("raw", &self.raw.0)
            .finish()
    }
}

// SAFETY: a HANDLE is an opaque kernel value usable from any thread; the only
// Close happens once, in Drop.
unsafe impl Send for JobHandle {}
unsafe impl Sync for JobHandle {}

impl Drop for JobHandle {
    fn drop(&mut self) {
        // Kill-on-close: dropping the last GroupHandle clone == force stop.
        let _ = unsafe { CloseHandle(self.raw) };
    }
}

/// Backend payload carried by a [`GroupHandle`] on Windows. Cloning shares the
/// job handle; the differential-sample base is duplicated (each clone tracks
/// its own previous sample).
#[derive(Debug)]
pub(crate) struct WinGroupInner {
    job: Arc<JobHandle>,
    prev: Mutex<Option<RateSample>>,
}

impl Clone for WinGroupInner {
    fn clone(&self) -> Self {
        let prev = self.prev.lock().map(|p| *p).unwrap_or_default();
        Self {
            job: Arc::clone(&self.job),
            prev: Mutex::new(prev),
        }
    }
}

/// `rate = round(10000 * cores / logical_cpus)` clamped to 1..=10000
/// (03 §6). Errors on non-finite/non-positive cores or zero logical CPUs.
/// Values above 10000 (requested cores exceed the host) clamp to 10000 =
/// the whole machine.
pub(crate) fn cpu_rate_value(cores: f64, logical_cpus: u32) -> io::Result<u32> {
    if !cores.is_finite() || cores <= 0.0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("cpu_max_cores must be finite and positive, got {cores}"),
        ));
    }
    if logical_cpus == 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "logical cpu count reported as zero",
        ));
    }
    let raw = 10_000.0 * cores / f64::from(logical_cpus);
    Ok(raw.clamp(1.0, 10_000.0).round() as u32)
}

fn active_processor_groups() -> u16 {
    unsafe { GetActiveProcessorGroupCount() }
}

fn logical_cpus() -> u32 {
    unsafe { GetActiveProcessorCount(ALL_PROCESSOR_GROUPS) }
}

/// Win32 error → `io::Error` with a mapped kind for the upstream
/// require/prefer decision (§6): access-denied (nested job / protected
/// process) → `PermissionDenied`; not-found-ish → `NotFound`; parameter
/// errors → `InvalidInput`.
fn win_err(context: &str, err: Error) -> io::Error {
    // windows-core wraps win32 errors as FACILITY_WIN32 HRESULTs; the low
    // 16 bits carry the original code.
    let code = (err.code().0 & 0xFFFF) as u32;
    let kind = match code {
        5 => io::ErrorKind::PermissionDenied,    // ERROR_ACCESS_DENIED
        6 | 1168 => io::ErrorKind::NotFound,     // ERROR_INVALID_HANDLE / ERROR_NOT_FOUND
        87 | 122 => io::ErrorKind::InvalidInput, // ERROR_INVALID_PARAMETER / ERROR_INSUFFICIENT_BUFFER
        _ => io::ErrorKind::Other,
    };
    io::Error::new(
        kind,
        format!("{context} failed: {err} (win32 error {code})"),
    )
}

fn job_of(group: &GroupHandle) -> io::Result<&WinGroupInner> {
    match &group.inner {
        GroupInner::Win(w) => Ok(w),
        _ => Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "not a windows job group handle",
        )),
    }
}

fn query<T>(job: HANDLE, class: JOBOBJECTINFOCLASS, out: &mut T) -> io::Result<()> {
    unsafe {
        QueryInformationJobObject(
            Some(job),
            class,
            (out as *mut T).cast(),
            std::mem::size_of::<T>() as u32,
            None,
        )
    }
    .map_err(|e| win_err("QueryInformationJobObject", e))
}

fn set_info<T>(job: HANDLE, class: JOBOBJECTINFOCLASS, info: &T) -> io::Result<()> {
    unsafe {
        SetInformationJobObject(
            job,
            class,
            (info as *const T).cast(),
            std::mem::size_of::<T>() as u32,
        )
    }
    .map_err(|e| win_err("SetInformationJobObject", e))
}

/// Member PIDs of the job right now (`JobObjectBasicProcessIdList`).
fn member_pids(job: HANDLE) -> io::Result<Vec<u32>> {
    let mut capacity = 64usize;
    loop {
        let len = std::mem::size_of::<JOBOBJECT_BASIC_PROCESS_ID_LIST>()
            .saturating_add(capacity.saturating_mul(std::mem::size_of::<usize>()));
        let mut buf = vec![0u8; len];
        let r = unsafe {
            QueryInformationJobObject(
                Some(job),
                JobObjectBasicProcessIdList,
                buf.as_mut_ptr().cast(),
                len as u32,
                None,
            )
        };
        match r {
            Ok(()) => {
                // SAFETY: buf was zero-filled with at least the struct header
                // size and the call validated it.
                let info = unsafe { &*(buf.as_ptr() as *const JOBOBJECT_BASIC_PROCESS_ID_LIST) };
                let n = info.NumberOfProcessIdsInList as usize;
                let list = unsafe { std::slice::from_raw_parts(info.ProcessIdList.as_ptr(), n) };
                return Ok(list.iter().map(|&p| p as u32).collect());
            }
            Err(e) => {
                let code = (e.code().0 & 0xFFFF) as u32;
                if code == 234 && capacity < (1 << 20) {
                    // ERROR_MORE_DATA: membership grew past the buffer; retry.
                    capacity = capacity.saturating_mul(2);
                    continue;
                }
                return Err(win_err("QueryInformationJobObject(ProcessIdList)", e));
            }
        }
    }
}

/// Private commit bytes of one process (`PrivateUsage`), or `None` when it
/// cannot be opened/queried (protected or already-gone processes).
fn process_commit_bytes(pid: u32) -> Option<u64> {
    let rights = PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_VM_READ;
    let handle = unsafe { OpenProcess(rights, false, pid) }.ok()?;
    let mut counters = PROCESS_MEMORY_COUNTERS_EX {
        cb: std::mem::size_of::<PROCESS_MEMORY_COUNTERS_EX>() as u32,
        ..Default::default()
    };
    let r = unsafe {
        GetProcessMemoryInfo(
            handle,
            (&mut counters as *mut PROCESS_MEMORY_COUNTERS_EX).cast(),
            counters.cb,
        )
    };
    let _ = unsafe { CloseHandle(handle) };
    r.ok()?;
    Some(counters.PrivateUsage as u64)
}

fn first_sample(reason: &'static str) -> (Metric<f64>, Metric<f64>, Metric<f64>) {
    (
        Metric::unavailable(SOURCE, reason),
        Metric::unavailable(SOURCE, reason),
        Metric::unavailable(SOURCE, reason),
    )
}

impl super::ResourcePlatform for WindowsJobPlatform {
    fn capabilities(&self) -> Capabilities {
        let supported = |reason: &str| LimitCapability {
            support: LimitSupport::Supported,
            reason: Some(reason.into()),
        };
        let unsupported = |reason: &str| LimitCapability {
            support: LimitSupport::Unsupported,
            reason: Some(reason.into()),
        };
        let groups = active_processor_groups();
        Capabilities {
            memory_limit_kind: supported(
                "job-wide commit limit (JOB_OBJECT_LIMIT_JOB_MEMORY); not a resident cap",
            ),
            cpu_quota: if groups > 1 {
                unsupported(&format!(
                    "{groups} active processor groups; cpu rate control untested on multi-group hosts (03 §6)"
                ))
            } else {
                supported("job cpu rate control (rate-based, single processor group)")
            },
            process_count_limit: supported("JOB_OBJECT_LIMIT_ACTIVE_PROCESS"),
            tree_accounting: supported(
                "job membership covers all descendants; breakaway not permitted",
            ),
            reattach: supported("daemon owns the job handle for the daemon lifetime"),
            resume: unsupported("daemon restart marks workloads INTERRUPTED (R1)"),
            // 08 §2: per-process 우선순위 클래스는 되돌릴 수 있고 자식이
            // 상속한다. Job의 CPU rate control은 건드리지 않는다 — 사용자가
            // 설정한 hard cap과 weight 기반 제어는 상호 배타 플래그다.
            scheduling_yield: supported("SetPriorityClass BELOW_NORMAL per verified member"),
            // 08 §5: job-object suspension is deferred; the guard stays
            // dormant here rather than half-applying.
            suspend_resume: LimitCapability {
                support: LimitSupport::Unsupported,
                reason: Some("job-object suspend/resume not implemented in R1".into()),
            },
            platform: "windows-job".into(),
            notes: vec![
                "dropping the job handle force-kills remaining members (KILL_ON_JOB_CLOSE)".into(),
                "termination has no graceful phase on Windows R1; UI labels it 강제 종료 (02 §7)"
                    .into(),
            ],
            mission_protocol: None,
            claude_provider_routing: false,
        }
    }

    fn create_group(&self, workload: &WorkloadDescriptor) -> io::Result<GroupHandle> {
        let policy = &workload.policy;
        // Named job (uuid v4 name) for diagnostics; collision odds are the
        // uuid-v4 odds.
        let name = format!("iyagi-{}", workload.workload_id);
        let name16: Vec<u16> = name.encode_utf16().chain(std::iter::once(0)).collect();
        let raw = unsafe { CreateJobObjectW(None, PCWSTR::from_raw(name16.as_ptr())) }
            .map_err(|e| win_err("CreateJobObjectW", e))?;
        let job = JobHandle { raw };

        let mut limit_flags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        let mut cpu_rate: Option<u32> = None;
        if let Some(cores) = policy.cpu_max_cores {
            let groups = active_processor_groups();
            if groups > 1 {
                return Err(io::Error::new(
                    io::ErrorKind::Unsupported,
                    format!("cpu hard cap unsupported: {groups} active processor groups (03 §6)"),
                ));
            }
            cpu_rate = Some(cpu_rate_value(cores, logical_cpus())?);
        }

        let mut ext = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
        if let Some(mem) = &policy.memory_max_bytes {
            let m = mem.get();
            if m == 0 {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "memory_max_bytes must be positive when set",
                ));
            }
            ext.JobMemoryLimit = m as usize;
            limit_flags |= JOB_OBJECT_LIMIT_JOB_MEMORY;
        }
        if let Some(pids) = policy.pids_max {
            if pids == 0 {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "pids_max must be positive when set",
                ));
            }
            // +1: the launch helper stays a job member for the whole session
            // (it waits on the target to report `Exited`), so the CLI tree
            // itself gets exactly `pids_max` slots. Readback reports the
            // kernel value (pids_max + 1).
            ext.BasicLimitInformation.ActiveProcessLimit = pids.saturating_add(1);
            limit_flags |= JOB_OBJECT_LIMIT_ACTIVE_PROCESS;
        }
        ext.BasicLimitInformation.LimitFlags = limit_flags;
        set_info(job.raw, JobObjectExtendedLimitInformation, &ext)?;

        // CPU rate control is enabled through JobObjectCpuRateControlInformation
        // alone. NOTE: additionally setting JOB_OBJECT_LIMIT_CPU_RATE_CONTROL
        // via JobObjectExtendedLimitInformation returns ERROR_INVALID_PARAMETER
        // on current Windows builds in either order (verified on Win 11 26200)
        // — the rate readback in `inspect_job_limits` is the verification.
        if let Some(rate) = cpu_rate {
            let mut cpu = JOBOBJECT_CPU_RATE_CONTROL_INFORMATION {
                // rate-based (no WEIGHT_BASED). HARD_CAP: without it the rate
                // is only enforced under contention — the job may exceed
                // `CpuRate` whenever the machine has idle cycles, which is
                // not the "CPU hard cap" 03-resources §6 promises.
                ControlFlags: JOB_OBJECT_CPU_RATE_CONTROL_ENABLE
                    | JOB_OBJECT_CPU_RATE_CONTROL_HARD_CAP,
                ..Default::default()
            };
            cpu.Anonymous.CpuRate = rate;
            set_info(job.raw, JobObjectCpuRateControlInformation, &cpu)?;
        }

        Ok(GroupHandle {
            workload_id: workload.workload_id.clone(),
            kind: GroupKind::Job,
            reference: name,
            inner: GroupInner::Win(WinGroupInner {
                job: Arc::new(job),
                prev: Mutex::new(None),
            }),
        })
    }

    fn attach_pid(&self, group: &GroupHandle, identity: &ProcessIdentity) -> io::Result<()> {
        let inner = job_of(group)?;
        // Full triple check immediately before the OS call: never attach a
        // recycled PID (01 §1).
        identity::process_identity(identity.pid)
            .filter(|live| live.same_process(identity))
            .ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "identity mismatch: process gone or pid reused",
                )
            })?;
        let rights = PROCESS_SET_QUOTA | PROCESS_TERMINATE | PROCESS_QUERY_LIMITED_INFORMATION;
        let proc = unsafe { OpenProcess(rights, false, identity.pid) }
            .map_err(|e| win_err("OpenProcess(attach)", e))?;
        let assigned = unsafe { AssignProcessToJobObject(inner.job.raw, proc) }
            .map_err(|e| win_err("AssignProcessToJobObject", e));
        // Membership verification mirrors the §5 step 3 discipline on Windows.
        let mut in_job = BOOL::default();
        let membership = unsafe { IsProcessInJob(proc, Some(inner.job.raw), &mut in_job) }
            .map_err(|e| win_err("IsProcessInJob", e));
        let _ = unsafe { CloseHandle(proc) };
        assigned?;
        membership?;
        if !in_job.as_bool() {
            return Err(io::Error::other(
                "membership verification failed after AssignProcessToJobObject (03 §5 step 3)",
            ));
        }
        Ok(())
    }

    fn sample_group(&self, group: &GroupHandle, now_ms: u64) -> io::Result<WorkloadUsage> {
        let inner = job_of(group)?;
        let job = inner.job.raw;

        let mut bio = JOBOBJECT_BASIC_AND_IO_ACCOUNTING_INFORMATION::default();
        query(job, JobObjectBasicAndIoAccountingInformation, &mut bio)?;
        let mut ext = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
        query(job, JobObjectExtendedLimitInformation, &mut ext)?;

        // Current job commit charge = sum of member PrivateUsage (the job
        // limit enforces exactly this quantity; the job API itself only
        // exposes the peak, kept for diagnostics via inspect_job_limits).
        let _peak = ext.PeakJobMemoryUsed;
        let pids = member_pids(job)?;
        let mut committed_sum = 0u64;
        let mut unreadable = 0u32;
        for &pid in &pids {
            match process_commit_bytes(pid) {
                Some(b) => committed_sum = committed_sum.saturating_add(b),
                None => unreadable += 1,
            }
        }
        let committed = if pids.is_empty() {
            Metric::measured(SOURCE_COMMIT, U64String::new(0).expect("0 in range"))
        } else if unreadable > 0 {
            Metric::unavailable(
                SOURCE_COMMIT,
                format!(
                    "{unreadable} of {} members unreadable; commit sum incomplete",
                    pids.len()
                ),
            )
        } else {
            bytes_metric(SOURCE_COMMIT, committed_sum)
        };

        let cpu_time_us = ((bio.BasicInfo.TotalUserTime.max(0) as u128
            + bio.BasicInfo.TotalKernelTime.max(0) as u128)
            / 10) as u64; // 100 ns units → µs
        let cur = RateSample {
            now_ms,
            cpu_time_us,
            read_bytes: bio.IoInfo.ReadTransferCount,
            write_bytes: bio.IoInfo.WriteTransferCount,
        };
        let mut prev = inner
            .prev
            .lock()
            .map_err(|_| io::Error::other("sample lock poisoned"))?;
        let (cpu_cores, read_rate, write_rate) = match prev.as_ref() {
            Some(p) => match rate_deltas(p, &cur) {
                // Advance the differential base only when the pair produced
                // usable deltas; a zero-elapsed sample keeps the old base.
                Some((cores, read, write)) => {
                    *prev = Some(cur);
                    (
                        Metric::measured(SOURCE, cores),
                        Metric::measured(SOURCE, read),
                        Metric::measured(SOURCE, write),
                    )
                }
                None => first_sample("zero elapsed time since previous sample"),
            },
            None => {
                *prev = Some(cur);
                first_sample("first differential sample (03 §2)")
            }
        };
        drop(prev);

        Ok(WorkloadUsage {
            workload_id: group.workload_id.clone(),
            cpu_cores,
            // Jobs expose no working-set sum; commit is reported separately
            // and must not be relabeled as resident (§2/§6).
            resident_bytes: Metric::unavailable(
                SOURCE,
                "job resident working set not exposed by job accounting; commit reported instead",
            ),
            accounted_bytes: Metric::unavailable(SOURCE, "cgroup-style accounting is linux-only"),
            committed_bytes: committed,
            read_bytes_per_sec: read_rate,
            write_bytes_per_sec: write_rate,
            network_rx_bytes_per_sec: Metric::unavailable(
                SOURCE,
                "job objects do not account networking",
            ),
            network_tx_bytes_per_sec: Metric::unavailable(
                SOURCE,
                "job objects do not account networking",
            ),
            process_count: Metric::measured(SOURCE, bio.BasicInfo.ActiveProcesses),
            coverage: UsageCoverage::Group,
        })
    }

    fn member_identities(&self, group: &GroupHandle) -> io::Result<Vec<ProcessIdentity>> {
        let inner = job_of(group)?;
        Ok(member_pids(inner.job.raw)?
            .iter()
            .filter_map(|&pid| identity::process_identity(pid))
            .collect())
    }

    fn terminate_owned(&self, group: &GroupHandle, phase: StopPhase) -> io::Result<()> {
        let inner = job_of(group)?;
        match phase {
            // R1 assumes no universal graceful CLI signal on Windows
            // (02-runner §7): Grace is a documented no-op; the caller labels
            // the stop 강제 종료 and follows with Force after its window.
            StopPhase::Grace => Ok(()),
            StopPhase::Force => unsafe { TerminateJobObject(inner.job.raw, 1) }
                .map_err(|e| win_err("TerminateJobObject", e)),
        }
    }

    /// 08 §2 양보: Job의 현재 멤버 pid 각각을 신원 재검증한 뒤
    /// `SetPriorityClass`를 건다. Job CPU rate control은 손대지 않는다
    /// (weight 기반과 hard cap은 상호 배타 플래그이므로 사용자가 설정한
    /// `cpu_max_cores`를 조용히 지워 버린다).
    fn set_scheduling(
        &self,
        group: &GroupHandle,
        tier: SchedulingTier,
    ) -> io::Result<SchedulingOutcome> {
        let inner = job_of(group)?;
        let members: Vec<_> = member_pids(inner.job.raw)?
            .iter()
            .filter_map(|&pid| identity::process_identity(pid))
            .collect();
        scheduling::set_process_scheduling(&members, tier)
    }

    fn is_empty(&self, group: &GroupHandle) -> io::Result<bool> {
        let inner = job_of(group)?;
        let mut basic = JOBOBJECT_BASIC_ACCOUNTING_INFORMATION::default();
        query(
            inner.job.raw,
            JobObjectBasicAccountingInformation,
            &mut basic,
        )?;
        Ok(basic.ActiveProcesses == 0)
    }
}

/// Limit readback (diagnostics/tests): what the kernel currently reports for
/// the job's extended limits and cpu rate — the §5 step 2 discipline applied
/// to Job Objects.
pub struct JobLimits {
    pub flags: JOB_OBJECT_LIMIT,
    pub job_memory_limit: Option<u64>,
    /// Kernel value: `pids_max + 1` (the waiting helper's slot).
    pub active_process_limit: Option<u32>,
    /// Effective rate (1..=10000) with `JOB_OBJECT_CPU_RATE_CONTROL_ENABLE`
    /// verified on read-back; `None` when rate control is not configured.
    pub cpu_rate: Option<u32>,
    /// `JOB_OBJECT_CPU_RATE_CONTROL_HARD_CAP` present on read-back.
    pub cpu_hard_cap: bool,
}

/// Read back the effective limits of `group`'s job.
pub fn inspect_job_limits(group: &GroupHandle) -> io::Result<JobLimits> {
    let inner = job_of(group)?;
    let job = inner.job.raw;
    let mut ext = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
    query(job, JobObjectExtendedLimitInformation, &mut ext)?;
    let mut cpu = JOBOBJECT_CPU_RATE_CONTROL_INFORMATION::default();
    let cpu_configured = query(job, JobObjectCpuRateControlInformation, &mut cpu).is_ok()
        && cpu.ControlFlags.0 & JOB_OBJECT_CPU_RATE_CONTROL_ENABLE.0 != 0;
    Ok(JobLimits {
        flags: ext.BasicLimitInformation.LimitFlags,
        job_memory_limit: (ext.BasicLimitInformation.LimitFlags.0 & JOB_OBJECT_LIMIT_JOB_MEMORY.0
            != 0)
            .then_some(ext.JobMemoryLimit as u64),
        active_process_limit: (ext.BasicLimitInformation.LimitFlags.0
            & JOB_OBJECT_LIMIT_ACTIVE_PROCESS.0
            != 0)
            .then_some(ext.BasicLimitInformation.ActiveProcessLimit),
        cpu_rate: cpu_configured.then_some(unsafe { cpu.Anonymous.CpuRate }),
        cpu_hard_cap: cpu_configured
            && cpu.ControlFlags.0 & JOB_OBJECT_CPU_RATE_CONTROL_HARD_CAP.0 != 0,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::group::testutil::{mono_ms, poll_until, spawn_lived_child, test_workload};
    use crate::group::ResourcePlatform;
    use std::time::Duration;

    #[test]
    fn job_cpu_rate_set_when_single_group() {
        if active_processor_groups() > 1 {
            // Multi-group host: capability unsupported, create must refuse.
            let platform = WindowsJobPlatform::new();
            let wl = test_workload(None, Some(1.0), None);
            assert!(platform.create_group(&wl).is_err());
            return;
        }
        let platform = WindowsJobPlatform::new();
        let logical = logical_cpus();
        let wl = test_workload(None, Some(1.0), None);
        let group = platform.create_group(&wl).expect("create with cpu rate");
        let limits = inspect_job_limits(&group).expect("readback");
        assert_eq!(
            limits.cpu_rate,
            Some(cpu_rate_value(1.0, logical).unwrap()),
            "effective rate verified by read-back"
        );
        assert!(limits.cpu_hard_cap, "rate must be a hard cap (03 §6)");
    }

    #[test]
    fn cpu_rate_value_math_and_clamping() {
        assert_eq!(cpu_rate_value(1.0, 4).unwrap(), 2_500);
        assert_eq!(cpu_rate_value(2.0, 8).unwrap(), 2_500);
        assert_eq!(cpu_rate_value(0.5, 8).unwrap(), 625);
        assert_eq!(cpu_rate_value(0.01, 64).unwrap(), 2); // round(1.5625)
        assert_eq!(cpu_rate_value(0.0001, 64).unwrap(), 1); // rounds to 0 → clamp 1
        assert_eq!(cpu_rate_value(8.0, 4).unwrap(), 10_000); // over 100% → clamp
        assert!(cpu_rate_value(0.0, 4).is_err());
        assert!(cpu_rate_value(-1.0, 4).is_err());
        assert!(cpu_rate_value(f64::NAN, 4).is_err());
        assert!(cpu_rate_value(f64::INFINITY, 4).is_err());
        assert!(cpu_rate_value(1.0, 0).is_err());
    }

    #[test]
    fn capabilities_reflect_processor_groups() {
        let caps = WindowsJobPlatform::new().capabilities();
        assert_eq!(caps.platform, "windows-job");
        assert_eq!(caps.memory_limit_kind.support, LimitSupport::Supported);
        assert_eq!(caps.process_count_limit.support, LimitSupport::Supported);
        let groups = active_processor_groups();
        let expect = if groups > 1 {
            LimitSupport::Unsupported
        } else {
            LimitSupport::Supported
        };
        assert_eq!(caps.cpu_quota.support, expect);
    }

    #[test]
    fn job_lifecycle_attach_sample_terminate() {
        let platform = WindowsJobPlatform::new();
        let mem_cap = 512 << 20;
        let wl = test_workload(Some(mem_cap), None, None);
        let group = platform.create_group(&wl).expect("create job group");

        // Limit readback (§5 step 2 discipline on Windows).
        let limits = inspect_job_limits(&group).expect("readback");
        assert!(limits.flags.0 & JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE.0 != 0);
        assert_eq!(limits.job_memory_limit, Some(mem_cap));

        let mut child = spawn_lived_child();
        let ident = identity::process_identity(child.id()).expect("child identity");
        platform
            .attach_pid(&group, &ident)
            .expect("attach own child");

        assert!(!platform.is_empty(&group).expect("is_empty"));
        let members = platform.member_identities(&group).expect("members");
        assert!(members.iter().any(|m| m.pid == child.id()));

        let usage1 = platform.sample_group(&group, mono_ms()).expect("sample 1");
        assert_eq!(usage1.workload_id, wl.workload_id);
        assert_eq!(usage1.coverage, UsageCoverage::Group);
        assert!(usage1.process_count.value >= Some(1));
        // First differential sample: rates unavailable, not zero (03 §2).
        assert!(usage1.cpu_cores.value.is_none());
        assert!(usage1.read_bytes_per_sec.value.is_none());
        // Commit measured; resident explicitly unavailable (§6).
        assert!(usage1.committed_bytes.value.is_some());
        assert!(usage1.resident_bytes.value.is_none());

        std::thread::sleep(Duration::from_millis(150));
        let usage2 = platform.sample_group(&group, mono_ms()).expect("sample 2");
        assert!(usage2.cpu_cores.value.is_some());
        assert!(usage2.read_bytes_per_sec.value.is_some());
        assert!(usage2.write_bytes_per_sec.value.is_some());
        assert!(usage2.committed_bytes.value.is_some());

        platform
            .terminate_owned(&group, StopPhase::Grace)
            .expect("grace is a documented no-op on windows R1");
        assert!(
            !platform
                .is_empty(&group)
                .expect("still alive after grace no-op"),
            "grace must not kill on windows R1"
        );
        platform
            .terminate_owned(&group, StopPhase::Force)
            .expect("force terminate");
        assert!(poll_until(Duration::from_secs(5), || {
            platform.is_empty(&group).unwrap_or(false)
        }));
        let _ = child.wait();
        assert!(platform.member_identities(&group).unwrap().is_empty());
    }

    #[test]
    fn job_pids_limit_readback() {
        let platform = WindowsJobPlatform::new();
        let wl = test_workload(None, None, Some(64));
        let group = platform.create_group(&wl).expect("create");
        let limits = inspect_job_limits(&group).expect("readback");
        // pids_max + 1: the helper occupies one slot for the whole session.
        assert_eq!(limits.active_process_limit, Some(65));
    }

    #[test]
    fn dropping_last_handle_kills_members() {
        let platform = WindowsJobPlatform::new();
        let wl = test_workload(None, None, None);
        let group = platform.create_group(&wl).expect("create");
        let mut child = spawn_lived_child();
        let ident = identity::process_identity(child.id()).expect("child identity");
        platform.attach_pid(&group, &ident).expect("attach");
        let cloned = group.clone();
        drop(group);
        // One clone still alive → job still open → member still running.
        assert!(cloned.reference.contains("iyagi-"));
        assert!(!platform.is_empty(&cloned).expect("job still open"));
        drop(cloned);
        // Last handle dropped → KILL_ON_JOB_CLOSE kills the member.
        assert!(poll_until(Duration::from_secs(5), || {
            child.try_wait().map(|w| w.is_some()).unwrap_or(false)
        }));
        let _ = child.wait();
    }

    #[test]
    fn attach_rejects_identity_mismatch_and_dead_pids() {
        let platform = WindowsJobPlatform::new();
        let wl = test_workload(None, None, None);
        let group = platform.create_group(&wl).expect("create");

        let mut child = spawn_lived_child();
        let mut tampered = identity::process_identity(child.id()).unwrap();
        tampered.start_token = "999999".into(); // simulate PID reuse
        let err = platform.attach_pid(&group, &tampered).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
        let _ = child.kill();
        let _ = child.wait();

        let gone = ProcessIdentity {
            pid: u32::MAX - 4,
            start_token: "1".into(),
            boot_id: identity::boot_id(),
        };
        assert!(platform.attach_pid(&group, &gone).is_err());
    }
}
