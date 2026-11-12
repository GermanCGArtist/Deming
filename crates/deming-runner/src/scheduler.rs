//! Resource-aware concurrency & RAM budget scheduler and cooperative/hard abort handle.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use deming_core::{DemingError, GlobalResourceLimits, Result};
use tokio::sync::Notify;

use crate::win_job::JobHandle;

#[derive(Debug, Default)]
struct AbortState {
    aborted: AtomicBool,
    notify: Notify,
    jobs: Mutex<Vec<Arc<JobHandle>>>,
}

/// Shared handle used to abort an active test run, waking waiting tasks and
/// immediately terminating all registered [`JobHandle`] process trees.
#[derive(Debug, Clone, Default)]
pub struct AbortHandle {
    inner: Arc<AbortState>,
}

impl AbortHandle {
    /// Creates a new un-triggered [`AbortHandle`].
    pub fn new() -> Self {
        Self::default()
    }

    /// Returns `true` if abort has been requested.
    pub fn is_aborted(&self) -> bool {
        self.inner.aborted.load(Ordering::SeqCst)
    }

    /// Registers a [`JobHandle`] so it will be terminated immediately if [`AbortHandle::abort`] is called.
    ///
    /// If the abort handle was already triggered before registration, the job is terminated immediately.
    pub fn register_job(&self, job: Arc<JobHandle>) -> Result<()> {
        {
            let mut jobs = self
                .inner
                .jobs
                .lock()
                .map_err(|_| DemingError::Other("abort handle mutex poisoned".to_string()))?;
            jobs.push(Arc::clone(&job));
        }

        if self.is_aborted() {
            job.terminate_all()?;
        }
        Ok(())
    }

    /// Triggers abort: sets the atomic flag, wakes all waiters, and terminates all registered jobs.
    pub fn abort(&self) -> Result<()> {
        self.inner.aborted.store(true, Ordering::SeqCst);
        self.inner.notify.notify_waiters();

        let jobs = {
            let guard = self
                .inner
                .jobs
                .lock()
                .map_err(|_| DemingError::Other("abort handle mutex poisoned".to_string()))?;
            guard.clone()
        };

        let mut first_err: Option<DemingError> = None;
        for job in jobs {
            if !job.is_terminated() {
                if let Err(err) = job.terminate_all() {
                    if first_err.is_none() {
                        first_err = Some(err);
                    }
                }
            }
        }

        match first_err {
            Some(err) => Err(err),
            None => Ok(()),
        }
    }

    /// Completes when [`AbortHandle::abort`] is called (or immediately if already aborted).
    pub async fn cancelled(&self) {
        loop {
            let notified = self.inner.notify.notified();
            if self.is_aborted() {
                return;
            }
            notified.await;
            if self.is_aborted() {
                return;
            }
        }
    }
}

#[derive(Debug, Default)]
struct BudgetState {
    active_instances: usize,
    allocated_ram_mb: u64,
}

#[derive(Debug)]
struct SchedulerInner {
    max_total_ram_mb: u64,
    max_concurrent_instances: usize,
    state: Mutex<BudgetState>,
    notify: Notify,
}

/// Resource-aware scheduler enforcing global concurrent instance count and RAM budget.
#[derive(Debug, Clone)]
pub struct ResourceScheduler {
    inner: Arc<SchedulerInner>,
}

impl ResourceScheduler {
    /// Creates a new scheduler with explicit limits.
    pub fn new(max_total_ram_mb: u64, max_concurrent_instances: usize) -> Self {
        Self {
            inner: Arc::new(SchedulerInner {
                max_total_ram_mb,
                max_concurrent_instances,
                state: Mutex::new(BudgetState::default()),
                notify: Notify::new(),
            }),
        }
    }

    /// Creates a new scheduler from [`GlobalResourceLimits`].
    pub fn from_limits(limits: &GlobalResourceLimits) -> Self {
        Self::new(limits.max_total_ram_mb, limits.max_concurrent_instances)
    }

    /// Maximum total RAM budget in MiB.
    pub fn max_total_ram_mb(&self) -> u64 {
        self.inner.max_total_ram_mb
    }

    /// Maximum concurrent instances allowed.
    pub fn max_concurrent_instances(&self) -> usize {
        self.inner.max_concurrent_instances
    }

    /// Currently allocated RAM in MiB.
    pub fn used_ram_mb(&self) -> u64 {
        self.inner
            .state
            .lock()
            .map(|s| s.allocated_ram_mb)
            .unwrap_or(0)
    }

    /// Currently active instance count.
    pub fn active_instances(&self) -> usize {
        self.inner
            .state
            .lock()
            .map(|s| s.active_instances)
            .unwrap_or(0)
    }

    /// Remaining unallocated RAM in MiB.
    pub fn available_ram_mb(&self) -> u64 {
        self.inner
            .max_total_ram_mb
            .saturating_sub(self.used_ram_mb())
    }

    /// Attempts to acquire a permit for one instance using `ram_mb` MiB without waiting.
    pub fn try_acquire(&self, ram_mb: u64) -> Result<Option<ResourcePermit>> {
        self.validate_request(ram_mb)?;

        let mut state = self
            .inner
            .state
            .lock()
            .map_err(|_| DemingError::Other("scheduler mutex poisoned".to_string()))?;

        if state.active_instances < self.inner.max_concurrent_instances
            && state.allocated_ram_mb.saturating_add(ram_mb) <= self.inner.max_total_ram_mb
        {
            state.active_instances += 1;
            state.allocated_ram_mb += ram_mb;
            return Ok(Some(ResourcePermit {
                inner: Arc::clone(&self.inner),
                ram_mb,
            }));
        }

        Ok(None)
    }

    /// Asynchronously waits until both an instance slot and `ram_mb` MiB of RAM budget are available.
    ///
    /// Returns an error immediately if `ram_mb` exceeds `max_total_ram_mb`, if
    /// `max_concurrent_instances == 0`, or if `abort` is triggered while waiting.
    pub async fn acquire(&self, ram_mb: u64, abort: Option<&AbortHandle>) -> Result<ResourcePermit> {
        self.validate_request(ram_mb)?;

        loop {
            if let Some(abort_handle) = abort {
                if abort_handle.is_aborted() {
                    return Err(DemingError::Process(
                        "aborted while waiting for resource permit".to_string(),
                    ));
                }
            }

            let notified = self.inner.notify.notified();
            if let Some(permit) = self.try_acquire(ram_mb)? {
                return Ok(permit);
            }

            match abort {
                Some(abort_handle) => {
                    tokio::select! {
                        _ = notified => {}
                        _ = abort_handle.cancelled() => {
                            return Err(DemingError::Process(
                                "aborted while waiting for resource permit".to_string(),
                            ));
                        }
                    }
                }
                None => {
                    notified.await;
                }
            }
        }
    }

    fn validate_request(&self, ram_mb: u64) -> Result<()> {
        if self.inner.max_concurrent_instances == 0 {
            return Err(DemingError::Config(
                "max_concurrent_instances must be at least 1".to_string(),
            ));
        }
        if ram_mb > self.inner.max_total_ram_mb {
            return Err(DemingError::Config(format!(
                "requested instance RAM ({ram_mb} MiB) exceeds global max_total_ram_mb ({} MiB)",
                self.inner.max_total_ram_mb
            )));
        }
        Ok(())
    }
}

/// RAII resource permit representing one running slot and its reserved RAM budget.
#[derive(Debug)]
pub struct ResourcePermit {
    inner: Arc<SchedulerInner>,
    ram_mb: u64,
}

impl ResourcePermit {
    /// Returns the RAM (in MiB) reserved by this permit.
    pub fn ram_mb(&self) -> u64 {
        self.ram_mb
    }
}

impl Drop for ResourcePermit {
    fn drop(&mut self) {
        if let Ok(mut state) = self.inner.state.lock() {
            state.active_instances = state.active_instances.saturating_sub(1);
            state.allocated_ram_mb = state.allocated_ram_mb.saturating_sub(self.ram_mb);
        }
        self.inner.notify.notify_waiters();
    }
}
