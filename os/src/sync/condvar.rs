//! Conditian variable

use crate::sync::resource::with_resource_manager;
use crate::sync::{Mutex, UPSafeCell};
use crate::task::{block_current_and_run_next, current_task, wakeup_task, TaskControlBlock};
use alloc::{collections::VecDeque, sync::Arc};

/// Condition variable structure
pub struct Condvar {
    /// Condition variable inner
    pub inner: UPSafeCell<CondvarInner>,
}

pub struct CondvarInner {
    pub wait_queue: VecDeque<Arc<TaskControlBlock>>,
    /// Index inside the owning process' `condvar_list`, so that the deadlock
    /// detector can name the mutex this condvar guards.
    pub ridx: usize,
}

impl Condvar {
    /// Create a new condition variable
    pub fn new() -> Self {
        trace!("kernel: Condvar::new");
        Self {
            inner: unsafe {
                UPSafeCell::new(CondvarInner {
                    wait_queue: VecDeque::new(),
                    ridx: usize::MAX,
                })
            },
        }
    }

    /// The resource index of this condvar (== the slot it lives in).
    pub fn ridx(&self) -> usize {
        self.inner.exclusive_access().ridx
    }

    /// Remember which slot of the owning process' `condvar_list` this condvar
    /// lives in, so that wait/signal can name it in the wait-for graph.
    pub fn set_ridx(&self, ridx: usize) {
        self.inner.exclusive_access().ridx = ridx;
    }

    /// Signal a task waiting on the condition variable
    pub fn signal(&self) {
        let mut inner = self.inner.exclusive_access();
        if let Some(task) = inner.wait_queue.pop_front() {
            let task = Arc::clone(&task);
            drop(inner);
            // The woken thread re-acquires the guard mutex; ownership is
            // transferred by `Mutex::unlock`/`Condvar::wait`, so here we only
            // have to erase "this thread waits for condvar ridx".
            with_resource_manager(|rm| rm.condvar_signalled(&task));
            wakeup_task(task);
        }
    }

    /// blocking current task, let it wait on the condition variable
    pub fn wait(&self, mutex: Arc<dyn Mutex>) {
        trace!("kernel: Condvar::wait_with_mutex");
        // `mutex.unlock()` already drops our holder edge in the graph
        let guard_ridx = mutex.ridx();
        mutex.unlock();
        let mut inner = self.inner.exclusive_access();
        let ridx = inner.ridx;
        inner.wait_queue.push_back(current_task().unwrap());
        drop(inner);
        // From now on this thread waits for a signal. Signalling is done by a
        // thread that usually holds (or is waiting for) the guard mutex, so the
        // condvar is modelled as a one-unit resource in the same namespace as
        // mutexes -- see `resource.rs`.
        with_resource_manager(|rm| rm.condvar_waiting(ridx));
        block_current_and_run_next();
        // resumed by `signal`: our condvar edge disappears, then we queue on
        // the guard mutex like any other `mutex_lock` would.
        with_resource_manager(|rm| rm.condvar_returned(guard_ridx));
        mutex.lock();
    }
}
