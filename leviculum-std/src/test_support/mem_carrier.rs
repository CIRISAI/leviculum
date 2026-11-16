//! +ciris: an in-memory [`HostCarrier`] pair that keeps the WIT host's
//! rules, for tests of [`HostInterface`](crate::interfaces::host).
//!
//! Readiness is strictly edge-triggered: `Readable` fires only when data
//! lands for a reader whose last `recv` answered `WouldBlock` (the start
//! counts as such), and `Writable` only when room opens for a writer whose
//! last `send` answered `WouldBlock`. An interface that stops draining
//! before `WouldBlock` stalls here, as it would on the real host.
//!
//! A small `capacity` and `max_chunk` make a stream carrier take short
//! writes and answer `WouldBlock` often.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll, Waker};

use crate::interfaces::host::{
    CarrierError, CarrierEvent, CarrierInfo, CarrierKind, CloseReason, Framing, HostCarrier,
};

/// How a pair behaves.
#[derive(Clone, Debug)]
pub struct MemCarrierConfig {
    /// The carrier both ends report.
    pub carrier: CarrierKind,
    /// The framing both ends report.
    pub framing: Framing,
    /// The largest message, for message framing.
    pub max_message: Option<u32>,
    /// What one direction holds: bytes for a stream, messages otherwise.
    pub capacity: usize,
    /// The most bytes one stream `send` takes or one `recv` returns.
    pub max_chunk: usize,
    /// Queue a `Connected` event on both ends, as a completed dial would.
    pub connected_event: bool,
}

impl MemCarrierConfig {
    /// A roomy pair with this framing.
    pub fn new(carrier: CarrierKind, framing: Framing) -> Self {
        Self {
            carrier,
            framing,
            max_message: None,
            capacity: 1 << 20,
            max_chunk: 1 << 20,
            connected_event: true,
        }
    }
}

/// What the pair saw, both ends together.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct MemCarrierStats {
    /// Stream sends that took less than they were given.
    pub short_writes: usize,
    /// Sends answered `WouldBlock`.
    pub send_would_blocks: usize,
    /// Receives answered `WouldBlock`.
    pub recv_would_blocks: usize,
    /// `Readable` events fired.
    pub readable_events: usize,
    /// `Writable` events fired.
    pub writable_events: usize,
    /// Messages or stream bytes delivered by `recv`.
    pub received_units: usize,
}

#[derive(Default)]
struct Pipe {
    bytes: VecDeque<u8>,
    msgs: VecDeque<Vec<u8>>,
    reader_armed: bool,
    writer_blocked: bool,
    writer_closed: bool,
}

impl Pipe {
    fn len(&self, framing: Framing) -> usize {
        match framing {
            Framing::Stream => self.bytes.len(),
            Framing::Message => self.msgs.len(),
        }
    }
}

#[derive(Default)]
struct End {
    events: VecDeque<CarrierEvent>,
    waker: Option<Waker>,
    closed: bool,
}

impl End {
    fn push(&mut self, ev: CarrierEvent) {
        if self.closed {
            return;
        }
        self.events.push_back(ev);
        if let Some(w) = self.waker.take() {
            w.wake();
        }
    }
}

struct Shared {
    cfg: MemCarrierConfig,
    /// `pipes[i]` carries what end `i` sends.
    pipes: [Pipe; 2],
    ends: [End; 2],
    stats: MemCarrierStats,
}

/// One end of a pair.
pub struct MemCarrier {
    side: usize,
    shared: Arc<Mutex<Shared>>,
}

/// A connected pair.
pub fn mem_carrier_pair(cfg: MemCarrierConfig) -> (MemCarrier, MemCarrier) {
    let mut shared = Shared {
        cfg,
        pipes: [Pipe::default(), Pipe::default()],
        ends: [End::default(), End::default()],
        stats: MemCarrierStats::default(),
    };
    for p in &mut shared.pipes {
        p.reader_armed = true;
    }
    if shared.cfg.connected_event {
        for e in &mut shared.ends {
            e.events.push_back(CarrierEvent::Connected);
        }
    }
    let shared = Arc::new(Mutex::new(shared));
    (
        MemCarrier {
            side: 0,
            shared: Arc::clone(&shared),
        },
        MemCarrier { side: 1, shared },
    )
}

impl MemCarrier {
    /// The pair's counts so far.
    pub fn stats(&self) -> MemCarrierStats {
        self.shared.lock().unwrap().stats
    }

    /// Whether this end was closed.
    pub fn is_closed(&self) -> bool {
        self.shared.lock().unwrap().ends[self.side].closed
    }

    /// A handle that sees the same pair, for watching from the test.
    pub fn observer(&self) -> MemCarrierObserver {
        MemCarrierObserver {
            side: self.side,
            shared: Arc::clone(&self.shared),
        }
    }
}

/// Reads a pair's state after its ends have moved into interfaces.
#[derive(Clone)]
pub struct MemCarrierObserver {
    side: usize,
    shared: Arc<Mutex<Shared>>,
}

impl MemCarrierObserver {
    /// The pair's counts so far.
    pub fn stats(&self) -> MemCarrierStats {
        self.shared.lock().unwrap().stats
    }

    /// Whether the observed end was closed.
    pub fn is_closed(&self) -> bool {
        self.shared.lock().unwrap().ends[self.side].closed
    }
}

impl HostCarrier for MemCarrier {
    fn info(&self) -> CarrierInfo {
        let s = self.shared.lock().unwrap();
        let mut info = CarrierInfo::new(s.cfg.carrier, s.cfg.framing);
        info.max_message = s.cfg.max_message;
        info.peer = Some(format!("mem:{}", 1 - self.side));
        info
    }

    fn send(&mut self, data: &[u8]) -> Result<usize, CarrierError> {
        let mut guard = self.shared.lock().unwrap();
        let s = &mut *guard;
        let (me, peer) = (self.side, 1 - self.side);
        if s.ends[me].closed || s.ends[peer].closed {
            return Err(CarrierError::Closed);
        }
        let framing = s.cfg.framing;
        let pipe = &mut s.pipes[me];
        let taken = match framing {
            Framing::Message => {
                if s.cfg.max_message.is_some_and(|m| data.len() > m as usize) {
                    return Err(CarrierError::Invalid);
                }
                if pipe.msgs.len() >= s.cfg.capacity {
                    0
                } else {
                    pipe.msgs.push_back(data.to_vec());
                    data.len()
                }
            }
            Framing::Stream => {
                let room = s.cfg.capacity.saturating_sub(pipe.bytes.len());
                let n = data.len().min(room).min(s.cfg.max_chunk);
                pipe.bytes.extend(&data[..n]);
                if n > 0 && n < data.len() {
                    s.stats.short_writes += 1;
                }
                n
            }
        };
        if taken == 0 {
            pipe.writer_blocked = true;
            s.stats.send_would_blocks += 1;
            return Err(CarrierError::WouldBlock);
        }
        if pipe.reader_armed {
            pipe.reader_armed = false;
            s.stats.readable_events += 1;
            s.ends[peer].push(CarrierEvent::Readable);
        }
        Ok(taken)
    }

    fn recv(&mut self, max: usize) -> Result<Vec<u8>, CarrierError> {
        let mut guard = self.shared.lock().unwrap();
        let s = &mut *guard;
        let (me, peer) = (self.side, 1 - self.side);
        if s.ends[me].closed {
            return Err(CarrierError::Closed);
        }
        let framing = s.cfg.framing;
        let pipe = &mut s.pipes[peer];
        let out = match framing {
            Framing::Message => match pipe.msgs.front() {
                Some(m) if m.len() > max => {
                    pipe.msgs.pop_front();
                    return Err(CarrierError::Invalid);
                }
                Some(_) => pipe.msgs.pop_front(),
                None => None,
            },
            Framing::Stream if pipe.bytes.is_empty() => None,
            Framing::Stream => {
                let n = pipe.bytes.len().min(max).min(s.cfg.max_chunk);
                Some(pipe.bytes.drain(..n).collect())
            }
        };
        match out {
            Some(data) => {
                s.stats.received_units += match framing {
                    Framing::Stream => data.len(),
                    Framing::Message => 1,
                };
                if pipe.writer_blocked && pipe.len(framing) < s.cfg.capacity {
                    pipe.writer_blocked = false;
                    s.stats.writable_events += 1;
                    s.ends[peer].push(CarrierEvent::Writable);
                }
                Ok(data)
            }
            None if pipe.writer_closed => Err(CarrierError::Closed),
            None => {
                pipe.reader_armed = true;
                s.stats.recv_would_blocks += 1;
                Err(CarrierError::WouldBlock)
            }
        }
    }

    fn close(&mut self) {
        let mut s = self.shared.lock().unwrap();
        let (me, peer) = (self.side, 1 - self.side);
        if s.ends[me].closed {
            return;
        }
        s.ends[me].closed = true;
        s.ends[me].events.clear();
        s.pipes[me].writer_closed = true;
        s.ends[peer].push(CarrierEvent::Closed(CloseReason::PeerClose));
    }

    fn poll_event(&mut self, cx: &mut Context<'_>) -> Poll<CarrierEvent> {
        let mut s = self.shared.lock().unwrap();
        let end = &mut s.ends[self.side];
        match end.events.pop_front() {
            Some(ev) => Poll::Ready(ev),
            None => {
                end.waker = Some(cx.waker().clone());
                Poll::Pending
            }
        }
    }
}
