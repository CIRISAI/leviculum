//! The M9's board as the M9's loop runs its client (1414; moved here in
//! 1415 so the cells share it): a bare `leviculum-core` node driven by
//! `loquax::home::board::Executor`, on a byte line to a transport node.
//!
//! The line is written the way the M9 writes its one UART: a debug line
//! before every frame (the `KBRAW` lines of the keyboard, 100 a second on
//! the board) and every frame with its XON and XOFF bytes escaped
//! (`leviculum_serial_mux::escape_flow`). The transport node's reader is
//! the core `Deframer`, as lnsd's serial interface is.
//!
//! Two knobs for the cells of 1415: the line can be cut after a page and
//! dialled again (the link breaking mid-pull, as a cable pulled and put
//! back or lnsd restarted does), and a flag goes up at the first page, so
//! a second board can start while this one is mid-pull.

#![allow(dead_code)]

use std::io::{ErrorKind, Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use leviculum_core::framing::hdlc::{self, DeframeResult, Deframer};
use leviculum_core::node::{NodeCore, NodeCoreBuilder};
use leviculum_core::packet::{Packet, PacketContext};
use leviculum_core::traits::{Clock, Interface, InterfaceError};
use leviculum_core::transport::dispatch_actions;
use leviculum_core::{Identity, InterfaceId, MemoryStorage, TickOutput};
use loquax::home::ask::{Ask, HomeError, Outcome};
use loquax::home::board::{Batch, Executor, LinkClass, Report};
use loquax::home::wire;
use rand_core::OsRng;

/// The board's clock: milliseconds since the board came up.
#[derive(Clone)]
pub struct Uptime(Instant);

impl Clock for Uptime {
    fn now_ms(&self) -> u64 {
        u64::try_from(self.0.elapsed().as_millis()).unwrap_or(u64::MAX)
    }
}

/// The board's serial interface: a queue the loop writes out.
#[derive(Default)]
pub struct Line(Vec<Vec<u8>>);

impl Interface for Line {
    fn id(&self) -> InterfaceId {
        InterfaceId(0)
    }
    fn name(&self) -> &str {
        "serial"
    }
    fn mtu(&self) -> usize {
        564
    }
    fn is_online(&self) -> bool {
        true
    }
    fn try_send(&mut self, data: &[u8]) -> Result<(), InterfaceError> {
        self.0.push(data.to_vec());
        Ok(())
    }
}

/// What one [`Board::run`] ended with: the ask's end (none when the
/// budget ran out first) and the pages it brought.
pub type Ran = (Option<Result<Outcome, HomeError>>, Vec<wire::Page>);

/// The board: its node, its line to the transport node, and what it wrote.
pub struct Board {
    pub node: Box<NodeCore<OsRng, Uptime, MemoryStorage>>,
    line: Line,
    stream: TcpStream,
    addr: SocketAddr,
    deframer: Deframer,
    clock: Uptime,
    /// Debug lines written between frames.
    pub text_lines: usize,
    /// Frames that carried an escaped flow byte.
    pub flow_escaped: usize,
    /// Cut the line after this many pages of the next run, once, and dial
    /// again: what the node had in flight on the old line is lost.
    pub cut_after_pages: Option<usize>,
    /// Lines cut so far.
    pub cuts: usize,
    /// Raised at the first page of a run.
    pub paged: Option<Arc<AtomicBool>>,
    /// The node's events after the first cut, by name and the board's
    /// time: what a cell that went wrong shows of the recovery.
    pub after_cut: Vec<(u64, String)>,
    /// Resource parts from the line the board loses, the first ones,
    /// as a UART that dropped them (1459).
    pub lose_parts: usize,
    /// Every resource part from the line the board loses, retransmissions
    /// too, until the board cancels the answer they belong to (its RCL):
    /// a resource answer the line lost whole, the link up (1461).
    pub vanish_answer: bool,
    /// Run the executor without 1461's rule, the control.
    pub answer_watch_off: bool,
    /// Resource parts the board took off the line, and lost.
    pub parts: usize,
    pub parts_lost: usize,
    /// Resource cancels (RCL) the board wrote.
    pub refusals: usize,
}

/// The context of a frame on the line.
fn context(frame: &[u8]) -> Option<PacketContext> {
    Packet::unpack(frame).ok().map(|packet| packet.context)
}

impl Board {
    pub fn connect(addr: SocketAddr) -> Self {
        let clock = Uptime(Instant::now());
        // As the M9 builds its node (`leviculum-esp/src/handheld.rs`): the
        // answer it takes in, and no decompressor, which no board build
        // links; a compressed resource is refused at once (1459).
        let mut node = NodeCoreBuilder::new()
            .enable_transport(true)
            .max_incoming_resource_size(256 * 1024)
            .decompresses(false)
            .build_boxed(OsRng, clock.clone(), MemoryStorage::with_defaults());
        node.set_interface_name(0, "serial".to_string());
        node.set_interface_hw_mtu(0, 564);
        node.set_interface_mode(0, leviculum_core::InterfaceMode::Gateway);
        let mut board = Self {
            node,
            line: Line::default(),
            stream: Self::dial(addr),
            addr,
            deframer: Deframer::new(),
            clock,
            text_lines: 0,
            flow_escaped: 0,
            cut_after_pages: None,
            cuts: 0,
            paged: None,
            after_cut: Vec::new(),
            lose_parts: 0,
            vanish_answer: false,
            answer_watch_off: false,
            parts: 0,
            parts_lost: 0,
            refusals: 0,
        };
        let up = board.node.handle_interface_up(0);
        board.dispatch(up);
        board
    }

    fn dial(addr: SocketAddr) -> TcpStream {
        let stream = TcpStream::connect(addr).expect("the transport node's port");
        stream
            .set_read_timeout(Some(Duration::from_millis(5)))
            .expect("timeout");
        stream
    }

    pub fn now(&self) -> u64 {
        self.clock.now_ms()
    }

    fn dispatch(&mut self, out: TickOutput) -> Vec<leviculum_core::node::NodeEvent> {
        let mut ifaces: [&mut dyn Interface; 1] = [&mut self.line];
        let dispatched = dispatch_actions(&mut ifaces, out.actions, &Default::default());
        self.node.record_dispatch_drops(&dispatched);
        out.events
    }

    /// Write what the node queued as the M9 writes it: a debug line, then
    /// the frame with its flow bytes escaped.
    fn flush(&mut self) {
        for packet in std::mem::take(&mut self.line.0) {
            if context(&packet) == Some(PacketContext::ResourceRcl) {
                self.refusals += 1;
                self.vanish_answer = false;
            }
            let line = format!("KBRAW {} 0000000000000000\r\n", self.now());
            self.stream.write_all(line.as_bytes()).expect("write");
            self.text_lines += 1;
            let mut framed = Vec::new();
            hdlc::frame(&packet, &mut framed);
            let mut wire = Vec::new();
            leviculum_serial_mux::escape_flow(&framed, &mut |piece| wire.extend_from_slice(piece));
            if wire.len() != framed.len() {
                self.flow_escaped += 1;
            }
            self.stream.write_all(&wire).expect("write");
        }
    }

    /// Cut the line and dial again: the transport node sees one interface
    /// go and another come; the board's node keeps its own.
    fn cut(&mut self) {
        let _ = self.stream.shutdown(std::net::Shutdown::Both);
        self.stream = Self::dial(self.addr);
        self.deframer = Deframer::new();
        self.cuts += 1;
    }

    /// Perform `ask` as the M9's loop does, until it is done or `budget`
    /// passes; the pages it brought.
    pub fn run(&mut self, device: &Identity, ask: Ask, max_answer: usize, budget: Duration) -> Ran {
        let mut exec = Executor::new(ask, max_answer);
        self.perform(device, &mut exec, budget)
    }

    /// Perform `ask` as the M9's loop does from 1439 on, its page fitted
    /// to the link it gets (`Executor::fitted`); what it brought, and the
    /// link class and page the fit chose.
    pub fn run_fitted(
        &mut self,
        device: &Identity,
        ask: Ask,
        batch: Batch,
        budget: Duration,
    ) -> (Ran, Option<(LinkClass, u64)>) {
        let mut exec = Executor::fitted(ask, batch);
        if self.answer_watch_off {
            exec = exec.without_answer_watch();
        }
        let ran = self.perform(device, &mut exec, budget);
        (ran, exec.fit())
    }

    fn perform(&mut self, device: &Identity, exec: &mut Executor, budget: Duration) -> Ran {
        let mut pages = Vec::new();
        let until = Instant::now() + budget;
        let mut buf = [0u8; 4096];
        while Instant::now() < until {
            let mut outs = Vec::new();
            match self.stream.read(&mut buf) {
                Ok(0) => panic!("the transport node closed the line"),
                Ok(n) => {
                    for result in self.deframer.process(&buf[..n]) {
                        if let DeframeResult::Frame(frame) = result {
                            if context(&frame) == Some(PacketContext::Resource) {
                                self.parts += 1;
                                if self.vanish_answer {
                                    self.parts_lost += 1;
                                    continue;
                                }
                                if self.lose_parts > 0 {
                                    self.lose_parts -= 1;
                                    self.parts_lost += 1;
                                    continue;
                                }
                            }
                            outs.push(self.node.handle_packet(InterfaceId(0), &frame));
                        }
                    }
                }
                Err(error)
                    if matches!(error.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) => {}
                Err(error) => panic!("read: {error}"),
            }
            if self
                .node
                .next_deadline()
                .is_some_and(|due| due <= self.now())
            {
                outs.push(self.node.handle_timeout());
            }
            let mut out = TickOutput::default();
            let now = self.now();
            let mut reports = vec![exec.poll(&mut self.node, device, now, &mut out)];
            outs.push(out);
            while let Some(out) = outs.pop() {
                for event in self.dispatch(out) {
                    let mut more = TickOutput::default();
                    let now = self.now();
                    if self.cuts > 0 {
                        let name = format!("{event:?}");
                        let name = name
                            .split([' ', '{', '('])
                            .next()
                            .unwrap_or("?")
                            .to_string();
                        self.after_cut.push((now, name));
                    }
                    reports.push(exec.event(&mut self.node, device, &event, now, &mut more));
                    outs.push(more);
                }
            }
            self.flush();
            for report in reports {
                match report {
                    Report::Idle => {}
                    Report::Page(page) => {
                        pages.push(page);
                        if let Some(paged) = &self.paged {
                            paged.store(true, Ordering::SeqCst);
                        }
                        if self.cut_after_pages == Some(pages.len()) {
                            self.cut_after_pages = None;
                            self.cut();
                        }
                    }
                    Report::Done(result) => return (Some(result), pages),
                }
            }
        }
        (None, pages)
    }
}
