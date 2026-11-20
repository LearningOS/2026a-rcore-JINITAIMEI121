//! Mutex (spin-like and blocking(sleep))

use super::resource::{with_resource_manager, Waiting};
use super::UPSafeCell;
use crate::task::TaskControlBlock;
use crate::task::{block_current_and_run_next, suspend_current_and_run_next};
use crate::task::{current_task, wakeup_task};
use alloc::collections::VecDeque;
use alloc::sync::Arc;

/// Mutex trait
pub trait Mutex: Sync + Send {
    /// Lock the mutex
    fn lock(&self);
    /// Unlock the mutex
    fn unlock(&self);
    /// The resource index of this mutex inside its owning process. Only
    /// blocking mutexes take part in deadlock detection, so the spin variant
    /// reports `None` and the detector is left alone.
    fn ridx(&self) -> Option<usize> {
        None
    }
    /// Block until the mutex is acquired. `Err(())` means the request would
    /// close a cycle in the wait-for graph and *nothing* was changed, which
    /// lets `sys_mutex_lock` return `-0xdead` instead of hanging forever.
    fn try_lock_or_block(&self) -> Result<(), ()> {
        self.lock();
        Ok(())
    }
}

/// Spinlock Mutex struct
pub struct MutexSpin {
    locked: UPSafeCell<bool>,
}

impl MutexSpin {
    /// Create a new spinlock mutex
    pub fn new() -> Self {
        Self {
            locked: unsafe { UPSafeCell::new(false) },
        }
    }
}

impl Mutex for MutexSpin {
    /// Lock the spinlock mutex
    fn lock(&self) {
        trace!("kernel: MutexSpin::lock");
        loop {
            let mut locked = self.locked.exclusive_access();
            if *locked {
                drop(locked);
                suspend_current_and_run_next();
                continue;
            } else {
                *locked = true;
                return;
            }
        }
    }

    fn unlock(&self) {
        trace!("kernel: MutexSpin::unlock");
        let mut locked = self.locked.exclusive_access();
        *locked = false;
    }

    /// A spin mutex never blocks, so it cannot take part in a deadlock cycle.
    fn try_lock_or_block(&self) -> Result<(), ()> {
        self.lock();
        Ok(())
    }
}

/// Blocking Mutex struct
pub struct MutexBlocking {
    inner: UPSafeCell<MutexBlockingInner>,
}

pub struct MutexBlockingInner {
    locked: bool,
    wait_queue: VecDeque<Arc<TaskControlBlock>>,
    /// Index inside the owning process' `mutex_list`, filled in by
    /// `sys_mutex_create`; it names this mutex in the wait-for graph.
    pub ridx: usize,
}

impl MutexBlocking {
    /// Create a new blocking mutex
    pub fn new() -> Self {
        trace!("kernel: MutexBlocking::new");
        Self {
            inner: unsafe {
                UPSafeCell::new(MutexBlockingInner {
                    locked: false,
                    wait_queue: VecDeque::new(),
                    ridx: usize::MAX,
                })
            },
        }
    }

    /// Remember which slot of the owning process' `mutex_list` this mutex lives
    /// in, so that lock/unlock can name it in the wait-for graph.
    pub fn set_ridx(&self, ridx: usize) {
        self.inner.exclusive_access().ridx = ridx;
    }
}

impl Mutex for MutexBlocking {
    /// Lock the blocking mutex. An uncontended lock simply takes it; a
    /// contended one first consults the deadlock detector.
    fn lock(&self) {
        let _ = self.try_lock_or_block();
    }

    fn ridx(&self) -> Option<usize> {
        Some(self.inner.exclusive_access().ridx)
    }

    fn try_lock_or_block(&self) -> Result<(), ()> {
        let mut mutex_inner = self.inner.exclusive_access();
        if !mutex_inner.locked {
            mutex_inner.locked = true;
            let ridx = mutex_inner.ridx;
            drop(mutex_inner);
            with_resource_manager(|rm| rm.grant(Waiting::Mutex(ridx)));
            return Ok(());
        }
        let ridx = mutex_inner.ridx;
        drop(mutex_inner);
        match with_resource_manager(|rm| rm.acquire(Waiting::Mutex(ridx))) {
            Err(()) => Err(()),
            Ok(_) => {
                let mut mutex_inner = self.inner.exclusive_access();
                mutex_inner.wait_queue.push_back(current_task().unwrap());
                drop(mutex_inner);
                block_current_and_run_next();
                // resumed: the unlocking thread handed us the lock already
                Ok(())
            }
        }
    }

    /// Unlock the blocking mutex, handing ownership straight to the first
    /// waiter when there is one (that is why the woken task does not have to
    /// re-acquire anything).
    fn unlock(&self) {
        trace!("kernel: MutexBlocking::unlock");
        let mut mutex_inner = self.inner.exclusive_access();
        assert!(mutex_inner.locked);
        let ridx = mutex_inner.ridx;
        if let Some(waking_task) = mutex_inner.wait_queue.pop_front() {
            let waking_task = Arc::clone(&waking_task);
            drop(mutex_inner);
            let pid = waking_task
                .process
                .upgrade()
                .map(|p| p.getpid())
                .unwrap_or(0);
            with_resource_manager(|rm| rm.wake_mutex_holder(pid, ridx, &waking_task));
            wakeup_task(waking_task);
        } else {
            mutex_inner.locked = false;
            drop(mutex_inner);
            with_resource_manager(|rm| rm.drop_holder(Waiting::Mutex(ridx)));
        }
    }
}
