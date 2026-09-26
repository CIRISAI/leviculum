//! Minimal Interface impl for embedded serial over USB CDC-ACM

extern crate alloc;

use alloc::vec::Vec;
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::channel::Sender;
use leviculum_core::traits::{Interface, InterfaceError};
use leviculum_core::InterfaceId;

/// Embedded serial interface backed by an Embassy channel.
///
/// `try_send()` pushes HDLC-unframed packet data to the channel.
/// The `retic_serial_task` reads from the other end, frames with HDLC,
/// and writes to USB CDC-ACM.
pub struct EmbeddedInterface<'a> {
    sender: Sender<'a, CriticalSectionRawMutex, Vec<u8>, 8>,
}

impl<'a> EmbeddedInterface<'a> {
    pub fn new(sender: Sender<'a, CriticalSectionRawMutex, Vec<u8>, 8>) -> Self {
        Self { sender }
    }
}

impl Interface for EmbeddedInterface<'_> {
    fn id(&self) -> InterfaceId {
        InterfaceId(0)
    }

    fn name(&self) -> &str {
        crate::iface_bytes::NAMES[crate::iface_bytes::SERIAL]
    }

    fn mtu(&self) -> usize {
        564
    }

    fn is_online(&self) -> bool {
        true
    }

    fn try_send(&mut self, data: &[u8]) -> Result<(), InterfaceError> {
        let bytes = data.len();
        self.sender
            .try_send(data.to_vec())
            // The reference counts the unframed packet where the interface
            // accepts it for the medium (`RNodeInterface.py:725`); HDLC
            // framing is added downstream and is outside the count, as it is
            // there.
            .map(|()| crate::iface_bytes::note_tx(crate::iface_bytes::SERIAL, bytes))
            .map_err(|_| {
                // Codeberg #344: the frame is going nowhere and the caller sees
                // only `BufferFull`. Say it at the interface, where the depth
                // that filled is known. Error path, so the cost is irrelevant;
                // the depth comes from the channel itself so the line cannot
                // drift from the queue it describes.
                crate::log::log_fmt(
                    "[IFACE_FULL] ",
                    format_args!(
                        "iface={} depth={} len={}",
                        self.name(),
                        self.sender.capacity(),
                        data.len()
                    ),
                );
                InterfaceError::BufferFull
            })
    }
}
