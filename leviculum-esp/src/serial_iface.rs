//! The node's interface to the host on the debug UART (1414): what the
//! core hands it waits here until the board's loop writes it as an HDLC
//! frame between the log lines (`UartLog::frame`), under the single-UART
//! rules of `leviculum-nrf/serial-mux`.
//!
//! The core is sans-I/O and the UART is the loop's, so this only queues.
//! A bounded queue: past [`QUEUE_FRAMES`] frames the core is told
//! `BufferFull` and drops the packet, best effort as on every interface,
//! rather than the heap growing behind a host that does not read.

use alloc::collections::VecDeque;
use alloc::vec::Vec;

use leviculum_core::traits::{Interface, InterfaceError};
use leviculum_core::InterfaceId;

/// The serial interface's index in the node.
pub const SERIAL_IFACE: InterfaceId = InterfaceId(0);
/// Its name in the core's lines.
pub const SERIAL_NAME: &str = "serial";
/// The HW_MTU every Leviculum serial interface declares
/// (`leviculum_serial_mux::SERIAL_HW_MTU`).
pub const SERIAL_HW_MTU: usize = leviculum_serial_mux::SERIAL_HW_MTU;
/// The most frames waiting for the UART: at 115200 baud a full frame is
/// 49 ms on the wire, so sixteen are under a second of backlog.
pub const QUEUE_FRAMES: usize = 16;

/// The host link as the core sees it.
#[derive(Default)]
pub struct SerialIface {
    queue: VecDeque<Vec<u8>>,
    dropped: u32,
}

impl SerialIface {
    /// An empty queue.
    pub fn new() -> Self {
        Self::default()
    }

    /// The next frame for the UART, oldest first.
    pub fn pop(&mut self) -> Option<Vec<u8>> {
        self.queue.pop_front()
    }

    /// Packets refused because the queue was full.
    pub fn dropped(&self) -> u32 {
        self.dropped
    }
}

impl Interface for SerialIface {
    fn id(&self) -> InterfaceId {
        SERIAL_IFACE
    }

    fn name(&self) -> &str {
        SERIAL_NAME
    }

    fn mtu(&self) -> usize {
        SERIAL_HW_MTU
    }

    fn is_online(&self) -> bool {
        true
    }

    fn try_send(&mut self, data: &[u8]) -> Result<(), InterfaceError> {
        if self.queue.len() >= QUEUE_FRAMES {
            self.dropped = self.dropped.saturating_add(1);
            return Err(InterfaceError::BufferFull);
        }
        self.queue.push_back(data.to_vec());
        Ok(())
    }
}
