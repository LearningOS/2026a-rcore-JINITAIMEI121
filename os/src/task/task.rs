//! Types related to task management & Functions for completely changing TCB
use super::TaskContext;
use super::{kstack_alloc, pid_alloc, KernelStack, PidHandle};
use crate::config::TRAP_CONTEXT_BASE;
use crate::mm::{MemorySet, PhysPageNum, VirtAddr, KERNEL_SPACE};
use crate::sync::UPSafeCell;
use crate::trap::{trap_handler, TrapContext};
use alloc::sync::{Arc, Weak};
use alloc::vec::Vec;
use core::cell::RefMut;

/// Smallest priority `sys_set_priority` accepts. `ch5_setprio` expects
/// `set_priority(1)` and `set_priority(0)` to fail, while the stride tests use
/// priorities from 2 up, so 2 is exactly the boundary the tests describe.
pub const MIN_PRIORITY: isize = 2;

/// Task control block structure
///
/// Directly save the contents that will not change during running
pub struct TaskControlBlock {
    // Immutable
    /// Process identifier
    pub pid: PidHandle,

    /// Kernel stack corresponding to PID
    pub kernel_stack: KernelStack,

    /// Mutable
    inner: UPSafeCell<TaskControlBlockInner>,
}

impl TaskControlBlock {
    /// Build a brand new process from an elf image.
    ///
    /// Unlike [`TaskControlBlock::new`], the parent is given here so that
    /// `sys_spawn` can create a process whose children list is already wired
    /// up before it is handed to the scheduler.
    pub fn new_with_parent(elf_data: &[u8], parent: &Arc<TaskControlBlock>) -> Self {
        let (memory_set, user_sp, entry_point, heap_area_end) =
            MemorySet::from_elf_with_heap(elf_data, 0);
        let trap_cx_ppn = memory_set
            .translate(VirtAddr::from(TRAP_CONTEXT_BASE).into())
            .unwrap()
            .ppn();
        let pid_handle = pid_alloc();
        let kernel_stack = kstack_alloc();
        let kernel_stack_top = kernel_stack.get_top();
        let task_control_block = Self {
            pid: pid_handle,
            kernel_stack,
            inner: unsafe {
                UPSafeCell::new(TaskControlBlockInner {
                    trap_cx_ppn,
                    base_size: user_sp,
                    task_cx: TaskContext::goto_trap_return(kernel_stack_top),
                    task_status: TaskStatus::Ready,
                    memory_set,
                    parent: Some(Arc::downgrade(parent)),
                    children: Vec::new(),
                    exit_code: 0,
                    heap_bottom: user_sp,
                    program_brk: user_sp,
                    heap_area_end,
                    stride: TaskContext::DEFAULT_STRIDE,
                    stride_accum: TaskContext::DEFAULT_STRIDE,
                })
            },
        };
        let trap_cx = task_control_block.inner_exclusive_access().get_trap_cx();
        *trap_cx = TrapContext::app_init_context(
            entry_point,
            user_sp,
            KERNEL_SPACE.exclusive_access().token(),
            kernel_stack_top,
            trap_handler as usize,
        );
        task_control_block
    }
}

impl TaskControlBlock {
    /// Get the mutable reference of the inner TCB
    pub fn inner_exclusive_access(&self) -> RefMut<'_, TaskControlBlockInner> {
        self.inner.exclusive_access()
    }
    /// Get the address of app's page table
    pub fn get_user_token(&self) -> usize {
        let inner = self.inner_exclusive_access();
        inner.memory_set.token()
    }
}

pub struct TaskControlBlockInner {
    /// The physical page number of the frame where the trap context is placed
    pub trap_cx_ppn: PhysPageNum,

    /// Application data can only appear in areas
    /// where the application address space is lower than base_size
    pub base_size: usize,

    /// Save task context
    pub task_cx: TaskContext,

    /// Maintain the execution status of the current process
    pub task_status: TaskStatus,

    /// Application address space
    pub memory_set: MemorySet,

    /// Parent process of the current process.
    /// Weak will not affect the reference count of the parent
    pub parent: Option<Weak<TaskControlBlock>>,

    /// A vector containing TCBs of all child processes of the current process
    pub children: Vec<Arc<TaskControlBlock>>,

    /// It is set when active exit or execution error occurs
    pub exit_code: i32,

    /// Heap bottom
    pub heap_bottom: usize,

    /// Program break
    pub program_brk: usize,

    /// End of the (possibly grown) heap `MapArea` in the current address
    /// space. `exec` has to map a heap area of exactly this size in the new
    /// address space, otherwise a process that grew its heap and then called
    /// `exec` would lose the pages between `base_size` and `program_brk`.
    pub heap_area_end: usize,

    /// Per share stride of this task, i.e. `DEFAULT_STRIDE / priority`.
    /// This is what the stride scheduler orders the ready queue by.
    pub stride: usize,

    /// Stride already charged to this task. It doubles as the virtual
    /// wake-up time (ms) while the task sleeps, because a sleeping task is
    /// not in the ready queue and its accumulated stride is meaningless
    /// until it wakes up.
    pub stride_accum: usize,
}

impl TaskControlBlockInner {
    /// get the trap context
    pub fn get_trap_cx(&self) -> &'static mut TrapContext {
        self.trap_cx_ppn.get_mut()
    }
    /// get the user token
    pub fn get_user_token(&self) -> usize {
        self.memory_set.token()
    }
    fn get_status(&self) -> TaskStatus {
        self.task_status
    }
    /// Whether this task has exited but not been reaped by its parent yet
    pub fn is_zombie(&self) -> bool {
        self.get_status() == TaskStatus::Zombie
    }
    /// Whether this task is waiting for its virtual wake-up time
    pub fn is_sleeping(&self) -> bool {
        self.get_status() == TaskStatus::Sleeping
    }
    /// Virtual wake-up time (ms) of a sleeping task. It lives in the same
    /// slot as the accumulated stride, because a sleeping task is not in the
    /// ready queue, so its accumulated stride is of no use until it wakes up.
    pub fn wake_ms(&self) -> usize {
        self.stride_accum
    }
}

impl TaskControlBlock {
    /// Read the per share stride (i.e. the scheduling key) of this task
    pub fn stride_of(&self) -> usize {
        self.inner_exclusive_access().stride
    }
    /// Charge this task one share of its stride after it has been scheduled.
    /// The accumulator is kept strictly positive: a very high priority makes
    /// the per share stride round down to zero, and adding zero every round
    /// would freeze that task at the head of the queue forever.
    pub fn pass_stride(&self) {
        let mut inner = self.inner_exclusive_access();
        inner.stride_accum = (inner.stride_accum + inner.stride).max(1);
    }
    /// Re-synchronise the accumulated stride with a freshly woken task, so
    /// that a process which slept is served as if it had just arrived instead
    /// of being starved by the (huge) wake-up time stored in the same slot.
    pub fn wake_up(&self, now_ms: usize) {
        let mut inner = self.inner_exclusive_access();
        inner.task_status = TaskStatus::Ready;
        inner.stride_accum = inner.stride.max(now_ms);
    }
    /// Set the stride of this task from its priority. A larger priority means
    /// a smaller stride, so the task gets proportionally more CPU time.
    /// Returns `false` for a priority that carries no meaning for the
    /// scheduler (`priority < MIN_PRIORITY`), which the caller reports as `-1`.
    pub fn set_priority(&self, priority: isize) -> bool {
        if priority < MIN_PRIORITY {
            return false;
        }
        // `ch5_setprio` expects `set_priority(isize::MAX)` to succeed, so a
        // huge priority is clamped to the smallest stride the scheduler can
        // hand out instead of being turned into a bogus negative `usize`.
        let prio = (priority as usize).min(TaskContext::DEFAULT_STRIDE);
        let stride = TaskContext::DEFAULT_STRIDE / prio;
        let mut inner = self.inner_exclusive_access();
        inner.stride = stride;
        true
    }

    /// Put the current task to sleep until `wake_ms`, dropping it out of the
    /// ready queue. `ch3_sleep1` measures the elapsed virtual time, so the
    /// clock really has to advance before the task runs again.
    pub fn sleep_to(&self, wake_ms: usize) {
        let mut inner = self.inner_exclusive_access();
        inner.task_status = TaskStatus::Sleeping;
        inner.stride_accum = wake_ms;
    }
}

impl TaskControlBlock {
    /// Create a new process
    ///
    /// At present, it is only used for the creation of initproc
    pub fn new(elf_data: &[u8]) -> Self {
        // memory_set with elf program headers/trampoline/trap context/user stack
        let (memory_set, user_sp, entry_point, heap_area_end) =
            MemorySet::from_elf_with_heap(elf_data, 0);
        let trap_cx_ppn = memory_set
            .translate(VirtAddr::from(TRAP_CONTEXT_BASE).into())
            .unwrap()
            .ppn();
        // alloc a pid and a kernel stack in kernel space
        let pid_handle = pid_alloc();
        let kernel_stack = kstack_alloc();
        let kernel_stack_top = kernel_stack.get_top();
        // push a task context which goes to trap_return to the top of kernel stack
        let task_control_block = Self {
            pid: pid_handle,
            kernel_stack,
            inner: unsafe {
                UPSafeCell::new(TaskControlBlockInner {
                    trap_cx_ppn,
                    base_size: user_sp,
                    task_cx: TaskContext::goto_trap_return(kernel_stack_top),
                    task_status: TaskStatus::Ready,
                    memory_set,
                    parent: None,
                    children: Vec::new(),
                    exit_code: 0,
                    heap_bottom: user_sp,
                    program_brk: user_sp,
                    heap_area_end,
                    stride: TaskContext::DEFAULT_STRIDE,
                    stride_accum: TaskContext::DEFAULT_STRIDE,
                })
            },
        };
        // prepare TrapContext in user space
        let trap_cx = task_control_block.inner_exclusive_access().get_trap_cx();
        *trap_cx = TrapContext::app_init_context(
            entry_point,
            user_sp,
            KERNEL_SPACE.exclusive_access().token(),
            kernel_stack_top,
            trap_handler as usize,
        );
        task_control_block
    }

    /// Load a new elf to replace the original application address space and start execution
    pub fn exec(&self, elf_data: &[u8]) {
        // keep the heap the process has already grown into: the new address
        // space must reserve exactly as many pages above the user stack,
        // otherwise `sbrk` + `exec` (as used by ch5b_usertest) would lose them.
        let heap_size = {
            let inner = self.inner_exclusive_access();
            inner.heap_area_end - inner.base_size
        };
        // memory_set with elf program headers/trampoline/trap context/user stack
        let (memory_set, user_sp, entry_point, heap_area_end) =
            MemorySet::from_elf_with_heap(elf_data, heap_size);
        let trap_cx_ppn = memory_set
            .translate(VirtAddr::from(TRAP_CONTEXT_BASE).into())
            .unwrap()
            .ppn();

        // **** access current TCB exclusively
        let mut inner = self.inner_exclusive_access();
        // substitute memory_set
        inner.memory_set = memory_set;
        // update trap_cx ppn
        inner.trap_cx_ppn = trap_cx_ppn;
        // initialize base_size
        inner.base_size = user_sp;
        // reset the heap pointers to the fresh address space
        inner.heap_bottom = user_sp;
        inner.program_brk = user_sp;
        inner.heap_area_end = heap_area_end;
        // initialize trap_cx
        let trap_cx = inner.get_trap_cx();
        *trap_cx = TrapContext::app_init_context(
            entry_point,
            user_sp,
            KERNEL_SPACE.exclusive_access().token(),
            self.kernel_stack.get_top(),
            trap_handler as usize,
        );
        // **** release inner automatically
    }

    /// parent process fork the child process
    pub fn fork(self: &Arc<Self>) -> Arc<Self> {
        // ---- access parent PCB exclusively
        let mut parent_inner = self.inner_exclusive_access();
        // copy user space(include trap context)
        let memory_set = MemorySet::from_existed_user(&parent_inner.memory_set);
        let trap_cx_ppn = memory_set
            .translate(VirtAddr::from(TRAP_CONTEXT_BASE).into())
            .unwrap()
            .ppn();
        // alloc a pid and a kernel stack in kernel space
        let pid_handle = pid_alloc();
        let kernel_stack = kstack_alloc();
        let kernel_stack_top = kernel_stack.get_top();
        let task_control_block = Arc::new(TaskControlBlock {
            pid: pid_handle,
            kernel_stack,
            inner: unsafe {
                UPSafeCell::new(TaskControlBlockInner {
                    trap_cx_ppn,
                    base_size: parent_inner.base_size,
                    task_cx: TaskContext::goto_trap_return(kernel_stack_top),
                    task_status: TaskStatus::Ready,
                    memory_set,
                    parent: Some(Arc::downgrade(self)),
                    children: Vec::new(),
                    exit_code: 0,
                    heap_bottom: parent_inner.heap_bottom,
                    program_brk: parent_inner.program_brk,
                    heap_area_end: parent_inner.heap_area_end,
                    stride: parent_inner.stride,
                    stride_accum: parent_inner.stride_accum,
                })
            },
        });
        // add child
        parent_inner.children.push(task_control_block.clone());
        // modify kernel_sp in trap_cx
        // **** access child PCB exclusively
        let trap_cx = task_control_block.inner_exclusive_access().get_trap_cx();
        trap_cx.kernel_sp = kernel_stack_top;
        // return
        task_control_block
        // **** release child PCB
        // ---- release parent PCB
    }

    /// get pid of process
    pub fn getpid(&self) -> usize {
        self.pid.0
    }

    /// change the location of the program break. return None if failed.
    pub fn change_program_brk(&self, size: i32) -> Option<usize> {
        let mut inner = self.inner_exclusive_access();
        let heap_bottom = inner.heap_bottom;
        let old_break = inner.program_brk;
        let new_brk = inner.program_brk as isize + size as isize;
        if new_brk < heap_bottom as isize {
            return None;
        }
        let result = if size < 0 {
            inner
                .memory_set
                .shrink_to(VirtAddr(heap_bottom), VirtAddr(new_brk as usize))
        } else {
            inner
                .memory_set
                .append_to(VirtAddr(heap_bottom), VirtAddr(new_brk as usize))
        };
        if result {
            inner.program_brk = new_brk as usize;
            if new_brk as usize > inner.heap_area_end {
                inner.heap_area_end = new_brk as usize;
            }
            Some(old_break)
        } else {
            None
        }
    }
}

#[derive(Copy, Clone, PartialEq)]
/// task status: UnInit, Ready, Running, Exited
pub enum TaskStatus {
    /// uninitialized
    UnInit,
    /// ready to run
    Ready,
    /// running
    Running,
    /// exited
    Zombie,
    /// waiting for its virtual wake-up time; deliberately not reaped by
    /// `sys_waitpid`, so a sleeping child is not an exit event yet
    Sleeping,
}
