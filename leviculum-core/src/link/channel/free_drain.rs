//! Draining past a full host sink for messages the host never sees
//! (+ciris, leviculum#70).

use alloc::vec::Vec;

use super::{Channel, Envelope, CHANNEL_SEQ_MODULUS};

impl Channel {
    /// Like [`Channel::drain_received_limit`], except that a message whose
    /// MSGTYPE `free` accepts does not count against `limit`.
    ///
    /// The direct-link signals are handled by the node, never delivered to
    /// the application, so a full application sink is no reason to leave one
    /// buffered behind a sequence gap: it would neither be handled nor
    /// proved until the application drained, and the upgrade would time out
    /// (Codex review on #74). Order is kept: draining still stops at the
    /// first message that does count once `limit` is spent.
    pub(crate) fn drain_received_budgeted(
        &mut self,
        limit: usize,
        free: impl Fn(u16) -> bool,
    ) -> Vec<(Envelope, [u8; 32])> {
        let mut ready = Vec::new();
        let mut counted = 0usize;
        while let Some(Some((envelope, _))) = self.rx_ring.front() {
            if envelope.sequence != self.next_rx_sequence {
                break;
            }
            let costs = !free(envelope.msgtype);
            if costs && counted >= limit {
                break;
            }
            let Some(Some((env, hash))) = self.rx_ring.pop_front() else {
                break;
            };
            self.next_rx_sequence =
                ((self.next_rx_sequence as u32 + 1) % CHANNEL_SEQ_MODULUS) as u16;
            if costs {
                counted += 1;
            }
            ready.push((env, hash));
        }
        ready
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::link::channel::ReceiveOutcome;
    use alloc::vec;

    const SIGNAL: u16 = 0xFE01;
    const APP: u16 = 0x0001;

    fn free(msgtype: u16) -> bool {
        msgtype == SIGNAL
    }

    #[test]
    fn a_buffered_signal_drains_past_a_spent_limit_and_order_holds() {
        let mut channel = Channel::new();
        // 1 (signal) and 2 (application) arrive ahead of 0.
        for (seq, kind) in [(1u16, SIGNAL), (2, APP)] {
            let outcome = channel
                .receive(
                    &Envelope::new(kind, seq, vec![seq as u8]).pack(),
                    [seq as u8; 32],
                )
                .unwrap();
            assert!(matches!(outcome, ReceiveOutcome::Buffered), "{outcome:?}");
        }
        // 0 fills the gap.
        let outcome = channel
            .receive(&Envelope::new(SIGNAL, 0, vec![0]).pack(), [0; 32])
            .unwrap();
        assert!(matches!(outcome, ReceiveOutcome::Delivered(_)));

        // With no application capacity left, the signal behind the gap still
        // drains; the application message after it waits.
        let drained = channel.drain_received_budgeted(0, free);
        assert_eq!(
            drained.iter().map(|(e, _)| e.sequence).collect::<Vec<_>>(),
            vec![1]
        );
        assert_eq!(channel.next_rx_sequence(), 2);

        // Capacity returns: the application message drains.
        let drained = channel.drain_received_budgeted(1, free);
        assert_eq!(drained.len(), 1);
        assert_eq!(drained[0].0.msgtype, APP);
    }
}
