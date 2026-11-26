//! Best-effort process hardening: keep other processes of the same user out of our memory,
//! keep secrets out of crash dumps, and lock buffers we own into RAM. Keys inside `snow`'s
//! state cannot be locked from here; with dumps off that is the accepted limit.
//!
//! - Linux: `PR_SET_DUMPABLE` 0 (also blocks same-uid `ptrace`), `RLIMIT_CORE` 0, `mlock`.
//! - Windows: a process DACL that only lets the user wait on, query and end the process (no
//!   memory reads, no handle duplication, no injected threads), Windows Error Reporting told to
//!   leave the heap out of crash reports and not to show a dialog, `VirtualLock`.

/// What [`apply`] managed to switch on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HardenReport {
    /// Other processes of the same user can no longer read or debug this one.
    pub memory_access_blocked: bool,
    /// Crash dumps are off (Linux) or carry no heap (Windows).
    pub dumps_limited: bool,
}

/// Applies every hardening step available on this platform.
/// Never panics; failures are reported as `false`.
pub fn apply() -> HardenReport {
    imp::apply()
}

/// Best-effort lock of `buf` into RAM so it is never swapped out. Returns false on failure
/// (for example past the lock limit); never panics. An empty buffer counts as locked.
pub fn lock(buf: &[u8]) -> bool {
    if buf.is_empty() {
        return true;
    }
    imp::lock(buf)
}

#[cfg(unix)]
mod imp {
    use super::HardenReport;

    pub fn apply() -> HardenReport {
        #[cfg(target_os = "linux")]
        // SAFETY: plain syscall with integer arguments.
        let memory_access_blocked = unsafe {
            libc::prctl(
                libc::PR_SET_DUMPABLE,
                0 as libc::c_ulong,
                0 as libc::c_ulong,
                0 as libc::c_ulong,
                0 as libc::c_ulong,
            )
        } == 0;
        #[cfg(not(target_os = "linux"))]
        let memory_access_blocked = false;
        let zero = libc::rlimit {
            rlim_cur: 0,
            rlim_max: 0,
        };
        // SAFETY: valid pointer to a local struct.
        let dumps_limited = unsafe { libc::setrlimit(libc::RLIMIT_CORE, &zero) } == 0;
        HardenReport {
            memory_access_blocked,
            dumps_limited,
        }
    }

    pub fn lock(buf: &[u8]) -> bool {
        // SAFETY: the pointer and length describe memory borrowed for the whole call.
        unsafe { libc::mlock(buf.as_ptr().cast(), buf.len()) == 0 }
    }
}

#[cfg(windows)]
mod imp {
    use std::mem::size_of;
    use std::ptr::{null, null_mut};

    use windows_sys::Win32::Foundation::{CloseHandle, ERROR_SUCCESS, HANDLE};
    use windows_sys::Win32::Security::Authorization::{SetSecurityInfo, SE_KERNEL_OBJECT};
    use windows_sys::Win32::Security::{
        AddAccessAllowedAce, CreateWellKnownSid, GetLengthSid, GetTokenInformation, InitializeAcl,
        TokenUser, WinLocalSystemSid, ACCESS_ALLOWED_ACE, ACL, ACL_REVISION,
        DACL_SECURITY_INFORMATION, TOKEN_QUERY, TOKEN_USER,
    };
    use windows_sys::Win32::System::Diagnostics::Debug::{
        SetErrorMode, SEM_FAILCRITICALERRORS, SEM_NOGPFAULTERRORBOX,
    };
    use windows_sys::Win32::System::ErrorReporting::{
        WerSetFlags, WER_FAULT_REPORTING_FLAG_NOHEAP,
    };
    use windows_sys::Win32::System::Memory::VirtualLock;
    use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

    use super::HardenReport;

    // Access rights, spelled out so they do not depend on where windows-sys files them.
    const SYNCHRONIZE: u32 = 0x0010_0000;
    const PROCESS_TERMINATE: u32 = 0x0001;
    const PROCESS_QUERY_LIMITED_INFORMATION: u32 = 0x1000;
    const PROCESS_ALL_ACCESS: u32 = 0x001F_FFFF;
    /// `SECURITY_MAX_SID_SIZE`.
    const MAX_SID: usize = 68;

    pub fn apply() -> HardenReport {
        // SAFETY: plain calls with integer flags.
        let dumps_limited = unsafe {
            SetErrorMode(SEM_FAILCRITICALERRORS | SEM_NOGPFAULTERRORBOX);
            WerSetFlags(WER_FAULT_REPORTING_FLAG_NOHEAP) >= 0
        };
        HardenReport {
            memory_access_blocked: restrict_process_dacl(),
            dumps_limited,
        }
    }

    pub fn lock(buf: &[u8]) -> bool {
        // SAFETY: the pointer and length describe memory borrowed for the whole call.
        unsafe { VirtualLock(buf.as_ptr().cast(), buf.len()) != 0 }
    }

    /// Replaces the process DACL with one that gives the current user only SYNCHRONIZE,
    /// PROCESS_QUERY_LIMITED_INFORMATION and PROCESS_TERMINATE, and SYSTEM full access
    /// (the console subsystem runs Ctrl handlers through it). Same approach as KeePassXC.
    fn restrict_process_dacl() -> bool {
        let Some(user_sid) = current_user_sid() else {
            return false;
        };
        let mut system_sid = [0u32; MAX_SID / 4];
        let mut system_len = MAX_SID as u32;
        // SAFETY: the buffer is MAX_SID bytes and its length is passed alongside.
        if unsafe {
            CreateWellKnownSid(
                WinLocalSystemSid,
                null_mut(),
                system_sid.as_mut_ptr().cast(),
                &mut system_len,
            )
        } == 0
        {
            return false;
        }
        let user_sid_ptr = user_sid.sid();
        let system_sid_ptr = system_sid.as_mut_ptr().cast();
        // SAFETY: both pointers point at valid SIDs that outlive this function's calls.
        let sids_len = unsafe { GetLengthSid(user_sid_ptr) + GetLengthSid(system_sid_ptr) };
        let ace_head = (size_of::<ACCESS_ALLOWED_ACE>() - size_of::<u32>()) as u32;
        let acl_len = size_of::<ACL>() as u32 + 2 * ace_head + sids_len;
        // u32 storage keeps the ACL DWORD-aligned as the API requires.
        let mut acl_buf = vec![0u32; (acl_len as usize).div_ceil(4)];
        let acl: *mut ACL = acl_buf.as_mut_ptr().cast();
        // SAFETY: `acl` points at `acl_len` writable bytes; the SIDs are valid.
        unsafe {
            InitializeAcl(acl, acl_len, ACL_REVISION) != 0
                && AddAccessAllowedAce(
                    acl,
                    ACL_REVISION,
                    SYNCHRONIZE | PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_TERMINATE,
                    user_sid_ptr,
                ) != 0
                && AddAccessAllowedAce(acl, ACL_REVISION, PROCESS_ALL_ACCESS, system_sid_ptr) != 0
                && SetSecurityInfo(
                    GetCurrentProcess(),
                    SE_KERNEL_OBJECT,
                    DACL_SECURITY_INFORMATION,
                    null_mut(),
                    null_mut(),
                    acl,
                    null(),
                ) == ERROR_SUCCESS
        }
    }

    /// The `TOKEN_USER` blob of the current process token; the SID lives inside it.
    struct UserSid(Vec<u64>);

    impl UserSid {
        fn sid(&self) -> *mut core::ffi::c_void {
            // SAFETY: the buffer was filled by GetTokenInformation(TokenUser) and is aligned.
            unsafe { (*(self.0.as_ptr() as *const TOKEN_USER)).User.Sid }
        }
    }

    fn current_user_sid() -> Option<UserSid> {
        let mut token: HANDLE = null_mut();
        // SAFETY: the pseudo-handle is always valid; `token` is a local out-parameter.
        if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) } == 0 {
            return None;
        }
        let mut len = 0u32;
        // SAFETY: a null buffer with length 0 asks for the needed size.
        unsafe { GetTokenInformation(token, TokenUser, null_mut(), 0, &mut len) };
        let mut buf = vec![0u64; (len as usize).div_ceil(8).max(1)];
        // SAFETY: `buf` holds at least `len` bytes.
        let ok = unsafe {
            GetTokenInformation(token, TokenUser, buf.as_mut_ptr().cast(), len, &mut len)
        } != 0;
        // SAFETY: `token` came from OpenProcessToken and is closed once.
        unsafe { CloseHandle(token) };
        ok.then_some(UserSid(buf))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(target_os = "linux")]
    #[test]
    fn hardening_applies() {
        let r = apply();
        assert!(r.memory_access_blocked && r.dumps_limited);
        assert_eq!(unsafe { libc::prctl(libc::PR_GET_DUMPABLE) }, 0);
        let mut lim = libc::rlimit {
            rlim_cur: 1,
            rlim_max: 1,
        };
        assert_eq!(unsafe { libc::getrlimit(libc::RLIMIT_CORE, &mut lim) }, 0);
        assert_eq!((lim.rlim_cur, lim.rlim_max), (0, 0));
    }

    #[cfg(windows)]
    #[test]
    fn hardening_applies() {
        let r = apply();
        assert!(r.memory_access_blocked && r.dumps_limited);
    }

    #[test]
    fn lock_never_panics() {
        assert!(lock(&[]));
        let buf = vec![7u8; 4096];
        let _ = lock(&buf);
    }
}
