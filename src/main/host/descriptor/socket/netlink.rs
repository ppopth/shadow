use std::net::Ipv4Addr;
use std::sync::{Arc, Weak};

use atomic_refcell::AtomicRefCell;
use netlink_packet_core::constants::{
    NLM_F_MULTIPART,
};
use netlink_packet_route::{
    AddressHeader, AddressMessage, ErrorMessage,
    NetlinkHeader, NetlinkMessage, NetlinkPayload, RtnlMessage,
};
use netlink_packet_route::rtnl::address::Nla;
use netlink_packet_route::rtnl::constants::{
    AF_UNSPEC, AF_INET, IFA_F_NOPREFIXROUTE, IFA_F_PERMANENT,
    RT_SCOPE_HOST, RT_SCOPE_UNIVERSE,
};
use nix::errno::Errno;
use nix::sys::socket::NetlinkAddr;

use crate::core::worker::Worker;
use crate::cshadow as c;
use crate::host::descriptor::shared_buf::{
    BufferHandle, BufferState, ReaderHandle, SharedBuf,
};
use crate::host::descriptor::{
    FileMode, FileState, FileStatus, StateEventSource, SyscallResult,
};
use crate::host::memory_manager::MemoryManager;
use crate::host::syscall_types::{PluginPtr, SysCallReg, SyscallError};
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
            let recv_buffer = SharedBuf::new(usize::MAX);
            let recv_buffer = Arc::new(AtomicRefCell::new(recv_buffer));

            let mut common = NetlinkSocketCommon {
                recv_buffer,
                state: FileState::ACTIVE,
                status,
                has_open_file: false,
                event_source: StateEventSource::new(),
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
        FileMode::READ | FileMode::WRITE
    }

    pub fn has_open_file(&self) -> bool {
        self.common.has_open_file
    }

    pub fn supports_sa_restart(&self) -> bool {
        true
    }

    pub fn set_has_open_file(&mut self, val: bool) {
        self.common.has_open_file = val;
    }

    pub fn getsockname(&self) -> Result<Option<SockaddrStorage>, SyscallError> {
        unimplemented!()
    }

    pub fn getpeername(&self) -> Result<Option<SockaddrStorage>, SyscallError> {
        unimplemented!()
    }

    pub fn address_family(&self) -> nix::sys::socket::AddressFamily {
        nix::sys::socket::AddressFamily::Netlink
    }

    pub fn close(&mut self, cb_queue: &mut CallbackQueue) -> Result<(), SyscallError> {
        self.protocol_state.close(&mut self.common, cb_queue)
    }

    fn refresh_file_state(&mut self, cb_queue: &mut CallbackQueue) {
        self.protocol_state
            .refresh_file_state(&mut self.common, cb_queue)
    }

    pub fn bind(
        socket: &Arc<AtomicRefCell<Self>>,
        addr: Option<&SockaddrStorage>,
        rng: impl rand::Rng,
    ) -> SyscallResult {
        let socket_ref = &mut *socket.borrow_mut();
        socket_ref
            .protocol_state
            .bind(&mut socket_ref.common, addr, rng)
    }

    pub fn read<W>(
        &mut self,
        mut _bytes: W,
        _offset: libc::off_t,
        _cb_queue: &mut CallbackQueue,
    ) -> SyscallResult
    where
        W: std::io::Write + std::io::Seek,
    {
        // we could call NetlinkSocket::recvfrom() here, but for now we expect that there are no code
        // paths that would call NetlinkSocket::read() since the read() syscall handler should have
        // called NetlinkSocket::recvfrom() instead
        panic!("Called NetlinkSocket::read() on a netlink socket.");
    }

    pub fn write<R>(
        &mut self,
        mut _bytes: R,
        _offset: libc::off_t,
        _cb_queue: &mut CallbackQueue,
    ) -> SyscallResult
    where
        R: std::io::Read + std::io::Seek,
    {
        // we could call NetlinkSocket::sendto() here, but for now we expect that there are no code
        // paths that would call NetlinkSocket::write() since the write() syscall handler should have
        // called NetlinkSocket::sendto() instead
        panic!("Called NetlinkSocket::write() on a netlink socket");
    }

    pub fn sendto<R>(
        &mut self,
        bytes: R,
        addr: Option<SockaddrStorage>,
        cb_queue: &mut CallbackQueue,
    ) -> SyscallResult
    where
        R: std::io::Read + std::io::Seek,
    {
        self.protocol_state
            .sendto(&mut self.common, bytes, addr, cb_queue)
    }

    pub fn recvfrom<W>(
        &mut self,
        bytes: W,
        cb_queue: &mut CallbackQueue,
    ) -> Result<(SysCallReg, Option<SockaddrStorage>), SyscallError>
    where
        W: std::io::Write + std::io::Seek,
    {
        self.protocol_state
            .recvfrom(&mut self.common, bytes, cb_queue)
    }

    pub fn ioctl(
        &mut self,
        request: u64,
        _arg_ptr: PluginPtr,
        _memory_manager: &mut MemoryManager,
    ) -> SyscallResult {
        log::warn!(
            "We do not yet handle ioctl request {} on netlink sockets",
            request
        );
        Err(Errno::EINVAL.into())
    }

    pub fn listen(
        &mut self,
        _backlog: i32,
        _cb_queue: &mut CallbackQueue,
    ) -> Result<(), SyscallError> {
        Err(Errno::EOPNOTSUPP.into())
    }

    pub fn connect(
        _socket: &Arc<AtomicRefCell<Self>>,
        _addr: &SockaddrStorage,
        _cb_queue: &mut CallbackQueue,
    ) -> Result<(), SyscallError> {
        unimplemented!()
    }

    pub fn accept(
        &mut self,
        _cb_queue: &mut CallbackQueue,
    ) -> Result<Arc<AtomicRefCell<Self>>, SyscallError> {
        Err(Errno::EOPNOTSUPP.into())
    }

    pub fn add_legacy_listener(&mut self, ptr: HostTreePointer<c::StatusListener>) {
        self.common.event_source.add_legacy_listener(ptr);
    }

    pub fn remove_legacy_listener(&mut self, ptr: *mut c::StatusListener) {
        self.common.event_source.remove_legacy_listener(ptr);
    }

    pub fn state(&self) -> FileState {
        self.common.state
    }
}

struct InitialState {
    // We don't keep the bound address so that we won't need to fill it.
    is_bound: bool,
    reader_handle: ReaderHandle,
    // this handle is never accessed, but we store it because of its drop impl
    _recv_buffer_handle: BufferHandle,
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
        // this is a new socket and there are no listeners, so safe to use a temporary event queue
        let mut cb_queue = CallbackQueue::new();

        // dgram netlink sockets are immediately able to receive data, so initialize the
        // receive buffer

        // increment the buffer's reader count
        let reader_handle = common.recv_buffer.borrow_mut().add_reader(&mut cb_queue);

        let weak = Weak::clone(socket);
        let recv_buffer_handle = common.recv_buffer.borrow_mut().add_listener(
            BufferState::READABLE,
            move |_, cb_queue| {
                if let Some(socket) = weak.upgrade() {
                    socket.borrow_mut().refresh_file_state(cb_queue);
                }
            },
        );

        // make sure no events were generated since if there were events to run, they would
        // probably not run correctly if the socket's Arc is not fully created yet (as in
        // the case of `Arc::new_cyclic`)
        assert!(cb_queue.is_empty());

        ProtocolState::Initial(Some(InitialState {
            is_bound: false,
            reader_handle,
            _recv_buffer_handle: recv_buffer_handle,
        }))
    }

    fn refresh_file_state(&self, common: &mut NetlinkSocketCommon, cb_queue: &mut CallbackQueue) {
        match self {
            Self::Initial(x) => x.as_ref().unwrap().refresh_file_state(common, cb_queue),
            Self::Closed(x) => x.as_ref().unwrap().refresh_file_state(common, cb_queue),
        }
    }

    fn close(
        &mut self,
        common: &mut NetlinkSocketCommon,
        cb_queue: &mut CallbackQueue,
    ) -> Result<(), SyscallError> {
        let (new_state, rv) = match self {
            Self::Initial(x) => x.take().unwrap().close(common, cb_queue),
            Self::Closed(x) => x.take().unwrap().close(common, cb_queue),
        };

        *self = new_state;
        rv
    }

    fn bind(
        &mut self,
        common: &mut NetlinkSocketCommon,
        addr: Option<&SockaddrStorage>,
        rng: impl rand::Rng,
    ) -> SyscallResult {
        match self {
            Self::Initial(x) => x.as_mut().unwrap().bind(common, addr, rng),
            Self::Closed(x) => x.as_mut().unwrap().bind(common, addr, rng),
        }
    }

    fn sendto<R>(
        &mut self,
        common: &mut NetlinkSocketCommon,
        bytes: R,
        addr: Option<SockaddrStorage>,
        cb_queue: &mut CallbackQueue,
    ) -> SyscallResult
    where
        R: std::io::Read + std::io::Seek,
    {
        match self {
            Self::Initial(x) => x.as_mut().unwrap().sendto(common, bytes, addr, cb_queue),
            Self::Closed(x) => x.as_mut().unwrap().sendto(common, bytes, addr, cb_queue),
        }
    }

    fn recvfrom<W>(
        &mut self,
        common: &mut NetlinkSocketCommon,
        bytes: W,
        cb_queue: &mut CallbackQueue,
    ) -> Result<(SysCallReg, Option<SockaddrStorage>), SyscallError>
    where
        W: std::io::Write + std::io::Seek,
    {
        match self {
            Self::Initial(x) => x.as_mut().unwrap().recvfrom(common, bytes, cb_queue),
            Self::Closed(x) => x.as_mut().unwrap().recvfrom(common, bytes, cb_queue),
        }
    }
}

struct InterfaceConfig {
    address: Ipv4Addr,
    label: String,
    prefix_len: u8,
    scope: u8,
    index: u32,
    flags: u32,
}

impl InitialState {
    fn refresh_file_state(&self, common: &mut NetlinkSocketCommon, cb_queue: &mut CallbackQueue) {
        let mut new_state = FileState::ACTIVE;

        {
            let recv_buffer = common.recv_buffer.borrow();

            new_state.set(FileState::READABLE, recv_buffer.has_data());
            // The socket is not allowed to write if there are some data to receive.
            // FIXME: Probably we want to change this behavior. I don't know how Linux's netlink
            // socket works.
            new_state.set(FileState::WRITABLE, !recv_buffer.has_data());
        }

        common.copy_state(/* mask= */ FileState::all(), new_state, cb_queue);
    }

    fn handle_interface(
        &mut self,
        common: &mut NetlinkSocketCommon,
        seq: u32,
        config: InterfaceConfig,
        cb_queue: &mut CallbackQueue,
    ) {
        let mut nlas = Vec::new();
        let address_vec = config.address.octets().to_vec();
        nlas.push(Nla::Address(address_vec.clone()));
        nlas.push(Nla::Local(address_vec.clone()));

        if config.prefix_len == 32 {
            nlas.push(Nla::Broadcast(address_vec));
        } else {
            let brd = Ipv4Addr::from(
                (0xffff_ffff_u32) >> u32::from(config.prefix_len) | u32::from(config.address),
            );
            nlas.push(Nla::Broadcast(brd.octets().to_vec()));
        };

        nlas.push(Nla::Label(config.label));
        nlas.push(Nla::Flags(config.flags));

        let message = AddressMessage {
            header: AddressHeader {
                family: AF_INET as u8,
                prefix_len: config.prefix_len,
                flags: IFA_F_PERMANENT as u8,
                scope: config.scope,
                index: config.index,
            },
            nlas,
        };
        let mut packet = NetlinkMessage {
            header: NetlinkHeader {
                sequence_number: seq,
                flags: NLM_F_MULTIPART,
                ..NetlinkHeader::default()
            },
            payload: RtnlMessage::NewAddress(message).into(),
        };
        packet.finalize();
        self.send_netlink_packet(common, &packet, cb_queue);
    }

    fn send_netlink_packet(
        &mut self,
        common: &mut NetlinkSocketCommon,
        packet: &NetlinkMessage<RtnlMessage>,
        cb_queue: &mut CallbackQueue,
    ) {
        // Prepare a buffer to serialize the packet.
        let mut buf = vec![0; packet.header.length as usize];
        // Serialize the packet
        packet.serialize(&mut buf[..]);
        // Unwrap is safe here. There will be no error.
        common.recv_buffer.borrow_mut().write_packet(&buf[..], buf.len(), cb_queue).unwrap();
    }

    fn close(
        self,
        common: &mut NetlinkSocketCommon,
        cb_queue: &mut CallbackQueue,
    ) -> (ProtocolState, Result<(), SyscallError>) {
        // inform the buffer that there is one fewer readers
        common
            .recv_buffer
            .borrow_mut()
            .remove_reader(self.reader_handle, cb_queue);

        let new_state = ClosedState {};
        new_state.refresh_file_state(common, cb_queue);
        (new_state.into(), Ok(()))
    }

    fn bind(
        &mut self,
        _common: &mut NetlinkSocketCommon,
        addr: Option<&SockaddrStorage>,
        _rng: impl rand::Rng,
    ) -> SyscallResult {
        // if already bound
        if self.is_bound {
            return Err(Errno::EINVAL.into());
        }

        if addr.map(|x| x.as_netlink()).flatten().is_none() {
            log::warn!(
                "Attempted to bind netlink socket to non-netlink address {:?}",
                addr
            );
            return Err(Errno::EINVAL.into());
        };

        self.is_bound = true;
        // 1. The pid of the address doesn't matter because we will not use it anywhere.
        // 2. The groups of the address also doesn't matter because we will not broadcast any
        //    message anyway.
        Ok(0.into())
    }

    fn sendto<R>(
        &mut self,
        common: &mut NetlinkSocketCommon,
        mut bytes: R,
        addr: Option<SockaddrStorage>,
        cb_queue: &mut CallbackQueue,
    ) -> SyscallResult
    where
        R: std::io::Read + std::io::Seek,
    {
        let Some(addr) = addr.as_ref().map(|x| x.as_netlink()).flatten() else {
            log::warn!("Attempted to send to non-netlink address {:?}", addr);
            return Err(Errno::EINVAL.into());
        };

        if addr.pid() != 0 {
            log::warn!("Attempted to send to non-kernel netlink address {:?}", addr);
            return Err(Errno::EINVAL.into());
        }

        if common.recv_buffer.borrow().has_data() {
            // we can send this when the buffer has nothing to read
            return Err(Errno::EAGAIN.into());
        }

        let mut buf = Vec::new();
        bytes.read_to_end(&mut buf)?;

        let mut handle_netlink_error = |seq: u32, msg: &str| {
            log::warn!("{}", msg);
            let mut packet = NetlinkMessage {
                header: NetlinkHeader {
                    sequence_number: seq,
                    ..NetlinkHeader::default()
                },
                payload: NetlinkPayload::Error(ErrorMessage {
                    code: -1,
                    header: msg.into(),
                }),
            };
            packet.finalize();
            self.send_netlink_packet(common, &packet, cb_queue);
            self.refresh_file_state(common, cb_queue);
        };

        let Ok(packet) = NetlinkMessage::<RtnlMessage>::deserialize(&buf) else {
            handle_netlink_error(0, "Deserialization error");
            return Ok(buf.len().into());
        };
        let seq = packet.header.sequence_number;
        let NetlinkPayload::InnerMessage(RtnlMessage::GetAddress(message)) = packet.payload else {
            handle_netlink_error(seq, "Unsupported message type (only RTM_GETADDR is supported)");
            return Ok(buf.len().into());
        };
        if message.header.family != AF_UNSPEC as u8 && message.header.family != AF_INET as u8 {
            handle_netlink_error(seq, "Unsupported ifa_family (only AF_UNSPEC and AF_INET are supported)");
            return Ok(buf.len().into());
        };
        if message.header.scope != RT_SCOPE_UNIVERSE {
            handle_netlink_error(seq, "Unsupported ifa_scope (only RT_SCOPE_UNIVERSE is supported)");
            return Ok(buf.len().into());
        };
        if message.header.prefix_len != 0 || message.header.flags != 0 || message.header.index != 0 {
            handle_netlink_error(seq, "Unsupported ifa_prefixlen, ifs_flags, or ifs_index (they have to be 0)");
            return Ok(buf.len().into());
        };

        Worker::with_active_host(|host| {
            // All the configurations are the same as in the syscall of getifaddrs
            self.handle_interface(common, seq, InterfaceConfig {
                address: Ipv4Addr::LOCALHOST,
                label: String::from("lo"),
                prefix_len: 8,
                scope: RT_SCOPE_HOST,
                index: 1,
                flags: IFA_F_NOPREFIXROUTE,
            }, cb_queue);
            self.handle_interface(common, seq, InterfaceConfig {
                address: host.default_ip(),
                label: String::from("eth0"),
                prefix_len: 24,
                scope: RT_SCOPE_UNIVERSE,
                index: 2,
                flags: IFA_F_PERMANENT,
            }, cb_queue);
            let mut packet = NetlinkMessage {
                header: NetlinkHeader {
                    sequence_number: seq,
                    ..NetlinkHeader::default()
                },
                payload: NetlinkPayload::Done,
            };
            packet.finalize();
            self.send_netlink_packet(common, &packet, cb_queue);
        }).unwrap();

        self.refresh_file_state(common, cb_queue);
        return Ok(buf.len().into());
    }

    fn recvfrom<W>(
        &mut self,
        common: &mut NetlinkSocketCommon,
        mut bytes: W,
        cb_queue: &mut CallbackQueue,
    ) -> Result<(SysCallReg, Option<SockaddrStorage>), SyscallError>
    where
        W: std::io::Write + std::io::Seek,
    {
        let mut recv_buffer = common.recv_buffer.borrow_mut();

        if !recv_buffer.has_data() {
            // return EWOULDBLOCK even if 'bytes' has length 0
            return Err(Errno::EWOULDBLOCK.into());
        }

        let (num_copied, _) = recv_buffer.read(&mut bytes, cb_queue)?;
        drop(recv_buffer);

        self.refresh_file_state(common, cb_queue);
        Ok((num_copied.into(), Some(SockaddrStorage::from_netlink(&NetlinkAddr::new(0, 0)))))
    }
}

impl ClosedState {
    fn refresh_file_state(&self, common: &mut NetlinkSocketCommon, cb_queue: &mut CallbackQueue) {
        common.copy_state(
            /* mask= */ FileState::all(),
            FileState::CLOSED,
            cb_queue,
        );
    }

    fn close(
        self,
        _common: &mut NetlinkSocketCommon,
        _cb_queue: &mut CallbackQueue,
    ) -> (ProtocolState, Result<(), SyscallError>) {
        // why are we trying to close an already closed file? we probably want a bt here...
        panic!("Trying to close an already closed socket");
    }

    fn bind(
        &mut self,
        _common: &mut NetlinkSocketCommon,
        _addr: Option<&SockaddrStorage>,
        _rng: impl rand::Rng,
    ) -> SyscallResult {
        Err(Errno::EBADF.into())
    }

    fn sendto<R>(
        &mut self,
        _common: &mut NetlinkSocketCommon,
        _bytes: R,
        _addr: Option<SockaddrStorage>,
        _cb_queue: &mut CallbackQueue,
    ) -> SyscallResult
    where
        R: std::io::Read + std::io::Seek,
    {
        Err(Errno::EBADF.into())
    }

    fn recvfrom<W>(
        &mut self,
        _common: &mut NetlinkSocketCommon,
        _bytes: W,
        _cb_queue: &mut CallbackQueue,
    ) -> Result<(SysCallReg, Option<SockaddrStorage>), SyscallError>
    where
        W: std::io::Write + std::io::Seek,
    {
        Err(Errno::EBADF.into())
    }
}

/// Common data and functionality that is useful for all states.
struct NetlinkSocketCommon {
    recv_buffer: Arc<AtomicRefCell<SharedBuf>>,
    state: FileState,
    status: FileStatus,
    // should only be used by `OpenFile` to make sure there is only ever one `OpenFile` instance for
    // this file
    has_open_file: bool,
    event_source: StateEventSource,
}

impl NetlinkSocketCommon {
    fn copy_state(&mut self, mask: FileState, state: FileState, cb_queue: &mut CallbackQueue) {
        let old_state = self.state;

        // remove the masked flags, then copy the masked flags
        self.state.remove(mask);
        self.state.insert(state & mask);

        self.handle_state_change(old_state, cb_queue);
    }

    fn handle_state_change(&mut self, old_state: FileState, cb_queue: &mut CallbackQueue) {
        let states_changed = self.state ^ old_state;

        // if nothing changed
        if states_changed.is_empty() {
            return;
        }

        self.event_source
            .notify_listeners(self.state, states_changed, cb_queue);
    }
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
