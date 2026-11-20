//!Implementation of [`TaskManager`]
use super::TaskControlBlock;
use crate::sync::UPSafeCell;
use crate::timer::get_time_ms;
use alloc::collections::VecDeque;
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::cell::RefCell;
use lazy_static::*;

///A array of `TaskControlBlock` that is thread-safe.
///
///The queue is kept sorted by the stride accumulated to every task, which is
///the classic stride scheduler: a task with a *smaller* accumulated stride has
///been served less (or has a *higher* priority), so it runs first.
///
///Accumulated strides grow without bound while the system runs, so they are
///always compared relative to a baseline (`min_stride`) that tracks the
///smallest accumulated stride in the queue. That keeps the comparison inside
///`usize` forever and is equivalent to the usual "subtract the minimum" trick.
pub struct TaskManager {
    ready_queue: VecDeque<Arc<TaskControlBlock>>,
    /// Baseline subtracted from every accumulated stride before the two are
    /// compared. It only ever decreases, so a task that is currently running
    /// (and hence not in the queue) can never see its key grow.
    min_stride: RefCell<usize>,
}

/// A stride scheduler.
impl TaskManager {
    ///Creat an empty TaskManager
    pub fn new() -> Self {
        Self {
            ready_queue: VecDeque::new(),
            min_stride: RefCell::new(0),
        }
    }
    /// The scheduling key of a queued task: its accumulated stride measured
    /// against the running baseline. Sleeping tasks are not queued at all, so
    /// their shared slot holds the virtual wake-up time instead.
    fn key(&self, task: &Arc<TaskControlBlock>) -> usize {
        let inner = task.inner_exclusive_access();
        inner.stride_accum.saturating_sub(*self.min_stride.borrow())
    }
    /// Lower the baseline to the smallest accumulated stride in the queue. It
    /// may only ever *decrease*: raising it would make the key of the task that
    /// is currently running (and therefore not queued) grow, which is exactly
    /// what a stride scheduler must never do. An empty queue leaves it alone,
    /// because keys are only ever compared relative to each other.
    fn sync_min_stride(&mut self) {
        if let Some(real_min) = self
            .ready_queue
            .iter()
            .map(|task| task.inner_exclusive_access().stride_accum)
            .min()
        {
            if real_min < *self.min_stride.borrow() {
                *self.min_stride.borrow_mut() = real_min;
            }
        }
    }
    /// Add process back to ready queue, keeping the queue sorted by stride
    pub fn add(&mut self, task: Arc<TaskControlBlock>) {
        let stride = task.inner_exclusive_access().stride_accum;
        // Drop a task that is already queued before inserting it at its new
        // position: `sys_set_priority` re-adds the running task, and if the old
        // entry stayed in the queue the process would be scheduled twice per
        // round (and its stride would be charged twice).
        // The comparison uses only the raw pointer, so no borrow of the TCB is
        // taken here.
        self.ready_queue
            .retain(|queued| !core::ptr::eq(Arc::as_ptr(queued), Arc::as_ptr(&task)));
        let baseline = *self.min_stride.borrow();
        let key = stride.saturating_sub(baseline);
        // Insert after the last task whose stride is not larger than ours, so
        // that equal strides keep FIFO order. That is what preserves the plain
        // round-robin behaviour all ch2/ch3/ch4 tests rely on.
        let mut pos = self.ready_queue.len();
        for (idx, queued) in self.ready_queue.iter().enumerate() {
            if self.key(queued) > key {
                pos = idx;
                break;
            }
        }
        self.ready_queue.insert(pos, task);
        self.sync_min_stride();
    }
    /// Take the most deserved process out of the ready queue and charge it
    /// one share of its stride
    pub fn fetch(&mut self) -> Option<Arc<TaskControlBlock>> {
        let task = self.ready_queue.pop_front()?;
        task.pass_stride();
        self.sync_min_stride();
        Some(task)
    }
    /// Length of the ready queue, only used for debugging
    #[allow(unused)]
    pub fn len(&self) -> usize {
        self.ready_queue.len()
    }
    /// Hand every task whose virtual wake-up time has passed back to the
    /// scheduler. Called once per timer tick, i.e. every 10ms of virtual time.
    pub fn wakeup_sleeping(&mut self) {
        let now = get_time_ms();
        // The two passes only borrow the TCBs one at a time, so no exclusive
        // borrow of the ready queue is held while a task is re-inserted.
        let mut woken: Vec<Arc<TaskControlBlock>> = Vec::new();
        for task in self.ready_queue.iter() {
            let due = {
                let inner = task.inner_exclusive_access();
                inner.is_sleeping() && inner.wake_ms() <= now
            };
            if due {
                woken.push(task.clone());
            }
        }
        for task in woken {
            task.wake_up(now);
            self.add(task);
        }
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

/// Resume every task that finished sleeping
pub fn wakeup_sleeping_tasks() {
    TASK_MANAGER.exclusive_access().wakeup_sleeping();
}
