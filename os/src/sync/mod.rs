//! Synchronization and interior mutability primitives

mod condvar;
mod mutex;
mod resource;
mod semaphore;
mod up;

pub use condvar::Condvar;
pub use mutex::{Mutex, MutexBlocking, MutexSpin};
pub use resource::{with_resource_manager, DEADLOCK};
pub use semaphore::Semaphore;
pub use up::UPSafeCell;
