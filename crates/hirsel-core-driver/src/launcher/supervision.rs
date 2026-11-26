//! Child supervision: indexed listeners and children, pipelined Linux exec acknowledgement,
//! pidfd/cgroup exit watching, serving child endpoints, and cold listener arrivals.
use super::places::{read_child_outputs, read_report};
use super::ready::{CHANGE, NONE, READ, Readiness, ReadyEvent, Watch};
use super::session::{
    Child, ChildState, End, Event, Group, HostCommand, HostReply, InstallKind, Launch, Mode, Phase,
    Refusal, Role, Status, Wire,
};
use super::spawn::{PreparedChild, RunningChild, cleanup_cgroup, spawn_child};
use super::{Conn, Image, readable, spawn};
use hirsel_core_ids::hex;
use rustc_hash::FxHashMap;
use std::collections::VecDeque;
use std::fs::File;
use std::io;
use std::net::TcpListener;
use std::os::fd::AsRawFd;
use std::path::PathBuf;

/// Maximum children simultaneously awaiting kernel exec acknowledgement.
pub(super) const MAX_CONCURRENT_STARTS: usize = 64;

/// Group admission availability for `Request::Admit`.
pub(super) enum GroupAdmitState {
    Unknown,
    Retiring,
    Active,
}

/// Result of checking a `Request::Start` against existing child and listener state.
pub(super) enum StartCheck {
    Existing(Phase, Option<HostReply>),
    Pending,
    Refused(Refusal),
    Ready,
}

/// Notices emitted by [`ChildSupervisor`] for request orchestration and object lifecycle.
pub(super) enum ChildNotice {
    Event(Event),
    Reply(Child, HostReply),
    ClaimReleased(Child),
    Exited(Child),
}

enum WatchTarget {
    Listener(Group),
    Endpoint(Child),
    ChildExit(Child),
    ExecPipe(Child),
    PendingExit(Child),
}

struct Listening {
    watch: Watch,
    socket: TcpListener,
    /// The host has been told this listener is readable, by `Arrived`, an inventory or an
    /// end's sample; the watch stays off until a serving child has run again.
    reported: bool,
    retiring: bool,
}

struct Process {
    watch: Option<Watch>,
    child: Option<RunningChild>,
    /// A serving child's private endpoint (descriptor 4).
    endpoint: Option<Conn>,
    /// A serving open awaiting the host's durable acceptance. Replay it after Hello.
    opened: Option<HostReply>,
    /// A serving child's wake report (descriptor 5), the launcher's read end.
    report: Option<File>,
    /// Standard output of a child with a bounded answer.
    answer: Option<(File, u64)>,
    install_target: Option<u64>,
    /// A prepare child's `TRIGGERS` file in its export directory, with its bound.
    triggers: Option<(PathBuf, u64)>,
}

struct Kept {
    state: ChildState,
    process: Option<Process>,
    install_reply: Option<HostReply>,
}

struct PendingChild {
    error_watch: Option<Watch>,
    pidfd_watch: Option<Watch>,
    spawn: Option<spawn::PendingSpawn>,
    prepared: PreparedChild,
    pending_signal: Option<i32>,
    exited_early: bool,
}

struct QueuedStart {
    launch: Launch,
    image: Image,
}

pub(super) struct ChildSupervisor {
    cgroup_root: Option<PathBuf>,
    scratch: PathBuf,
    listeners: FxHashMap<Group, Listening>,
    listener_by_name: FxHashMap<String, Group>,
    children: FxHashMap<Child, Kept>,
    pending: FxHashMap<Child, PendingChild>,
    queued_signals: FxHashMap<Child, Option<i32>>,
    start_queue: VecDeque<QueuedStart>,
    group_live: FxHashMap<Group, usize>,
    group_serving: FxHashMap<Group, usize>,
    tokens: FxHashMap<u64, WatchTarget>,
    already_exited: Vec<Child>,
}

impl ChildSupervisor {
    pub(super) fn new(cgroup_root: Option<PathBuf>, scratch: PathBuf) -> Self {
        Self {
            cgroup_root,
            scratch,
            listeners: FxHashMap::default(),
            listener_by_name: FxHashMap::default(),
            children: FxHashMap::default(),
            pending: FxHashMap::default(),
            queued_signals: FxHashMap::default(),
            start_queue: VecDeque::new(),
            group_live: FxHashMap::default(),
            group_serving: FxHashMap::default(),
            tokens: FxHashMap::default(),
            already_exited: Vec::new(),
        }
    }

    pub(super) fn scratch_of(&self, group: &Group) -> PathBuf {
        self.scratch.join(format!(
            "{}-{}",
            hex::encode(group.name.as_bytes()),
            group.incarnation
        ))
    }

    pub(super) fn group_admit_state(&self, group: &Group) -> GroupAdmitState {
        match self.listeners.get(group) {
            None => GroupAdmitState::Unknown,
            Some(listener) if listener.retiring => GroupAdmitState::Retiring,
            Some(_) => GroupAdmitState::Active,
        }
    }

    fn group_live_count(&self, group: &Group) -> usize {
        self.group_live.get(group).copied().unwrap_or(0)
    }

    fn group_serving_count(&self, group: &Group) -> usize {
        self.group_serving.get(group).copied().unwrap_or(0)
    }

    fn sync_listener_watch(&mut self, group: &Group) -> io::Result<()> {
        let serving = self.group_serving_count(group) > 0;
        if let Some(listener) = self.listeners.get_mut(group) {
            let cold = !listener.reported && !listener.retiring && !serving;
            listener.watch.set(if cold { READ } else { NONE })?;
        }
        Ok(())
    }

    fn inc_active(&mut self, child: &Child) -> io::Result<()> {
        *self.group_live.entry(child.group.clone()).or_insert(0) += 1;
        if matches!(child.role, Role::Serving(_)) {
            *self.group_serving.entry(child.group.clone()).or_insert(0) += 1;
            self.sync_listener_watch(&child.group)?;
        }
        Ok(())
    }

    fn dec_active(&mut self, child: &Child) -> io::Result<()> {
        if let Some(count) = self.group_live.get_mut(&child.group) {
            *count -= 1;
            if *count == 0 {
                self.group_live.remove(&child.group);
            }
        }
        if matches!(child.role, Role::Serving(_)) {
            if let Some(count) = self.group_serving.get_mut(&child.group) {
                *count -= 1;
                if *count == 0 {
                    self.group_serving.remove(&child.group);
                }
            }
            self.sync_listener_watch(&child.group)?;
        }
        Ok(())
    }

    pub(super) fn bind(&mut self, poll: &Readiness, group: Group, address: &str) -> Event {
        if let Some(held) = self.listener_by_name.get(&group.name) {
            return if held.incarnation != group.incarnation {
                let held = held.incarnation;
                Event::BindFailed(group, format!("the name is bound at incarnation {held}"))
            } else {
                Event::Bound(group)
            };
        }
        if self.group_live_count(&group) > 0 {
            return Event::BindFailed(group, "the group has live children".into());
        }
        match TcpListener::bind(address) {
            Ok(socket) => {
                let mut watch = poll.watch(socket.as_raw_fd());
                let token = watch.token();
                if let Err(error) = watch.set(READ) {
                    return Event::BindFailed(group, error.to_string());
                }
                self.listener_by_name
                    .insert(group.name.clone(), group.clone());
                self.tokens
                    .insert(token, WatchTarget::Listener(group.clone()));
                self.listeners.insert(
                    group.clone(),
                    Listening {
                        watch,
                        socket,
                        reported: false,
                        retiring: false,
                    },
                );
                Event::Bound(group)
            }
            Err(error) => Event::BindFailed(group, error.to_string()),
        }
    }

    pub(super) fn close(&mut self, group: Group) -> Event {
        if let Some(listener) = self.listeners.remove(&group) {
            self.listener_by_name.remove(&group.name);
            self.tokens.remove(&listener.watch.token());
            drop(listener);
        }
        Event::Closed(group)
    }

    pub(super) fn retire(&mut self, group: &Group, notices: &mut Vec<ChildNotice>) {
        if let Some(listener) = self.listeners.get_mut(group) {
            listener.retiring = true;
            let _ = listener.watch.set(NONE);
        }
        self.check_retired(group, notices);
    }

    fn check_retired(&self, group: &Group, notices: &mut Vec<ChildNotice>) {
        if self.listeners.get(group).is_some_and(|l| l.retiring)
            && self.group_live_count(group) == 0
        {
            notices.push(ChildNotice::Event(Event::Retired(group.clone())));
        }
    }

    pub(super) fn check_start(&self, launch: &Launch) -> StartCheck {
        let child = launch.child();
        if let Some(kept) = self.children.get(&child) {
            return StartCheck::Existing(kept.state.phase.clone(), kept.install_reply.clone());
        }
        if self.pending.contains_key(&child) || self.queued_signals.contains_key(&child) {
            return StartCheck::Pending;
        }
        let refusal = match self.listeners.get(&launch.group) {
            None => Some(Refusal::UnknownGroup),
            Some(listener) if listener.retiring => Some(Refusal::Retiring),
            Some(_) if launch.assembly.len() != 32 => Some(Refusal::Mismatch),
            Some(_)
                if launch.recording.as_ref().is_some_and(|r| {
                    r.id.len() != 16 || !matches!(launch.mode, Mode::Serving(..))
                }) =>
            {
                Some(Refusal::Mismatch)
            }
            Some(_) if matches!(launch.mode, Mode::Install(_, InstallKind::Standalone(0), _)) => {
                Some(Refusal::Mismatch)
            }
            Some(_) => None,
        };
        match refusal {
            Some(why) => StartCheck::Refused(why),
            None => StartCheck::Ready,
        }
    }

    /// Reserve and launch (or enqueue when at the concurrent-start bound) a validated start
    /// whose store-holder claim has already been acquired.
    pub(super) fn start_reserved(
        &mut self,
        poll: &Readiness,
        launch: Launch,
        image: Image,
        notices: &mut Vec<ChildNotice>,
    ) {
        let child = launch.child();
        if let Err(error) = self.inc_active(&child) {
            notices.push(ChildNotice::ClaimReleased(child.clone()));
            notices.push(ChildNotice::Event(Event::Refused(
                child,
                Refusal::Spawn(error.to_string()),
            )));
            return;
        }
        if self.pending.len() >= MAX_CONCURRENT_STARTS {
            self.queued_signals.insert(child, None);
            self.start_queue.push_back(QueuedStart { launch, image });
            return;
        }
        self.begin_spawn(poll, launch, image, None, notices);
    }

    fn fail_active_start(
        &mut self,
        child: &Child,
        refusal: Refusal,
        notices: &mut Vec<ChildNotice>,
    ) {
        let _ = self.dec_active(child);
        notices.push(ChildNotice::ClaimReleased(child.clone()));
        notices.push(ChildNotice::Event(Event::Refused(child.clone(), refusal)));
        self.check_retired(&child.group, notices);
    }

    fn begin_spawn(
        &mut self,
        poll: &Readiness,
        launch: Launch,
        image: Image,
        pending_signal: Option<i32>,
        notices: &mut Vec<ChildNotice>,
    ) {
        let child = launch.child();
        let Some(listener_fd) = self
            .listeners
            .get(&launch.group)
            .map(|l| l.socket.as_raw_fd())
        else {
            return self.fail_active_start(&child, Refusal::UnknownGroup, notices);
        };
        let scratch = self.scratch_of(&launch.group);
        match spawn_child(
            poll,
            self.cgroup_root.as_deref(),
            &scratch,
            &launch,
            &image,
            listener_fd,
        ) {
            Ok((spawn::StartOutcome::Started(spawned), prepared)) => {
                self.finish_started_sync(poll, child, spawned, prepared, pending_signal, notices);
            }
            Ok((spawn::StartOutcome::Pending(pending_spawn), prepared)) => {
                self.install_pending(
                    poll,
                    child,
                    pending_spawn,
                    prepared,
                    pending_signal,
                    notices,
                );
            }
            Err(error) => {
                self.fail_active_start(&child, Refusal::Spawn(error.to_string()), notices);
            }
        }
    }

    fn install_pending(
        &mut self,
        poll: &Readiness,
        child: Child,
        pending_spawn: spawn::PendingSpawn,
        mut prepared: PreparedChild,
        pending_signal: Option<i32>,
        notices: &mut Vec<ChildNotice>,
    ) {
        let mut error_watch = poll.watch(pending_spawn.error_fd());
        let error_token = error_watch.token();
        let (source, exit_token, exit_fd) = pending_spawn.exit_source();
        let watch_setup = error_watch
            .set(READ)
            .and_then(|()| poll.watch_exit_source(source, exit_token, exit_fd));
        let pidfd_watch = match watch_setup {
            Ok(watch) => watch,
            Err(error) => {
                drop(error_watch);
                pending_spawn.kill_and_reap();
                if let Some(group) = prepared.cgroup.take() {
                    let _ = cleanup_cgroup(group);
                }
                return self.fail_active_start(&child, Refusal::Spawn(error.to_string()), notices);
            }
        };
        if pending_signal == Some(libc::SIGKILL) {
            let _ = pending_spawn.signal(libc::SIGKILL);
        }
        self.tokens
            .insert(error_token, WatchTarget::ExecPipe(child.clone()));
        self.tokens
            .insert(pidfd_watch.token(), WatchTarget::PendingExit(child.clone()));
        let exited_early = pidfd_watch.is_exited();
        self.pending.insert(
            child,
            PendingChild {
                error_watch: Some(error_watch),
                pidfd_watch: Some(pidfd_watch),
                spawn: Some(pending_spawn),
                prepared,
                pending_signal,
                exited_early,
            },
        );
    }

    fn finish_started_sync(
        &mut self,
        poll: &Readiness,
        child: Child,
        spawned: spawn::Spawned,
        mut prepared: PreparedChild,
        pending_signal: Option<i32>,
        notices: &mut Vec<ChildNotice>,
    ) {
        let running = RunningChild {
            process: spawned,
            group: prepared.cgroup.take(),
            status: None,
        };
        let watch = match poll.watch_exit(&running.process) {
            Ok(watch) => watch,
            Err(error) => {
                if running.signal(libc::SIGKILL).is_ok() {
                    let _ = running.wait();
                }
                return self.fail_active_start(&child, Refusal::Spawn(error.to_string()), notices);
            }
        };
        let mut exits = Vec::new();
        if let Err(error) = self.promote_started(
            child.clone(),
            running,
            watch,
            prepared,
            pending_signal,
            false,
            &mut exits,
            notices,
        ) {
            notices.push(ChildNotice::ClaimReleased(child.clone()));
            notices.push(ChildNotice::Event(Event::Refused(
                child.clone(),
                Refusal::Spawn(error.to_string()),
            )));
            self.check_retired(&child.group, notices);
            return;
        }
        self.already_exited.extend(exits);
    }

    #[expect(clippy::too_many_arguments, reason = "child promotion state transfer")]
    fn promote_started(
        &mut self,
        child: Child,
        running: RunningChild,
        watch: Watch,
        mut prepared: PreparedChild,
        pending_signal: Option<i32>,
        exited_now: bool,
        exits: &mut Vec<Child>,
        notices: &mut Vec<ChildNotice>,
    ) -> io::Result<()> {
        if let Some(sig) = pending_signal {
            let _ = running.signal(sig);
        }
        if let Some(conn) = &mut prepared.endpoint {
            if let Err(error) = conn.watch() {
                self.tokens.remove(&watch.token());
                drop(watch);
                if running.signal(libc::SIGKILL).is_ok() {
                    let _ = running.wait();
                }
                let _ = self.dec_active(&child);
                return Err(error);
            }
            self.tokens
                .insert(conn.watch.token(), WatchTarget::Endpoint(child.clone()));
        }
        let exit_immediately = exited_now || watch.is_exited();
        self.tokens
            .insert(watch.token(), WatchTarget::ChildExit(child.clone()));
        if matches!(child.role, Role::Serving(_))
            && let Some(listener) = self.listeners.get_mut(&child.group)
        {
            listener.reported = false;
        }
        let process = Process {
            watch: Some(watch),
            child: Some(running),
            endpoint: prepared.endpoint.take(),
            opened: None,
            report: prepared.report.take(),
            answer: prepared.answer.take(),
            install_target: prepared.install_target.take(),
            triggers: prepared.triggers.take(),
        };
        self.children.insert(
            child.clone(),
            Kept {
                state: ChildState {
                    child: child.clone(),
                    phase: Phase::Started,
                    selection: prepared.selection.take(),
                    answer: None,
                },
                process: Some(process),
                install_reply: None,
            },
        );
        notices.push(ChildNotice::Event(Event::Launched(
            child.clone(),
            Phase::Started,
        )));
        if exit_immediately {
            exits.push(child);
        }
        Ok(())
    }

    fn poll_pending_exec(
        &mut self,
        child: &Child,
        exit_fired: bool,
        exits: &mut Vec<Child>,
        notices: &mut Vec<ChildNotice>,
    ) -> io::Result<()> {
        let Some(entry) = self.pending.get_mut(child) else {
            return Ok(());
        };
        let poll_result = entry.spawn.as_mut().expect("pending spawn").poll_exec();
        match poll_result {
            spawn::ExecPoll::Pending => {
                if exit_fired {
                    entry.exited_early = true;
                }
                Ok(())
            }
            spawn::ExecPoll::Ready(spawned) => {
                let mut entry = self.pending.remove(child).expect("checked above");
                if let Some(error_watch) = entry.error_watch.take() {
                    self.tokens.remove(&error_watch.token());
                    drop(error_watch);
                }
                drop(entry.spawn.take());
                let pidfd_watch = entry.pidfd_watch.take().expect("pending pidfd watch");
                let running = RunningChild {
                    process: spawned,
                    group: entry.prepared.cgroup.take(),
                    status: None,
                };
                if let Err(error) = self.promote_started(
                    child.clone(),
                    running,
                    pidfd_watch,
                    entry.prepared,
                    entry.pending_signal,
                    exit_fired || entry.exited_early,
                    exits,
                    notices,
                ) {
                    notices.push(ChildNotice::ClaimReleased(child.clone()));
                    notices.push(ChildNotice::Event(Event::Refused(
                        child.clone(),
                        Refusal::Spawn(error.to_string()),
                    )));
                    self.check_retired(&child.group, notices);
                }
                Ok(())
            }
            spawn::ExecPoll::Failed(error) => {
                let mut entry = self.pending.remove(child).expect("checked above");
                if let Some(error_watch) = entry.error_watch.take() {
                    self.tokens.remove(&error_watch.token());
                    drop(error_watch);
                }
                if let Some(pidfd_watch) = entry.pidfd_watch.take() {
                    self.tokens.remove(&pidfd_watch.token());
                    drop(pidfd_watch);
                }
                drop(entry.spawn.take());
                if let Some(group) = entry.prepared.cgroup.take() {
                    cleanup_cgroup(group)?;
                }
                self.dec_active(child)?;
                notices.push(ChildNotice::ClaimReleased(child.clone()));
                notices.push(ChildNotice::Event(Event::Refused(
                    child.clone(),
                    Refusal::Spawn(error.to_string()),
                )));
                self.check_retired(&child.group, notices);
                Ok(())
            }
        }
    }

    fn pump_start_queue(&mut self, poll: &Readiness, notices: &mut Vec<ChildNotice>) {
        while self.pending.len() < MAX_CONCURRENT_STARTS {
            let Some(queued) = self.start_queue.pop_front() else {
                break;
            };
            let child = queued.launch.child();
            let pending_signal = self.queued_signals.remove(&child).flatten();
            self.begin_spawn(poll, queued.launch, queued.image, pending_signal, notices);
        }
    }

    pub(super) fn signal(&mut self, child: &Child, signal: i32) {
        if let Some(kept) = self.children.get(child)
            && let Some(process) = &kept.process
            && let Some(running) = &process.child
        {
            let _ = running.signal(signal);
            return;
        }
        if let Some(entry) = self.pending.get_mut(child) {
            if signal == libc::SIGKILL {
                entry.pending_signal = Some(libc::SIGKILL);
                if let Some(spawn) = &entry.spawn {
                    let _ = spawn.signal(libc::SIGKILL);
                }
            } else if entry.pending_signal.is_none() {
                entry.pending_signal = Some(signal);
            }
            return;
        }
        if let Some(slot) = self.queued_signals.get_mut(child)
            && (signal == libc::SIGKILL || slot.is_none())
        {
            *slot = Some(signal);
        }
    }

    pub(super) fn acknowledge(&mut self, child: &Child) {
        if self
            .children
            .get(child)
            .is_some_and(|kept| kept.process.is_none())
        {
            self.children.remove(child);
        }
    }

    /// Forward `command` to a live serving child's endpoint. Returns `true` if sent.
    pub(super) fn send_command(
        &mut self,
        child: &Child,
        command: &HostCommand,
        accepting: bool,
    ) -> bool {
        let Some(process) = self
            .children
            .get_mut(child)
            .and_then(|kept| kept.process.as_mut())
        else {
            return false;
        };
        if accepting && process.opened.take().is_none() {
            return false;
        }
        let Some(conn) = process.endpoint.as_mut() else {
            return false;
        };
        conn.queue(command);
        conn.flush();
        let _ = conn.watch();
        true
    }

    pub(super) fn record_answer(&mut self, child: &Child, reply: &HostReply) {
        if let Some(kept) = self.children.get_mut(child)
            && kept
                .process
                .as_ref()
                .is_some_and(|process| process.opened.is_none())
        {
            kept.state.answer = Some(reply.clone());
        }
    }

    pub(super) fn record_deposed(&mut self, child: &Child, reply: &HostReply) {
        if let Some(kept) = self.children.get_mut(child) {
            kept.state.answer = Some(reply.clone());
        }
    }

    /// Snapshot child inventory, cold readable listeners (marking them reported and
    /// disarming their watches), and replayed `Placed` replies for `Request::Hello`.
    pub(super) fn inventory_for_hello(
        &mut self,
    ) -> (Vec<ChildState>, Vec<Group>, Vec<(Child, HostReply)>) {
        let children = self
            .children
            .values()
            .map(|kept| kept.state.clone())
            .collect();
        let mut cold = Vec::new();
        for (group, listener) in &mut self.listeners {
            let serving = self.group_serving.get(group).copied().unwrap_or(0) > 0;
            if !serving && readable(listener.socket.as_raw_fd()) {
                listener.reported = true;
                let _ = listener.watch.set(NONE);
                cold.push(group.clone());
            }
        }
        let mut replayed = Vec::new();
        for kept in self.children.values() {
            let serving_open = kept.process.as_ref().and_then(|p| p.opened.as_ref());
            if let Some(reply) = serving_open.or(kept.install_reply.as_ref()) {
                replayed.push((kept.state.child.clone(), reply.clone()));
            }
        }
        (children, cold, replayed)
    }

    /// Process one readiness turn's events in O(fired_events) time:
    /// 1. Exec pipe acknowledgements (publishing `Launched` before any exit or reply).
    /// 2. Serving child endpoints (flush outbound, drain inbound replies).
    /// 3. Child exits (final endpoint drain, answers/reports, listener sample, `Ended`, `Retired`).
    /// 4. Cold listener arrivals (`Arrived`, disarming watch once reported).
    pub(super) fn step(
        &mut self,
        poll: &Readiness,
        events: &[ReadyEvent],
        notices: &mut Vec<ChildNotice>,
    ) -> io::Result<()> {
        let mut exec_ready = Vec::new();
        let mut pending_exited = Vec::new();
        let mut endpoints = Vec::new();
        let mut exits = std::mem::take(&mut self.already_exited);
        let mut arrivals = Vec::new();

        for event in events {
            match self.tokens.get(&event.token()) {
                Some(WatchTarget::ExecPipe(child)) => exec_ready.push(child.clone()),
                Some(WatchTarget::PendingExit(child)) => pending_exited.push(child.clone()),
                Some(WatchTarget::Endpoint(child)) => endpoints.push(child.clone()),
                Some(WatchTarget::ChildExit(child)) => exits.push(child.clone()),
                Some(WatchTarget::Listener(group)) => arrivals.push(group.clone()),
                None => {}
            }
        }

        for child in &exec_ready {
            let exit_fired = pending_exited.contains(child);
            self.poll_pending_exec(child, exit_fired, &mut exits, notices)?;
        }
        for child in &pending_exited {
            if self.pending.contains_key(child) {
                self.poll_pending_exec(child, true, &mut exits, notices)?;
            }
        }
        if !exec_ready.is_empty() || !pending_exited.is_empty() {
            self.pump_start_queue(poll, notices);
        }

        for child in endpoints {
            self.service_endpoint(&child, notices)?;
        }

        for child in exits {
            self.complete_child_exit(poll, &child, notices)?;
        }

        for group in arrivals {
            let serving = self.group_serving_count(&group) > 0;
            let arrived = self.listeners.get_mut(&group).is_some_and(|listener| {
                if !listener.reported
                    && !listener.retiring
                    && !serving
                    && readable(listener.socket.as_raw_fd())
                {
                    listener.reported = true;
                    let _ = listener.watch.set(NONE);
                    true
                } else {
                    false
                }
            });
            if arrived {
                notices.push(ChildNotice::Event(Event::Arrived(group)));
            }
        }
        Ok(())
    }

    fn service_endpoint(
        &mut self,
        child: &Child,
        notices: &mut Vec<ChildNotice>,
    ) -> io::Result<()> {
        if let Some(conn) = self
            .children
            .get_mut(child)
            .and_then(|kept| kept.process.as_mut())
            .and_then(|p| p.endpoint.as_mut())
        {
            conn.flush();
        }
        self.drain_endpoint(child, notices);
        if let Some(conn) = self
            .children
            .get_mut(child)
            .and_then(|kept| kept.process.as_mut())
            .and_then(|p| p.endpoint.as_mut())
        {
            conn.watch()?;
        }
        Ok(())
    }

    fn drain_endpoint(&mut self, child: &Child, notices: &mut Vec<ChildNotice>) {
        let Some(process) = self
            .children
            .get_mut(child)
            .and_then(|kept| kept.process.as_mut())
        else {
            return;
        };
        let Some(conn) = process.endpoint.as_mut() else {
            return;
        };
        conn.fill();
        let mut replies = Vec::new();
        while let Some(frame) = conn.frame() {
            match HostReply::decode(&frame) {
                Ok(reply) => replies.push(reply),
                Err(_) => conn.closed = true,
            }
        }
        for reply in replies {
            if matches!(reply, HostReply::Opened(_)) {
                process.opened = Some(reply.clone());
            }
            if let HostReply::StoreMismatch(ehi, elo, ahi, alo) = reply {
                eprintln!(
                    "hirsel host: group {}: store identity mismatch (expected {ehi:016x}{elo:016x}, found {ahi:016x}{alo:016x})",
                    child.group.name
                );
            }
            notices.push(ChildNotice::Reply(child.clone(), reply));
        }
    }

    fn complete_child_exit(
        &mut self,
        poll: &Readiness,
        child: &Child,
        notices: &mut Vec<ChildNotice>,
    ) -> io::Result<()> {
        let Some(process) = self
            .children
            .get_mut(child)
            .and_then(|kept| kept.process.as_mut())
        else {
            return Ok(());
        };
        if let Some(watch) = process.watch.take() {
            self.tokens.remove(&watch.token());
            drop(watch);
        }
        let running = process.child.as_mut().expect("a live child");
        running.reap_leader()?;
        if let Some(group) = &running.group
            && !group.empty()?
        {
            let mut watch = poll.watch(group.event_fd());
            watch.set(CHANGE)?;
            self.tokens
                .insert(watch.token(), WatchTarget::ChildExit(child.clone()));
            process.watch = Some(watch);
            return Ok(());
        }
        let status = process.child.take().expect("a live child").finish()?;
        self.ended(child, status, notices)
    }

    /// A child has exited: relay its endpoint's last frames, read its report and answer,
    /// sample its listener, emit its end, and only then re-arm the listener watch.
    fn ended(
        &mut self,
        child: &Child,
        status: Status,
        notices: &mut Vec<ChildNotice>,
    ) -> io::Result<()> {
        self.drain_endpoint(child, notices);
        let mut process = self
            .children
            .get_mut(child)
            .and_then(|kept| kept.process.take())
            .expect("a live child");
        if let Some(conn) = process.endpoint.take() {
            self.tokens.remove(&conn.watch.token());
            drop(conn);
        }
        let report = process.report.as_mut().and_then(read_report);
        let (saved_install, output_events) = read_child_outputs(
            child,
            status,
            process.answer.take(),
            process.install_target,
            process.triggers.take(),
        )?;
        if let Some(reply) = saved_install
            && let Some(kept) = self.children.get_mut(child)
        {
            kept.install_reply = Some(reply);
        }
        for event in output_events {
            notices.push(ChildNotice::Event(event));
        }
        let mut sample = false;
        if let Role::Serving(_) = child.role
            && let Some(listener) = self.listeners.get_mut(&child.group)
        {
            sample = readable(listener.socket.as_raw_fd());
            listener.reported = sample;
        }
        let end = End {
            status,
            report,
            readable: sample,
        };
        if let Some(kept) = self.children.get_mut(child) {
            kept.state.phase = Phase::Exited(end.clone());
        }
        self.dec_active(child)?;
        notices.push(ChildNotice::Exited(child.clone()));
        notices.push(ChildNotice::ClaimReleased(child.clone()));
        notices.push(ChildNotice::Event(Event::Ended(child.clone(), end)));
        self.check_retired(&child.group, notices);
        Ok(())
    }

    #[cfg(test)]
    pub(super) fn has_pending_starts(&self) -> bool {
        !self.pending.is_empty() || !self.start_queue.is_empty()
    }

    #[cfg(test)]
    pub(super) fn has_cgroup_root(&self) -> bool {
        self.cgroup_root.is_some()
    }

    #[cfg(test)]
    pub(super) fn listener_addr(&self, group: &Group) -> Option<std::net::SocketAddr> {
        self.listeners
            .get(group)
            .and_then(|l| l.socket.local_addr().ok())
    }

    #[cfg(test)]
    pub(super) fn is_child_live(&self, child: &Child) -> bool {
        self.children
            .get(child)
            .is_some_and(|kept| kept.process.is_some())
            || self.pending.contains_key(child)
            || self.queued_signals.contains_key(child)
    }
}

impl Drop for ChildSupervisor {
    fn drop(&mut self) {
        self.tokens.clear();
        self.start_queue.clear();
        self.queued_signals.clear();
        for (_, mut entry) in self.pending.drain() {
            entry.error_watch.take();
            entry.pidfd_watch.take();
            if let Some(spawn) = entry.spawn.take() {
                spawn.kill_and_reap();
            }
            if let Some(group) = entry.prepared.cgroup.take() {
                let _ = cleanup_cgroup(group);
            }
        }
        for (_, mut kept) in self.children.drain() {
            if let Some(mut process) = kept.process.take()
                && let Some(child) = process.child.take()
            {
                process.watch.take();
                if child.signal(libc::SIGKILL).is_ok() {
                    let _ = child.wait();
                }
            }
        }
    }
}
