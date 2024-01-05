//! Test sending and receiving.

use std::cell::{Ref, RefCell};
use std::rc::Rc;

use bytes::Bytes;

use crate::tests::{establish_helper, Host, Scheduler, TcpSocket, TestEnvState};
use crate::{Ipv4Header, Payload, Shutdown, TcpFlags, TcpHeader, TcpState};

#[test]
fn test_send_recv() {
    let scheduler = Scheduler::new();
    let mut host = Host::new();

    /// Helper to get the state from a socket.
    fn s(tcp: &Rc<RefCell<TcpSocket>>) -> Ref<TcpState<TestEnvState>> {
        Ref::map(tcp.borrow(), |x| x.tcp_state())
    }

    // get an established tcp socket
    let tcp = establish_helper(&scheduler, &mut host);

    // send on the socket
    TcpSocket::sendmsg(&tcp, &b"hello"[..], 5).unwrap();

    // check the packet sent by the socket
    let (_, payload) = scheduler.pop_packet().unwrap();
    assert_eq!(payload.concat()[..], b"hello"[..]);

    // send a packet to the socket
    let header = TcpHeader {
        ip: Ipv4Header {
            src: "5.6.7.8".parse().unwrap(),
            dst: host.ip_addr,
        },
        flags: TcpFlags::empty(),
        src_port: 20,
        dst_port: 10,
        seq: 1,
        ack: 6,
        window_size: 10000,
        selective_acks: None,
        window_scale: None,
        timestamp: None,
        timestamp_echo: None,
    };
    let message = b"world";
    let pushed_len = tcp
        .borrow_mut()
        .push_in_packet(&header, Bytes::from(&message[..]).into());
    assert_eq!(pushed_len, message.len());

    // recv on the socket
    let mut recv_buf = vec![0; 5];
    TcpSocket::recvmsg(&tcp, &mut recv_buf[..], 5).unwrap();
    assert_eq!(recv_buf, b"world");
}

/// This test tries to make sure that an acknowledgement sent while the socket's usable send window
/// (send window excluding in-flight not-acked data) is empty uses the correct sequence number.
/// (This test doesn't require that the usable send window is actually empty, just that it's empty
/// enough that TCP decides it can't/shouldn't send more payload packets.)
#[test]
// this test causes miri to time-out
#[cfg_attr(miri, ignore)]
fn test_ack_with_empty_usable_send_window() {
    let scheduler = Scheduler::new();
    let mut host = Host::new();

    /// Helper to get the state from a socket.
    fn s(tcp: &Rc<RefCell<TcpSocket>>) -> Ref<TcpState<TestEnvState>> {
        Ref::map(tcp.borrow(), |x| x.tcp_state())
    }

    // get an established tcp socket
    let tcp = establish_helper(&scheduler, &mut host);

    // send on the socket until the send buffer has more data than the usable send window allows
    let max_in_flight = s(&tcp)
        .as_established()
        .unwrap()
        .connection
        .send_window()
        .len();

    let mut buffered = 0;
    while buffered <= max_in_flight as usize {
        buffered += TcpSocket::sendmsg(&tcp, &b"hello"[..], 5).unwrap();
    }

    // read all of the packets it sent and make sure the sequence number is consistent
    let mut next_seq = None;
    while let Some((header, payload)) = scheduler.pop_packet() {
        if next_seq.is_none() {
            next_seq = Some(header.seq as usize);
        }

        let next_seq = next_seq.as_mut().unwrap();
        assert_eq!(*next_seq, header.seq as usize);
        *next_seq += payload.len() as usize;
    }

    // send a packet with a payload to trigger an acknowledgement
    let header = TcpHeader {
        ip: Ipv4Header {
            src: "5.6.7.8".parse().unwrap(),
            dst: host.ip_addr,
        },
        flags: TcpFlags::empty(),
        src_port: 20,
        dst_port: 10,
        seq: 1,
        ack: 1,
        window_size: 10000,
        selective_acks: None,
        window_scale: None,
        timestamp: None,
        timestamp_echo: None,
    };
    let message = b"world";
    let pushed_len = tcp
        .borrow_mut()
        .push_in_packet(&header, Bytes::from(&message[..]).into());
    assert_eq!(pushed_len, message.len());

    // check the packet sent by the socket
    let (header, payload) = scheduler.pop_packet().unwrap();

    // should have acked the packet we sent above using the correct sequence number
    assert!(header.flags.contains(TcpFlags::ACK));
    assert_eq!(header.ack, 6);
    assert_eq!(header.seq as usize, next_seq.unwrap());

    // we haven't acked any of the data it sent, so its usable send window should still be empty and
    // should not have sent any data
    assert!(payload.is_empty());
}

#[test]
fn test_coalesce_send() {
    let scheduler = Scheduler::new();
    let mut host = Host::new();

    /// Helper to get the state from a socket.
    fn s(tcp: &Rc<RefCell<TcpSocket>>) -> Ref<TcpState<TestEnvState>> {
        Ref::map(tcp.borrow(), |x| x.tcp_state())
    }

    // get an established tcp socket
    let tcp = establish_helper(&scheduler, &mut host);

    // PART 1: We test with `collect_packets == true`, which will immediately pop a packet as soon
    // as its ready. We expect two `sendmsg` calls to result in two packets.

    // write two small buffers to the socket
    TcpSocket::sendmsg(&tcp, &b"hello"[..], 5).unwrap();
    TcpSocket::sendmsg(&tcp, &b"world"[..], 5).unwrap();

    // check that both buffers were sent in separate packets
    let (_, payload) = scheduler.pop_packet().unwrap();
    assert_eq!(payload.concat()[..], b"hello"[..]);
    let (_, payload) = scheduler.pop_packet().unwrap();
    assert_eq!(payload.concat()[..], b"world"[..]);

    // PART 2: We test with `collect_packets == false`, which will not pop a packet until we later
    // re-enable `collect_packets`. We expect two `sendmsg` calls to result in a single packet.

    tcp.borrow_mut().collect_packets(false);

    // write two small buffers to the socket
    TcpSocket::sendmsg(&tcp, &b"hello"[..], 5).unwrap();
    TcpSocket::sendmsg(&tcp, &b"world"[..], 5).unwrap();

    tcp.borrow_mut().collect_packets(true);

    // check that both buffers were sent in a single packet
    let (_, payload) = scheduler.pop_packet().unwrap();
    assert_eq!(payload.concat()[..], b"helloworld"[..]);
}

#[test]
fn test_coalesce_recv() {
    let scheduler = Scheduler::new();
    let mut host = Host::new();

    /// Helper to get the state from a socket.
    fn s(tcp: &Rc<RefCell<TcpSocket>>) -> Ref<TcpState<TestEnvState>> {
        Ref::map(tcp.borrow(), |x| x.tcp_state())
    }

    // get an established tcp socket
    let tcp = establish_helper(&scheduler, &mut host);

    // send two packets to the socket
    let header = TcpHeader {
        ip: Ipv4Header {
            src: "5.6.7.8".parse().unwrap(),
            dst: host.ip_addr,
        },
        flags: TcpFlags::empty(),
        src_port: 20,
        dst_port: 10,
        seq: 1,
        ack: 6,
        window_size: 10000,
        selective_acks: None,
        window_scale: None,
        timestamp: None,
        timestamp_echo: None,
    };
    let message = b"hello";
    let pushed_len = tcp
        .borrow_mut()
        .push_in_packet(&header, Bytes::from(&message[..]).into());
    assert_eq!(pushed_len, message.len());

    let header = TcpHeader {
        ip: Ipv4Header {
            src: "5.6.7.8".parse().unwrap(),
            dst: host.ip_addr,
        },
        flags: TcpFlags::empty(),
        src_port: 20,
        dst_port: 10,
        seq: 6,
        ack: 6,
        window_size: 10000,
        selective_acks: None,
        window_scale: None,
        timestamp: None,
        timestamp_echo: None,
    };
    let message = b"world";
    let pushed_len = tcp
        .borrow_mut()
        .push_in_packet(&header, Bytes::from(&message[..]).into());
    assert_eq!(pushed_len, message.len());

    // recv on the socket
    let mut recv_buf = vec![0; 10];
    TcpSocket::recvmsg(&tcp, &mut recv_buf[..], 10).unwrap();
    assert_eq!(recv_buf, b"helloworld");
}

#[test]
fn test_close_with_non_empty_recv_buffer() {
    let scheduler = Scheduler::new();
    let mut host = Host::new();

    /// Helper to get the state from a socket.
    fn s(tcp: &Rc<RefCell<TcpSocket>>) -> Ref<TcpState<TestEnvState>> {
        Ref::map(tcp.borrow(), |x| x.tcp_state())
    }

    // get an established tcp socket
    let tcp = establish_helper(&scheduler, &mut host);

    // send a payload packet to the socket
    let header = TcpHeader {
        ip: Ipv4Header {
            src: "5.6.7.8".parse().unwrap(),
            dst: host.ip_addr,
        },
        flags: TcpFlags::empty(),
        src_port: 20,
        dst_port: 10,
        seq: 1,
        ack: 1,
        window_size: 10000,
        selective_acks: None,
        window_scale: None,
        timestamp: None,
        timestamp_echo: None,
    };
    let message = b"hello";
    let pushed_len = tcp
        .borrow_mut()
        .push_in_packet(&header, Bytes::from(&message[..]).into());
    assert_eq!(pushed_len, message.len());

    // check that our payload packet was acknowledged
    let (header, _) = scheduler.pop_packet().unwrap();
    assert!(header.flags.contains(TcpFlags::ACK));

    // close the socket, which should send a RST response since there is data in the receive buffer
    tcp.borrow_mut().close().unwrap();
    assert!(s(&tcp).as_closed().is_some());

    // check that a RST packet was sent by the socket
    let (header, _) = scheduler.pop_packet().unwrap();
    assert!(header.flags.contains(TcpFlags::RST));
}

#[test]
fn test_recv_after_shutdown_both() {
    let scheduler = Scheduler::new();
    let mut host = Host::new();

    /// Helper to get the state from a socket.
    fn s(tcp: &Rc<RefCell<TcpSocket>>) -> Ref<TcpState<TestEnvState>> {
        Ref::map(tcp.borrow(), |x| x.tcp_state())
    }

    // get an established tcp socket
    let tcp = establish_helper(&scheduler, &mut host);

    tcp.borrow_mut().collect_packets(false);

    // send a payload packet to the socket
    let header = TcpHeader {
        ip: Ipv4Header {
            src: "5.6.7.8".parse().unwrap(),
            dst: host.ip_addr,
        },
        flags: TcpFlags::empty(),
        src_port: 20,
        dst_port: 10,
        seq: 1,
        ack: 1,
        window_size: 10000,
        selective_acks: None,
        window_scale: None,
        timestamp: None,
        timestamp_echo: None,
    };
    let message = b"hello";
    let pushed_len = tcp
        .borrow_mut()
        .push_in_packet(&header, Bytes::from(&message[..]).into());
    assert_eq!(pushed_len, message.len());

    tcp.borrow_mut().shutdown(Shutdown::Both).unwrap();
    assert!(s(&tcp).as_fin_wait_one().is_some());

    // group the payload acknowledgement and FIN into one packet
    tcp.borrow_mut().collect_packets(true);

    // check that a FIN packet was sent by the socket
    let (header, _) = scheduler.pop_packet().unwrap();
    assert!(header.flags.contains(TcpFlags::FIN));

    // should still be able to recv old data on the socket
    let mut recv_buf = vec![0; 2];
    TcpSocket::recvmsg(&tcp, &mut recv_buf[..], 2).unwrap();
    assert_eq!(recv_buf, b"he");

    // send a FIN packet to the socket
    let header = TcpHeader {
        ip: Ipv4Header {
            src: "5.6.7.8".parse().unwrap(),
            dst: host.ip_addr,
        },
        flags: TcpFlags::FIN | TcpFlags::ACK,
        src_port: 20,
        dst_port: 10,
        seq: 6,
        ack: 2,
        window_size: 10000,
        selective_acks: None,
        window_scale: None,
        timestamp: None,
        timestamp_echo: None,
    };
    tcp.borrow_mut().push_in_packet(&header, Payload::default());

    assert!(s(&tcp).as_time_wait().is_some());

    // should still be able to recv old data on the socket
    let mut recv_buf = vec![0; 2];
    TcpSocket::recvmsg(&tcp, &mut recv_buf[..], 2).unwrap();
    assert_eq!(recv_buf, b"ll");

    // check that our FIN was acknowledged
    let (header, _) = scheduler.pop_packet().unwrap();
    assert!(header.flags.contains(TcpFlags::ACK));

    assert!(s(&tcp).as_time_wait().is_some());

    // advance past the time-wait period
    scheduler.advance(std::time::Duration::from_secs(120));
    assert!(s(&tcp).as_closed().is_some());

    // should still be able to recv old data on the socket
    let mut recv_buf = vec![0; 2];
    TcpSocket::recvmsg(&tcp, &mut recv_buf[..], 2).unwrap();
    assert_eq!(recv_buf, b"o\0");
}

#[test]
fn test_incoming_payload_after_close() {
    let scheduler = Scheduler::new();
    let mut host = Host::new();

    /// Helper to get the state from a socket.
    fn s(tcp: &Rc<RefCell<TcpSocket>>) -> Ref<TcpState<TestEnvState>> {
        Ref::map(tcp.borrow(), |x| x.tcp_state())
    }

    // get an established tcp socket
    let tcp = establish_helper(&scheduler, &mut host);

    // close the socket
    tcp.borrow_mut().close().unwrap();
    assert!(s(&tcp).as_fin_wait_one().is_some());

    // check that a FIN was sent
    let (header, _) = scheduler.pop_packet().unwrap();
    assert!(header.flags.contains(TcpFlags::FIN));

    // send a payload packet to the socket
    let header = TcpHeader {
        ip: Ipv4Header {
            src: "5.6.7.8".parse().unwrap(),
            dst: host.ip_addr,
        },
        flags: TcpFlags::empty(),
        src_port: 20,
        dst_port: 10,
        seq: 1,
        ack: 1,
        window_size: 10000,
        selective_acks: None,
        window_scale: None,
        timestamp: None,
        timestamp_echo: None,
    };
    let message = b"hello";
    let pushed_len = tcp
        .borrow_mut()
        .push_in_packet(&header, Bytes::from(&message[..]).into());
    // no data is pushed because the socket is already closed
    assert_eq!(pushed_len, 0);

    assert!(s(&tcp).as_closed().is_some());

    // check that a RST packet was sent by the socket in response to the payload packet
    let (header, _) = scheduler.pop_packet().unwrap();
    assert!(header.flags.contains(TcpFlags::RST));

    // try to recv on the socket, but there should be no data and we should receive an EOF
    // (typically on linux we'd receive an ECONNRESET for the first read, but we don't use the error
    // state in our test socket wrapper)
    let mut recv_buf = vec![0; 5];
    assert_eq!(TcpSocket::recvmsg(&tcp, &mut recv_buf[..], 5), Ok(0));
}

#[test]
fn test_incoming_payload_after_shutdown_read() {
    let scheduler = Scheduler::new();
    let mut host = Host::new();

    /// Helper to get the state from a socket.
    fn s(tcp: &Rc<RefCell<TcpSocket>>) -> Ref<TcpState<TestEnvState>> {
        Ref::map(tcp.borrow(), |x| x.tcp_state())
    }

    // get an established tcp socket
    let tcp = establish_helper(&scheduler, &mut host);

    // on Linux you would need to `shutdown(Both)` for a RST to be sent when payload data is
    // received, but in this TCP library only `shutdown(Read)` is required
    tcp.borrow_mut().shutdown(Shutdown::Read).unwrap();
    assert!(s(&tcp).as_established().is_some());

    // check that no packets were sent by the socket
    assert!(scheduler.pop_packet().is_none());

    // send a payload packet to the socket
    let header = TcpHeader {
        ip: Ipv4Header {
            src: "5.6.7.8".parse().unwrap(),
            dst: host.ip_addr,
        },
        flags: TcpFlags::empty(),
        src_port: 20,
        dst_port: 10,
        seq: 1,
        ack: 1,
        window_size: 10000,
        selective_acks: None,
        window_scale: None,
        timestamp: None,
        timestamp_echo: None,
    };
    let message = b"hello";
    let pushed_len = tcp
        .borrow_mut()
        .push_in_packet(&header, Bytes::from(&message[..]).into());
    // no data is pushed because the socket is already shutdown on read
    assert_eq!(pushed_len, 0);

    assert!(s(&tcp).as_closed().is_some());

    // check that a RST packet was sent by the socket in response to the payload packet
    let (header, _) = scheduler.pop_packet().unwrap();
    assert!(header.flags.contains(TcpFlags::RST));

    // try to recv on the socket, but there should be no data and we should receive an EOF
    // (typically on linux we'd receive an ECONNRESET for the first read, but we don't use the error
    // state in our test socket wrapper)
    let mut recv_buf = vec![0; 5];
    assert_eq!(TcpSocket::recvmsg(&tcp, &mut recv_buf[..], 5), Ok(0));
}
