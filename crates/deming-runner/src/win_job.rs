//! Windows Job Object wrapper for process-tree containment, CPU affinity, priority, and instant kill.

use std::sync::atomic::{AtomicBool, Ordering};

use deming_core::{DemingError, ProcessPriority, Result};

/// RAII wrapper around an OS process containment group (Windows Job Object).
///
/// Configured with `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE` on Windows so all assigned
/// processes and their descendants are terminated automatically when dropped or when
/// [`JobHandle::terminate_all`] is invoked.
#[derive(Debug)]
pub struct JobHandle {
    #[cfg(windows)]
    raw: isize,
    terminated: AtomicBool,
}

impl JobHandle {
    /// Creates a new Job Object configured with `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE`.
    pub fn new() -> Result<Self> {
        #[cfg(windows)]
        {
            use std::mem::{size_of, zeroed};
            use std::ptr::{null, null_mut};
            use windows_sys::Win32::Foundation::CloseHandle;
            use windows_sys::Win32::System::JobObjects::{
                CreateJobObjectW, JobObjectExtendedLimitInformation, SetInformationJobObject,
                JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
            };

            let handle = unsafe { CreateJobObjectW(null(), null()) };
            if handle.is_null() {
                return Err(DemingError::Process(format!(
                    "CreateJobObjectW failed: {}",
                    std::io::Error::last_os_error()
                )));
            }

            let mut info: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = unsafe { zeroed() };
            info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;

            let ok = unsafe {
                SetInformationJobObject(
                    handle,
                    JobObjectExtendedLimitInformation,
                    (&raw const info).cast(),
                    size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
                )
            };

            if ok == 0 {
                let err = std::io::Error::last_os_error();
                unsafe {
                    CloseHandle(handle);
                }
                return Err(DemingError::Process(format!(
                    "SetInformationJobObject(KILL_ON_JOB_CLOSE) failed: {err}"
                )));
            }

            let _ = null_mut::<core::ffi::c_void>();
            Ok(Self {
                raw: handle as isize,
                terminated: AtomicBool::new(false),
            })
        }

        #[cfg(not(windows))]
        {
            Ok(Self {
                terminated: AtomicBool::new(false),
            })
        }
    }

    /// Returns `true` if [`JobHandle::terminate_all`] has already been called.
    pub fn is_terminated(&self) -> bool {
        self.terminated.load(Ordering::SeqCst)
    }

    /// Assigns an existing process handle (`HANDLE` on Windows) to this Job Object.
    #[cfg(windows)]
    pub fn assign_process(&self, process_handle: std::os::windows::io::RawHandle) -> Result<()> {
        use windows_sys::Win32::Foundation::HANDLE;
        use windows_sys::Win32::System::JobObjects::AssignProcessToJobObject;

        if process_handle.is_null() {
            return Err(DemingError::Process(
                "cannot assign null process handle to job object".to_string(),
            ));
        }

        let job = self.raw as HANDLE;
        let ok = unsafe { AssignProcessToJobObject(job, process_handle as HANDLE) };
        if ok == 0 {
            return Err(DemingError::Process(format!(
                "AssignProcessToJobObject failed: {}",
                std::io::Error::last_os_error()
            )));
        }
        Ok(())
    }

    /// Fallback no-op stub on non-Windows platforms.
    #[cfg(not(windows))]
    pub fn assign_process(&self, _process_handle: usize) -> Result<()> {
        Ok(())
    }

    /// Assigns a `tokio::process::Child` to this Job Object.
    pub fn assign_tokio_child(&self, child: &tokio::process::Child) -> Result<()> {
        #[cfg(windows)]
        {
            if let Some(handle) = child.raw_handle() {
                self.assign_process(handle)?;
            } else if let Some(pid) = child.id() {
                self.assign_pid(pid)?;
            } else {
                return Err(DemingError::Process(
                    "cannot assign exited child without handle or PID".to_string(),
                ));
            }
            Ok(())
        }

        #[cfg(not(windows))]
        {
            let _ = child;
            Ok(())
        }
    }

    /// Assigns a `std::process::Child` to this Job Object.
    pub fn assign_std_child(&self, child: &std::process::Child) -> Result<()> {
        #[cfg(windows)]
        {
            use std::os::windows::io::AsRawHandle;
            self.assign_process(child.as_raw_handle())
        }

        #[cfg(not(windows))]
        {
            let _ = child;
            Ok(())
        }
    }

    /// Opens a process by PID and assigns it to this Job Object.
    pub fn assign_pid(&self, pid: u32) -> Result<()> {
        #[cfg(windows)]
        {
            use windows_sys::Win32::Foundation::CloseHandle;
            use windows_sys::Win32::System::Threading::{
                OpenProcess, PROCESS_SET_QUOTA, PROCESS_TERMINATE,
            };

            let handle =
                unsafe { OpenProcess(PROCESS_SET_QUOTA | PROCESS_TERMINATE, 0, pid) };
            if handle.is_null() {
                return Err(DemingError::Process(format!(
                    "OpenProcess({pid}) failed: {}",
                    std::io::Error::last_os_error()
                )));
            }
            let res = self.assign_process(handle as std::os::windows::io::RawHandle);
            unsafe {
                CloseHandle(handle);
            }
            res
        }

        #[cfg(not(windows))]
        {
            let _ = pid;
            Ok(())
        }
    }

    /// Terminates all processes in this Job Object immediately with exit code `1`.
    pub fn terminate_all(&self) -> Result<()> {
        self.terminate_with_exit_code(1)
    }

    /// Terminates all processes in this Job Object immediately with the given exit code.
    pub fn terminate_with_exit_code(&self, exit_code: u32) -> Result<()> {
        self.terminated.store(true, Ordering::SeqCst);

        #[cfg(windows)]
        {
            use windows_sys::Win32::Foundation::HANDLE;
            use windows_sys::Win32::System::JobObjects::TerminateJobObject;

            let ok = unsafe { TerminateJobObject(self.raw as HANDLE, exit_code) };
            if ok == 0 {
                return Err(DemingError::Process(format!(
                    "TerminateJobObject failed: {}",
                    std::io::Error::last_os_error()
                )));
            }
            Ok(())
        }

        #[cfg(not(windows))]
        {
            let _ = exit_code;
            Ok(())
        }
    }
}

#[cfg(windows)]
impl Drop for JobHandle {
    fn drop(&mut self) {
        use windows_sys::Win32::Foundation::{CloseHandle, HANDLE};
        if self.raw != 0 {
            unsafe {
                CloseHandle(self.raw as HANDLE);
            }
            self.raw = 0;
        }
    }
}

/// Applies CPU core affinity (up to `cpu_cores` logical processors) and OS scheduling priority
/// to the given Windows process handle.
#[cfg(windows)]
pub fn set_cpu_affinity_and_priority(
    process_handle: std::os::windows::io::RawHandle,
    cpu_cores: Option<usize>,
    priority: ProcessPriority,
) -> Result<()> {
    use windows_sys::Win32::Foundation::HANDLE;
    use windows_sys::Win32::System::Threading::{
        GetProcessAffinityMask, SetPriorityClass, SetProcessAffinityMask,
        ABOVE_NORMAL_PRIORITY_CLASS, BELOW_NORMAL_PRIORITY_CLASS, HIGH_PRIORITY_CLASS,
        NORMAL_PRIORITY_CLASS,
    };

    if process_handle.is_null() {
        return Err(DemingError::Process(
            "cannot configure null process handle".to_string(),
        ));
    }

    let handle = process_handle as HANDLE;

    let priority_class = match priority {
        ProcessPriority::BelowNormal => BELOW_NORMAL_PRIORITY_CLASS,
        ProcessPriority::Normal => NORMAL_PRIORITY_CLASS,
        ProcessPriority::AboveNormal => ABOVE_NORMAL_PRIORITY_CLASS,
        ProcessPriority::High => HIGH_PRIORITY_CLASS,
    };

    let ok = unsafe { SetPriorityClass(handle, priority_class) };
    if ok == 0 {
        return Err(DemingError::Process(format!(
            "SetPriorityClass failed: {}",
            std::io::Error::last_os_error()
        )));
    }

    if let Some(cores) = cpu_cores {
        if cores > 0 {
            let mut proc_mask: usize = 0;
            let mut sys_mask: usize = 0;
            let got = unsafe { GetProcessAffinityMask(handle, &raw mut proc_mask, &raw mut sys_mask) };
            if got == 0 {
                return Err(DemingError::Process(format!(
                    "GetProcessAffinityMask failed: {}",
                    std::io::Error::last_os_error()
                )));
            }

            let target_mask = select_affinity_mask(sys_mask, cores);
            if target_mask != 0 {
                let set_ok = unsafe { SetProcessAffinityMask(handle, target_mask) };
                if set_ok == 0 {
                    return Err(DemingError::Process(format!(
                        "SetProcessAffinityMask({target_mask:#x}) failed: {}",
                        std::io::Error::last_os_error()
                    )));
                }
            }
        }
    }

    Ok(())
}

/// Non-Windows fallback stub for [`set_cpu_affinity_and_priority`].
#[cfg(not(windows))]
pub fn set_cpu_affinity_and_priority(
    _process_handle: usize,
    _cpu_cores: Option<usize>,
    _priority: ProcessPriority,
) -> Result<()> {
    Ok(())
}

/// Applies CPU core affinity and OS scheduling priority to a `tokio::process::Child`.
pub fn configure_tokio_child(
    child: &tokio::process::Child,
    cpu_cores: Option<usize>,
    priority: ProcessPriority,
) -> Result<()> {
    #[cfg(windows)]
    {
        if let Some(handle) = child.raw_handle() {
            set_cpu_affinity_and_priority(handle, cpu_cores, priority)
        } else if let Some(pid) = child.id() {
            configure_pid(pid, cpu_cores, priority)
        } else {
            Ok(())
        }
    }

    #[cfg(not(windows))]
    {
        let _ = (child, cpu_cores, priority);
        Ok(())
    }
}

/// Applies CPU core affinity and OS scheduling priority to an existing PID.
pub fn configure_pid(
    pid: u32,
    cpu_cores: Option<usize>,
    priority: ProcessPriority,
) -> Result<()> {
    #[cfg(windows)]
    {
        use windows_sys::Win32::Foundation::CloseHandle;
        use windows_sys::Win32::System::Threading::{
            OpenProcess, PROCESS_QUERY_INFORMATION, PROCESS_SET_INFORMATION,
        };

        let handle = unsafe {
            OpenProcess(PROCESS_SET_INFORMATION | PROCESS_QUERY_INFORMATION, 0, pid)
        };
        if handle.is_null() {
            return Err(DemingError::Process(format!(
                "OpenProcess({pid}) failed: {}",
                std::io::Error::last_os_error()
            )));
        }
        let res = set_cpu_affinity_and_priority(
            handle as std::os::windows::io::RawHandle,
            cpu_cores,
            priority,
        );
        unsafe {
            CloseHandle(handle);
        }
        res
    }

    #[cfg(not(windows))]
    {
        let _ = (pid, cpu_cores, priority);
        Ok(())
    }
}

/// Selects up to `max_cores` lowest-index set bits from `available_mask`.
pub(crate) fn select_affinity_mask(available_mask: usize, max_cores: usize) -> usize {
    if max_cores == 0 || available_mask == 0 {
        return 0;
    }
    let mut selected = 0usize;
    let mut remaining = available_mask;
    let mut count = 0usize;
    while remaining != 0 && count < max_cores {
        let lowest_bit = remaining.isolate_lowest_one();
        selected |= lowest_bit;
        remaining &= !lowest_bit;
        count += 1;
    }
    selected
}

#[cfg(test)]
mod tests {
    use super::select_affinity_mask;

    #[test]
    fn affinity_mask_selects_lowest_available_bits() {
        assert_eq!(select_affinity_mask(0b1111, 2), 0b0011);
        assert_eq!(select_affinity_mask(0b1010_1000, 2), 0b0010_1000);
        assert_eq!(select_affinity_mask(0b0101, 8), 0b0101);
        assert_eq!(select_affinity_mask(0b1111, 0), 0);
        assert_eq!(select_affinity_mask(0, 2), 0);
    }
}
