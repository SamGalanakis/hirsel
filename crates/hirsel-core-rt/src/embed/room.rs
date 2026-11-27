//! Room peer roster and state machine matching `sdk/client/room.ts` + EMB-4.
use super::codec::{MessageIn, Part, Peer, PeerState, PeersChange, RoomEvent};
use std::collections::{BTreeMap, BTreeSet};

/// Room peer roster and state machine matching `sdk/client/room.ts` + EMB-4.
pub(crate) struct RoomState {
    pub(crate) self_peer: Option<u64>,
    pub(crate) full: bool,
    pub(crate) incarnation: Option<[u8; 16]>,
    pub(crate) revision: Option<u64>,
    pub(crate) peers: BTreeMap<u64, (Vec<u8>, Option<Vec<u8>>)>,
    pub(crate) unconfirmed: Option<BTreeSet<u64>>,
    pub(crate) stored_state: Option<Option<Vec<u8>>>,
    pub(crate) owed_state: bool,
}

impl RoomState {
    pub(crate) fn new() -> Self {
        Self {
            self_peer: None,
            full: false,
            incarnation: None,
            revision: None,
            peers: BTreeMap::new(),
            unconfirmed: None,
            stored_state: None,
            owed_state: false,
        }
    }

    /// Apply one `Part` according to `docs/spec/embed.md` lines 872-888 and `sdk/client/room.ts`.
    pub(crate) fn apply_part(&mut self, part: &Part) -> Vec<RoomEvent> {
        let mut events = Vec::new();
        let before_rev = self.revision;
        let before_me = self.self_peer;
        let mut renewed = false;
        let mut pre_left: Vec<u64> = Vec::new();

        if part.complete {
            self.self_peer = None;
            self.full = false;
        }
        if !part.full {
            let mut inc = [0u8; 16];
            inc[..8].copy_from_slice(&part.incarnation.0.to_le_bytes());
            inc[8..16].copy_from_slice(&part.incarnation.1.to_le_bytes());
            if self.incarnation != Some(inc) {
                renewed = true;
                pre_left.extend(self.peers.keys().copied());
                self.peers.clear();
                self.unconfirmed = None;
                self.incarnation = Some(inc);
                self.revision = None;
            } else if part.complete {
                self.unconfirmed = Some(self.peers.keys().copied().collect());
                self.revision = None;
            }
        }
        if let Some(you) = part.you {
            self.self_peer = Some(u64::from(you));
            self.full = false;
            self.owed_state = self.stored_state.is_some();
        }
        let mut joined_peers: Vec<Peer> = Vec::new();
        let mut state_changes: Vec<PeerState> = Vec::new();
        for (pid32, caller) in &part.joined {
            let pid = u64::from(*pid32);
            let caller_bytes = caller.encode_bytes();
            let known = self.peers.get(&pid).cloned();
            if let Some(ref mut unc) = self.unconfirmed {
                unc.remove(&pid);
            }
            self.peers.insert(pid, (caller_bytes.clone(), None));
            match known {
                None => joined_peers.push(Peer {
                    peer: pid,
                    caller: caller_bytes,
                    state: None,
                }),
                Some((_, Some(_))) => state_changes.push(PeerState {
                    peer: pid,
                    state: None,
                }),
                Some((_, None)) => {}
            }
        }
        for (pid32, st) in &part.states {
            let pid = u64::from(*pid32);
            if let Some(entry) = self.peers.get_mut(&pid) {
                entry.1 = st.clone();
                // Keep `joined` state up to date if peer joined in same part; also record in `states`.
                if let Some(jp) = joined_peers.iter_mut().find(|p| p.peer == pid) {
                    jp.state = st.clone();
                }
                state_changes.push(PeerState {
                    peer: pid,
                    state: st.clone(),
                });
            }
        }

        let me_changed = self.self_peer != before_me;
        let has_msgs = !part.msgs.is_empty();

        if has_msgs {
            // EMB-4: pre-message Peers if renewed, me, joined, or states changed.
            if renewed || me_changed || !joined_peers.is_empty() || !state_changes.is_empty() {
                events.push(RoomEvent::Peers(PeersChange {
                    incarnation: self.incarnation.map(|b| b.to_vec()),
                    revision: self.revision,
                    renewed,
                    me: self.self_peer,
                    joined: joined_peers,
                    states: state_changes,
                    left: pre_left,
                }));
            }
            for (from, direct, msg) in &part.msgs {
                events.push(RoomEvent::Message(MessageIn {
                    from: u64::from(*from),
                    message: msg.clone(),
                    direct: *direct,
                }));
            }
            let mut post_left: Vec<u64> = Vec::new();
            for &pid32 in &part.left {
                let pid = u64::from(pid32);
                if self.peers.remove(&pid).is_some() {
                    post_left.push(pid);
                }
            }
            if part.snapshot && !part.full {
                if let Some(unc) = self.unconfirmed.take() {
                    for pid in unc {
                        if self.peers.remove(&pid).is_some() {
                            post_left.push(pid);
                        }
                    }
                }
                self.revision = Some(part.revision);
            }
            if part.full {
                self.full = true;
                self.self_peer = None;
            }
            if !post_left.is_empty() || self.revision != before_rev {
                events.push(RoomEvent::Peers(PeersChange {
                    incarnation: self.incarnation.map(|b| b.to_vec()),
                    revision: self.revision,
                    renewed: false,
                    me: self.self_peer,
                    joined: Vec::new(),
                    states: Vec::new(),
                    left: post_left,
                }));
            }
        } else {
            let mut all_left = pre_left;
            for &pid32 in &part.left {
                let pid = u64::from(pid32);
                if self.peers.remove(&pid).is_some() {
                    all_left.push(pid);
                }
            }
            if part.snapshot && !part.full {
                if let Some(unc) = self.unconfirmed.take() {
                    for pid in unc {
                        if self.peers.remove(&pid).is_some() {
                            all_left.push(pid);
                        }
                    }
                }
                self.revision = Some(part.revision);
            }
            if part.full {
                self.full = true;
                self.self_peer = None;
            }
            if renewed
                || me_changed
                || !joined_peers.is_empty()
                || !state_changes.is_empty()
                || !all_left.is_empty()
                || self.revision != before_rev
            {
                events.push(RoomEvent::Peers(PeersChange {
                    incarnation: self.incarnation.map(|b| b.to_vec()),
                    revision: self.revision,
                    renewed,
                    me: self.self_peer,
                    joined: joined_peers,
                    states: state_changes,
                    left: all_left,
                }));
            }
        }

        if part.gap > 0 {
            events.push(RoomEvent::Gap(u64::from(part.gap)));
        }
        if part.lost > 0 {
            events.push(RoomEvent::Lost(u64::from(part.lost)));
        }
        if part.full {
            events.push(RoomEvent::Full);
        }
        events
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::embed::codec::Caller;

    #[test]
    fn room_first_full_part_emits_full_only_and_mixed_part_splits_peers() {
        let mut room = RoomState::new();
        // First Full Part (zero incarnation): must emit Full ONLY, no Peers event.
        let full_part = Part {
            sub: 1,
            complete: true,
            you: None,
            incarnation: (0, 0),
            revision: 0,
            snapshot: false,
            joined: vec![],
            states: vec![],
            msgs: vec![],
            left: vec![],
            gap: 0,
            lost: 0,
            full: true,
        };
        assert_eq!(room.apply_part(&full_part), vec![RoomEvent::Full]);
        assert_eq!(room.incarnation, None);

        // Mixed Part with joined, states, msgs, left, snapshot, gap, lost:
        // must emit pre-message Peers -> Message -> post-message Peers -> Gap -> Lost.
        let mixed = Part {
            sub: 1,
            complete: true,
            you: Some(2),
            incarnation: (1, 2),
            revision: 7,
            snapshot: true,
            joined: vec![(
                2,
                Caller {
                    issuer: 1,
                    subject: vec![0xaa],
                },
            )],
            states: vec![(2, Some(vec![0x11]))],
            msgs: vec![(2, true, vec![0x22])],
            left: vec![2],
            gap: 3,
            lost: 4,
            full: false,
        };
        let events = room.apply_part(&mixed);
        assert_eq!(events.len(), 5);
        assert!(matches!(
            &events[0],
            RoomEvent::Peers(p) if p.renewed && p.me == Some(2) && p.joined.len() == 1 && p.states.len() == 1 && p.left.is_empty() && p.revision.is_none()
        ));
        assert_eq!(
            events[1],
            RoomEvent::Message(MessageIn {
                from: 2,
                message: vec![0x22],
                direct: true
            })
        );
        assert!(matches!(
            &events[2],
            RoomEvent::Peers(p) if !p.renewed && p.left == vec![2] && p.revision == Some(7)
        ));
        assert_eq!(events[3], RoomEvent::Gap(3));
        assert_eq!(events[4], RoomEvent::Lost(4));
    }
}
