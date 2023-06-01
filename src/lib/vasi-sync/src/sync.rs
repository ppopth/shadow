//! Synchronization primitives that are modeled in loom
//!
//! This module provides some very low-level primitives, such as atomics,
//! and futex. When testing under loom they model the corresponding operation
//! in loom instead of executing it natively.

// Use std sync primitives, or loom equivalents
#[cfg(not(loom))]
pub use core::{
    sync::atomic,
    sync::atomic::{AtomicI32, AtomicU32, Ordering},
};
#[cfg(loom)]
use std::collections::HashMap;

// Map a *virtual* address to a list of Condvars. This doesn't support mapping into multiple
// processes, or into different virtual addresses in the same process, etc.
#[cfg(loom)]
use loom::sync::{Condvar, Mutex};
#[cfg(loom)]
pub use loom::{
    sync::atomic,
    sync::atomic::{AtomicI32, AtomicU32, Ordering},
    sync::Arc,
};
#[cfg(not(loom))]
use vasi::VirtualAddressSpaceIndependent;
#[cfg(loom)]
loom::lazy_static! {
    pub static ref FUTEXES: Mutex<HashMap<usize, Arc<Condvar>>> = Mutex::new(HashMap::new());
}

#[cfg(not(loom))]
unsafe fn futex(futex_word: &AtomicU32, futex_operation: i32, val: u32) -> nix::Result<i64> {
    type SourceT = AtomicU32;
    type TargetT = u32;
    let futex_word: &SourceT = futex_word;
    static_assertions::assert_eq_size!(SourceT, TargetT);
    static_assertions::assert_eq_align!(SourceT, TargetT);
    // TODO: Consider using
    // [`as_mut_ptr`](https://doc.rust-lang.org/std/sync/atomic/struct.AtomicU32.html#method.as_mut_ptr)
    // here once it's stabilized.
    let futex_word: *const TargetT = futex_word as *const SourceT as *const TargetT;

    #[cfg(not(miri))]
    {
        use linux_syscall::Result64;

        // We use linux_syscall here to avoid the libc dependency, and touching
        // libc's `errno` in particular.
        let raw_res = unsafe {
            linux_syscall::syscall!(
                linux_syscall::SYS_futex,
                futex_word,
                futex_operation,
                val,
                core::ptr::null() as *const libc::timespec,
                core::ptr::null_mut() as *mut u32,
                0u32,
            )
        };
        raw_res
            .try_i64()
            .map_err(|e| nix::errno::Errno::from_i32(e.into()))
    }
    // Miri doesn't understands raw syscall assembly, but does understand
    // futex syscalls made through the libc syscall wrapper.
    // It's ok to depend on libc for miri testing.
    #[cfg(miri)]
    {
        nix::errno::Errno::result(unsafe {
            libc::syscall(
                libc::SYS_futex,
                futex_word,
                futex_operation,
                val,
                core::ptr::null() as *const libc::timespec,
                core::ptr::null_mut() as *mut u32,
                0u32,
            )
        })
    }
}

pub fn futex_wait(futex_word: &AtomicU32, val: u32) -> nix::Result<i64> {
    // In "production" we use linux_syscall to avoid going through libc, and to
    // avoid touching libc's `errno` in particular.
    #[cfg(not(loom))]
    {
        unsafe { futex(futex_word, libc::FUTEX_WAIT, val) }
    }
    #[cfg(loom)]
    {
        // From futex(2):
        //   This load, the comparison with the expected value, and starting to
        //   sleep are performed atomically and totally ordered with
        //   respect to other futex operations on the same futex word.
        //
        // We hold a lock on our FUTEXES to represent this.
        // TODO: If we want to run loom tests with multiple interacting locks,
        // we should have per-futex mutexes here, and not hold a lock over the
        // whole list the whole time.
        let mut hashmap = FUTEXES.lock().unwrap();
        let futex_word_val = futex_word.load(Ordering::Relaxed);
        if futex_word_val != val {
            return Err(nix::errno::Errno::EAGAIN);
        }
        let condvar = hashmap
            .entry(futex_word as *const _ as usize)
            .or_insert(Arc::new(Condvar::new()))
            .clone();
        // We could get a spurious wakeup here, but that's ok.
        // Futexes are subject to spurious wakeups too.
        condvar.wait(hashmap).unwrap();
        Ok(0)
    }
}

pub fn futex_wake(futex_word: &AtomicU32) -> nix::Result<()> {
    #[cfg(not(loom))]
    {
        unsafe { futex(futex_word, libc::FUTEX_WAKE, 1) }.map(|_| ())
    }
    // loom doesn't understand syscalls; emulate via loom primitives.
    #[cfg(loom)]
    {
        let hashmap = FUTEXES.lock().unwrap();
        let Some(condvar) = hashmap.get(&(futex_word as *const _ as usize)) else {
            return Ok(());
        };
        condvar.notify_one();
        Ok(())
    }
}

#[cfg(not(loom))]
pub struct MutPtr<T: ?Sized>(*mut T);
#[cfg(not(loom))]
impl<T: ?Sized> MutPtr<T> {
    /// # Safety
    ///
    /// See `loom::cell::MutPtr::deref`.
    #[allow(clippy::mut_from_ref)]
    pub unsafe fn deref(&self) -> &mut T {
        unsafe { &mut *self.0 }
    }

    pub fn with<F, R>(&self, f: F) -> R
    where
        F: FnOnce(*mut T) -> R,
    {
        f(self.0)
    }
}
// We have to wrap loom's MutPtr as well, since it's otherwise !Send.
// https://github.com/tokio-rs/loom/issues/294
#[cfg(loom)]
pub struct MutPtr<T: ?Sized>(loom::cell::MutPtr<T>);
#[cfg(loom)]
impl<T: ?Sized> MutPtr<T> {
    #[allow(clippy::mut_from_ref)]
    pub unsafe fn deref(&self) -> &mut T {
        unsafe { self.0.deref() }
    }

    pub fn with<F, R>(&self, f: F) -> R
    where
        F: FnOnce(*mut T) -> R,
    {
        self.0.with(f)
    }
}

unsafe impl<T: ?Sized> Send for MutPtr<T> where T: Send {}

// From https://docs.rs/loom/latest/loom/#handling-loom-api-differences
#[cfg(not(loom))]
#[derive(Debug, VirtualAddressSpaceIndependent)]
#[repr(transparent)]
pub struct UnsafeCell<T>(core::cell::UnsafeCell<T>);
#[cfg(not(loom))]
impl<T> UnsafeCell<T> {
    pub fn new(data: T) -> UnsafeCell<T> {
        UnsafeCell(core::cell::UnsafeCell::new(data))
    }

    pub fn get_mut(&self) -> MutPtr<T> {
        MutPtr(self.0.get())
    }
}
#[cfg(loom)]
#[derive(Debug)]
pub struct UnsafeCell<T>(loom::cell::UnsafeCell<T>);
#[cfg(loom)]
impl<T> UnsafeCell<T> {
    pub fn new(data: T) -> UnsafeCell<T> {
        UnsafeCell(loom::cell::UnsafeCell::new(data))
    }

    pub fn get_mut(&self) -> MutPtr<T> {
        MutPtr(self.0.get_mut())
    }
}

/// Lets us clear global state in between loom iterations, in loom tests.
#[cfg(loom)]
pub fn loom_reset() {
    FUTEXES.lock().unwrap().clear();
}
