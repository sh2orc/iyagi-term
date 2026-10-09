//! Ticket I06 — process identity probes: PID + start token + boot id
//! (spec `01-contracts.md` §1).
//!
//! PID reuse protection: every ownership claim or signal goes through a full
//! `ProcessIdentity` comparison (`same_process`); a bare PID is never enough.
//!
//! | OS | `start_token` | `boot_id` | reliable |
//! |----|---------------|-----------|----------|
//! | Linux | `/proc/<pid>/stat` field 22 (`starttime`, clock ticks since boot) | `/proc/sys/kernel/random/boot_id` | yes |
//! | Windows | process creation FILETIME via `GetProcessTimes` (decimal `u64`, 100 ns units since 1601-01-01) | host name + OS version composite — **not** boot-scoped | no |
//! | macOS | `sysctl(KERN_PROC_PID)` → `kinfo_proc.kp_proc.p_starttime` (µs since epoch) | `kern.bootsessionuuid`, fallback `kern.boottime` seconds | yes (uuid form; fallback no) |
//! | other | unavailable | `unknown-boot` | no |
//!
//! Windows note (`01-contracts.md` §1): a boot-scoped identity would need
//! admin-only sources (boot EventLog records) or the registry `MachineGuid`,
//! which is install-scoped, not boot-scoped — and reading the registry needs
//! the `Win32_System_Registry` feature that this crate deliberately does not
//! enable. R1 therefore reports a documented conservative composite
//! (host name + OS version via sysinfo) with [`boot_id_reliable`] == `false`;
//! the daemon must disable post-restart ownership restoration on Windows.
//!
//! [`process_identity`] returns `None` when the process is gone (or cannot be
//! queried, e.g. access-denied on protected processes): callers treat `None`
//! as "unverifiable", never as "matches".

use std::sync::OnceLock;

use term_contracts::ids::ProcessIdentity;

/// Linux: `/proc` starttime + kernel boot_id — both reliable per boot.
#[cfg(target_os = "linux")]
mod imp {
    use std::fs;

    use term_contracts::ids::ProcessIdentity;

    pub fn boot() -> Option<(String, bool)> {
        let raw = fs::read_to_string("/proc/sys/kernel/random/boot_id").ok()?;
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            return None;
        }
        Some((trimmed.to_string(), true))
    }

    pub fn current() -> Option<ProcessIdentity> {
        of_pid(std::process::id())
    }

    pub fn of_pid(pid: u32) -> Option<ProcessIdentity> {
        let stat = fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
        Some(ProcessIdentity {
            pid,
            start_token: start_token_of_stat(&stat)?,
            boot_id: super::boot_id(),
        })
    }

    /// `starttime` is field 22. The comm field (2) may contain spaces and
    /// parentheses, so parsing starts after the *last* `)` of the line.
    /// A zombie (`Z`) or dead (`X`) entry keeps its pid until reaped but is
    /// not a live process — it cannot be signalled or observed — so it must
    /// not count as an owned descendant (macOS rejects `SZOMB` the same way;
    /// the observed-tree path would otherwise drain forever on it).
    pub(super) fn start_token_of_stat(stat: &str) -> Option<String> {
        let after_comm = stat.rsplit_once(')')?.1;
        let mut fields = after_comm.split_whitespace();
        // Field 3 is the state letter.
        let state = fields.next()?;
        if matches!(state, "Z" | "X" | "x") {
            return None;
        }
        // starttime (22) is the 19th token after `state` (3).
        fields.nth(18).map(str::to_string)
    }
}

/// Windows: creation FILETIME via `GetProcessTimes` opened with
/// `PROCESS_QUERY_LIMITED_INFORMATION`; composite install-scoped boot id.
#[cfg(target_os = "windows")]
mod imp {
    use term_contracts::ids::ProcessIdentity;
    use windows::Win32::Foundation::{CloseHandle, FILETIME, HANDLE};
    use windows::Win32::System::Threading::{
        GetCurrentProcess, GetProcessTimes, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
    };

    pub fn boot() -> Option<(String, bool)> {
        // Not boot-scoped: stable across reboots on the same install. The
        // daemon must not restore ownership after a restart (01 §1).
        let host = sysinfo::System::host_name().unwrap_or_else(|| "unknown-host".into());
        let os = sysinfo::System::os_version().unwrap_or_else(|| "unknown-os".into());
        Some((format!("windows-host:{host}:{os}"), false))
    }

    pub fn current() -> Option<ProcessIdentity> {
        // Pseudo handle, no CloseHandle required.
        let handle = unsafe { GetCurrentProcess() };
        Some(ProcessIdentity {
            pid: std::process::id(),
            start_token: creation_token(handle)?.to_string(),
            boot_id: super::boot_id(),
        })
    }

    pub fn of_pid(pid: u32) -> Option<ProcessIdentity> {
        let handle = match unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) } {
            Ok(h) => h,
            // Gone, recycled, or a protected process we may not query: all
            // count as unverifiable → None (never "matches").
            Err(_) => return None,
        };
        // Close on EVERY path: an exited-but-unreaped pid (ExitTime != 0)
        // or a failed GetProcessTimes used to return before CloseHandle,
        // leaking one handle per tick and pinning the dead pid.
        let token = creation_token(handle);
        let _ = unsafe { CloseHandle(handle) };
        let token = token?;
        Some(ProcessIdentity {
            pid,
            start_token: token.to_string(),
            boot_id: super::boot_id(),
        })
    }

    fn creation_token(handle: HANDLE) -> Option<u64> {
        let mut creation = FILETIME::default();
        let mut exit = FILETIME::default();
        let mut kernel = FILETIME::default();
        let mut user = FILETIME::default();
        unsafe {
            GetProcessTimes(handle, &mut creation, &mut exit, &mut kernel, &mut user).ok()?;
        }
        // Exited-but-unreaped processes keep a valid PID on Windows until all
        // handles close; a non-zero ExitTime means the process is gone.
        if exit.dwLowDateTime != 0 || exit.dwHighDateTime != 0 {
            return None;
        }
        Some(((creation.dwHighDateTime as u64) << 32) | creation.dwLowDateTime as u64)
    }
}

/// macOS: start time via the public libproc `proc_pidinfo(PROC_PIDTBSDINFO)`
/// interface (`pbi_start_tvsec/tvusec`); boot identity from
/// `kern.bootsessionuuid` (per-boot UUID) with `kern.boottime` fallback.
#[cfg(target_os = "macos")]
mod imp {
    use std::mem;

    use term_contracts::ids::ProcessIdentity;

    pub fn boot() -> Option<(String, bool)> {
        if let Some(uuid) = sysctl_string(b"kern.bootsessionuuid\0") {
            let trimmed = uuid.trim();
            if !trimmed.is_empty() {
                return Some((trimmed.to_string(), true));
            }
        }
        // Fallback: second-granularity boot time. Conservatively unreliable
        // (a same-second reboot would alias), so ownership restore stays off.
        let mut mib = [libc::CTL_KERN, libc::KERN_BOOTTIME];
        let mut tv: libc::timeval = unsafe { mem::zeroed() };
        let mut len = mem::size_of::<libc::timeval>();
        let r = unsafe {
            libc::sysctl(
                mib.as_mut_ptr(),
                2,
                (&mut tv as *mut libc::timeval).cast(),
                &mut len,
                std::ptr::null_mut(),
                0,
            )
        };
        if r == 0 {
            Some((format!("boottime:{}", tv.tv_sec), false))
        } else {
            None
        }
    }

    pub fn current() -> Option<ProcessIdentity> {
        of_pid(std::process::id())
    }

    pub fn of_pid(pid: u32) -> Option<ProcessIdentity> {
        checked(pid).ok().flatten()
    }

    pub fn checked(pid: u32) -> std::io::Result<Option<ProcessIdentity>> {
        if pid == 0 || pid > i32::MAX as u32 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "invalid process pid",
            ));
        }
        let mut info: libc::proc_bsdinfo = unsafe { mem::zeroed() };
        let size = mem::size_of::<libc::proc_bsdinfo>() as libc::c_int;
        let r = unsafe {
            libc::proc_pidinfo(
                pid as libc::c_int,
                libc::PROC_PIDTBSDINFO,
                0,
                (&mut info as *mut libc::proc_bsdinfo).cast(),
                size,
            )
        };
        if r <= 0 {
            let error = std::io::Error::last_os_error();
            return if error.raw_os_error() == Some(libc::ESRCH) {
                Ok(None)
            } else {
                Err(error)
            };
        }
        if r != size {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "short process identity response",
            ));
        }
        // Zombies have no ownership left to claim (p_stat values, e.g. SZOMB).
        if info.pbi_status == libc::SZOMB {
            return Ok(None);
        }
        let token = (info.pbi_start_tvsec as i128) * 1_000_000 + info.pbi_start_tvusec as i128;
        Ok(Some(ProcessIdentity {
            pid,
            start_token: token.to_string(),
            boot_id: super::boot_id(),
        }))
    }

    fn sysctl_string(name: &[u8]) -> Option<String> {
        let mut len = 0usize;
        let r = unsafe {
            libc::sysctlbyname(
                name.as_ptr().cast(),
                std::ptr::null_mut(),
                &mut len,
                std::ptr::null_mut(),
                0,
            )
        };
        if r != 0 || len == 0 {
            return None;
        }
        let mut buf = vec![0u8; len];
        let r = unsafe {
            libc::sysctlbyname(
                name.as_ptr().cast(),
                buf.as_mut_ptr().cast(),
                &mut len,
                std::ptr::null_mut(),
                0,
            )
        };
        if r != 0 {
            return None;
        }
        buf.truncate(len);
        String::from_utf8(buf).ok()
    }
}

/// Documented fallback for OSes without a probe implementation: identity is
/// unprovable, so all ownership features must stay disabled rather than trust
/// a bare PID (`01-contracts.md` §1).
#[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
mod imp {
    use term_contracts::ids::ProcessIdentity;

    pub fn boot() -> Option<(String, bool)> {
        Some(("unknown-boot".to_string(), false))
    }

    pub fn current() -> Option<ProcessIdentity> {
        None
    }

    pub fn of_pid(_pid: u32) -> Option<ProcessIdentity> {
        None
    }
}

static BOOT: OnceLock<(String, bool)> = OnceLock::new();

fn boot_cached() -> &'static (String, bool) {
    BOOT.get_or_init(|| imp::boot().unwrap_or(("unknown-boot".to_string(), false)))
}

/// Boot identity string for the running OS instance. Cheap after the first
/// call (kernel interface result is cached for the daemon lifetime).
pub fn boot_id() -> String {
    boot_cached().0.clone()
}

/// Whether [`boot_id`] actually distinguishes boots on this platform. R1:
/// `false` on Windows (composite is install-scoped) — the daemon disables
/// post-restart ownership restore there (spec `01-contracts.md` §1).
pub fn boot_id_reliable() -> bool {
    boot_cached().1
}

/// A macOS identity query that distinguishes confirmed absence from unreadable
/// ownership. Recovery must propagate an error, never treat it as process exit.
#[cfg(target_os = "macos")]
pub fn process_identity_checked(pid: u32) -> std::io::Result<Option<ProcessIdentity>> {
    imp::checked(pid)
}

/// Identity of the daemon process itself.
pub fn current_process_identity() -> Option<ProcessIdentity> {
    imp::current()
}

/// Identity of an arbitrary PID, or `None` when the process is gone,
/// already-exited-but-unreaped, or not queryable by this process.
pub fn process_identity(pid: u32) -> Option<ProcessIdentity> {
    imp::of_pid(pid)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Long-lived own child for identity comparison; never touches unrelated
    /// processes.
    fn spawn_lived_child() -> std::process::Child {
        if cfg!(windows) {
            std::process::Command::new("ping")
                .args(["-n", "30", "127.0.0.1"])
                .spawn()
                .expect("spawn ping")
        } else {
            std::process::Command::new("sleep")
                .arg("30")
                .spawn()
                .expect("spawn sleep")
        }
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn linux_stat_parser_rejects_zombies_and_handles_comm_with_spaces() {
        // 22 fields; comm carries spaces and parentheses; starttime = 987654.
        let live = "42 (my (weird) proc) S 1 42 42 0 -1 4194560 100 0 0 0 5 3 0 0 20 0 1 0 987654 12345 100 18446744073709551615 1 1 0 0 0 0 0 0 0 0 0 0 17 0 0 0 0 0 0";
        assert_eq!(
            super::imp::start_token_of_stat(live).as_deref(),
            Some("987654")
        );
        let zombie = live.replacen(" S ", " Z ", 1);
        assert_eq!(super::imp::start_token_of_stat(&zombie), None);
        let dead = live.replacen(" S ", " X ", 1);
        assert_eq!(super::imp::start_token_of_stat(&dead), None);
        assert_eq!(super::imp::start_token_of_stat("garbage"), None);
    }

    #[test]
    fn current_identity_is_some_and_stable() {
        let a = current_process_identity().expect("current identity");
        let b = current_process_identity().expect("current identity again");
        assert!(a.same_process(&b));
        assert_eq!(a.pid, std::process::id());
        assert!(!a.start_token.is_empty());
        assert!(!a.boot_id.is_empty());
    }

    #[test]
    fn nonexistent_pid_is_none() {
        // Far beyond every supported OS PID space (Linux default pid_max is
        // 4_194_304; Windows allocates far lower).
        assert!(process_identity(u32::MAX - 4).is_none());
    }

    #[test]
    fn different_processes_have_different_identities() {
        let me = current_process_identity().expect("current identity");
        let mut child = spawn_lived_child();
        let them = process_identity(child.id()).expect("child identity");
        assert!(!me.same_process(&them));
        assert_eq!(me.boot_id, them.boot_id, "same boot");
        let _ = child.kill();
        let _ = child.wait();
    }

    #[test]
    fn boot_id_shape_and_reliability() {
        assert!(!boot_id().is_empty());
        // Windows R1 must report unreliable so the daemon disables
        // post-restart ownership restore (01-contracts §1).
        if cfg!(windows) {
            assert!(!boot_id_reliable());
        }
    }
}
