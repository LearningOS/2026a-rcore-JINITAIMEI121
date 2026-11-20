//! Resource manager: the wait-for graph behind chapter 8's deadlock detection.
//!
//! # Nodes and edges
//!
//! The graph is bipartite and alternates between two kinds of nodes:
//!
//! * **thread** nodes, identified by `(pid, tid)`. Their single outgoing edge
//!   is read straight from [`TaskControlBlockInner`]: `wait_sem` for a thread
//!   blocked in `semaphore_down`, `wait_mutex` for one blocked in
//!   `mutex_lock` / `condvar_wait`.
//! * **resource** nodes, one per blocking mutex or per semaphore of a process,
//!   keyed `(pid, is_semaphore, ridx)`. A resource points back at every thread
//!   that owns one of its units (its *holders*).
//!
//! ```text
//! T1 --needs--> Mutex(m) --held_by--> T2 --needs--> Mutex(m)   => cycle => deadlock
//! ```
//!
//! A semaphore with `k` units is represented by its holder set, so a `down`
//! only creates an edge once all `k` units are handed out; `ch8_deadlock_sem2`
//! relies on that.
//!
//! # How the check is used
//!
//! Before a primitive puts a task to sleep, [`ResourceManager::acquire`] walks
//! the graph from the resource being asked for. If it reaches the requester
//! again, granting the request would close a cycle, so the syscall returns
//! [`DEADLOCK`] (`-0xdead`) to user space and *no* state is modified anywhere
//! (the primitives keep their real queues as the source of truth). Requests
//! that merely queue behind other waiters but belong to no cycle are granted
//! normally, which is what `ch8_deadlock_sem2` requires.
//!
//! This module never calls `wakeup_task`/`block_current_and_run_next`: doing
//! the actual sleeping stays inside the primitives, which avoids double
//! enqueueing. It only mirrors who-waits-for-what and answers "would this
//! block forever?". All bookkeeping happens while the requesting task is still
//! inside its syscall (kernel mode, uniprocessor), hence a plain
//! [`UPSafeCell`] singleton is enough.

use super::UPSafeCell;
use crate::task::{current_task, TaskControlBlock, TaskControlBlockInner};
use alloc::collections::{BTreeMap, BTreeSet, VecDeque};
use alloc::sync::Arc;
use alloc::vec::Vec;
use lazy_static::*;

/// Error code returned to user space when a request would deadlock.
pub const DEADLOCK: isize = -0xdead;

/// A thread node id.
type Tid = (usize, usize);
/// A resource node key.
type Key = (usize, bool, usize);

/// What a thread may be waiting for. Mutexes and condition variables share one
/// namespace because waiting on a condvar means waiting for its mutex to be
/// handed over.
#[derive(Clone, Copy)]
pub enum Waiting {
    Semaphore(usize),
    Mutex(usize),
    Condvar(usize),
}

impl Waiting {
    fn key(self, pid: usize) -> Key {
        match self {
            Waiting::Semaphore(ridx) => (pid, true, ridx),
            Waiting::Mutex(ridx) | Waiting::Condvar(ridx) => (pid, false, ridx),
        }
    }
    fn is_semaphore(self) -> bool {
        matches!(self, Waiting::Semaphore(_))
    }
}

/// One resource node: a blocking mutex (`total == 1`) or a semaphore.
struct REntry {
    total: usize,
    /// Threads owning a unit. At most one entry for a mutex.
    holders: Vec<Tid>,
    /// Threads blocked on it, FIFO order (mirrors the primitive's own queue).
    waiters: Vec<Tid>,
}

impl REntry {
    fn new(total: usize) -> Self {
        Self {
            total,
            holders: Vec::new(),
            waiters: Vec::new(),
        }
    }
    fn insert_holder(&mut self, tid: Tid) {
        if !self.holders.contains(&tid) {
            self.holders.push(tid);
        }
    }
    fn remove_holder(&mut self, tid: Tid) {
        self.holders.retain(|t| *t != tid);
    }
    fn insert_waiter(&mut self, tid: Tid) {
        if !self.waiters.contains(&tid) {
            self.waiters.push(tid);
        }
    }
    fn remove_waiter(&mut self, tid: Tid) {
        self.waiters.retain(|t| *t != tid);
    }
    /// Would a brand new request be satisfied immediately: nobody holds a unit
    /// and nobody is queued in front (both primitives are FIFO).
    fn free_now(&self) -> bool {
        self.holders.is_empty() && self.waiters.is_empty()
    }
}

/// The whole wait-for graph.
pub struct ResourceManager {
    enabled: bool,
    resources: BTreeMap<Key, REntry>,
    /// Thread nodes known to the detector, kept alive so that a dying process
    /// can be erased completely.
    tasks: BTreeMap<Tid, Arc<TaskControlBlock>>,
    /// pid -> tids, same purpose, indexed by process.
    threads: BTreeMap<usize, BTreeSet<usize>>,
}

impl ResourceManager {
    fn new() -> Self {
        Self {
            enabled: false,
            resources: BTreeMap::new(),
            tasks: BTreeMap::new(),
            threads: BTreeMap::new(),
        }
    }

    /// Turn detection on/off. Switching it off drops every recorded edge so the
    /// kernel falls back to the plain rCore6 behaviour.
    pub fn enable(&mut self, enabled: bool) -> isize {
        self.enabled = enabled;
        if !enabled {
            self.resources.clear();
            self.tasks.clear();
            self.threads.clear();
        }
        0
    }

    /// Is detection active? Callers use it to skip all bookkeeping otherwise.
    pub fn enabled(&self) -> bool {
        self.enabled
    }

    /// Register a freshly created blocking mutex (`total == 1`) or semaphore.
    pub fn add_resource(&mut self, pid: usize, is_semaphore: bool, ridx: usize, total: usize) {
        if !self.enabled {
            return;
        }
        self.resources.insert((pid, is_semaphore, ridx), REntry::new(total));
    }

    fn ids_of(task: &Arc<TaskControlBlock>) -> Option<Tid> {
        let pid = task.process.upgrade()?.getpid();
        let tid = task.inner_exclusive_access().res.as_ref()?.tid;
        Some((pid, tid))
    }

    fn current_ids() -> Option<Tid> {
        let task = current_task()?;
        Self::ids_of(&task)
    }

    fn remember(&mut self, ids: Tid, task: Arc<TaskControlBlock>) {
        self.threads.entry(ids.0).or_default().insert(ids.1);
        self.tasks.insert(ids, task);
    }

    /// Read the outgoing edge of a thread node from its TCB.
    fn edge_of(tasks: &BTreeMap<Tid, Arc<TaskControlBlock>>, ids: &Tid) -> Option<Waiting> {
        let inner = tasks.get(ids)?.inner_exclusive_access();
        if let Some(ridx) = inner.wait_sem {
            return Some(Waiting::Semaphore(ridx));
        }
        if let Some((ridx, is_condvar)) = inner.wait_mutex {
            return Some(if is_condvar {
                Waiting::Condvar(ridx)
            } else {
                Waiting::Mutex(ridx)
            });
        }
        None
    }

    /// Breadth-first walk of the graph starting at `start`, looking for a
    /// holder equal to `target`. A free function so that `&self.resources` and
    /// `&self.tasks` can be borrowed independently.
    fn cycle_exists(
        resources: &BTreeMap<Key, REntry>,
        tasks: &BTreeMap<Tid, Arc<TaskControlBlock>>,
        start: Key,
        target: Tid,
    ) -> bool {
        let mut visited: BTreeSet<Key> = BTreeSet::new();
        let mut queue: VecDeque<Key> = VecDeque::new();
        visited.insert(start);
        queue.push_back(start);
        while let Some(key) = queue.pop_front() {
            let entry = match resources.get(&key) {
                Some(entry) => entry,
                None => continue,
            };
            for holder in entry.holders.iter() {
                if *holder == target {
                    return true;
                }
                if let Some(waiting) = Self::edge_of(tasks, holder) {
                    // a thread always waits for a resource of its own process
                    let next = waiting.key(holder.0);
                    if next != key && visited.insert(next) {
                        queue.push_back(next);
                    }
                }
            }
        }
        false
    }

    /// Ask for a unit of `waiting` on behalf of the current task.
    ///
    /// * `Ok(true)`  - grant it right away; the caller must not block and the
    ///   holder edge was recorded.
    /// * `Ok(false)` - the caller has to queue/block the task; the wait edge is
    ///   already recorded, so the graph reflects the pending request.
    /// * `Err(())`  - granting would close a cycle: nothing was modified and
    ///   the syscall must return [`DEADLOCK`].
    pub fn acquire(&mut self, waiting: Waiting) -> Result<bool, ()> {
        if !self.enabled {
            return Ok(true);
        }
        let task = match current_task() {
            Some(task) => task,
            None => return Ok(true),
        };
        let ids = match Self::ids_of(&task) {
            Some(ids) => ids,
            None => return Ok(true),
        };
        let key = waiting.key(ids.0);
        // Asking twice for the same mutex while owning it can never be
        // satisfied: that is exactly the `ch8_deadlock_mutex1` case.
        if !waiting.is_semaphore() {
            if let Some(entry) = self.resources.get(&key) {
                if entry.holders.contains(&ids) {
                    return Err(());
                }
            }
        }
        let free_now = self
            .resources
            .get(&key)
            .map(|entry| entry.free_now())
            .unwrap_or(true);
        if free_now {
            self.grant(waiting);
            return Ok(true);
        }
        // Tentatively add the edge `ids -> key` and look for a path back to
        // `ids`. Working on a copy keeps `Err(())` side-effect free.
        let mut probed = self.probe_graph();
        let total = self.resources.get(&key).map(|e| e.total).unwrap_or(1);
        let entry = probed.entry(key).or_insert_with(|| REntry::new(total));
        entry.insert_waiter(ids);
        if Self::cycle_exists(&probed, &self.tasks, key, ids) {
            return Err(());
        }
        let entry = self
            .resources
            .entry(key)
            .or_insert_with(|| REntry::new(total));
        entry.insert_waiter(ids);
        self.remember(ids, task.clone());
        set_waiting(&mut task.inner_exclusive_access(), waiting);
        Ok(false)
    }

    /// Record that the current task owns a unit of `waiting`.
    pub fn grant(&mut self, waiting: Waiting) {
        if !self.enabled {
            return;
        }
        let task = match current_task() {
            Some(task) => task,
            None => return,
        };
        let ids = match Self::ids_of(&task) {
            Some(ids) => ids,
            None => return,
        };
        self.remember(ids, task.clone());
        let key = waiting.key(ids.0);
        let total = self.resources.get(&key).map(|e| e.total).unwrap_or(1);
        let entry = self.resources.entry(key).or_insert_with(|| REntry::new(total));
        entry.insert_holder(ids);
        entry.remove_waiter(ids);
        let mut inner = task.inner_exclusive_access();
        clear_waiting(&mut inner, waiting.is_semaphore());
        if !waiting.is_semaphore() {
            inner.hold_mutex = true;
        }
    }

    /// Record that `task` (a thread woken up by a mutex unlock) now owns the
    /// mutex `ridx` of process `pid`.
    pub fn wake_mutex_holder(&mut self, pid: usize, ridx: usize, task: &Arc<TaskControlBlock>) {
        if !self.enabled {
            return;
        }
        let ids = match Self::ids_of(task) {
            Some(ids) => ids,
            None => return,
        };
        let key = (pid, false, ridx);
        self.remember(ids, task.clone());
        if let Some(entry) = self.resources.get_mut(&key) {
            entry.remove_waiter(ids);
            entry.holders.clear();
            entry.insert_holder(ids);
        } else {
            let mut entry = REntry::new(1);
            entry.insert_holder(ids);
            self.resources.insert(key, entry);
        }
        let mut inner = task.inner_exclusive_access();
        inner.wait_mutex = None;
        inner.hold_mutex = true;
    }

    /// Nobody owns the mutex/condvar `ridx` of `pid` any more and no waiter was
    /// handed the lock directly (uncontended unlock).
    pub fn drop_holder(&mut self, waiting: Waiting) {
        if !self.enabled {
            return;
        }
        let pid = match current_pid() {
            Some(pid) => pid,
            None => return,
        };
        let key = waiting.key(pid);
        if let Some(entry) = self.resources.get_mut(&key) {
            entry.holders.clear();
        }
        if let Some(ids) = Self::current_ids() {
            if let Some(task) = self.tasks.get(&ids) {
                task.inner_exclusive_access().hold_mutex = false;
            }
        }
    }

    /// Give the freed semaphore unit to `sem` and mirror that in the graph.
    /// Called by `Semaphore::up` right before it wakes the task.
    pub fn hand_over_semaphore(&mut self, sem: &Arc<TaskControlBlock>, ridx: usize) {
        if !self.enabled {
            return;
        }
        let ids = match Self::ids_of(sem) {
            Some(ids) => ids,
            None => return,
        };
        let key = (ids.0, true, ridx);
        self.remember(ids, sem.clone());
        let entry = self.resources.entry(key).or_insert_with(|| REntry::new(1));
        entry.remove_waiter(ids);
        entry.insert_holder(ids);
        sem.inner_exclusive_access().wait_sem = None;
    }

    /// Release one semaphore unit held by the current task.
    ///
    /// Returns `Some(true)` when the current task really owned a unit (so a
    /// waiter may proceed), `Some(false)` when it did not (an "extra" `up`,
    /// which must not promote anybody), and `None` when detection is off.
    pub fn release_unit(&mut self, ridx: usize) -> Option<bool> {
        if !self.enabled {
            return None;
        }
        let ids = Self::current_ids()?;
        let key = (ids.0, true, ridx);
        let entry = self.resources.entry(key).or_insert_with(|| REntry::new(1));
        let owned = entry.holders.contains(&ids);
        if owned {
            entry.remove_holder(ids);
            entry.remove_waiter(ids);
            if let Some(task) = self.tasks.get(&ids) {
                task.inner_exclusive_access().wait_sem = None;
            }
        }
        Some(owned)
    }

    /// An `up` that woke nobody while the semaphore was still oversubscribed:
    /// forget any holder edge of the caller so the graph cannot grow stale.
    pub fn note_missing_up(&mut self, ridx: usize) {
        if !self.enabled {
            return;
        }
        let ids = match Self::current_ids() {
            Some(ids) => ids,
            None => return,
        };
        if let Some(entry) = self.resources.get_mut(&(ids.0, true, ridx)) {
            entry.remove_holder(ids);
        }
    }

    /// The current thread entered `condvar_wait(ridx, guard)`: it now waits for
    /// a signal (modelled as a mutex-flavoured node, since a condvar is only
    /// released by another thread). Its guard edge was already dropped by
    /// [`Mutex::unlock`].
    pub fn condvar_waiting(&mut self, ridx: usize) {
        if !self.enabled {
            return;
        }
        let task = match current_task() {
            Some(task) => task,
            None => return,
        };
        let ids = match Self::ids_of(&task) {
            Some(ids) => ids,
            None => return,
        };
        self.remember(ids, task.clone());
        let key = (ids.0, false, ridx);
        // A thread may only own one mutex at a time in this model; if a stale
        // holder edge is left over (a condvar guard that was never unlocked),
        // drop it before we claim the new one.
        if let Some(stale) = self.held_mutex_of(ids) {
            if stale != key {
                if let Some(entry) = self.resources.get_mut(&stale) {
                    entry.remove_holder(ids);
                }
            }
        }
        let total = self.resources.get(&key).map(|e| e.total).unwrap_or(1);
        let entry = self.resources.entry(key).or_insert_with(|| REntry::new(total));
        entry.insert_waiter(ids);
        let mut inner = task.inner_exclusive_access();
        inner.wait_mutex = Some((ridx, true));
        inner.hold_mutex = false;
    }

    /// A signalled thread is about to re-acquire the guard mutex `guard_ridx`:
    /// its condvar edge disappears (the thread becomes ready again).
    pub fn condvar_returned(&mut self, guard_ridx: Option<usize>) {
        if !self.enabled {
            return;
        }
        let ids = match Self::current_ids() {
            Some(ids) => ids,
            None => return,
        };
        if let Some(waiting) = Self::edge_of(&self.tasks, &ids) {
            let key = waiting.key(ids.0);
            if let Some(entry) = self.resources.get_mut(&key) {
                entry.remove_waiter(ids);
            }
        }
        if let Some(task) = self.tasks.get(&ids) {
            let mut inner = task.inner_exclusive_access();
            inner.wait_mutex = None;
            // The guard was released by `Condvar::wait` before we slept, so no
            // holder edge should point at us any more. Clear it defensively in
            // case an unbalanced unlock left one behind.
            if let Some(ridx) = guard_ridx {
                if let Some(entry) = self.resources.get_mut(&(ids.0, false, ridx)) {
                    entry.remove_holder(ids);
                }
            }
        }
    }

    /// `condvar_signal` picked `task` out of its queue: drop its wait edge.
    pub fn condvar_signalled(&mut self, task: &Arc<TaskControlBlock>) {
        if !self.enabled {
            return;
        }
        let ids = match Self::ids_of(task) {
            Some(ids) => ids,
            None => return,
        };
        if let Some(waiting) = Self::edge_of(&self.tasks, &ids) {
            let key = waiting.key(ids.0);
            if let Some(entry) = self.resources.get_mut(&key) {
                entry.remove_waiter(ids);
            }
        }
        task.inner_exclusive_access().wait_mutex = None;
    }

    /// Which mutex does `ids` currently own? Used by the condvar path to make
    /// sure a thread that left `condvar_wait` without unlocking cannot keep two
    /// mutex holder edges.
    fn held_mutex_of(&self, ids: Tid) -> Option<Key> {
        for (key, entry) in self.resources.iter() {
            if !key.1 && entry.holders.contains(&ids) {
                return Some(*key);
            }
        }
        None
    }

    /// Erase everything belonging to `pid` (called when its PCB disappears).
    /// Its threads are killed by the kernel, so they are *not* woken here.
    pub fn deallocate(&mut self, pid: usize) {
        if !self.enabled {
            return;
        }
        let keys: Vec<Key> = self
            .resources
            .range((pid, false, usize::MIN)..=(pid, true, usize::MAX))
            .map(|(k, _)| *k)
            .collect();
        for key in keys {
            self.resources.remove(&key);
        }
        if let Some(tids) = self.threads.remove(&pid) {
            for tid in tids {
                self.tasks.remove(&(pid, tid));
            }
        }
        for entry in self.resources.values_mut() {
            entry.holders.retain(|p| p.0 != pid);
            entry.waiters.retain(|p| p.0 != pid);
        }
    }

    /// Drop a thread node that leaves the system (thread exit) without going
    /// through a release, so that no dangling edge survives.
    pub fn cleanup_task(&mut self, task: &Arc<TaskControlBlock>) {
        if !self.enabled {
            return;
        }
        let ids = match Self::ids_of(task) {
            Some(ids) => ids,
            None => return,
        };
        if let Some(waiting) = Self::edge_of(&self.tasks, &ids) {
            let key = waiting.key(ids.0);
            if let Some(entry) = self.resources.get_mut(&key) {
                entry.remove_waiter(ids);
                entry.remove_holder(ids);
            }
        }
        let mut inner = task.inner_exclusive_access();
        inner.wait_sem = None;
        inner.wait_mutex = None;
        inner.hold_mutex = false;
        drop(inner);
        if let Some(tids) = self.threads.get_mut(&ids.0) {
            tids.remove(&ids.1);
        }
        self.tasks.remove(&ids);
    }

    /// Shallow copy of the graph so the probe can see the edge it is about to
    /// add without mutating anything.
    fn probe_graph(&self) -> BTreeMap<Key, REntry> {
        let mut map: BTreeMap<Key, REntry> = BTreeMap::new();
        for (k, entry) in self.resources.iter() {
            let mut copy = REntry::new(entry.total);
            copy.holders.extend_from_slice(&entry.holders);
            copy.waiters.extend_from_slice(&entry.waiters);
            map.insert(*k, copy);
        }
        map
    }
}

fn current_pid() -> Option<usize> {
    let task = current_task()?;
    Some(task.process.upgrade()?.getpid())
}

/// Set the "outgoing edge" fields of a thread node.
fn set_waiting(inner: &mut TaskControlBlockInner, waiting: Waiting) {
    match waiting {
        Waiting::Semaphore(ridx) => inner.wait_sem = Some(ridx),
        Waiting::Mutex(ridx) => inner.wait_mutex = Some((ridx, false)),
        Waiting::Condvar(ridx) => inner.wait_mutex = Some((ridx, true)),
    }
}

/// Clear the fields belonging to a granted request.
fn clear_waiting(inner: &mut TaskControlBlockInner, semaphore: bool) {
    if semaphore {
        inner.wait_sem = None;
    } else {
        inner.wait_mutex = None;
    }
}

lazy_static! {
    /// Singleton graph shared by the sync primitives and the syscalls.
    pub static ref RESOURCE_MANAGER: UPSafeCell<ResourceManager> =
        unsafe { UPSafeCell::new(ResourceManager::new()) };
}

/// Run `f` on the global graph.
pub fn with_resource_manager<F, R>(f: F) -> R
where
    F: FnOnce(&mut ResourceManager) -> R,
{
    f(&mut RESOURCE_MANAGER.exclusive_access())
}
