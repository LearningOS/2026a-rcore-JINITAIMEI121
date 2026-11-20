//! Implementation of [`TaskManager`]
//!
//! It is only used to manage processes and schedule process based on ready queue.
//! Other CPU process monitoring functions are in Processor.

use super::{ProcessControlBlock, TaskControlBlock, TaskStatus};
use crate::sync::UPSafeCell;
use alloc::collections::{BTreeMap, VecDeque};
use alloc::sync::Arc;
use lazy_static::*;
///A array of `TaskControlBlock` that is thread-safe
pub struct TaskManager {
    ready_queue: VecDeque<Arc<TaskControlBlock>>,

    /// The stopping task, leave a reference so that the kernel stack will not be recycled when switching tasks
    stop_task: Option<Arc<TaskControlBlock>>,
}

/// A stride scheduler.
///
/// Every task owns a `stride` counter which is advanced by
/// `STRIDE_1 / priority` each time it consumes a scheduling quantum.  The
/// ready task with the smallest stride runs next, which makes the CPU time
/// distribution proportional to the priorities (`ch5_stride*` tests).  A task
/// that joins the queue with a stride below everybody else's (a woken-up
/// sleeping task, a brand new process) is raised to the current baseline, so
/// it neither jumps the whole queue nor drags the other counters around.
impl TaskManager {
    ///Creat an empty TaskManager
    pub fn new() -> Self {
        Self {
            ready_queue: VecDeque::new(),
            stop_task: None,
        }
    }
    /// Add process back to ready queue
    pub fn add(&mut self, task: Arc<TaskControlBlock>) {
        // Raise the newcomer to the baseline instead of shifting every queued
        // task up by `min - stride`: the latter adds the queue minimum to all
        // strides on *every* wakeup, so they grow exponentially and wrap
        // `isize` within seconds.  A wrapped task sits at the smallest stride
        // forever and starves every runnable thread, silently.
        let base = self
            .ready_queue
            .iter()
            .map(|t| t.inner_exclusive_access().stride)
            .min();
        if let Some(min_stride) = base {
            let mut inner = task.inner_exclusive_access();
            if inner.stride < min_stride {
                inner.stride = min_stride;
            }
        }
        self.ready_queue.push_back(task);
    }
    /// Take the most urgent process out of the ready queue
    pub fn fetch(&mut self) -> Option<Arc<TaskControlBlock>> {
        let min_idx = self
            .ready_queue
            .iter()
            .enumerate()
            .min_by_key(|(_, t)| t.inner_exclusive_access().stride)
            .map(|(i, _)| i)?;
        let task = self.ready_queue.remove(min_idx)?;
        // The task is about to consume one scheduling quantum, so charge it for
        // it now: at `fetch` time (instead of at `suspend` time) because a task
        // that gets woken up and then exits may never be suspended at all.
        task.inner_exclusive_access().advance_stride();
        Some(task)
    }
    pub fn remove(&mut self, task: Arc<TaskControlBlock>) {
        if let Some((id, _)) = self
            .ready_queue
            .iter()
            .enumerate()
            .find(|(_, t)| Arc::as_ptr(t) == Arc::as_ptr(&task))
        {
            self.ready_queue.remove(id);
        }
    }
    /// Add a task to stopping task
    pub fn add_stop(&mut self, task: Arc<TaskControlBlock>) {
        // NOTE: as the last stopping task has completely stopped (not
        // using kernel stack any more, at least in the single-core
        // case) so that we can simply replace it;
        self.stop_task = Some(task);
    }

}

lazy_static! {
    /// TASK_MANAGER instance through lazy_static!
    pub static ref TASK_MANAGER: UPSafeCell<TaskManager> =
        unsafe { UPSafeCell::new(TaskManager::new()) };
    /// PID2PCB instance (map of pid to pcb)
    pub static ref PID2PCB: UPSafeCell<BTreeMap<usize, Arc<ProcessControlBlock>>> =
        unsafe { UPSafeCell::new(BTreeMap::new()) };
}

/// Add a task to ready queue
pub fn add_task(task: Arc<TaskControlBlock>) {
    //trace!("kernel: TaskManager::add_task");
    TASK_MANAGER.exclusive_access().add(task);
}

/// Wake up a task
pub fn wakeup_task(task: Arc<TaskControlBlock>) {
    trace!("kernel: TaskManager::wakeup_task");
    let mut task_inner = task.inner_exclusive_access();
    task_inner.task_status = TaskStatus::Ready;
    // A blocked task was not charged for the time it spent waiting, so let it
    // re-enter the queue at the current baseline: `TaskManager::add` raises it
    // to the smallest stride already queued, which keeps the order between the
    // already runnable tasks untouched.
    task_inner.stride = 0;
    drop(task_inner);
    add_task(task);
}

/// Remove a task from the ready queue
pub fn remove_task(task: Arc<TaskControlBlock>) {
    //trace!("kernel: TaskManager::remove_task");
    TASK_MANAGER.exclusive_access().remove(task);
}

/// Fetch a task out of the ready queue
pub fn fetch_task() -> Option<Arc<TaskControlBlock>> {
    //trace!("kernel: TaskManager::fetch_task");
    TASK_MANAGER.exclusive_access().fetch()
}

/// Set a task to stop-wait status, waiting for its kernel stack out of use.
pub fn add_stopping_task(task: Arc<TaskControlBlock>) {
    TASK_MANAGER.exclusive_access().add_stop(task);
}

/// Get process by pid
pub fn pid2process(pid: usize) -> Option<Arc<ProcessControlBlock>> {
    let map = PID2PCB.exclusive_access();
    map.get(&pid).map(Arc::clone)
}

/// Insert item(pid, pcb) into PID2PCB map (called by do_fork AND ProcessControlBlock::new)
pub fn insert_into_pid2process(pid: usize, process: Arc<ProcessControlBlock>) {
    PID2PCB.exclusive_access().insert(pid, process);
}

/// Remove item(pid, _some_pcb) from PDI2PCB map (called by exit_current_and_run_next)
pub fn remove_from_pid2process(pid: usize) {
    let mut map = PID2PCB.exclusive_access();
    if map.remove(&pid).is_none() {
        panic!("cannot find pid {} in pid2task!", pid);
    }
}
