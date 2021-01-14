use atomic_refcell::AtomicRefCell;
use std::sync::Arc;

use crate::cshadow as c;
use crate::utility::event_queue::{EventQueue, EventSource, Handle};

pub mod pipe;

/// A trait we can use as a compile-time check to make sure that an object is Send.
trait IsSend: Send {}

/// A trait we can use as a compile-time check to make sure that an object is Sync.
trait IsSync: Sync {}

/// A type that allows us to make a pointer Send + Sync since there is no way
/// to add these traits to the pointer itself.
#[derive(Clone, Copy)]
pub struct SyncSendPointer<T>(*mut T);

unsafe impl<T> Send for SyncSendPointer<T> {}
unsafe impl<T> Sync for SyncSendPointer<T> {}

impl<T> SyncSendPointer<T> {
    /// Get the pointer.
    pub fn ptr(&self) -> *mut T {
        self.0
    }

    /// Get a mutable reference to the pointer.
    pub fn ptr_ref(&mut self) -> &mut *mut T {
        &mut self.0
    }
}

#[derive(PartialEq)]
pub enum SyscallReturn {
    Success(i32),
    Error(nix::errno::Errno),
}

bitflags::bitflags! {
    // file modes: https://github.com/torvalds/linux/blob/master/include/linux/fs.h#L111
    pub struct FileMode: u32 {
        const READ = 0b00000001;
        const WRITE = 0b00000010;
    }
}

#[derive(Clone)]
pub enum NewStatusListenerFilter {
    Never,
    OffToOn,
    OnToOff,
    Always,
}

/// A wrapper for a `*mut c::StatusListener` that increments its ref count when created,
/// and decrements when dropped.
struct LegacyStatusListener(SyncSendPointer<c::StatusListener>);

impl LegacyStatusListener {
    fn new(ptr: *mut c::StatusListener) -> Self {
        assert!(!ptr.is_null());
        unsafe { c::statuslistener_ref(ptr) };
        Self(SyncSendPointer(ptr))
    }

    fn ptr(&self) -> *mut c::StatusListener {
        self.0.ptr()
    }
}

impl Drop for LegacyStatusListener {
    fn drop(&mut self) {
        unsafe { c::statuslistener_unref(self.0.ptr()) };
    }
}

/// Stores event listener handles so that `c::StatusListener` objects can subscribe to events.
struct LegacyListenerHelper {
    handle_map: std::collections::HashMap<usize, Handle<(c::Status, c::Status)>>,
}

impl LegacyListenerHelper {
    fn new() -> Self {
        Self {
            handle_map: std::collections::HashMap::new(),
        }
    }

    fn add_listener(
        &mut self,
        ptr: *mut c::StatusListener,
        event_source: &mut EventSource<(c::Status, c::Status)>,
    ) {
        assert!(!ptr.is_null());

        // if it's already listening, don't add a second time
        if self.handle_map.contains_key(&(ptr as usize)) {
            return;
        }

        // this will ref the pointer and unref it when the closure is dropped
        let ptr_wrapper = LegacyStatusListener::new(ptr);

        let handle = event_source.add_listener(move |(status, changed), _event_queue| unsafe {
            c::statuslistener_onStatusChanged(ptr_wrapper.ptr(), status, changed)
        });

        // use a usize as the key so we don't accidentally deref the pointer
        self.handle_map.insert(ptr as usize, handle);
    }

    fn remove_listener(&mut self, ptr: *mut c::StatusListener) {
        assert!(!ptr.is_null());
        self.handle_map.remove(&(ptr as usize));
    }
}

/// A specified event source that passes a status and the changed bits to the function, but only if
/// the monitored bits have changed and if the change the filter is satisfied.
struct StatusEventSource {
    inner: EventSource<(c::Status, c::Status)>,
    legacy_helper: LegacyListenerHelper,
}

impl StatusEventSource {
    pub fn new() -> Self {
        Self {
            inner: EventSource::new(),
            legacy_helper: LegacyListenerHelper::new(),
        }
    }

    pub fn add_listener(
        &mut self,
        monitoring: c::Status,
        filter: NewStatusListenerFilter,
        notify_fn: impl Fn(c::Status, c::Status, &mut EventQueue) + Send + Sync + 'static,
    ) -> Handle<(c::Status, c::Status)> {
        self.inner
            .add_listener(move |(status, changed), event_queue| {
                // true if any of the bits we're monitoring have changed
                let flipped = monitoring & changed != 0;

                // true if any of the bits we're monitoring are set
                let on = monitoring & status != 0;

                let notify = match filter {
                    // at least one monitored bit is on, and at least one has changed
                    NewStatusListenerFilter::OffToOn => flipped && on,
                    // all monitored bits are off, and at least one has changed
                    NewStatusListenerFilter::OnToOff => flipped && !on,
                    // at least one monitored bit has changed
                    NewStatusListenerFilter::Always => flipped,
                    NewStatusListenerFilter::Never => false,
                };

                if !notify {
                    return;
                }

                (notify_fn)(status, changed, event_queue)
            })
    }

    pub fn add_legacy_listener(&mut self, ptr: *mut c::StatusListener) {
        self.legacy_helper.add_listener(ptr, &mut self.inner);
    }

    pub fn remove_legacy_listener(&mut self, ptr: *mut c::StatusListener) {
        self.legacy_helper.remove_listener(ptr);
    }

    pub fn notify_listeners(
        &mut self,
        status: c::Status,
        changed: c::Status,
        event_queue: &mut EventQueue,
    ) {
        self.inner.notify_listeners((status, changed), event_queue)
    }
}

/// Represents a POSIX description, or a Linux "struct file".
pub enum PosixFile {
    Pipe(pipe::PipeFile),
}

// will not compile if `PosixFile` is not Send + Sync
impl IsSend for PosixFile {}
impl IsSync for PosixFile {}

impl PosixFile {
    pub fn read(&mut self, bytes: &mut [u8], event_queue: &mut EventQueue) -> SyscallReturn {
        match self {
            Self::Pipe(f) => f.read(bytes, event_queue),
        }
    }

    pub fn write(&mut self, bytes: &[u8], event_queue: &mut EventQueue) -> SyscallReturn {
        match self {
            Self::Pipe(f) => f.write(bytes, event_queue),
        }
    }

    pub fn status(&self) -> c::Status {
        match self {
            Self::Pipe(f) => f.status(),
        }
    }

    pub fn add_legacy_listener(&mut self, ptr: *mut c::StatusListener) {
        match self {
            Self::Pipe(f) => f.add_legacy_listener(ptr),
        }
    }

    pub fn remove_legacy_listener(&mut self, ptr: *mut c::StatusListener) {
        match self {
            Self::Pipe(f) => f.remove_legacy_listener(ptr),
        }
    }
}

#[derive(Clone)]
pub struct Descriptor {
    file: Arc<AtomicRefCell<PosixFile>>,
    flags: i32,
}

impl Descriptor {
    pub fn new(file: Arc<AtomicRefCell<PosixFile>>) -> Self {
        Self { file, flags: 0 }
    }

    pub fn get_file(&self) -> &Arc<AtomicRefCell<PosixFile>> {
        &self.file
    }

    pub fn get_flags(&self) -> i32 {
        self.flags
    }

    pub fn set_flags(&mut self, flags: i32) {
        self.flags = flags;
    }

    pub fn add_flags(&mut self, flags: i32) {
        self.flags |= flags;
    }
}

// don't implement copy or clone without considering the legacy descriptor's ref count
pub enum CompatDescriptor {
    New(Descriptor),
    Legacy(SyncSendPointer<c::LegacyDescriptor>),
}

// will not compile if `CompatDescriptor` is not Send + Sync
impl IsSend for CompatDescriptor {}
impl IsSync for CompatDescriptor {}

impl Drop for CompatDescriptor {
    fn drop(&mut self) {
        if let CompatDescriptor::Legacy(d) = self {
            // unref the legacy descriptor object
            unsafe { c::descriptor_unref(d.ptr() as *mut core::ffi::c_void) };
        }
    }
}

mod export {
    use super::*;

    /// An opaque type used when passing `*const AtomicRefCell<File>` to C.
    #[no_mangle]
    pub enum PosixFileArc {}

    /// The new compat descriptor takes ownership of the reference to the legacy descriptor and
    /// does not increment its ref count, but will decrement the ref count when this compat
    /// descriptor is freed/dropped.
    #[no_mangle]
    pub extern "C" fn compatdescriptor_fromLegacy(
        legacy_descriptor: *mut c::LegacyDescriptor,
    ) -> *mut CompatDescriptor {
        assert!(!legacy_descriptor.is_null());

        let descriptor = CompatDescriptor::Legacy(SyncSendPointer(legacy_descriptor));
        Box::into_raw(Box::new(descriptor))
    }

    /// If the compat descriptor is a legacy descriptor, returns a pointer to the legacy
    /// descriptor object. Otherwise returns NULL. The legacy descriptor's ref count is not
    /// modified, so the pointer must not outlive the lifetime of the compat descriptor.
    #[no_mangle]
    pub extern "C" fn compatdescriptor_asLegacy(
        descriptor: *const CompatDescriptor,
    ) -> *mut c::LegacyDescriptor {
        assert!(!descriptor.is_null());

        let descriptor = unsafe { &*descriptor };

        if let CompatDescriptor::Legacy(d) = descriptor {
            d.ptr()
        } else {
            std::ptr::null_mut()
        }
    }

    /// When the compat descriptor is freed/dropped, it will decrement the legacy descriptor's
    /// ref count.
    #[no_mangle]
    pub extern "C" fn compatdescriptor_free(descriptor: *mut CompatDescriptor) {
        if descriptor.is_null() {
            return;
        }

        let descriptor = unsafe { &mut *descriptor };
        unsafe { Box::from_raw(descriptor) };
    }

    /// This is a no-op for non-legacy descriptors.
    #[no_mangle]
    pub extern "C" fn compatdescriptor_setHandle(
        descriptor: *mut CompatDescriptor,
        handle: libc::c_int,
    ) {
        assert!(!descriptor.is_null());

        let descriptor = unsafe { &mut *descriptor };

        if let CompatDescriptor::Legacy(d) = descriptor {
            unsafe { c::descriptor_setHandle(d.ptr(), handle) }
        }

        // new descriptor types don't store their file handle, so do nothing
    }

    /// If the compat descriptor is a new descriptor, returns a pointer to the reference-counted
    /// posix file object. Otherwise returns NULL. The posix file object's ref count is not
    /// modified, so the pointer must not outlive the lifetime of the compat descriptor.
    #[no_mangle]
    pub extern "C" fn compatdescriptor_borrowPosixFile(
        descriptor: *mut CompatDescriptor,
    ) -> *const PosixFileArc {
        assert!(!descriptor.is_null());

        let descriptor = unsafe { &mut *descriptor };

        (match descriptor {
            CompatDescriptor::Legacy(_) => std::ptr::null_mut(),
            CompatDescriptor::New(d) => Arc::as_ptr(d.get_file()),
        }) as *const PosixFileArc
    }

    /// If the compat descriptor is a new descriptor, returns a pointer to the reference-counted
    /// posix file object. Otherwise returns NULL. The posix file object's ref count is
    /// incremented, so the pointer must always later be passed to `posixfile_drop()`, otherwise
    /// the memory will leak.
    #[no_mangle]
    pub extern "C" fn compatdescriptor_newRefPosixFile(
        descriptor: *mut CompatDescriptor,
    ) -> *const PosixFileArc {
        assert!(!descriptor.is_null());

        let descriptor = unsafe { &mut *descriptor };

        (match descriptor {
            CompatDescriptor::Legacy(_) => std::ptr::null_mut(),
            CompatDescriptor::New(d) => Arc::into_raw(Arc::clone(&d.get_file())),
        }) as *const PosixFileArc
    }

    /// Decrement the ref count of the posix file object. The pointer must not be used after
    /// calling this function.
    #[no_mangle]
    pub extern "C" fn posixfile_drop(file: *const PosixFileArc) {
        assert!(!file.is_null());

        unsafe { Arc::from_raw(file as *const AtomicRefCell<PosixFile>) };
    }

    /// Get the status of the posix file object.
    #[allow(unused_variables)]
    #[no_mangle]
    pub extern "C" fn posixfile_getStatus(file: *const PosixFileArc) -> c::Status {
        assert!(!file.is_null());

        let file = file as *const AtomicRefCell<PosixFile>;
        let file = unsafe { &*file };

        file.borrow().status()
    }

    /// Add a status listener to the posix file object. This will increment the status
    /// listener's ref count, and will decrement the ref count when this status listener is
    /// removed or when the posix file is freed/dropped.
    #[no_mangle]
    pub extern "C" fn posixfile_addListener(
        file: *const PosixFileArc,
        listener: *mut c::StatusListener,
    ) {
        assert!(!file.is_null());
        assert!(!listener.is_null());

        let file = file as *const AtomicRefCell<PosixFile>;
        let file = unsafe { &*file };

        file.borrow_mut().add_legacy_listener(listener);
    }

    /// Remove a listener from the posix file object.
    #[no_mangle]
    pub extern "C" fn posixfile_removeListener(
        file: *const PosixFileArc,
        listener: *mut c::StatusListener,
    ) {
        assert!(!file.is_null());
        assert!(!listener.is_null());

        let file = file as *const AtomicRefCell<PosixFile>;
        let file = unsafe { &*file };

        file.borrow_mut().remove_legacy_listener(listener);
    }
}
