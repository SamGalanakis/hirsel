use super::*;

impl session::Selection {
    pub(super) fn new(activation: u64, assembly: Vec<u8>) -> Self {
        Self {
            activation,
            assembly,
        }
    }
}

impl<I: Images> Launcher<I> {
    /// Relay every whole frame a child's endpoint holds.
    pub(super) fn drain_endpoint(&mut self, i: usize) {
        let Some(conn) = self.children[i]
            .process
            .as_mut()
            .and_then(|p| p.endpoint.as_mut())
        else {
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
        let child = self.children[i].state.child.clone();
        for reply in replies {
            if matches!(reply, HostReply::Opened(_))
                && let Some(process) = self.children[i].process.as_mut()
            {
                process.opened = Some(reply.clone());
            }
            if let HostReply::StoreMismatch(ehi, elo, ahi, alo) = reply {
                eprintln!(
                    "hirsel host: group {}: store identity mismatch (expected {ehi:016x}{elo:016x}, found {ahi:016x}{alo:016x})",
                    child.group.name
                );
            }
            // OLD's next answer after a forwarded cutover is that cutover's; a held-write
            // acknowledgement is a notice.
            if !matches!(
                reply,
                HostReply::Opened(_) | HostReply::WriteAck(_) | HostReply::StoreMismatch(..)
            ) && let Some(((c, a), forwarded)) = self
                .cutovers
                .iter_mut()
                .find(|((c, _), f)| *c == child && f.reply.is_none())
            {
                forwarded.reply = Some(reply.clone());
                self.out.push(Event::CutoverResult(c.clone(), *a, reply));
            } else {
                if matches!(
                    reply,
                    HostReply::Current(..) | HostReply::Refused(_) | HostReply::StoreMismatch(..)
                ) && self.children[i]
                    .process
                    .as_ref()
                    .is_some_and(|process| process.opened.is_none())
                {
                    self.children[i].state.answer = Some(reply.clone());
                }
                self.out.push(Event::Placed(child.clone(), reply));
            }
        }
    }

    /// A child has exited: relay its endpoint's last frames, read its report and answer,
    /// sample its listener, send its end, and only then let the listener's watch start.
    pub(super) fn ended(&mut self, i: usize, status: Status) -> io::Result<()> {
        self.drain_endpoint(i);
        let mut process = self.children[i].process.take().expect("a live child");
        let report = process.report.as_mut().and_then(read_report);
        let child = self.children[i].state.child.clone();
        if let Some((mut out, bound)) = process.answer.take()
            && (status == Status::Code(0)
                || (process.install_target.is_some() && status == Status::Code(1)))
        {
            let mut answer = Vec::new();
            out.rewind()?;
            Read::by_ref(&mut out)
                .take(bound + 1)
                .read_to_end(&mut answer)?;
            if answer.len() as u64 <= bound {
                if let Some(target) = process.install_target {
                    let reply = match (HostReply::decode(&answer), status) {
                        (Ok(reply @ HostReply::Installed(version, false)), Status::Code(0))
                            if version == target =>
                        {
                            Some(reply)
                        }
                        (Ok(reply @ HostReply::InstallRefused(_)), Status::Code(1)) => Some(reply),
                        _ => None,
                    };
                    if let Some(reply) = reply {
                        self.children[i].install_reply = Some(reply.clone());
                        self.emit(Event::Placed(child.clone(), reply));
                    }
                } else {
                    self.emit(Event::Answered(child.clone(), answer));
                }
            }
        }
        if let Some((path, bound)) = process.triggers.take()
            && status == Status::Code(0)
        {
            // An unreadable or oversized report is no answer: the host then fails the
            // prepare, which ended without one.
            let mut answer = Vec::new();
            let read =
                File::open(&path).and_then(|file| file.take(bound + 1).read_to_end(&mut answer));
            if read.is_ok() && answer.len() as u64 <= bound {
                self.emit(Event::Answered(child.clone(), answer));
            }
        }
        let mut sample = false;
        if let Role::Serving(_) = child.role
            && let Some(l) = self.listener(&child.group)
        {
            sample = readable(self.listeners[l].socket.as_raw_fd());
            self.listeners[l].reported = sample;
        }
        let end = End {
            status,
            report,
            readable: sample,
        };
        // OLD ended with a forwarded cutover unanswered: the launcher cannot tell whether
        // it installed, so its saved reply is Uncertain.
        for ((c, a), forwarded) in &mut self.cutovers {
            if *c == child && forwarded.reply.is_none() {
                forwarded.reply = Some(HostReply::Uncertain);
                self.out
                    .push(Event::CutoverResult(c.clone(), *a, HostReply::Uncertain));
            }
        }
        self.children[i].state.phase = Phase::Exited(end.clone());
        self.emit(Event::Ended(child.clone(), end));
        self.retired(&child.group);
        Ok(())
    }

    #[cfg(test)]
    pub(crate) fn turn(&mut self, timeout: Duration) -> io::Result<()> {
        self.wait_turn(Some(timeout))
    }
}
