//! The board's `/dfu` verb (OTA stage 1, Codeberg #235,
//! `docs/src/concepts/ota-stage-1-ble-dfu.md`).
//!
//! The core decides (`leviculum_core::node::remote_dfu`): which events are a
//! `/dfu` command, accepted or refused and why, the typed reply, and the
//! order of the exit. This module supplies the board's acts behind the core's
//! `DfuExit`: dispatching the reply, reading the three carriers' queues for
//! "has it left", and the `GPREGRET` write plus reset through the seam the
//! 1200-baud UF2 touch uses ([`crate::usb::write_gpregret`]).

use leviculum_core::node::remote_dfu::{
    dfu_command, exit_to_dfu, send_dfu_reply, DfuExit, ReplyLeave, DFU_REPLY_POLL_MS,
    DFU_REPLY_WAIT_MS,
};
use leviculum_core::node::{NodeCore, NodeEvent};
use leviculum_core::traits::{Clock, Storage};
use leviculum_core::transport::TickOutput;
use leviculum_log_line::MgmtDfuBody;
use rand_core::CryptoRngCore;

// The bound must outlast the LoRa queue's own stale-drop age, so a reply
// held behind a duty-cycle lock is settled (sent or dropped) before the
// bound gives up on it; the bound is then only ever a stuck interface's.
const _: () = assert!(DFU_REPLY_WAIT_MS > leviculum_queue_budget::HOLD_MAX_AGE_MS as u64);

/// Decide every `/dfu` command in `events`. Each is logged; a refusal's
/// typed reply is merged into `out`, which the caller dispatches with the
/// `/status` replies; an accepted command's reply is parked in `exit` for
/// [`exit_if_commanded`].
///
/// Synchronous and generic only over the node, so the engine-pass macro's
/// five expansions share one copy. The async exit is expanded once, at the
/// top of the main loop: expanded in the macro, the exit and its dispatch
/// closure were monomorphised per arm and cost 6.4 KiB of flash per board
/// (measured with `just nrf-flash-budget`, order 1605).
pub fn handle_events<R, C, S>(
    node: &mut NodeCore<R, C, S>,
    events: &[NodeEvent],
    out: &mut TickOutput,
    exit: &mut Option<TickOutput>,
) where
    R: CryptoRngCore,
    C: Clock,
    S: Storage,
{
    for event in events {
        let Some(command) = dfu_command(node, event) else {
            continue;
        };
        // Before the reply: the line naming who sent the board away has to
        // be on the debug port even if nothing after it is.
        crate::log::log_fmt(
            "MGMT_DFU ",
            format_args!(
                "{}",
                MgmtDfuBody {
                    accepted: command.verdict.accepted(),
                    from: command.from,
                    reason: command.verdict.reason(),
                }
            ),
        );
        let reply = match send_dfu_reply(node, &command) {
            Ok(reply) => reply,
            Err(e) => {
                crate::log::log_fmt("MGMT_DFU ", format_args!("reply failed: {:?}", e));
                // An accepted command whose reply could not be built is
                // still accepted: the operator asked, the list allowed it.
                TickOutput::empty()
            }
        };
        if command.verdict.accepted() {
            // The first accepted command wins; a second in the same batch
            // would only say the same thing again.
            if exit.is_none() {
                *exit = Some(reply);
            }
        } else {
            out.merge(reply);
        }
    }
}

/// Leave for the bootloader if [`handle_events`] parked an accepted command:
/// the core's [`exit_to_dfu`] order over this board's carriers. Does not
/// return when it does anything. `dispatch` hands a `TickOutput`'s actions to
/// the interfaces, as the caller's `/status` arm does.
pub async fn exit_if_commanded<R, C, S, D>(
    node: &mut NodeCore<R, C, S>,
    exit: &mut Option<TickOutput>,
    mut dispatch: D,
) where
    R: CryptoRngCore,
    C: Clock,
    S: Storage,
    D: FnMut(&mut NodeCore<R, C, S>, TickOutput),
{
    let Some(reply) = exit.take() else {
        return;
    };
    let mut board = BoardExit {
        node,
        dispatch: &mut dispatch,
        lora_mark: 0,
    };
    exit_to_dfu(&mut board, reply).await;
}

/// The board's side of the core's exit order.
struct BoardExit<'a, R, C, S, D>
where
    R: CryptoRngCore,
    C: Clock,
    S: Storage,
    D: FnMut(&mut NodeCore<R, C, S>, TickOutput),
{
    node: &'a mut NodeCore<R, C, S>,
    dispatch: &'a mut D,
    /// The LoRa acceptance count right after the hand-over: the reply is the
    /// last LoRa packet accepted by then, if it went out on LoRa at all.
    lora_mark: u32,
}

impl<R, C, S, D> DfuExit for BoardExit<'_, R, C, S, D>
where
    R: CryptoRngCore,
    C: Clock,
    S: Storage,
    D: FnMut(&mut NodeCore<R, C, S>, TickOutput),
{
    fn hand_over(&mut self, reply: TickOutput) {
        (self.dispatch)(self.node, reply);
        self.lora_mark = crate::lora::accepted_packets();
    }

    fn reply_left(&mut self) -> bool {
        // All three carriers, not the one the reply was routed to: which
        // interface the link runs over is the core's business, and a queue
        // that is empty costs one load to confirm.
        crate::lora::settled_through(self.lora_mark)
            && crate::usb::outgoing_drained()
            && leviculum_firmware::ble_link::outgoing_drained()
    }

    fn now_ms(&self) -> u64 {
        embassy_time::Instant::now().as_millis()
    }

    fn pause(&mut self) -> impl core::future::Future<Output = ()> {
        embassy_time::Timer::after_millis(DFU_REPLY_POLL_MS)
    }

    fn leaving(&mut self, _outcome: ReplyLeave) {
        // Silent on the board. A line here would be best effort at most (the
        // reset can beat the debug port's drain), and its formatting cost
        // the bytes that put the RAK4631 under its flash floor (order 1605).
        // The `MGMT_DFU accepted=yes` line before the reply is the record.
    }

    fn write_magic(&mut self, magic: u8) {
        crate::usb::write_gpregret(magic);
    }

    fn reset(&mut self) {
        cortex_m::peripheral::SCB::sys_reset();
    }
}
