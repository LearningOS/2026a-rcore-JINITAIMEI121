//!Implementation of [`TaskManager`]
use super::TaskControlBlock;
use crate::sync::UPSafeCell;
use alloc::collections::VecDeque;
use alloc::sync::Arc;
use lazy_static::*;

/// A task is queued at most once, so a plain `Vec` stays small and a linear scan for the
/// minimum virtual time is cheaper than maintaining an ordered structure.
pub struct TaskManager {
    ready_queue: VecDeque<Arc<TaskControlBlock>>,
}

/// A stride (lottery/virtual-time) scheduler.
///
/// Every process owns a virtual time `vtime` and a `stride`. `fetch()` hands the CPU to
/// the runnable process with the smallest `vtime` and then charges it `stride` ticks, so
/// over any window the CPU share of a process is proportional to `1 / stride`.
/// `add()` keeps the queue duplicate-free, which is what makes "re-charge only on
/// dispatch" well defined: a preempted task that goes back to the queue is not billed
/// twice for the same slice.
impl TaskManager {
    ///Creat an empty TaskManager
    pub fn new() -> Self {
        Self {
            ready_queue: VecDeque::new(),
        }
    }
    /// Add process back to ready queue
    pub fn add(&mut self, task: Arc<TaskControlBlock>) {
        // The virtual-time tie-break below relies on the queue holding each task at most
        // once. That holds by construction: a task is only added when it is created
        // (`sys_fork`/`sys_spawn`, `add_initproc`) or after it was taken out by `fetch`
        // (timer preemption in `suspend_current_and_run_next`). A defensive check would
        // have to compare `Arc`s, since pid/kernel-stack slots are recycled on exit --
        // and the current task is deliberately *not* in the queue while it runs, so an
        // `Arc::ptr_eq` scan here can never hit.
        debug_assert!(
            !self.ready_queue.iter().any(|t| Arc::ptr_eq(t, &task)),
            "task already queued"
        );
        self.ready_queue.push_back(task);
    }
    /// Take a process out of the ready queue
    pub fn fetch(&mut self) -> Option<Arc<TaskControlBlock>> {
        let best = self
            .ready_queue
            .iter()
            .enumerate()
            .min_by_key(|(idx, task)| {
                // Ordering key: smallest virtual time wins; ties break on the smaller
                // stride (higher priority first), then on arrival order -- `idx` is stable
                // and unique, so the comparison never depends on recycled ids.
                let inner = task.inner_exclusive_access();
                (inner.vtime, inner.stride, *idx)
            })?
            .0;
        let task = self.ready_queue.remove(best)?;
        // charge the slice we are about to run. `vtime`/`stride` live behind a
        // `RefCell`, so both borrows must not overlap.
        {
            let mut inner = task.inner_exclusive_access();
            inner.vtime += inner.stride;
        }
        Some(task)
    }
}

lazy_static! {
    /// TASK_MANAGER instance through lazy_static!
    pub static ref TASK_MANAGER: UPSafeCell<TaskManager> =
        unsafe { UPSafeCell::new(TaskManager::new()) };
}

/// Add process to ready queue
pub fn add_task(task: Arc<TaskControlBlock>) {
    //trace!("kernel: TaskManager::add_task");
    TASK_MANAGER.exclusive_access().add(task);
}

/// Take a process out of the ready queue
pub fn fetch_task() -> Option<Arc<TaskControlBlock>> {
    //trace!("kernel: TaskManager::fetch_task");
    TASK_MANAGER.exclusive_access().fetch()
}
