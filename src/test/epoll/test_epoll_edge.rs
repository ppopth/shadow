use std::time::Duration;

use nix::sys::epoll::{self, EpollFlags};
use nix::unistd;

use test_utils::socket_utils::{socket_init_helper, SocketInitMethod};
use test_utils::{ensure_ord, set, ShadowTest, TestEnvironment};

#[derive(Debug)]
struct WaiterResult {
    duration: Duration,
    epoll_res: nix::Result<usize>,
    events: Vec<epoll::EpollEvent>,
}

fn do_epoll_wait(epoll_fd: i32, timeout: Duration, do_read: bool) -> WaiterResult {
    let mut events = Vec::new();
    events.resize(10, epoll::EpollEvent::empty());

    let t0 = std::time::Instant::now();

    let res = epoll::epoll_wait(
        epoll_fd,
        &mut events,
        timeout.as_millis().try_into().unwrap(),
    );

    let t1 = std::time::Instant::now();

    events.resize(res.unwrap_or(0), epoll::EpollEvent::empty());

    if do_read {
        for ev in &events {
            let fd = ev.data() as i32;
            // we don't care if the read is successful or not (another thread may have already read)
            let _ = unistd::read(fd, &mut [0]);
        }
    }

    WaiterResult {
        duration: t1.duration_since(t0),
        epoll_res: res,
        events,
    }
}

fn test_multi_write() -> anyhow::Result<()> {
    let (fd_client, fd_server) = socket_init_helper(
        SocketInitMethod::Inet,
        libc::SOCK_STREAM,
        libc::SOCK_NONBLOCK,
        /* bind_client = */ false,
    );
    let epollfd = epoll::epoll_create()?;

    test_utils::run_and_close_fds(&[epollfd, fd_client, fd_server], || {
        let mut event = epoll::EpollEvent::new(EpollFlags::EPOLLET | EpollFlags::EPOLLIN, 0);
        epoll::epoll_ctl(
            epollfd,
            epoll::EpollOp::EpollCtlAdd,
            fd_server,
            Some(&mut event),
        )?;

        let timeout = Duration::from_millis(100);

        let thread = std::thread::spawn(move || {
            vec![
                do_epoll_wait(epollfd, timeout, /* do_read= */ false),
                do_epoll_wait(epollfd, timeout, /* do_read= */ false),
                // The last one is supposed to timeout.
                do_epoll_wait(epollfd, timeout, /* do_read= */ false),
            ]
        });

        // Wait for readers to block.
        std::thread::sleep(timeout / 3);

        // Make the read-end readable.
        unistd::write(fd_client, &[0])?;

        // Wait again and make the read-end readable again.
        std::thread::sleep(timeout / 3);
        unistd::write(fd_client, &[0])?;

        let results = thread.join().unwrap();

        // The first two waits should have received the event
        for res in &results[..2] {
            ensure_ord!(res.epoll_res, ==, Ok(1));
            ensure_ord!(res.duration, <, timeout);
            ensure_ord!(res.events[0], ==, epoll::EpollEvent::new(EpollFlags::EPOLLIN, 0));
        }

        // The last wait should have timed out with no events received.
        ensure_ord!(results[2].epoll_res, ==, Ok(0));
        ensure_ord!(results[2].duration, >=, timeout);

        Ok(())
    })
}

fn test_write_then_read() -> anyhow::Result<()> {
    let (fd_client, fd_server) = socket_init_helper(
        SocketInitMethod::Inet,
        libc::SOCK_STREAM,
        libc::SOCK_NONBLOCK,
        /* bind_client = */ false,
    );
    let epollfd = epoll::epoll_create()?;

    test_utils::run_and_close_fds(&[epollfd, fd_client, fd_server], || {
        let mut event = epoll::EpollEvent::new(EpollFlags::EPOLLET | EpollFlags::EPOLLIN, 0);
        epoll::epoll_ctl(
            epollfd,
            epoll::EpollOp::EpollCtlAdd,
            fd_server,
            Some(&mut event),
        )?;

        let timeout = Duration::from_millis(100);

        let thread = std::thread::spawn(move || {
            vec![
                do_epoll_wait(epollfd, timeout, /* do_read= */ false),
                // The second one is supposed to timeout.
                do_epoll_wait(epollfd, timeout, /* do_read= */ false),
            ]
        });

        // Wait for readers to block.
        std::thread::sleep(timeout / 3);

        // Make the read-end readable.
        unistd::write(fd_client, &[0, 0])?;

        // Wait and read some, but not all, from the buffer.
        std::thread::sleep(timeout / 3);
        unistd::read(fd_server, &mut [0])?;

        let results = thread.join().unwrap();

        // The first wait should have received the event
        ensure_ord!(results[0].epoll_res, ==, Ok(1));
        ensure_ord!(results[0].duration, <, timeout);
        ensure_ord!(results[0].events[0], ==, epoll::EpollEvent::new(EpollFlags::EPOLLIN, 0));

        // The second wait should have timed out with no events received.
        ensure_ord!(results[1].epoll_res, ==, Ok(0));
        ensure_ord!(results[1].duration, >=, timeout);

        Ok(())
    })
}

fn test_threads_multi_write() -> anyhow::Result<()> {
    let (fd_client, fd_server) = socket_init_helper(
        SocketInitMethod::Inet,
        libc::SOCK_STREAM,
        libc::SOCK_NONBLOCK,
        /* bind_client = */ false,
    );
    let epollfd = epoll::epoll_create()?;

    test_utils::run_and_close_fds(&[epollfd, fd_client, fd_server], || {
        let mut event = epoll::EpollEvent::new(EpollFlags::EPOLLET | EpollFlags::EPOLLIN, 0);
        epoll::epoll_ctl(
            epollfd,
            epoll::EpollOp::EpollCtlAdd,
            fd_server,
            Some(&mut event),
        )?;

        let timeout = Duration::from_millis(100);

        let threads = [
            std::thread::spawn(move || do_epoll_wait(epollfd, timeout, /* do_read= */ false)),
            std::thread::spawn(move || do_epoll_wait(epollfd, timeout, /* do_read= */ false)),
            std::thread::spawn(move || do_epoll_wait(epollfd, timeout, /* do_read= */ false)),
        ];

        // Wait for readers to block.
        std::thread::sleep(timeout / 3);

        // Make the read-end readable.
        unistd::write(fd_client, &[0])?;

        // Wait again and make the read-end readable again.
        std::thread::sleep(timeout / 3);
        unistd::write(fd_client, &[0])?;

        let mut results = threads.map(|t| t.join().unwrap());

        // Two of the threads should have gotten an event, but we don't know which one.
        // Sort results by number of events received.
        results.sort_by(|lhs, rhs| lhs.events.len().cmp(&rhs.events.len()));

        // One thread should have timed out with no events received.
        ensure_ord!(results[0].epoll_res, ==, Ok(0));
        ensure_ord!(results[0].duration, >=, timeout);

        // The rest should have received the event
        for res in &results[1..] {
            ensure_ord!(res.epoll_res, ==, Ok(1));
            ensure_ord!(res.duration, <, timeout);
            ensure_ord!(res.events[0], ==, epoll::EpollEvent::new(EpollFlags::EPOLLIN, 0));
        }

        Ok(())
    })
}

fn test_writable() -> anyhow::Result<()> {
    let (fd_client, fd_server) = socket_init_helper(
        SocketInitMethod::Inet,
        libc::SOCK_STREAM,
        libc::SOCK_NONBLOCK,
        /* bind_client = */ false,
    );
    let epollfd = epoll::epoll_create()?;

    test_utils::run_and_close_fds(&[epollfd, fd_client, fd_server], || {
        let mut event = epoll::EpollEvent::new(EpollFlags::EPOLLET | EpollFlags::EPOLLOUT, 0);
        epoll::epoll_ctl(
            epollfd,
            epoll::EpollOp::EpollCtlAdd,
            fd_client,
            Some(&mut event),
        )?;

        let timeout = Duration::from_millis(100);

        let thread =
            std::thread::spawn(move || do_epoll_wait(epollfd, timeout, /* do_read= */ false));

        let res = thread.join().unwrap();

        ensure_ord!(res.epoll_res, ==, Ok(1));
        ensure_ord!(res.duration, <, timeout);
        ensure_ord!(res.events[0], ==, epoll::EpollEvent::new(EpollFlags::EPOLLOUT, 0));

        Ok(())
    })
}

fn test_writable_when_write() -> anyhow::Result<()> {
    let (fd_client, fd_server) = socket_init_helper(
        SocketInitMethod::Inet,
        libc::SOCK_STREAM,
        libc::SOCK_NONBLOCK,
        /* bind_client = */ false,
    );
    let epollfd = epoll::epoll_create()?;

    test_utils::run_and_close_fds(&[epollfd, fd_client, fd_server], || {
        let mut event = epoll::EpollEvent::new(EpollFlags::EPOLLET | EpollFlags::EPOLLOUT, 0);
        epoll::epoll_ctl(
            epollfd,
            epoll::EpollOp::EpollCtlAdd,
            fd_client,
            Some(&mut event),
        )?;

        let timeout = Duration::from_millis(100);

        let thread = std::thread::spawn(move || {
            vec![
                do_epoll_wait(epollfd, timeout, /* do_read= */ false),
                // The second one is supposed to timeout.
                do_epoll_wait(epollfd, timeout, /* do_read= */ false),
            ]
        });

        // Wait for the waiter to block.
        std::thread::sleep(timeout / 2);

        // Write more.
        unistd::write(fd_client, &[0])?;

        let results = thread.join().unwrap();

        // The first wait should have received the event
        ensure_ord!(results[0].epoll_res, ==, Ok(1));
        ensure_ord!(results[0].duration, <, timeout);
        ensure_ord!(results[0].events[0], ==, epoll::EpollEvent::new(EpollFlags::EPOLLOUT, 0));

        // The second wait should have timed out with no events received.
        ensure_ord!(results[1].epoll_res, ==, Ok(0));
        ensure_ord!(results[1].duration, >=, timeout);

        Ok(())
    })
}

fn main() -> anyhow::Result<()> {
    // should we restrict the tests we run?
    let filter_shadow_passing = std::env::args().any(|x| x == "--shadow-passing");
    let filter_libc_passing = std::env::args().any(|x| x == "--libc-passing");
    // should we summarize the results rather than exit on a failed test
    let summarize = std::env::args().any(|x| x == "--summarize");

    let all_envs = set![TestEnvironment::Libc, TestEnvironment::Shadow];
    let mut tests: Vec<test_utils::ShadowTest<(), anyhow::Error>> = vec![
        ShadowTest::new("multi-write", test_multi_write, all_envs.clone()),
        ShadowTest::new(
            "write-then-read",
            test_write_then_read,
            set![TestEnvironment::Libc],
        ),
        ShadowTest::new(
            "threads-multi-write",
            test_threads_multi_write,
            all_envs.clone(),
        ),
        ShadowTest::new("writable", test_writable, all_envs.clone()),
        ShadowTest::new(
            "writable-when-write",
            test_writable_when_write,
            all_envs.clone(),
        ),
    ];

    if filter_shadow_passing {
        tests.retain(|x| x.passing(TestEnvironment::Shadow));
    }
    if filter_libc_passing {
        tests.retain(|x| x.passing(TestEnvironment::Libc));
    }

    test_utils::run_tests(&tests, summarize)?;

    println!("Success.");

    Ok(())
}
