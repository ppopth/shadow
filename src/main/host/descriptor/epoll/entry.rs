use linux_api::epoll::EpollEvents;

use crate::host::descriptor::listener::StateListenHandle;
use crate::host::descriptor::{FileSignals, FileState};

/// Used to track the status of a file we are monitoring for events. Any complicated logic for
/// deciding when a file has events that epoll should report should be specified in this object's
/// implementation.
pub(super) struct Entry {
    /// Priority value among other ready entries.
    priority: Option<u64>,
    /// The events of interest registered by the managed process.
    interest: EpollEvents,
    /// The data registered by the managed process, to be returned upon event notification.
    data: u64,
    /// The handle to the currently registered file status listener.
    listener_handle: Option<StateListenHandle>,
    /// The current state of the file.
    state: FileState,
    /// The file state changes we have already reported since the state last changed. When a state
    /// changes, that event becomes uncollected until `collect_ready_events` is called.
    collected: FileState,
    /// TODO remove when legacy tcp is removed.
    is_legacy: bool,
}

impl Entry {
    pub fn new(interest: EpollEvents, data: u64, state: FileState) -> Self {
        Self {
            priority: None,
            interest,
            data,
            listener_handle: None,
            state,
            collected: FileState::empty(),
            is_legacy: false,
        }
    }

    // TODO remove when legacy tcp is removed.
    pub fn set_legacy(&mut self) {
        self.is_legacy = true;
    }

    /// Updates the events that should be tracked in this entry, and the data that should be
    /// returned to the managed process when those events occur.
    ///
    /// Note that this operation causes us to store the given current file state so that future
    /// changes are tracked from the state at the time `modify()` was called, and internal state for
    /// tracking which events have been collected by the managed process are updated accordingly.
    pub fn modify(&mut self, interest: EpollEvents, data: u64, state: FileState) {
        log::trace!("Reset old state {:?}, new state {:?}", self.state, state);
        self.interest = interest;
        self.data = data;
        self.state = state;
        self.collected = FileState::empty();
    }

    pub fn set_priority(&mut self, priority: Option<u64>) {
        self.priority = priority;
    }

    pub fn priority(&self) -> Option<u64> {
        self.priority
    }

    pub fn notify(&mut self, new_state: FileState, changed: FileState, signals: FileSignals) {
        log::trace!(
            "Notify old state {:?}, new state {:?}, changed {:?}, signals {:?}",
            self.state,
            new_state,
            changed,
            signals,
        );
        self.state = new_state;
        self.collected.remove(changed);

        // If the file is written again, let the epoll waiter collect the events again.
        if signals.contains(FileSignals::READ_BUFFER_GREW) {
            // We only subscribe to `READ_BUFFER_GREW` signals for edge-triggered entries.
            debug_assert!(self.interest.intersects(EpollEvents::EPOLLET));

            // Ignore the `READ_BUFFER_GREW` if the file isn't READABLE.
            if new_state.contains(FileState::READABLE) {
                self.collected.remove(FileState::READABLE);
            } else {
                // If this occurs, we probably want to fix whatever file is broadcasting `READ_BUFFER_GREW`
                // when not readable.
                warn_once_then_debug!("Epoll received READ_BUFFER_GREW but state is not READABLE");
            }
        }
    }

    pub fn get_listener_state(&self) -> FileState {
        // TODO remove this if block when legacy tcp is removed.
        if self.is_legacy {
            return FileState::all();
        }

        // Return the file state changes that we want to be notified about.
        Self::state_from_events(self.interest).union(FileState::CLOSED)
    }

    pub fn get_listener_signals(&self) -> FileSignals {
        let mut signals = FileSignals::empty();

        if self.interest.intersects(EpollEvents::EPOLLET) {
            signals.insert(FileSignals::READ_BUFFER_GREW);
        }

        signals
    }

    pub fn set_listener_handle(&mut self, handle: Option<StateListenHandle>) {
        self.listener_handle = handle;
    }

    pub fn has_ready_events(&self) -> bool {
        // TODO remove this if block when legacy tcp is removed.
        if self.is_legacy {
            if self.state.contains(FileState::CLOSED) {
                return false;
            } else if self.state.contains(FileState::ACTIVE) {
                return !self.get_ready_events().is_empty();
            } else {
                return false;
            }
        }

        !self.state.contains(FileState::CLOSED) && !self.get_ready_events().is_empty()
    }

    pub fn collect_ready_events(&mut self) -> Option<(EpollEvents, u64)> {
        let events = self.get_ready_events();

        if events.is_empty() {
            return None;
        }

        self.collected.insert(Self::state_from_events(events));

        if self.interest.contains(EpollEvents::EPOLLONESHOT) {
            self.interest.remove(events)
        }

        log::trace!(
            "Collected ready events {events:?} interest {:?} state {:?}",
            self.interest,
            self.state
        );

        Some((events, self.data))
    }

    fn get_ready_events(&self) -> EpollEvents {
        let events = Self::events_from_state(self.get_ready_state());
        self.interest.intersection(events)
    }

    fn get_ready_state(&self) -> FileState {
        if self.interest.contains(EpollEvents::EPOLLET) {
            // Edge-triggered: report event, then don't report again until that state changes.
            self.state.difference(self.collected)
        } else {
            // Level-triggered: report event, keep reporting until state turns off.
            self.state
        }
    }

    fn events_from_state(state: FileState) -> EpollEvents {
        let mut events = EpollEvents::empty();

        if state.intersects(FileState::READABLE) {
            events.insert(EpollEvents::EPOLLIN);
        }
        if state.intersects(FileState::WRITABLE) {
            events.insert(EpollEvents::EPOLLOUT);
        }

        events
    }

    fn state_from_events(events: EpollEvents) -> FileState {
        let mut state = FileState::empty();

        if events.intersects(EpollEvents::EPOLLIN) {
            state.insert(FileState::READABLE)
        }
        if events.intersects(EpollEvents::EPOLLOUT) {
            state.insert(FileState::WRITABLE)
        }

        state
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const DATA: u64 = 1234;

    fn poll_init(init: FileState, interest: EpollEvents) {
        let mut entry = Entry::new(interest, DATA, init);
        assert!(entry.has_ready_events());

        let (ev, data) = entry.collect_ready_events().unwrap();
        assert!(interest.contains(ev));
        assert_eq!(data, DATA);
    }

    #[test]
    fn poll_init_r() {
        let init = FileState::READABLE;
        poll_init(init, EpollEvents::EPOLLIN);
    }

    #[test]
    fn poll_init_w() {
        let init = FileState::WRITABLE;
        poll_init(init, EpollEvents::EPOLLOUT);
    }

    #[test]
    fn poll_init_rw() {
        let init = FileState::READABLE | FileState::WRITABLE;
        poll_init(init, EpollEvents::EPOLLIN);
        poll_init(init, EpollEvents::EPOLLOUT);
        poll_init(init, EpollEvents::EPOLLIN | EpollEvents::EPOLLOUT);
    }

    /// Checks that an entry starting in state `init` is only ready after `change` turns on when
    /// waiting for `interest`.
    fn poll_on_state(
        init: FileState,
        interest: EpollEvents,
        change_on: FileState,
        signals: FileSignals,
    ) {
        let mut entry = Entry::new(interest, DATA, init);
        assert!(!entry.has_ready_events());

        entry.notify(init.union(change_on), change_on, signals);
        assert!(entry.has_ready_events());

        let (ev, data) = entry.collect_ready_events().unwrap();
        assert!(interest.contains(ev));
        assert_eq!(data, DATA);
    }

    #[test]
    fn poll_on_r() {
        let on = FileState::READABLE;
        poll_on_state(
            FileState::empty(),
            EpollEvents::EPOLLIN,
            on,
            FileSignals::empty(),
        );
        poll_on_state(
            FileState::empty(),
            EpollEvents::EPOLLIN | EpollEvents::EPOLLOUT,
            on,
            FileSignals::empty(),
        );
        poll_on_state(
            FileState::WRITABLE,
            EpollEvents::EPOLLIN,
            on,
            FileSignals::empty(),
        );
    }

    #[test]
    fn poll_on_w() {
        let on = FileState::WRITABLE;
        poll_on_state(
            FileState::empty(),
            EpollEvents::EPOLLOUT,
            on,
            FileSignals::empty(),
        );
        poll_on_state(
            FileState::empty(),
            EpollEvents::EPOLLIN | EpollEvents::EPOLLOUT,
            on,
            FileSignals::empty(),
        );
        poll_on_state(
            FileState::READABLE,
            EpollEvents::EPOLLOUT,
            on,
            FileSignals::empty(),
        );
    }

    #[test]
    fn poll_on_rw() {
        let on = FileState::READABLE | FileState::WRITABLE;
        poll_on_state(
            FileState::empty(),
            EpollEvents::EPOLLIN,
            on,
            FileSignals::empty(),
        );
        poll_on_state(
            FileState::empty(),
            EpollEvents::EPOLLOUT,
            on,
            FileSignals::empty(),
        );
        poll_on_state(
            FileState::empty(),
            EpollEvents::EPOLLIN | EpollEvents::EPOLLOUT,
            on,
            FileSignals::empty(),
        );
    }

    /// Checks that an entry starting in state `init` is only not ready after `change` turns off
    /// when waiting for `interest`.
    fn poll_off_state(
        init: FileState,
        interest: EpollEvents,
        change_off: FileState,
        signals: FileSignals,
    ) {
        let mut entry = Entry::new(interest, DATA, init);
        assert!(entry.has_ready_events());

        entry.notify(init.difference(change_off), change_off, signals);
        assert!(!entry.has_ready_events());
        assert!(entry.collect_ready_events().is_none());
    }

    #[test]
    fn poll_off_r() {
        let interest = EpollEvents::EPOLLIN;
        let off = FileState::READABLE;
        poll_off_state(off, interest, off, FileSignals::empty());
        poll_off_state(
            FileState::WRITABLE | off,
            interest,
            off,
            FileSignals::empty(),
        );
    }

    #[test]
    fn poll_off_w() {
        let interest = EpollEvents::EPOLLOUT;
        let off = FileState::WRITABLE;
        poll_off_state(off, interest, off, FileSignals::empty());
        poll_off_state(
            FileState::READABLE | off,
            interest,
            off,
            FileSignals::empty(),
        );
    }

    #[test]
    fn poll_off_rw() {
        let off = FileState::READABLE | FileState::WRITABLE;
        poll_off_state(off, EpollEvents::EPOLLIN, off, FileSignals::empty());
        poll_off_state(off, EpollEvents::EPOLLOUT, off, FileSignals::empty());
        poll_off_state(
            off,
            EpollEvents::EPOLLIN | EpollEvents::EPOLLOUT,
            off,
            FileSignals::empty(),
        );
    }

    #[test]
    fn level_trigger() {
        let in_lt = EpollEvents::EPOLLIN;
        let mut entry = Entry::new(in_lt, DATA, FileState::empty());
        assert!(!entry.has_ready_events());

        entry.notify(
            FileState::READABLE,
            FileState::READABLE,
            FileSignals::empty(),
        );
        assert!(entry.has_ready_events());

        for _ in 0..3 {
            assert_eq!(
                entry.collect_ready_events(),
                Some((EpollEvents::EPOLLIN, DATA))
            );
            assert!(entry.has_ready_events());
        }

        entry.notify(
            FileState::empty(),
            FileState::READABLE,
            FileSignals::empty(),
        );
        assert!(!entry.has_ready_events());
        entry.notify(
            FileState::READABLE,
            FileState::READABLE,
            FileSignals::empty(),
        );
        assert!(entry.has_ready_events());

        for _ in 0..3 {
            assert_eq!(
                entry.collect_ready_events(),
                Some((EpollEvents::EPOLLIN, DATA))
            );
            assert!(entry.has_ready_events());
        }
    }

    #[test]
    fn edge_trigger() {
        let in_et = EpollEvents::EPOLLIN | EpollEvents::EPOLLET;
        let mut entry = Entry::new(in_et, DATA, FileState::empty());
        assert!(!entry.has_ready_events());

        entry.notify(
            FileState::READABLE,
            FileState::READABLE,
            FileSignals::empty(),
        );

        assert!(entry.has_ready_events());
        assert_eq!(
            entry.collect_ready_events(),
            Some((EpollEvents::EPOLLIN, DATA))
        );

        // Event was collected and should only be reported once.
        assert!(!entry.has_ready_events());
        assert_eq!(entry.collect_ready_events(), None);

        // Nothing changed, so still no events.
        entry.notify(
            FileState::READABLE,
            FileState::empty(),
            FileSignals::empty(),
        );
        assert!(!entry.has_ready_events());

        // Nothing changes, but this time we signal that the buffer is written more.
        entry.notify(
            FileState::READABLE,
            FileState::empty(),
            FileSignals::READ_BUFFER_GREW,
        );
        assert!(entry.has_ready_events());
        assert_eq!(
            entry.collect_ready_events(),
            Some((EpollEvents::EPOLLIN, DATA))
        );

        // When the file is not readable but we receives `READ_BUFFER_GREW`, there should be no
        // ready events.
        entry.notify(
            FileState::empty(),
            FileState::empty(),
            FileSignals::READ_BUFFER_GREW,
        );
        assert!(!entry.has_ready_events());

        // State turns off.
        entry.notify(
            FileState::empty(),
            FileState::READABLE,
            FileSignals::empty(),
        );
        assert!(!entry.has_ready_events());

        // State turns on again.
        entry.notify(
            FileState::READABLE,
            FileState::READABLE,
            FileSignals::empty(),
        );
        assert!(entry.has_ready_events());
        assert_eq!(
            entry.collect_ready_events(),
            Some((EpollEvents::EPOLLIN, DATA))
        );

        assert!(!entry.has_ready_events());
    }

    #[test]
    fn one_shot() {
        let in_os = EpollEvents::EPOLLIN | EpollEvents::EPOLLONESHOT;
        let mut entry = Entry::new(in_os, DATA, FileState::empty());
        assert!(!entry.has_ready_events());

        entry.notify(
            FileState::READABLE,
            FileState::READABLE,
            FileSignals::empty(),
        );

        assert!(entry.has_ready_events());
        assert_eq!(
            entry.collect_ready_events(),
            Some((EpollEvents::EPOLLIN, DATA))
        );

        // Should never report that event again until we reset.
        assert!(!entry.has_ready_events());
        assert_eq!(entry.collect_ready_events(), None);
        entry.notify(
            FileState::READABLE,
            FileState::empty(),
            FileSignals::empty(),
        );
        assert!(!entry.has_ready_events());
        entry.notify(
            FileState::empty(),
            FileState::READABLE,
            FileSignals::empty(),
        );
        assert!(!entry.has_ready_events());
        entry.notify(
            FileState::READABLE,
            FileState::READABLE,
            FileSignals::empty(),
        );
        assert!(!entry.has_ready_events());

        entry.modify(in_os, DATA, FileState::READABLE);

        assert!(entry.has_ready_events());
        assert_eq!(
            entry.collect_ready_events(),
            Some((EpollEvents::EPOLLIN, DATA))
        );

        entry.notify(
            FileState::empty(),
            FileState::READABLE,
            FileSignals::empty(),
        );
        entry.notify(
            FileState::READABLE,
            FileState::READABLE,
            FileSignals::empty(),
        );

        assert!(!entry.has_ready_events());
        assert_eq!(entry.collect_ready_events(), None);
    }
}
