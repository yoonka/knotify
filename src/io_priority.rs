//! Best-effort, per-worker background I/O. Event requests retain normal priority.
pub struct Guard {
    #[cfg(target_os = "linux")]
    previous: Option<libc::c_long>,
    #[cfg(windows)]
    changed: bool,
}
impl Guard {
    pub fn enter(background: bool) -> Self {
        #[cfg(target_os = "linux")]
        {
            // IOPRIO_WHO_PROCESS with id 0 addresses this Linux task/thread.
            let prior = unsafe { libc::syscall(libc::SYS_ioprio_get, 1, 0) };
            let changed = background
                && prior >= 0
                && unsafe { libc::syscall(libc::SYS_ioprio_set, 1, 0, 3 << 13) } == 0;
            Self {
                previous: changed.then_some(prior),
            }
        }
        #[cfg(windows)]
        {
            use windows_sys::Win32::System::Threading::{GetCurrentThread, SetThreadPriority};
            let changed =
                background && unsafe { SetThreadPriority(GetCurrentThread(), 0x00010000) } != 0;
            Self { changed }
        }
        #[cfg(not(any(target_os = "linux", windows)))]
        {
            let _ = background;
            Self {}
        }
    }
}
impl Drop for Guard {
    fn drop(&mut self) {
        #[cfg(target_os = "linux")]
        if let Some(prior) = self.previous {
            unsafe {
                libc::syscall(libc::SYS_ioprio_set, 1, 0, prior);
            }
        }
        #[cfg(windows)]
        if self.changed {
            use windows_sys::Win32::System::Threading::{GetCurrentThread, SetThreadPriority};
            unsafe {
                SetThreadPriority(GetCurrentThread(), 0x00020000);
            }
        }
    }
}
