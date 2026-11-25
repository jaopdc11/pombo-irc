//! Best-effort process hardening: no core dumps, no ptrace by same-uid processes, and
//! `mlock` for buffers we own. Keys inside `snow`'s state cannot be locked from here; with
//! dumps off that is the accepted limit.

/// What [`apply`] managed to switch on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HardenReport {
    pub dumpable_off: bool,
    pub core_limit_zero: bool,
}

/// Marks the process non-dumpable (`PR_SET_DUMPABLE` 0) and sets `RLIMIT_CORE` to 0.
/// Never panics; failures are reported as `false`.
pub fn apply() -> HardenReport {
    // SAFETY: plain syscalls with integer arguments and a valid pointer to a local struct.
    let dumpable_off = unsafe {
        libc::prctl(
            libc::PR_SET_DUMPABLE,
            0 as libc::c_ulong,
            0 as libc::c_ulong,
            0 as libc::c_ulong,
            0 as libc::c_ulong,
        )
    } == 0;
    let zero = libc::rlimit {
        rlim_cur: 0,
        rlim_max: 0,
    };
    let core_limit_zero = unsafe { libc::setrlimit(libc::RLIMIT_CORE, &zero) } == 0;
    HardenReport {
        dumpable_off,
        core_limit_zero,
    }
}

/// Best-effort `mlock` of `buf` so it is never swapped out. Returns false on failure
/// (for example past `RLIMIT_MEMLOCK`); never panics. An empty buffer counts as locked.
pub fn lock(buf: &[u8]) -> bool {
    if buf.is_empty() {
        return true;
    }
    // SAFETY: the pointer and length describe memory borrowed for the whole call.
    unsafe { libc::mlock(buf.as_ptr().cast(), buf.len()) == 0 }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hardening_applies() {
        let r = apply();
        assert!(r.dumpable_off && r.core_limit_zero);
        assert_eq!(unsafe { libc::prctl(libc::PR_GET_DUMPABLE) }, 0);
        let mut lim = libc::rlimit {
            rlim_cur: 1,
            rlim_max: 1,
        };
        assert_eq!(unsafe { libc::getrlimit(libc::RLIMIT_CORE, &mut lim) }, 0);
        assert_eq!((lim.rlim_cur, lim.rlim_max), (0, 0));
    }

    #[test]
    fn lock_never_panics() {
        assert!(lock(&[]));
        let buf = vec![7u8; 4096];
        let _ = lock(&buf);
    }
}
