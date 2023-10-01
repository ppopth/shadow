use std::io::{Cursor, Read, Write};
use std::net::Ipv4Addr;
use std::sync::{Arc, Weak};

use atomic_refcell::AtomicRefCell;
use neli::{FromBytes, ToBytes};
use neli::consts::rtnl::{Ifa, IfaF, IfaFFlags, RtAddrFamily, Rtm, RtScope};
use neli::consts::nl::{NlmF, NlmFFlags, Nlmsg};
use neli::nl::{Nlmsghdr, NlPayload};
use neli::rtnl::{Ifaddrmsg, Rtattr};
use neli::types::{Buffer, RtBuffer};
use nix::errno::Errno;
use nix::sys::socket::{MsgFlags, NetlinkAddr, Shutdown};
use shadow_shim_helper_rs::syscall_types::ForeignPtr;

use crate::core::worker::Worker;
use crate::cshadow as c;
use crate::host::descriptor::shared_buf::SharedBuf;
use crate::host::descriptor::socket::{RecvmsgArgs, RecvmsgReturn, SendmsgArgs, Socket};
use crate::host::descriptor::{
    File, FileMode, FileState, FileStatus, StateEventSource, StateListenerFilter,
    SyscallResult,
};
use crate::host::memory_manager::MemoryManager;
use crate::host::syscall::io::{IoVec, IoVecReader, IoVecWriter};
use crate::host::syscall_types::SyscallError;
use crate::network::net_namespace::NetworkNamespace;
use crate::utility::callback_queue::{CallbackQueue, Handle};
use crate::utility::sockaddr::SockaddrStorage;
use crate::utility::HostTreePointer;

// this constant is copied from UNIX_SOCKET_DEFAULT_BUFFER_SIZE
const NETLINK_SOCKET_DEFAULT_BUFFER_SIZE: u64 = 212_992;

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
            // each socket tracks its own send limit
            let buffer = SharedBuf::new(usize::MAX);
            let buffer = Arc::new(AtomicRefCell::new(buffer));

            let mut common = NetlinkSocketCommon {
                buffer,
                send_limit: NETLINK_SOCKET_DEFAULT_BUFFER_SIZE,
                sent_len: 0,
                event_source: StateEventSource::new(),
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
        self.common.supports_sa_restart()
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

    pub fn sendmsg(
        socket: &Arc<AtomicRefCell<Self>>,
        args: SendmsgArgs,
        mem: &mut MemoryManager,
        cb_queue: &mut CallbackQueue,
    ) -> Result<libc::ssize_t, SyscallError> {
        let socket_ref = &mut *socket.borrow_mut();
        socket_ref
            .protocol_state
            .sendmsg(&mut socket_ref.common, socket, args, mem, cb_queue)
    }

    pub fn recvmsg(
        socket: &Arc<AtomicRefCell<Self>>,
        args: RecvmsgArgs,
        mem: &mut MemoryManager,
        cb_queue: &mut CallbackQueue,
    ) -> Result<RecvmsgReturn, SyscallError> {
        let socket_ref = &mut *socket.borrow_mut();
        socket_ref
            .protocol_state
            .recvmsg(&mut socket_ref.common, socket, args, mem, cb_queue)
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

    pub fn add_listener(
        &mut self,
        monitoring: FileState,
        filter: StateListenerFilter,
        notify_fn: impl Fn(FileState, FileState, &mut CallbackQueue) + Send + Sync + 'static,
    ) -> Handle<(FileState, FileState)> {
        self.common
            .event_source
            .add_listener(monitoring, filter, notify_fn)
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

    fn sendmsg(
        &mut self,
        common: &mut NetlinkSocketCommon,
        socket: &Arc<AtomicRefCell<NetlinkSocket>>,
        args: SendmsgArgs,
        mem: &mut MemoryManager,
        cb_queue: &mut CallbackQueue,
    ) -> Result<libc::ssize_t, SyscallError> {
        match self {
            Self::Initial(x) => x.as_mut().unwrap()
                .sendmsg(common, socket, args, mem, cb_queue),
            Self::Closed(x) => x.as_mut().unwrap()
                .sendmsg(common, socket, args, mem, cb_queue),
        }
    }

    fn recvmsg(
        &mut self,
        common: &mut NetlinkSocketCommon,
        socket: &Arc<AtomicRefCell<NetlinkSocket>>,
        args: RecvmsgArgs,
        mem: &mut MemoryManager,
        cb_queue: &mut CallbackQueue,
    ) -> Result<RecvmsgReturn, SyscallError> {
        match self {
            Self::Initial(x) => x.as_mut().unwrap()
                .recvmsg(common, socket, args, mem, cb_queue),
            Self::Closed(x) => x.as_mut().unwrap()
                .recvmsg(common, socket, args, mem, cb_queue),
        }
    }
}

impl InitialState {
    fn refresh_file_state(&self, common: &mut NetlinkSocketCommon, cb_queue: &mut CallbackQueue) {
        let mut new_state = FileState::ACTIVE;

        {
            let buffer = common.buffer.borrow();

            new_state.set(FileState::READABLE, buffer.has_data());
            new_state.set(FileState::WRITABLE, common.sent_len < common.send_limit);
        }

        common.copy_state(/* mask= */ FileState::all(), new_state, cb_queue);
    }

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

    fn sendmsg(
        &mut self,
        common: &mut NetlinkSocketCommon,
        socket: &Arc<AtomicRefCell<NetlinkSocket>>,
        args: SendmsgArgs,
        mem: &mut MemoryManager,
        cb_queue: &mut CallbackQueue,
    ) -> Result<libc::ssize_t, SyscallError> {
        if !args.control_ptr.ptr().is_null() {
            log::debug!("Netlink sockets don't yet support control data for sendmsg()");
            return Err(Errno::EINVAL.into());
        }

        let Some(addr) = args.addr else {
            log::warn!("Attempted to send in netlink socket without destination address");
            return Err(Errno::EINVAL.into());
        };
        let Some(addr) = addr.as_netlink() else {
            log::warn!("Attempted to send to non-netlink address {:?}", args.addr);
            return Err(Errno::EINVAL.into());
        };
        // Sending to non-kernel address is not supported
        if addr.pid() != 0 {
            log::warn!("Attempted to send to non-kernel netlink address {:?}", addr);
            return Err(Errno::EINVAL.into());
        }
        // Sending to groups is not supported
        if addr.groups() != 0 {
            log::warn!("Attempted to send to netlink groups {:?}", addr);
            return Err(Errno::EINVAL.into());
        }

        let rv = common.sendmsg(socket, args.iovs, args.flags, mem, cb_queue)?;

        self.refresh_file_state(common, cb_queue);

        Ok(rv.try_into().unwrap())
    }

    fn recvmsg(
        &mut self,
        common: &mut NetlinkSocketCommon,
        socket: &Arc<AtomicRefCell<NetlinkSocket>>,
        args: RecvmsgArgs,
        mem: &mut MemoryManager,
        cb_queue: &mut CallbackQueue,
    ) -> Result<RecvmsgReturn, SyscallError> {
        if !args.control_ptr.ptr().is_null() {
            log::debug!("Netlink sockets don't yet support control data for recvmsg()");
            return Err(Errno::EINVAL.into());
        }

        let mut packet_buffer = Vec::new();
        let (_rv, _num_removed_from_buf) =
            common.recvmsg(socket, &mut packet_buffer, args.flags, mem, cb_queue)?;
        self.refresh_file_state(common, cb_queue);

        let mut writer = IoVecWriter::new(args.iovs, mem);

        // We set the source address as the netlink address of the kernel
        let src_addr = SockaddrStorage::from_netlink(&NetlinkAddr::new(0, 0));

        // This closure generates the error message and its RecvmsgReturn
        let get_errmsg = |seq: Option<u32>| {
            let errmsg = {
                let len = None;
                let nl_type = Nlmsg::Error;
                let flags = NlmFFlags::empty();
                let pid = None;
                let payload = NlPayload::<Nlmsg, ()>::Empty;
                Nlmsghdr::new(len, nl_type, flags, seq, pid, payload)
            };

            let errmsg_buffer = {
                let mut errmsg_buffer = Cursor::new(Vec::new());
                errmsg.to_bytes(&mut errmsg_buffer).unwrap();
                errmsg_buffer.into_inner()
            };

            let ret = RecvmsgReturn {
                return_val: errmsg_buffer.len().try_into().unwrap(),
                addr: Some(src_addr),
                msg_flags: 0,
                control_len: 0,
            };

            (errmsg_buffer, ret)
        };

        let Ok(nlmsg) = Nlmsghdr::<Rtm, Ifaddrmsg>::from_bytes(&mut Cursor::new(&packet_buffer)) else {
            log::warn!("Failed to deserialize the netlink packet");
            // Since we don't know the sequence number yet, we don't put it here
            let (errmsg_buffer, ret) = get_errmsg(None);
            writer.write(&errmsg_buffer)?;
            return Ok(ret);
        };

        // Use the same sequence number as the request
        let (errmsg_buffer, errmsg_ret) = get_errmsg(Some(nlmsg.nl_seq));

        if nlmsg.nl_type != Rtm::Getaddr {
            log::warn!("Unsupported message type (only RTM_GETADDR is supported)");
            writer.write(&errmsg_buffer)?;
            return Ok(errmsg_ret);
        }

        // This will never panic because we already checked that the nl_type is RTM_GETADDR
        let ifaddrmsg = nlmsg.get_payload().unwrap();

        // The only supported interface address family is AF_INET
        if ifaddrmsg.ifa_family != RtAddrFamily::Unspecified && ifaddrmsg.ifa_family != RtAddrFamily::Inet {
            log::warn!("Unsupported ifa_family (only AF_UNSPEC and AF_INET are supported)");
            writer.write(&errmsg_buffer)?;
            return Ok(errmsg_ret);
        }

        // The rest of the fields are unsupported. We limit only the interest to the zero values
        if ifaddrmsg.ifa_prefixlen != 0 || ifaddrmsg.ifa_flags != IfaFFlags::empty()
            || ifaddrmsg.ifa_index != 0 || ifaddrmsg.ifa_scope != libc::c_uchar::from(RtScope::Universe) {
            log::warn!("Unsupported ifa_prefixlen, ifa_flags, ifa_scope, or ifa_index (they have to be 0)");
            writer.write(&errmsg_buffer)?;
            return Ok(errmsg_ret);
        }

        // The struct used to describe the network interface
        struct Interface {
            address: Ipv4Addr,
            label: String,
            prefix_len: u8,
            scope: RtScope,
            index: libc::c_int,
        }

        // Get the IP address of the host
        let default_ip = Worker::with_active_host(|host| host.default_ip()).unwrap();
        // List of both interfaces of the host
        let interfaces = [
            // All the configurations are the same as in the syscall of getifaddrs
            Interface {
                address: Ipv4Addr::LOCALHOST,
                label: String::from("lo"),
                prefix_len: 8,
                scope: RtScope::Host,
                index: 1,
            },
            Interface {
                address: default_ip,
                label: String::from("eth0"),
                prefix_len: 24,
                scope: RtScope::Universe,
                index: 2,
            },
        ];

        let mut buffer = Cursor::new(Vec::new());
        // Send the interface addresses
        for interface in interfaces {
            let address = interface.address.octets();
            let broadcast = if interface.prefix_len == 32 {
                address
            } else {
                Ipv4Addr::from(
                    (0xffff_ffff_u32) >> u32::from(interface.prefix_len) | u32::from(interface.address),
                )
                .octets()
            };
            let mut label = Vec::from(interface.label.as_bytes());
            label.push(0); // Null-terminate
            // List of attribtes sent with the response for the current interface
            let attrs = [
                // I don't know the difference between IFA_ADDRESS and IFA_LOCAL. However, Linux
                // provides the same address for both attributes, so I do the same.
                // Run `strace ip addr` to see.
                Rtattr::new(None, Ifa::Address, Buffer::from(&address[..])).unwrap(),
                Rtattr::new(None, Ifa::Local, Buffer::from(&address[..])).unwrap(),
                Rtattr::new(None, Ifa::Broadcast, Buffer::from(&broadcast[..])).unwrap(),
                Rtattr::new(None, Ifa::Label, Buffer::from(label)).unwrap(),
            ];
            let ifaddrmsg = Ifaddrmsg {
                ifa_family: RtAddrFamily::Inet,
                ifa_prefixlen: interface.prefix_len,
                // IFA_F_PERMANENT is used to indicate that the address is permanent
                ifa_flags: IfaFFlags::new(&[IfaF::Permanent]),
                ifa_scope: libc::c_uchar::from(interface.scope),
                ifa_index: interface.index,
                rtattrs: RtBuffer::from_iter(attrs),
            };
            let nlmsg = {
                let len = None;
                let nl_type = Rtm::Newaddr;
                // The NLM_F_MULTI flag is used to indicate that we will send multiple messages
                let flags = NlmFFlags::new(&[NlmF::Multi]);
                // Use the same sequence number as the request
                let seq = Some(nlmsg.nl_seq);
                let pid = None;
                let payload = NlPayload::Payload(ifaddrmsg);
                Nlmsghdr::new(len, nl_type, flags, seq, pid, payload)
            };
            nlmsg.to_bytes(&mut buffer).unwrap();
        }
        // After sending the messages with the NLM_F_MULTI flag set, we need to send the NLMSG_DONE message
        let done_msg = {
            let len = None;
            let nl_type = Nlmsg::Done;
            let flags = NlmFFlags::new(&[NlmF::Multi]);
            // Use the same sequence number as the request
            let seq = Some(nlmsg.nl_seq);
            let pid = None;
            // Linux also emits 4 bytes of zeroes after the header. See `strace ip addr`
            let payload: NlPayload<Nlmsg, u32> = NlPayload::Payload(0);
            Nlmsghdr::new(len, nl_type, flags, seq, pid, payload)
        };
        done_msg.to_bytes(&mut buffer).unwrap();

        let buffer = buffer.into_inner();
        writer.write(&buffer)?;

        Ok(RecvmsgReturn {
            return_val: buffer.len().try_into().unwrap(),
            addr: Some(src_addr),
            msg_flags: 0,
            control_len: 0,
        })
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

    fn sendmsg(
        &mut self,
        _common: &mut NetlinkSocketCommon,
        _socket: &Arc<AtomicRefCell<NetlinkSocket>>,
        _args: SendmsgArgs,
        _mem: &mut MemoryManager,
        _cb_queue: &mut CallbackQueue,
    ) -> Result<libc::ssize_t, SyscallError> {
        // We follow the same approach as UnixSocket
        log::warn!("sendmsg() while in state {}", std::any::type_name::<Self>());
        Err(Errno::EOPNOTSUPP.into())
    }

    fn recvmsg(
        &mut self,
        _common: &mut NetlinkSocketCommon,
        _socket: &Arc<AtomicRefCell<NetlinkSocket>>,
        _args: RecvmsgArgs,
        _mem: &mut MemoryManager,
        _cb_queue: &mut CallbackQueue,
    ) -> Result<RecvmsgReturn, SyscallError> {
        // We follow the same approach as UnixSocket
        log::warn!("recvmsg() while in state {}", std::any::type_name::<Self>());
        Err(Errno::EOPNOTSUPP.into())
    }
}

/// Common data and functionality that is useful for all states.
struct NetlinkSocketCommon {
    buffer: Arc<AtomicRefCell<SharedBuf>>,
    /// The max number of "in flight" bytes (sent but not yet read from the receiving socket).
    send_limit: u64,
    /// The number of "in flight" bytes.
    sent_len: u64,
    event_source: StateEventSource,
    state: FileState,
    status: FileStatus,
    // should only be used by `OpenFile` to make sure there is only ever one `OpenFile` instance for
    // this file
    has_open_file: bool,
}

impl NetlinkSocketCommon {
    pub fn supports_sa_restart(&self) -> bool {
        true
    }

    pub fn sendmsg(
        &mut self,
        socket: &Arc<AtomicRefCell<NetlinkSocket>>,
        iovs: &[IoVec],
        flags: libc::c_int,
        mem: &mut MemoryManager,
        cb_queue: &mut CallbackQueue,
    ) -> Result<usize, SyscallError> {
        // MSG_NOSIGNAL is a no-op, since netlink sockets are not stream-oriented.
        // Ignore the MSG_TRUNC flag since it doesn't do anything when sending.
        let supported_flags = MsgFlags::MSG_DONTWAIT | MsgFlags::MSG_NOSIGNAL | MsgFlags::MSG_TRUNC;

        // if there's a flag we don't support, it's probably best to raise an error rather than do
        // the wrong thing
        let Some(mut flags) = MsgFlags::from_bits(flags) else {
            log::warn!("Unrecognized send flags: {:#b}", flags);
            return Err(Errno::EINVAL.into());
        };
        if flags.intersects(!supported_flags) {
            log::warn!("Unsupported send flags: {:?}", flags);
            return Err(Errno::EINVAL.into());
        }

        if self.status.contains(FileStatus::NONBLOCK) {
            flags.insert(MsgFlags::MSG_DONTWAIT);
        }

        // run in a closure so that an early return doesn't return from the syscall handler
        let result = (|| {
            let len = iovs.iter().map(|x| x.len).sum::<libc::size_t>();

            // we keep track of the send buffer size manually, since the netlink socket buffers all
            // have usize::MAX length
            let space_available = self
                .send_limit
                .saturating_sub(self.sent_len)
                .try_into()
                .unwrap();

            if space_available == 0 {
                return Err(Errno::EAGAIN);
            }

            if len > space_available {
                if len <= self.send_limit.try_into().unwrap() {
                    // we can send this when the buffer has more space available
                    return Err(Errno::EAGAIN);
                } else {
                    // we could never send this message
                    return Err(Errno::EMSGSIZE);
                }
            }

            let reader = IoVecReader::new(iovs, mem);
            let reader = reader.take(len.try_into().unwrap());

            // send the packet directly to the buffer of the socket so that it will be
            // processed when the socket is read.
            self.buffer.borrow_mut()
                .write_packet(reader, len, cb_queue)
                .map_err(|e| e.try_into().unwrap())?;

            // if we successfully sent bytes, update the sent count
            self.sent_len += u64::try_from(len).unwrap();
            Ok(len)
        })();

        // if the syscall would block and we don't have the MSG_DONTWAIT flag
        if result.as_ref().err() == Some(&Errno::EWOULDBLOCK)
            && !flags.contains(MsgFlags::MSG_DONTWAIT)
        {
            return Err(SyscallError::new_blocked(
                File::Socket(Socket::Netlink(socket.clone())),
                FileState::WRITABLE,
                self.supports_sa_restart(),
            ));
        }

        Ok(result?)
    }

    pub fn recvmsg<W: Write>(
        &mut self,
        socket: &Arc<AtomicRefCell<NetlinkSocket>>,
        dst: W,
        flags: libc::c_int,
        _mem: &mut MemoryManager,
        cb_queue: &mut CallbackQueue,
    ) -> Result<(usize, usize), SyscallError> {
        let supported_flags = MsgFlags::MSG_DONTWAIT | MsgFlags::MSG_TRUNC;

        // if there's a flag we don't support, it's probably best to raise an error rather than do
        // the wrong thing
        let Some(mut flags) = MsgFlags::from_bits(flags) else {
            log::warn!("Unrecognized recv flags: {:#b}", flags);
            return Err(Errno::EINVAL.into());
        };
        if flags.intersects(!supported_flags) {
            log::warn!("Unsupported recv flags: {:?}", flags);
            return Err(Errno::EINVAL.into());
        }

        if self.status.contains(FileStatus::NONBLOCK) {
            flags.insert(MsgFlags::MSG_DONTWAIT);
        }

        // run in a closure so that an early return doesn't return from the syscall handler
        let result = (|| {
            let mut buffer = self.buffer.borrow_mut();

            // the read would block if the buffer has no data
            if !buffer.has_data() {
                return Err(Errno::EWOULDBLOCK);
            }

            let (num_copied, num_removed_from_buf) = buffer
                .read(dst, cb_queue)
                .map_err(|e| e.try_into().unwrap())?;

            if flags.contains(MsgFlags::MSG_TRUNC) {
                // return the total size of the message, not the number of bytes we read
                Ok((num_removed_from_buf, num_removed_from_buf))
            } else {
                Ok((num_copied, num_removed_from_buf))
            }
        })();

        // if the syscall would block and we don't have the MSG_DONTWAIT flag
        if result.as_ref().err() == Some(&Errno::EWOULDBLOCK)
            && !flags.contains(MsgFlags::MSG_DONTWAIT)
        {
            return Err(SyscallError::new_blocked(
                File::Socket(Socket::Netlink(socket.clone())),
                FileState::READABLE,
                self.supports_sa_restart(),
            ));
        }

        Ok(result?)
    }

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
