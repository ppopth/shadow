use std::sync::{Arc, Weak};

use atomic_refcell::AtomicRefCell;
use nix::errno::Errno;
use nix::sys::socket::Shutdown;
use shadow_shim_helper_rs::syscall_types::ForeignPtr;

use crate::cshadow as c;
use crate::host::descriptor::{
    FileMode, FileState, FileStatus, SyscallResult,
};
use crate::host::memory_manager::MemoryManager;
use crate::host::syscall::io::IoVec;
use crate::host::syscall_types::SyscallError;
use crate::network::net_namespace::NetworkNamespace;
use crate::utility::callback_queue::CallbackQueue;
use crate::utility::sockaddr::SockaddrStorage;
use crate::utility::HostTreePointer;

pub struct NetlinkSocket {
    /// Data and functionality that is general for all states.
    common: NetlinkSocketCommon,
    /// State-specific data and functionality.
    protocol_state: ProtocolState,
}

impl NetlinkSocket {
    pub fn new(
        status: FileStatus,
        _socket_type: NetlinkSocketType,
        _family: NetlinkFamily,
    ) -> Arc<AtomicRefCell<Self>> {
        Arc::new_cyclic(|weak| {
            let mut common = NetlinkSocketCommon {
                state: FileState::ACTIVE,
                status,
                has_open_file: false,
            };
            let protocol_state = ProtocolState::new(&mut common, weak);
            AtomicRefCell::new(Self {
                common,
                protocol_state,
            })
        })
    }

    pub fn get_status(&self) -> FileStatus {
        self.common.status
    }

    pub fn set_status(&mut self, status: FileStatus) {
        self.common.status = status;
    }

    pub fn mode(&self) -> FileMode {
        unimplemented!()
    }

    pub fn has_open_file(&self) -> bool {
        self.common.has_open_file
    }

    pub fn supports_sa_restart(&self) -> bool {
        unimplemented!()
    }

    pub fn set_has_open_file(&mut self, val: bool) {
        self.common.has_open_file = val;
    }

    pub fn address_family(&self) -> nix::sys::socket::AddressFamily {
        unimplemented!()
    }

    pub fn close(&mut self, cb_queue: &mut CallbackQueue) -> Result<(), SyscallError> {
        unimplemented!()
    }

    pub fn shutdown(
        &mut self,
        _how: Shutdown,
        _cb_queue: &mut CallbackQueue,
    ) -> Result<(), SyscallError> {
        // We follow the same approach as UnixSocket
        log::warn!("shutdown() syscall not yet supported for netlink sockets; Returning ENOSYS");
        Err(Errno::ENOSYS.into())
    }

    pub fn getsockopt(
        &self,
        _level: libc::c_int,
        _optname: libc::c_int,
        _optval_ptr: ForeignPtr<()>,
        _optlen: libc::socklen_t,
        _memory_manager: &mut MemoryManager,
    ) -> Result<libc::socklen_t, SyscallError> {
        // We follow the same approach as UnixSocket
        log::warn!("getsockopt() syscall not yet supported for netlink sockets; Returning ENOSYS");
        Err(Errno::ENOSYS.into())
    }

    pub fn setsockopt(
        &self,
        _level: libc::c_int,
        _optname: libc::c_int,
        _optval_ptr: ForeignPtr<()>,
        _optlen: libc::socklen_t,
        _memory_manager: &MemoryManager,
    ) -> Result<(), SyscallError> {
        // We follow the same approach as UnixSocket
        log::warn!("setsockopt() syscall not yet supported for netlink sockets; Returning ENOSYS");
        Err(Errno::ENOSYS.into())
    }

    pub fn bind(
        socket: &Arc<AtomicRefCell<Self>>,
        addr: Option<&SockaddrStorage>,
        _net_ns: &NetworkNamespace,
        rng: impl rand::Rng,
    ) -> SyscallResult {
        let socket_ref = &mut *socket.borrow_mut();
        socket_ref
            .protocol_state
            .bind(&mut socket_ref.common, socket, addr, rng)
    }

    pub fn readv(
        &mut self,
        _iovs: &[IoVec],
        _offset: Option<libc::off_t>,
        _flags: libc::c_int,
        _mem: &mut MemoryManager,
        _cb_queue: &mut CallbackQueue,
    ) -> Result<libc::ssize_t, SyscallError> {
        // we could call NetlinkSocket::recvmsg() here, but for now we expect that there are no code
        // paths that would call NetlinkSocket::readv() since the readv() syscall handler should have
        // called NetlinkSocket::recvmsg() instead
        panic!("Called NetlinkSocket::readv() on a netlink socket.");
    }

    pub fn writev(
        &mut self,
        _iovs: &[IoVec],
        _offset: Option<libc::off_t>,
        _flags: libc::c_int,
        _mem: &mut MemoryManager,
        _cb_queue: &mut CallbackQueue,
    ) -> Result<libc::ssize_t, SyscallError> {
        // we could call NetlinkSocket::sendmsg() here, but for now we expect that there are no code
        // paths that would call NetlinkSocket::writev() since the writev() syscall handler should have
        // called NetlinkSocket::sendmsg() instead
        panic!("Called NetlinkSocket::writev() on a netlink socket");
    }

    pub fn ioctl(
        &mut self,
        request: u64,
        _arg_ptr: ForeignPtr<()>,
        _memory_manager: &mut MemoryManager,
    ) -> SyscallResult {
        // We follow the same approach as UnixSocket
        log::warn!(
            "We do not yet handle ioctl request {} on netlink sockets",
            request
        );
        Err(Errno::EINVAL.into())
    }

    pub fn add_legacy_listener(&mut self, ptr: HostTreePointer<c::StatusListener>) {
        unimplemented!()
    }

    pub fn remove_legacy_listener(&mut self, ptr: *mut c::StatusListener) {
        unimplemented!()
    }

    pub fn state(&self) -> FileState {
        self.common.state
    }
}

struct InitialState {
    // Indicate that if the socket is already bound or not. We don't keep the bound address so that
    // we won't need to fill it.
    is_bound: bool,
}
struct ClosedState {
}
/// The current protocol state of the netlink socket. An `Option` is required for each variant so that
/// the inner state object can be removed, transformed into a new state, and then re-added as a
/// different variant.
enum ProtocolState {
    Initial(Option<InitialState>),
    Closed(Option<ClosedState>),
}

/// Upcast from a type to an enum variant.
macro_rules! state_upcast {
    ($type:ty, $parent:ident::$variant:ident) => {
        impl From<$type> for $parent {
            fn from(x: $type) -> Self {
                Self::$variant(Some(x))
            }
        }
    };
}

// implement upcasting for all state types
state_upcast!(InitialState, ProtocolState::Initial);
state_upcast!(ClosedState, ProtocolState::Closed);

impl ProtocolState {
    fn new(
        common: &mut NetlinkSocketCommon,
        socket: &Weak<AtomicRefCell<NetlinkSocket>>,
    ) -> Self {
        ProtocolState::Initial(Some(InitialState {
            is_bound: false,
        }))
    }

    fn bind(
        &mut self,
        common: &mut NetlinkSocketCommon,
        socket: &Arc<AtomicRefCell<NetlinkSocket>>,
        addr: Option<&SockaddrStorage>,
        rng: impl rand::Rng,
    ) -> SyscallResult {
        match self {
            Self::Initial(x) => x.as_mut().unwrap().bind(common, socket, addr, rng),
            Self::Closed(x) => x.as_mut().unwrap().bind(common, socket, addr, rng),
        }
    }
}

impl InitialState {
    fn bind(
        &mut self,
        _common: &mut NetlinkSocketCommon,
        _socket: &Arc<AtomicRefCell<NetlinkSocket>>,
        addr: Option<&SockaddrStorage>,
        _rng: impl rand::Rng,
    ) -> SyscallResult {
        // if already bound
        if self.is_bound {
            return Err(Errno::EINVAL.into());
        }

        // get the netlink address
        let Some(addr) = addr.and_then(|x| x.as_netlink()) else {
            log::warn!(
                "Attempted to bind netlink socket to non-netlink address {:?}",
                addr
            );
            return Err(Errno::EINVAL.into());
        };
        // remember that the socket is bound
        self.is_bound = true;

        // According to netlink(7), if the pid is zero, the kernel takes care of assigning it, but
        // we will leave it untouched at the moment. We can implement the assignment later when we
        // want to support it.

        // According to netlink(7), if the groups is non-zero, it means that the socket wants to
        // listen to some groups. Since we don't support broadcasting to groups yet, we will emit
        // the error here.
        if addr.groups() != 0 {
            log::warn!(
                "Attempted to bind netlink socket to an address with non-zero groups {}",
                addr.groups()
            );
            return Err(Errno::EINVAL.into());
        }

        Ok(0.into())
    }
}

impl ClosedState {
    fn bind(
        &mut self,
        _common: &mut NetlinkSocketCommon,
        _socket: &Arc<AtomicRefCell<NetlinkSocket>>,
        _addr: Option<&SockaddrStorage>,
        _rng: impl rand::Rng,
    ) -> SyscallResult {
        // We follow the same approach as UnixSocket
        log::warn!("bind() while in state {}", std::any::type_name::<Self>());
        Err(Errno::EOPNOTSUPP.into())
    }
}

/// Common data and functionality that is useful for all states.
struct NetlinkSocketCommon {
    state: FileState,
    status: FileStatus,
    // should only be used by `OpenFile` to make sure there is only ever one `OpenFile` instance for
    // this file
    has_open_file: bool,
}

impl NetlinkSocketCommon {
}

#[derive(Copy, Clone, Debug, Hash, PartialEq, Eq)]
pub enum NetlinkSocketType {
    Dgram,
    Raw,
}

impl TryFrom<libc::c_int> for NetlinkSocketType {
    type Error = NetlinkSocketTypeConversionError;
    fn try_from(val: libc::c_int) -> Result<Self, Self::Error> {
        match val {
            libc::SOCK_DGRAM => Ok(Self::Dgram),
            libc::SOCK_RAW => Ok(Self::Raw),
            x => Err(NetlinkSocketTypeConversionError(x)),
        }
    }
}

#[derive(Copy, Clone, Debug)]
pub struct NetlinkSocketTypeConversionError(libc::c_int);

impl std::error::Error for NetlinkSocketTypeConversionError {}

impl std::fmt::Display for NetlinkSocketTypeConversionError {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        write!(
            f,
            "Invalid socket type {}; netlink sockets only support SOCK_DGRAM and SOCK_RAW",
            self.0
        )
    }
}

#[derive(Copy, Clone, Debug, Hash, PartialEq, Eq)]
pub enum NetlinkFamily {
    Route,
}

impl TryFrom<libc::c_int> for NetlinkFamily {
    type Error = NetlinkFamilyConversionError;
    fn try_from(val: libc::c_int) -> Result<Self, Self::Error> {
        match val {
            libc::NETLINK_ROUTE => Ok(Self::Route),
            x => Err(NetlinkFamilyConversionError(x)),
        }
    }
}

#[derive(Copy, Clone, Debug)]
pub struct NetlinkFamilyConversionError(libc::c_int);

impl std::error::Error for NetlinkFamilyConversionError {}

impl std::fmt::Display for NetlinkFamilyConversionError {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        write!(
            f,
            "Invalid netlink family {}; netlink families only support NETLINK_ROUTE",
            self.0
        )
    }
}
