use std::sync::Arc;

use atomic_refcell::AtomicRefCell;

use crate::cshadow as c;
use crate::host::descriptor::{FileMode, FileState, FileStatus, SyscallResult};
use crate::host::memory_manager::MemoryManager;
use crate::host::syscall_types::{PluginPtr, SysCallReg, SyscallError};
use crate::utility::callback_queue::CallbackQueue;
use crate::utility::sockaddr::SockaddrStorage;
use crate::utility::HostTreePointer;

use netlink::NetlinkSocket;
use unix::UnixSocket;

pub mod abstract_unix_ns;
pub mod netlink;
pub mod unix;

#[derive(Clone)]
pub enum Socket {
    Unix(Arc<AtomicRefCell<UnixSocket>>),
    Netlink(Arc<AtomicRefCell<NetlinkSocket>>),
}

impl Socket {
    pub fn borrow(&self) -> SocketRef {
        match self {
            Self::Unix(ref f) => SocketRef::Unix(f.borrow()),
            Self::Netlink(ref f) => SocketRef::Netlink(f.borrow()),
        }
    }

    pub fn try_borrow(&self) -> Result<SocketRef, atomic_refcell::BorrowError> {
        Ok(match self {
            Self::Unix(ref f) => SocketRef::Unix(f.try_borrow()?),
            Self::Netlink(ref f) => SocketRef::Netlink(f.try_borrow()?),
        })
    }

    pub fn borrow_mut(&self) -> SocketRefMut {
        match self {
            Self::Unix(ref f) => SocketRefMut::Unix(f.borrow_mut()),
            Self::Netlink(ref f) => SocketRefMut::Netlink(f.borrow_mut()),
        }
    }

    pub fn try_borrow_mut(&self) -> Result<SocketRefMut, atomic_refcell::BorrowMutError> {
        Ok(match self {
            Self::Unix(ref f) => SocketRefMut::Unix(f.try_borrow_mut()?),
            Self::Netlink(ref f) => SocketRefMut::Netlink(f.try_borrow_mut()?),
        })
    }

    pub fn canonical_handle(&self) -> usize {
        match self {
            Self::Unix(f) => Arc::as_ptr(f) as usize,
            Self::Netlink(f) => Arc::as_ptr(f) as usize,
        }
    }

    pub fn bind(&self, addr: Option<&SockaddrStorage>, rng: impl rand::Rng) -> SyscallResult {
        match self {
            Self::Unix(socket) => UnixSocket::bind(socket, addr, rng),
            Self::Netlink(socket) => NetlinkSocket::bind(socket, addr, rng),
        }
    }

    pub fn connect(
        &self,
        addr: &SockaddrStorage,
        cb_queue: &mut CallbackQueue,
    ) -> Result<(), SyscallError> {
        match self {
            Self::Unix(socket) => UnixSocket::connect(socket, addr, cb_queue),
            Self::Netlink(socket) => NetlinkSocket::connect(socket, addr, cb_queue),
        }
    }
}

impl std::fmt::Debug for Socket {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unix(_) => write!(f, "Unix")?,
            Self::Netlink(_) => write!(f, "Netlink")?,
        }

        if let Ok(file) = self.try_borrow() {
            write!(
                f,
                "(state: {:?}, status: {:?})",
                file.state(),
                file.get_status()
            )
        } else {
            write!(f, "(already borrowed)")
        }
    }
}

pub enum SocketRef<'a> {
    Unix(atomic_refcell::AtomicRef<'a, UnixSocket>),
    Netlink(atomic_refcell::AtomicRef<'a, NetlinkSocket>),
}

pub enum SocketRefMut<'a> {
    Unix(atomic_refcell::AtomicRefMut<'a, UnixSocket>),
    Netlink(atomic_refcell::AtomicRefMut<'a, NetlinkSocket>),
}

// file functions
impl SocketRef<'_> {
    enum_passthrough!(self, (), Unix, Netlink;
        pub fn state(&self) -> FileState
    );
    enum_passthrough!(self, (), Unix, Netlink;
        pub fn mode(&self) -> FileMode
    );
    enum_passthrough!(self, (), Unix, Netlink;
        pub fn get_status(&self) -> FileStatus
    );
    enum_passthrough!(self, (), Unix, Netlink;
        pub fn has_open_file(&self) -> bool
    );
    enum_passthrough!(self, (), Unix, Netlink;
        pub fn supports_sa_restart(&self) -> bool
    );
}

// socket-specific functions
impl SocketRef<'_> {
    pub fn getpeername(&self) -> Result<Option<SockaddrStorage>, SyscallError> {
        match self {
            Self::Unix(socket) => socket.getpeername().map(|opt| opt.map(Into::into)),
            Self::Netlink(socket) => socket.getpeername(),
        }
    }

    pub fn getsockname(&self) -> Result<Option<SockaddrStorage>, SyscallError> {
        match self {
            Self::Unix(socket) => socket.getsockname().map(|opt| opt.map(Into::into)),
            Self::Netlink(socket) => socket.getsockname(),
        }
    }

    enum_passthrough!(self, (), Unix, Netlink;
        pub fn address_family(&self) -> nix::sys::socket::AddressFamily
    );
}

// file functions
impl SocketRefMut<'_> {
    enum_passthrough!(self, (), Unix, Netlink;
        pub fn state(&self) -> FileState
    );
    enum_passthrough!(self, (), Unix, Netlink;
        pub fn mode(&self) -> FileMode
    );
    enum_passthrough!(self, (), Unix, Netlink;
        pub fn get_status(&self) -> FileStatus
    );
    enum_passthrough!(self, (), Unix, Netlink;
        pub fn has_open_file(&self) -> bool
    );
    enum_passthrough!(self, (val), Unix, Netlink;
        pub fn set_has_open_file(&mut self, val: bool)
    );
    enum_passthrough!(self, (), Unix, Netlink;
        pub fn supports_sa_restart(&self) -> bool
    );
    enum_passthrough!(self, (cb_queue), Unix, Netlink;
        pub fn close(&mut self, cb_queue: &mut CallbackQueue) -> Result<(), SyscallError>
    );
    enum_passthrough!(self, (status), Unix, Netlink;
        pub fn set_status(&mut self, status: FileStatus)
    );
    enum_passthrough!(self, (request, arg_ptr, memory_manager), Unix, Netlink;
        pub fn ioctl(&mut self, request: u64, arg_ptr: PluginPtr, memory_manager: &mut MemoryManager) -> SyscallResult
    );
    enum_passthrough!(self, (ptr), Unix, Netlink;
        pub fn add_legacy_listener(&mut self, ptr: HostTreePointer<c::StatusListener>)
    );
    enum_passthrough!(self, (ptr), Unix, Netlink;
        pub fn remove_legacy_listener(&mut self, ptr: *mut c::StatusListener)
    );

    enum_passthrough_generic!(self, (bytes, offset, cb_queue), Unix, Netlink;
        pub fn read<W>(&mut self, bytes: W, offset: libc::off_t, cb_queue: &mut CallbackQueue) -> SyscallResult
        where W: std::io::Write + std::io::Seek
    );

    enum_passthrough_generic!(self, (source, offset, cb_queue), Unix, Netlink;
        pub fn write<R>(&mut self, source: R, offset: libc::off_t, cb_queue: &mut CallbackQueue) -> SyscallResult
        where R: std::io::Read + std::io::Seek
    );
}

// socket-specific functions
impl SocketRefMut<'_> {
    pub fn getpeername(&self) -> Result<Option<SockaddrStorage>, SyscallError> {
        match self {
            Self::Unix(socket) => socket.getpeername().map(|opt| opt.map(Into::into)),
            Self::Netlink(socket) => socket.getpeername(),
        }
    }

    pub fn getsockname(&self) -> Result<Option<SockaddrStorage>, SyscallError> {
        match self {
            Self::Unix(socket) => socket.getsockname().map(|opt| opt.map(Into::into)),
            Self::Netlink(socket) => socket.getsockname(),
        }
    }

    enum_passthrough!(self, (), Unix, Netlink;
        pub fn address_family(&self) -> nix::sys::socket::AddressFamily
    );

    enum_passthrough_generic!(self, (source, addr, cb_queue), Unix, Netlink;
        pub fn sendto<R>(&mut self, source: R, addr: Option<SockaddrStorage>, cb_queue: &mut CallbackQueue)
            -> SyscallResult
        where R: std::io::Read + std::io::Seek
    );

    enum_passthrough_generic!(self, (bytes, cb_queue), Unix, Netlink;
        pub fn recvfrom<W>(&mut self, bytes: W, cb_queue: &mut CallbackQueue)
            -> Result<(SysCallReg, Option<SockaddrStorage>), SyscallError>
        where W: std::io::Write + std::io::Seek
    );

    enum_passthrough!(self, (backlog, cb_queue), Unix, Netlink;
        pub fn listen(&mut self, backlog: i32, cb_queue: &mut CallbackQueue) -> Result<(), SyscallError>
    );

    pub fn accept(&mut self, cb_queue: &mut CallbackQueue) -> Result<Socket, SyscallError> {
        match self {
            Self::Unix(socket) => socket.accept(cb_queue).map(Socket::Unix),
            Self::Netlink(socket) => socket.accept(cb_queue).map(Socket::Netlink),
        }
    }
}

impl std::fmt::Debug for SocketRef<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unix(_) => write!(f, "Unix")?,
            Self::Netlink(_) => write!(f, "Netlink")?,
        }

        write!(
            f,
            "(state: {:?}, status: {:?})",
            self.state(),
            self.get_status()
        )
    }
}

impl std::fmt::Debug for SocketRefMut<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unix(_) => write!(f, "Unix")?,
            Self::Netlink(_) => write!(f, "Netlink")?,
        }

        write!(
            f,
            "(state: {:?}, status: {:?})",
            self.state(),
            self.get_status()
        )
    }
}
