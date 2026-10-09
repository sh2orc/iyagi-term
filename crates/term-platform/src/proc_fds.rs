//! 한 프로세스가 **열어 둔 파일 경로** 열거(spec `02-runner.md` §8).
//!
//! Codex는 스레드 잠금 파일(`~/.codex/thread-writer-locks/<id>.lock`)을
//! 세션이 사는 내내 열어 둔다 — 그 fd가 가리키는 경로가 곧 세션 id다.
//! 레지스트리 파일을 남기는 Claude Code와 달리 파일 이름을 밖에서
//! 알아낼 방법이 이것뿐이라, 관찰은 fd 테이블에서 한다.
//!
//! 읽기 전용 관찰이며 실패는 언제나 `None`이다 — 권한이 없거나(다른
//! 사용자의 프로세스) 프로세스가 사라졌거나 플랫폼이 지원하지 않으면
//! 세션 식별만 포기하고 데몬은 그대로 돈다. 소켓·파이프처럼 경로가 없는
//! fd는 건너뛴다.
//!
//! 플랫폼: macOS는 공개 libproc(`PROC_PIDLISTFDS` →
//! `PROC_PIDFDVNODEPATHINFO`), Linux는 `/proc/<pid>/fd` 심링크,
//! Windows는 지원하지 않는다(잠금 파일 후보는 mtime 대조로 좁힌다).

use std::path::PathBuf;

/// `pid`가 열어 둔 파일들의 경로. 열거할 수 없으면 `None`(빈 벡터는
/// "열어 둔 파일이 없다"는 관찰 결과이므로 구분한다).
pub fn open_file_paths(pid: u32) -> Option<Vec<PathBuf>> {
    imp::open_file_paths(pid)
}

#[cfg(target_os = "macos")]
mod imp {
    //! `<sys/proc_info.h>`의 구조체를 그대로 옮긴다. 레이아웃이 어긋나면
    //! 경로가 아니라 쓰레기를 읽게 되므로 필드 순서·너비를 헤더와 정확히
    //! 맞추고, 커널이 채운 바이트 수가 구조체보다 적으면 버린다.

    use std::mem;
    use std::path::PathBuf;

    /// `<sys/param.h>` MAXPATHLEN.
    const MAXPATHLEN: usize = 1024;
    /// `<sys/proc_info.h>` PROC_PIDFDVNODEPATHINFO.
    const PROC_PIDFDVNODEPATHINFO: libc::c_int = 2;
    /// fd 목록을 한 번에 받기 위한 여유분(두 호출 사이에 fd가 늘 수 있다).
    const FD_SLACK: usize = 32;
    /// 한 프로세스에서 훑을 fd 상한 — 비정상적으로 큰 테이블에서 틱을
    /// 잡아먹지 않게 한다.
    const FD_MAX: usize = 4_096;

    #[repr(C)]
    #[derive(Clone, Copy)]
    struct ProcFdInfo {
        proc_fd: i32,
        proc_fdtype: u32,
    }

    #[repr(C)]
    #[derive(Clone, Copy)]
    struct ProcFileInfo {
        fi_openflags: u32,
        fi_status: u32,
        fi_offset: i64,
        fi_type: i32,
        fi_guardflags: u32,
    }

    #[repr(C)]
    #[derive(Clone, Copy)]
    struct VinfoStat {
        vst_dev: u32,
        vst_mode: u16,
        vst_nlink: u16,
        vst_ino: u64,
        vst_uid: u32,
        vst_gid: u32,
        vst_atime: i64,
        vst_atimensec: i64,
        vst_mtime: i64,
        vst_mtimensec: i64,
        vst_ctime: i64,
        vst_ctimensec: i64,
        vst_birthtime: i64,
        vst_birthtimensec: i64,
        vst_size: i64,
        vst_blocks: i64,
        vst_blksize: i32,
        vst_flags: u32,
        vst_gen: u32,
        vst_rdev: u32,
        vst_qspare: [i64; 2],
    }

    #[repr(C)]
    #[derive(Clone, Copy)]
    struct Fsid {
        val: [i32; 2],
    }

    #[repr(C)]
    #[derive(Clone, Copy)]
    struct VnodeInfo {
        vi_stat: VinfoStat,
        vi_type: i32,
        vi_pad: i32,
        vi_fsid: Fsid,
    }

    #[repr(C)]
    #[derive(Clone, Copy)]
    struct VnodeInfoPath {
        vip_vi: VnodeInfo,
        /// 경로의 꼬리(NUL 종단). 커널이 `MAXPATHLEN`만큼 자리를 잡는다.
        vip_path: [libc::c_char; MAXPATHLEN],
    }

    #[repr(C)]
    #[derive(Clone, Copy)]
    struct VnodeFdInfoWithPath {
        pfi: ProcFileInfo,
        pvip: VnodeInfoPath,
    }

    pub(super) fn open_file_paths(pid: u32) -> Option<Vec<PathBuf>> {
        let entries = list_fds(pid)?;
        let mut out = Vec::new();
        for entry in entries {
            if entry.proc_fdtype != libc::PROX_FDTYPE_VNODE as u32 {
                continue;
            }
            if let Some(path) = vnode_path(pid, entry.proc_fd) {
                out.push(path);
            }
        }
        Some(out)
    }

    /// `PROC_PIDLISTFDS`: 필요한 크기를 물어보고(버퍼 NULL) 여유분을 더해
    /// 한 번에 받는다.
    fn list_fds(pid: u32) -> Option<Vec<ProcFdInfo>> {
        let entry_size = mem::size_of::<ProcFdInfo>();
        let needed = unsafe {
            libc::proc_pidinfo(
                pid as libc::c_int,
                libc::PROC_PIDLISTFDS,
                0,
                std::ptr::null_mut(),
                0,
            )
        };
        // 0 바이트 / 음수 errno: 사라졌거나 권한이 없다.
        if needed <= 0 {
            return None;
        }
        let capacity = (needed as usize / entry_size)
            .saturating_add(FD_SLACK)
            .min(FD_MAX);
        let mut buffer: Vec<ProcFdInfo> = vec![
            ProcFdInfo {
                proc_fd: 0,
                proc_fdtype: 0,
            };
            capacity
        ];
        let size = libc::c_int::try_from(capacity.saturating_mul(entry_size)).ok()?;
        let used = unsafe {
            libc::proc_pidinfo(
                pid as libc::c_int,
                libc::PROC_PIDLISTFDS,
                0,
                buffer.as_mut_ptr().cast(),
                size,
            )
        };
        if used <= 0 {
            return None;
        }
        buffer.truncate((used as usize / entry_size).min(capacity));
        Some(buffer)
    }

    /// `PROC_PIDFDVNODEPATHINFO`: 구조체 꼬리의 `vip_path`를 읽는다.
    /// 커널이 구조체를 다 채우지 못했으면(짧은 반환) 그 fd는 버린다.
    fn vnode_path(pid: u32, fd: i32) -> Option<PathBuf> {
        let mut info: VnodeFdInfoWithPath = unsafe { mem::zeroed() };
        let size = mem::size_of::<VnodeFdInfoWithPath>() as libc::c_int;
        let r = unsafe {
            libc::proc_pidfdinfo(
                pid as libc::c_int,
                fd,
                PROC_PIDFDVNODEPATHINFO,
                (&mut info as *mut VnodeFdInfoWithPath).cast(),
                size,
            )
        };
        if r < size {
            return None;
        }
        let bytes: &[u8] = unsafe {
            std::slice::from_raw_parts(info.pvip.vip_path.as_ptr().cast::<u8>(), MAXPATHLEN)
        };
        let end = bytes.iter().position(|b| *b == 0).unwrap_or(MAXPATHLEN);
        if end == 0 {
            return None;
        }
        Some(PathBuf::from(
            String::from_utf8_lossy(&bytes[..end]).into_owned(),
        ))
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        /// 레이아웃이 어긋나면 경로 대신 쓰레기를 읽는다 — 헤더의 크기를
        /// 그대로 못 박아 둔다(64비트 macOS).
        #[test]
        fn struct_sizes_match_the_system_header() {
            assert_eq!(mem::size_of::<ProcFdInfo>(), 8);
            assert_eq!(mem::size_of::<ProcFileInfo>(), 24);
            assert_eq!(mem::size_of::<VinfoStat>(), 136);
            assert_eq!(mem::size_of::<VnodeInfo>(), 152);
            assert_eq!(mem::size_of::<VnodeInfoPath>(), 152 + MAXPATHLEN);
            assert_eq!(mem::size_of::<VnodeFdInfoWithPath>(), 24 + 152 + MAXPATHLEN);
        }
    }
}

#[cfg(target_os = "linux")]
mod imp {
    use std::path::PathBuf;

    pub(super) fn open_file_paths(pid: u32) -> Option<Vec<PathBuf>> {
        let dir = std::fs::read_dir(format!("/proc/{pid}/fd")).ok()?;
        let mut out = Vec::new();
        for entry in dir.flatten() {
            // 소켓·파이프는 "socket:[…]" 같은 가짜 대상이라 절대 경로만 쓴다.
            if let Ok(target) = std::fs::read_link(entry.path()) {
                if target.is_absolute() {
                    out.push(target);
                }
            }
        }
        Some(out)
    }
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
mod imp {
    use std::path::PathBuf;

    /// Windows: 열린 핸들 열거는 특권 API를 요구한다. 호출자는 잠금 파일
    /// 후보를 mtime 대조로 좁히는 대체 경로를 쓴다.
    pub(super) fn open_file_paths(_pid: u32) -> Option<Vec<PathBuf>> {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    fn our_own_open_file_shows_up_in_our_fd_table() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("held-open.lock");
        let mut file = std::fs::File::create(&path).unwrap();
        file.write_all(b"x").unwrap();
        file.flush().unwrap();
        // 심링크가 섞인 임시 경로(macOS /var → /private/var)를 정규화한다.
        let canonical = std::fs::canonicalize(&path).unwrap();

        let paths = open_file_paths(std::process::id()).expect("own fd table");
        assert!(
            paths.contains(&canonical),
            "{canonical:?} not among {} open paths",
            paths.len()
        );

        // 파일을 닫으면 더 이상 보이지 않는다.
        drop(file);
        let after = open_file_paths(std::process::id()).expect("own fd table");
        assert!(!after.contains(&canonical));
    }

    #[test]
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    fn a_dead_pid_yields_none_instead_of_panicking() {
        assert!(open_file_paths(u32::MAX - 17).is_none());
    }
}
