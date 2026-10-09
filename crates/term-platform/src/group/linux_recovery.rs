//! Pinned cgroup v2 handles. The kernfs file handle carries the full node
//! generation; a directory name or a truncated inode number cannot replace it.
use std::{
    fs::{File, OpenOptions},
    io,
    os::{
        fd::{AsRawFd, FromRawFd},
        unix::fs::{MetadataExt, OpenOptionsExt},
    },
    path::{Path, PathBuf},
    sync::atomic::{AtomicBool, Ordering},
};
use term_contracts::workload::GroupRecoveryIdentity;

#[derive(Debug)]
pub(super) struct CgroupAnchor {
    directory: File,
    removed_by_owner: AtomicBool,
}

impl CgroupAnchor {
    pub fn open(path: &Path) -> io::Result<Self> {
        let directory = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(path)?;
        let mut stat = std::mem::MaybeUninit::<libc::statfs>::uninit();
        // SAFETY: the descriptor is live and stat has space for one statfs.
        if unsafe { libc::fstatfs(directory.as_raw_fd(), stat.as_mut_ptr()) } != 0 {
            return Err(io::Error::last_os_error());
        }
        if unsafe { stat.assume_init() }.f_type != libc::CGROUP2_SUPER_MAGIC {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "not a cgroup v2 directory",
            ));
        }
        Ok(Self {
            directory,
            removed_by_owner: AtomicBool::new(false),
        })
    }

    pub fn path(&self) -> PathBuf {
        PathBuf::from(format!("/proc/self/fd/{}", self.directory.as_raw_fd()))
    }

    pub fn removed(&self) -> io::Result<bool> {
        Ok(
            self.removed_by_owner.load(Ordering::Acquire)
                || self.directory.metadata()?.nlink() == 0,
        )
    }

    pub fn mark_removed(&self) {
        // kernfs may retain i_nlink on a pinned deleted directory. Only the
        // successful owned rmdir path may set this fallback, never ENOENT.
        self.removed_by_owner.store(true, Ordering::Release);
    }

    pub fn identity(&self) -> io::Result<GroupRecoveryIdentity> {
        if !crate::identity::boot_id_reliable() {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "boot identity unavailable",
            ));
        }
        // name_to_handle_at accepts a variable-length struct file_handle.
        // kernfs encodes exactly 8 bytes (kn->id) as FILEID_KERNFS (0xfe).
        #[repr(C)]
        struct Handle {
            bytes: u32,
            kind: i32,
            id: u64,
        }
        let mut handle = Handle {
            bytes: 8,
            kind: 0,
            id: 0,
        };
        let mut mount_id = 0;
        // SAFETY: handle has the header and requested 8-byte buffer, the empty
        // path is NUL terminated, and AT_EMPTY_PATH resolves the pinned FD.
        let result = unsafe {
            libc::name_to_handle_at(
                self.directory.as_raw_fd(),
                c"".as_ptr(),
                (&mut handle as *mut Handle).cast(),
                &mut mount_id,
                libc::AT_EMPTY_PATH,
            )
        };
        if result != 0 {
            return Err(io::Error::last_os_error());
        }
        if handle.bytes != 8 || handle.kind != 0xfe || handle.id == 0 {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "unrecognized cgroup kernel handle",
            ));
        }
        Ok(GroupRecoveryIdentity::CgroupV2 {
            boot_id: crate::identity::boot_id(),
            kernel_id: format!("{:016x}", handle.id),
        })
    }
}

/// Pin a process before checking its current membership. PID reuse after the
/// check cannot redirect the signal. Unsupported pidfds are an error, never a
/// reason to fall back to kill(pid). Force uses cgroup.kill where available.
pub(super) fn signal_member(dir: &Path, pid: u32, signal: i32) -> io::Result<()> {
    // SAFETY: pidfd_open has no pointer arguments and returns an owned FD.
    let fd = unsafe { libc::syscall(libc::SYS_pidfd_open, pid as libc::pid_t, 0) };
    if fd < 0 {
        let error = io::Error::last_os_error();
        return if error.raw_os_error() == Some(libc::ESRCH) {
            Ok(())
        } else {
            Err(error)
        };
    }
    let process = unsafe { File::from_raw_fd(fd as i32) };
    if !super::linux_cgroup::member_pids(dir)?.contains(&pid) {
        return Ok(());
    }
    let result = unsafe {
        libc::syscall(
            libc::SYS_pidfd_send_signal,
            process.as_raw_fd(),
            signal,
            std::ptr::null::<libc::siginfo_t>(),
            0,
        )
    };
    if result < 0 {
        let error = io::Error::last_os_error();
        if error.raw_os_error() != Some(libc::ESRCH) {
            return Err(error);
        }
    }
    Ok(())
}

pub(super) fn remove_empty_children(
    anchor: &CgroupAnchor,
    remaining: &mut usize,
    depth: usize,
) -> io::Result<()> {
    if depth > 32 {
        return Err(io::Error::other("cgroup cleanup depth limit"));
    }
    for entry in std::fs::read_dir(anchor.path())? {
        let entry = entry?;
        if !entry.file_type()?.is_dir() {
            continue;
        }
        *remaining = remaining
            .checked_sub(1)
            .ok_or_else(|| io::Error::other("cgroup cleanup entry limit"))?;
        let child = CgroupAnchor::open(&entry.path())?;
        if super::linux_cgroup::populated(&child.path())? {
            return Err(io::Error::other("cgroup cleanup found live descendants"));
        }
        remove_empty_children(&child, remaining, depth + 1)?;
        if CgroupAnchor::open(&entry.path())?.identity()? != child.identity()? {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "cgroup child was replaced",
            ));
        }
        std::fs::remove_dir(entry.path())?;
    }
    Ok(())
}
