//! Types related to task management

use super::TaskContext;

/// Highest syscall id we are willing to keep a counter for. The tutorial's
/// syscall ids all sit below 512, so one slot per id is both simple and enough.
pub const SYSCALL_COUNTER_SLOTS: usize = 512;

/// The task control block (TCB) of a task.
#[derive(Copy, Clone)]
pub struct TaskControlBlock {
    /// The task status in it's lifecycle
    pub task_status: TaskStatus,
    /// The task context
    pub task_cx: TaskContext,
    /// How often this task has trapped into the kernel for each syscall id.
    /// This is what `sys_trace` reports; the trap handler bumps it as soon as
    /// the environment call is taken, so a syscall counts itself before its own
    /// result reaches the application.
    pub syscall_count: [usize; SYSCALL_COUNTER_SLOTS],
}

/// A freshly created TCB, with every syscall counter at zero.
impl TaskControlBlock {
    /// Build an uninitialized task control block.
    pub fn new() -> Self {
        Self {
            task_status: TaskStatus::UnInit,
            task_cx: TaskContext::zero_init(),
            syscall_count: [0; SYSCALL_COUNTER_SLOTS],
        }
    }
}

/// The status of a task
#[derive(Copy, Clone, PartialEq)]
pub enum TaskStatus {
    /// uninitialized
    UnInit,
    /// ready to run
    Ready,
    /// running
    Running,
    /// exited
    Exited,
}
