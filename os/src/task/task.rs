//! Types related to task management & Functions for completely changing TCB

use super::id::TaskUserRes;
use super::{kstack_alloc, KernelStack, ProcessControlBlock, TaskContext};
use crate::trap::TrapContext;
use crate::{mm::PhysPageNum, sync::UPSafeCell};
use alloc::sync::{Arc, Weak};
use core::cell::RefMut;

/// The stride of a task with the lowest priority: one quantum costs this much.
pub const STRIDE_1: isize = 1024;
/// Smallest schedulable priority.
pub const MIN_PRIORITY: usize = 1;
/// Largest schedulable priority.
pub const MAX_PRIORITY: usize = 16;
/// Default priority of a freshly created task/thread.
pub const DEFAULT_PRIORITY: usize = 8;

/// The stride a task should carry right after its priority became `priority`.
///
/// A task that already ran for a while keeps its accumulated runtime (`ls`):
/// the new stride is what it *would* have reached at the new priority for the
/// same amount of service, which is exactly `STRIDE_1 / priority`.  A brand new
/// task (`ls == 0`) starts at `STRIDE_1 - STRIDE_1 / priority`, i.e. as if it
/// had just been scheduled once at the lowest priority. This follows the
/// rCore-Tutorial stride scheduler and is what makes the ratio of quanta in the
/// `ch5_stride*` tests equal the ratio of the priorities.
pub fn stride_of(priority: usize, last_stride: isize) -> isize {
    let stride_1 = (STRIDE_1 / priority as isize) as isize;
    if last_stride == 0 {
        STRIDE_1 - stride_1
    } else {
        stride_1
    }
}

/// Task control block structure
pub struct TaskControlBlock {
    /// immutable
    pub process: Weak<ProcessControlBlock>,    /// Kernel stack corresponding to PID
    pub kstack: KernelStack,
    /// mutable
    inner: UPSafeCell<TaskControlBlockInner>,
}

impl TaskControlBlock {
    /// Get the mutable reference of the inner TCB
    pub fn inner_exclusive_access(&self) -> RefMut<'_, TaskControlBlockInner> {
        self.inner.exclusive_access()
    }
    /// Set the priority of this task and recompute its stride so that the
    /// scheduler immediately honours the new value.  The new stride is relative
    /// to the smallest stride currently in the ready queue (see
    /// [`super::manager::TaskManager::add`], which rebases the queue onto a
    /// newcomer), so it does not have to be shifted by any global baseline.
    pub fn set_priority(&self, priority: usize) {
        let mut inner = self.inner_exclusive_access();
        inner.priority = priority;
        inner.stride = stride_of(priority, inner.last_stride);
    }
    /// Get the address of app's page table
    pub fn get_user_token(&self) -> usize {
        let process = self.process.upgrade().unwrap();
        let inner = process.inner_exclusive_access();
        inner.memory_set.token()
    }
}

/// Inner of the task control block, i.e. everything that may change while the
/// task lives. It is re-exported for [`crate::sync::resource`], which reads the
/// wait-for edges straight out of it.
pub struct TaskControlBlockInner {
    /// The user resources (tid, ustack, trap context) of this thread
    pub res: Option<TaskUserRes>,
    /// The physical page number of the frame where the trap context is placed
    pub trap_cx_ppn: PhysPageNum,
    /// Save task context
    pub task_cx: TaskContext,

    /// Maintain the execution status of the current process
    pub task_status: TaskStatus,
    /// It is set when active exit or execution error occurs
    pub exit_code: Option<i32>,
    /// The priority used by the stride scheduler (larger means more CPU).
    pub priority: usize,
    /// Stride accumulated so far; the ready task with the smallest stride is
    /// picked next by [`super::manager::TaskManager`].
    pub stride: isize,
    /// The stride this task had when its priority was last changed.  It is the
    /// reference point that keeps `stride` monotonic across priority changes.
    pub last_stride: isize,
    /// Deadlock detection: does this task currently hold a blocking mutex?
    pub hold_mutex: bool,
    /// Deadlock detection: the semaphore this task is waiting on, if any.
    pub wait_sem: Option<usize>,
    /// Deadlock detection: the mutex or condition variable this task is waiting
    /// on, if any.  Mutex and condition variables share one namespace because
    /// waiting for a condition variable means waiting for its mutex to be
    /// handed over.
    pub wait_mutex: Option<(usize, bool)>,
}

impl TaskControlBlockInner {
    /// Mutable reference to this thread's trap context (mapped in kernel space)
    pub fn get_trap_cx(&self) -> &'static mut TrapContext {
        self.trap_cx_ppn.get_mut()
    }

    #[allow(unused)]
    fn get_status(&self) -> TaskStatus {
        self.task_status
    }

    /// Consume one scheduling quantum: advance the stride by
    /// `STRIDE_1 / priority`.
    pub fn advance_stride(&mut self) {
        self.last_stride = self.stride;
        self.stride += (STRIDE_1 / self.priority as isize) as isize;
    }
}

impl TaskControlBlock {
    /// Create a new task
    pub fn new(
        process: Arc<ProcessControlBlock>,
        ustack_base: usize,
        alloc_user_res: bool,
    ) -> Self {
        let res = TaskUserRes::new(Arc::clone(&process), ustack_base, alloc_user_res);
        let trap_cx_ppn = res.trap_cx_ppn();
        let kstack = kstack_alloc();
        let kstack_top = kstack.get_top();
        Self {
            process: Arc::downgrade(&process),
            kstack,
            inner: unsafe {
                UPSafeCell::new(TaskControlBlockInner {
                    res: Some(res),
                    trap_cx_ppn,
                    task_cx: TaskContext::goto_trap_return(kstack_top),
                    task_status: TaskStatus::Ready,
                    exit_code: None,
                    priority: DEFAULT_PRIORITY,
                    stride: 0,
                    last_stride: 0,
                    hold_mutex: false,
                    wait_sem: None,
                    wait_mutex: None,
                })
            },
        }
    }
}

#[derive(Copy, Clone, PartialEq)]
/// The execution status of the current process
pub enum TaskStatus {
    /// ready to run
    Ready,
    /// running
    Running,
    /// blocked
    Blocked,
}
