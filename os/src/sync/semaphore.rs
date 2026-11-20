//! Semaphore

use super::resource::{with_resource_manager, Waiting};
use super::UPSafeCell;
use crate::task::{block_current_and_run_next, current_task, wakeup_task, TaskControlBlock};
use alloc::collections::VecDeque;
use alloc::sync::Arc;

/// semaphore structure
pub struct Semaphore {
    /// semaphore inner
    pub inner: UPSafeCell<SemaphoreInner>,
}

pub struct SemaphoreInner {
    pub count: isize,
    pub wait_queue: VecDeque<Arc<TaskControlBlock>>,
    /// Index inside the owning process' `semaphore_list`, filled in by
    /// `sys_semaphore_create`; it names this semaphore in the wait-for graph.
    pub ridx: usize,
}

impl Semaphore {
    /// Create a new semaphore
    pub fn new(res_count: usize) -> Self {
        trace!("kernel: Semaphore::new");
        Self {
            inner: unsafe {
                UPSafeCell::new(SemaphoreInner {
                    count: res_count as isize,
                    wait_queue: VecDeque::new(),
                    ridx: usize::MAX,
                })
            },
        }
    }

    /// up operation of semaphore
    pub fn up(&self) {
        trace!("kernel: Semaphore::up");
        let mut inner = self.inner.exclusive_access();
        inner.count += 1;
        let ridx = inner.ridx;
        if inner.count <= 0 {
            if let Some(task) = inner.wait_queue.pop_front() {
                let task = Arc::clone(&task);
                drop(inner);
                // mirror the hand-over in the graph before waking, so that no
                // stale "still waiting" edge is left behind
                with_resource_manager(|rm| rm.hand_over_semaphore(&task, ridx));
                wakeup_task(task);
            } else {
                drop(inner);
                with_resource_manager(|rm| rm.note_missing_up(ridx));
            }
            return;
        }
        drop(inner);
        with_resource_manager(|rm| rm.release_unit(ridx));
    }

    /// down operation of semaphore
    pub fn down(&self) {
        let _ = self.try_down_or_block();
    }

    /// Remember which slot of the owning process' `semaphore_list` this
    /// semaphore lives in, so lock/unlock can name it in the wait-for graph.
    pub fn set_ridx(&self, ridx: usize) {
        self.inner.exclusive_access().ridx = ridx;
    }

    /// The resource index of this semaphore, used by `sys_semaphore_down` to
    /// report a deadlock.
    pub fn ridx(&self) -> usize {
        self.inner.exclusive_access().ridx
    }

    /// Same as [`Semaphore::down`] but tells the caller that the request would
    /// deadlock instead of blocking forever. On `Err(())` nothing at all was
    /// changed (neither `count` nor the queues).
    pub fn try_down_or_block(&self) -> Result<(), ()> {
        let mut inner = self.inner.exclusive_access();
        let ridx = inner.ridx;
        // `count` is the number of units still available, so a request has to
        // block when it is already 0 — testing `< 0` here let one thread past
        // an exhausted semaphore (count went to -1 without queueing), which
        // breaks every exclusion/synchronisation test built on it.
        if inner.count <= 0 {
            // somebody already owns every unit: only now can a cycle appear
            drop(inner);
            match with_resource_manager(|rm| rm.acquire(Waiting::Semaphore(ridx))) {
                Err(()) => return Err(()),
                Ok(_) => {
                    let mut inner = self.inner.exclusive_access();
                    inner.count -= 1;
                    inner.wait_queue.push_back(current_task().unwrap());
                    drop(inner);
                    block_current_and_run_next();
                    Ok(())
                }
            }
        } else {
            inner.count -= 1;
            drop(inner);
            with_resource_manager(|rm| rm.grant(Waiting::Semaphore(ridx)));
            Ok(())
        }
    }
}
