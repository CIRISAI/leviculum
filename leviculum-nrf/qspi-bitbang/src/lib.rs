#![no_std]
//! The reads a silent NOR part is allowed to answer, clocked out one pin
//! edge at a time.
//!
//! This is the second opinion on `state=no-answer` (Codeberg #384). The
//! QSPI peripheral asked the part who it was and got `00:00:00` back, and
//! that single answer cannot tell two very different boards apart: a part
//! that is absent or unpowered, and a part that is present but which the
//! QSPI peripheral's own setup never reaches. The first is not a software
//! problem at all; the second is our bug, and would be waiting on every
//! other board with the same driver.
//!
//! So the failure path drops the peripheral, takes the same pins back as
//! ordinary GPIOs, and asks again by hand at a few hundred kHz. Two
//! drivers, one bus. If the hand-clocked read names the part, the part is
//! there and the peripheral setup is at fault. If it is silent too,
//! nothing on that unit drives MISO under either driver.
//!
//! # Why four opcodes and not one
//!
//! `9Fh` alone was the first batch, and the part's own datasheet says why
//! that is not enough to conclude silence. Macronix MX25R1635F rev. 1.6,
//! §10-3: "While Program/Erase operation is in progress, it will not decode
//! the RDID instruction." A part busy with an erase is mute to `9Fh`
//! *specifically* and answers `05h` (RDSR) normally, so asking `9Fh` first
//! and stopping there reads a busy part as an absent one. And a part parked
//! in a state the `66h`/`99h` reset pair would clear answers nothing at all
//! until it gets that pair.
//!
//! Hence [`read_status`], [`read_jedec_id`], [`read_manufacturer_device_id`]
//! and [`reset`]: the four questions a part in an unknown state is allowed
//! to answer, in the order that distinguishes the states it could be in.
//! The firmware pairs them into one line per boot; the table it reads them
//! by is in `leviculum_nrf::qspi`.
//!
//! # Why the shifter is here and not in the firmware
//!
//! What could go wrong in a bit-bang read is arithmetic: which edge
//! launches a bit, which edge samples it, which end of the byte goes
//! first, how many address bytes sit between the opcode and the answer. An
//! off-by-one in the sampling edge does not fail loudly — it returns a
//! *plausible* id, shifted by a bit, and sends the next person chasing a
//! part number that was never on the bus. That is exactly the kind of
//! defect a host test catches and a board never will, and the firmware
//! crate cross-compiles and runs no host tests. So the edges live here,
//! over a [`Bus`] trait, with a fake part replaying a known response
//! underneath them; the firmware supplies six GPIOs and a delay.
//!
//! # The transfer
//!
//! SPI mode 0 on the part's single-line path, which is the mode both
//! candidate parts (MX25R1635F, IS25LP080D) power up in and the only one
//! these opcodes are specified for:
//!
//! - CS# high and SCK low to begin, so the first clock edge of the
//!   transfer is a rising one.
//! - The master presents a command bit on IO0 (the part's SI) while SCK is
//!   low; the part samples it on the rising edge.
//! - The part presents a data bit on IO1 (its SO) on a falling edge; the
//!   master samples it on the next rising edge.
//!
//! Eight command bits, MSB first, then the address bytes the opcode carries
//! (three for `90h`, none for the rest), then the clocks of reading. The
//! part launches the first answer bit on the falling edge that ends the
//! last byte the master sent, so the first *sample* is the rising edge
//! after it, and [`transfer`] gets that ordering by construction:
//! [`write_bit`] ends on a falling edge, [`read_bit`] starts on a rising
//! one.
//!
//! IO2 and IO3 are the part's WP# and HOLD#; they must be held high for
//! the whole transfer or the part ignores or suspends it. Holding them is
//! the caller's job, not this crate's — they carry no edges, so they are
//! setup rather than arithmetic, and there is nothing here for a test to
//! catch about them. (The QSPI peripheral does the same thing implicitly,
//! via `CINSTRCONF.LIO2`/`LIO3`, `embassy-nrf-0.9.0/src/qspi.rs:290`.)
//!
//! # Read-only, by construction
//!
//! Five opcodes, and every one of them is a read or a reset: `05h` RDSR,
//! `9Fh` RDID, `90h` REMS, and the `66h`/`99h` reset pair. No erase, no
//! program, no status-register write — and no `06h` WREN either, so
//! nothing here can even arm a part for a write.
//!
//! That is enforced by shape and not by intent: [`transfer`], the only
//! function that puts an opcode on the wire, is private, and every public
//! function in this file hands it a constant. A caller cannot choose an
//! opcode, so there is no path from outside this crate to one that
//! destroys data. `only_read_and_reset_opcodes_ever_reach_the_wire` is the
//! test that says so against a fake part that records every opcode it is
//! asked.

/// Read Status Register (opcode 0x05). Answers in states where `9Fh` does
/// not — see the module docs.
pub const CMD_READ_STATUS: u8 = 0x05;

/// Read JEDEC ID (opcode 0x9F): manufacturer, memory type, capacity.
pub const CMD_READ_JEDEC_ID: u8 = 0x9F;

/// Read Electronic Manufacturer & Device ID (opcode 0x90, `REMS`): three
/// address bytes out, two id bytes back. A second, independently-decoded
/// way to ask the same question `9Fh` asks.
pub const CMD_READ_MANUFACTURER_DEVICE_ID: u8 = 0x90;

/// Reset Enable (opcode 0x66). Arms the reset; useless on its own.
pub const CMD_RESET_ENABLE: u8 = 0x66;

/// Reset Memory (opcode 0x99). Only honoured immediately after
/// [`CMD_RESET_ENABLE`] — see [`reset`].
pub const CMD_RESET_MEMORY: u8 = 0x99;

/// Write In Progress, bit 0 of the status register on both parts. Set while
/// a program or erase is running, which is exactly the state in which the
/// part does not decode `9Fh`.
pub const STATUS_WIP: u8 = 0x01;

/// Bits in the JEDEC answer: three bytes, MSB first.
const RESPONSE_BITS: usize = 24;

/// Address bytes `90h` carries before its answer. The low bit of the last
/// one selects which of the two id bytes comes first; we send zero, so the
/// manufacturer id is first.
const REMS_ADDRESS: [u8; 3] = [0, 0, 0];

/// The six-wire bus, as much of it as a hand-clocked read touches.
///
/// Four pins carry the transfer — SCK, CS#, IO0 (the part's SI) and IO1
/// (its SO) — and the implementor is responsible for having IO2 and IO3
/// driven high before it hands the bus over, since those are WP# and
/// HOLD#.
///
/// [`Bus::settle`] is one half clock period. Everything here is a
/// diagnostic run once per boot on a board that has already failed to
/// answer, so the implementation should be generous: hundreds of kHz with
/// slack beats anything tuned.
pub trait Bus {
    /// Drive SCK. `true` is the rising edge, `false` the falling one.
    fn set_sck(&mut self, high: bool);
    /// Drive CS#. `false` selects the part.
    fn set_cs(&mut self, high: bool);
    /// Drive IO0, the part's SI.
    fn set_io0(&mut self, high: bool);
    /// Sample IO1, the part's SO. `true` is a high level on the wire, so a
    /// bus nobody drives reads as whatever the pin's pull leaves it at.
    fn read_io1(&mut self) -> bool;
    /// Wait half a clock period.
    fn settle(&mut self);
}

/// Clock out `0x9F` and shift in the three bytes the part answers with.
///
/// Returns `[0, 0, 0]` when nothing on the bus drives IO1 low-to-high —
/// which is a real answer, not an error: it says the wire stayed at its
/// idle level for all 24 clocks. The caller reports the bytes and draws no
/// conclusion here.
///
/// Leaves CS# high and SCK low, the state it started the transfer from.
/// Restoring the *pin configuration* is the caller's job.
pub fn read_jedec_id<B: Bus>(bus: &mut B) -> [u8; 3] {
    let mut id = [0u8; RESPONSE_BITS / 8];
    transfer(bus, CMD_READ_JEDEC_ID, &[], &mut id);
    id
}

/// Clock out `0x05` and shift in the one status byte.
///
/// The question to ask a part that answered nothing to `9Fh`: a part in
/// program/erase does not decode `9Fh` at all but answers this, with
/// [`STATUS_WIP`] set (MX25R1635F rev. 1.6, §10-3). An answer here and
/// silence there is therefore a *state*, not an absence.
pub fn read_status<B: Bus>(bus: &mut B) -> u8 {
    let mut status = [0u8; 1];
    transfer(bus, CMD_READ_STATUS, &[], &mut status);
    status[0]
}

/// Clock out `0x90` with three zero address bytes and shift in the two id
/// bytes: manufacturer, then device.
///
/// The second opinion on the second opinion. `9Fh` and `90h` are decoded
/// separately inside the part, so a part that names itself under one and
/// not the other says something a single opcode cannot.
pub fn read_manufacturer_device_id<B: Bus>(bus: &mut B) -> [u8; 2] {
    let mut id = [0u8; 2];
    transfer(bus, CMD_READ_MANUFACTURER_DEVICE_ID, &REMS_ADDRESS, &mut id);
    id
}

/// The datasheet's software reset: `66h` then `99h`.
///
/// Two instructions, each its own CS# assertion, and **adjacent** — the
/// datasheet is explicit that any instruction between them makes the reset
/// ignored, which is why this is one function and not two exported
/// opcodes. It resets nothing that is stored: the part returns to its
/// power-on state, and a part that was answering nothing because of the
/// state it was parked in starts answering.
///
/// The caller has to wait out the part's reset recovery time (`tREADY1`,
/// 35 us max on the MX25R1635F) before the next instruction — this
/// function only clocks, it does not delay.
pub fn reset<B: Bus>(bus: &mut B) {
    transfer(bus, CMD_RESET_ENABLE, &[], &mut []);
    transfer(bus, CMD_RESET_MEMORY, &[], &mut []);
}

/// One transaction: select, opcode, address bytes, `response.len()` bytes
/// shifted in, deselect.
///
/// **Private, and that is the whole read-only argument** (see the module
/// docs): `opcode` is never a value a caller outside this file supplies.
///
/// `response` is cleared first, so a caller's buffer contents cannot show
/// up in an answer the part never drove.
fn transfer<B: Bus>(bus: &mut B, opcode: u8, address: &[u8], response: &mut [u8]) {
    // Idle: CS# deasserted, clock parked low, and a settle so the part
    // sees a clean level on both before the select.
    bus.set_cs(true);
    bus.set_sck(false);
    bus.settle();

    bus.set_cs(false);
    bus.settle();

    write_byte(bus, opcode);
    for byte in address {
        write_byte(bus, *byte);
    }

    for byte in response.iter_mut() {
        *byte = 0;
    }
    for bit in 0..response.len() * 8 {
        if read_bit(bus) {
            response[bit / 8] |= 0x80 >> (bit % 8);
        }
    }

    bus.set_cs(true);
    bus.settle();
}

/// Eight bits out, MSB first.
fn write_byte<B: Bus>(bus: &mut B, byte: u8) {
    for i in (0..8).rev() {
        write_bit(bus, (byte >> i) & 1 == 1);
    }
}

/// One command bit: present it while SCK is low, then a full clock.
///
/// Ends on the falling edge, which is the edge the part launches its own
/// data on. That is what makes the first [`read_bit`] after the last of
/// these sample the part's first answer bit rather than a clock too early.
fn write_bit<B: Bus>(bus: &mut B, bit: bool) {
    bus.set_io0(bit);
    // Setup time: the level is on IO0 before the part samples it.
    bus.settle();
    bus.set_sck(true);
    bus.settle();
    bus.set_sck(false);
    bus.settle();
}

/// One data bit: sample IO1 at the end of the high phase, then fall.
///
/// Sampling *after* the settle rather than immediately at the edge gives
/// the part a whole half period to have driven the level, which on a bus
/// clocked this slowly is a large margin over any part's output delay.
fn read_bit<B: Bus>(bus: &mut B) -> bool {
    bus.set_sck(true);
    bus.settle();
    let bit = bus.read_io1();
    bus.set_sck(false);
    bus.settle();
    bit
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A falling-edge index no transfer reaches: the part is not going to
    /// drive IO1 at all in this transaction.
    const NO_LAUNCH: usize = usize::MAX;

    /// A NOR part on the other end of four wires, in software.
    ///
    /// It models the two things this crate can get wrong — which edge does
    /// what, and how many bytes go out before the answer comes back — and
    /// the two datasheet states that change what a part answers:
    ///
    /// - [`FakePart::busy`]: a program or erase is running, so `05h`
    ///   answers with [`STATUS_WIP`] set and `9Fh` is not decoded at all
    ///   (MX25R1635F rev. 1.6, §10-3).
    /// - [`FakePart::asleep`]: nothing is answered under any opcode until
    ///   `66h` immediately followed by `99h` reaches it.
    ///
    /// It samples IO0 on rising edges, launches its answer on falling
    /// ones, counts both, and refuses to do anything at all while CS# is
    /// high — so a shifter that clocks a bit too early, too late, or
    /// outside the selection produces a different answer here.
    struct FakePart {
        /// What `9Fh` answers with, when it answers at all.
        jedec: [u8; 3],
        /// What `05h` answers with.
        status: u8,
        /// What `90h` answers with.
        rems: [u8; 2],
        /// Whether anything is on the bus to drive IO1 at all. A `false`
        /// part never launches; the wire keeps [`FakePart::idle`].
        present: bool,
        /// Program/erase in progress: `05h` answers, `9Fh` does not.
        busy: bool,
        /// Answers nothing under any opcode until the reset pair arrives.
        asleep: bool,
        /// `66h` was the last opcode decoded and nothing has run since.
        reset_armed: bool,
        /// Level IO1 sits at before the part drives it, and whenever it is
        /// not driving. A real deselected part leaves the wire to its pull.
        idle: bool,
        cs_high: bool,
        sck_high: bool,
        io0: bool,
        /// Rising edges seen since select — the part's sample clock.
        rising: usize,
        /// Falling edges seen since select — the part's launch clock.
        falling: usize,
        /// Command byte as sampled off IO0, MSB first. Survives the
        /// deselect so a test can read it after the transfer.
        command: u8,
        /// Address bytes as sampled off IO0 after the command byte. Only
        /// meaningful for the last opcode that carried any; also survives
        /// the deselect.
        address: [u8; 3],
        /// Address bytes the current opcode carries. A property of the
        /// opcode, so the part knows where the address ends and the
        /// master's idle read clocks begin.
        address_len: usize,
        /// Every opcode this part has been asked, in order.
        seen: [u8; 16],
        seen_len: usize,
        /// What the part will answer the current command with, if
        /// anything, MSB first.
        response: [u8; 4],
        response_bytes: usize,
        /// Falling edge on which the first response bit is launched, or
        /// [`NO_LAUNCH`].
        launch_falling: usize,
        /// What the part is presenting on IO1 right now.
        so: bool,
        /// Edges clocked while CS# was high. Any is a bug in the shifter.
        edges_while_deselected: usize,
        /// Half periods waited. Only ever asserted as "nonzero".
        settles: usize,
    }

    impl FakePart {
        fn new(jedec: [u8; 3]) -> Self {
            Self {
                jedec,
                status: 0,
                rems: [0, 0],
                present: true,
                busy: false,
                asleep: false,
                reset_armed: false,
                idle: false,
                cs_high: true,
                sck_high: false,
                io0: false,
                rising: 0,
                falling: 0,
                command: 0,
                address: [0; 3],
                address_len: 0,
                seen: [0; 16],
                seen_len: 0,
                response: [0; 4],
                response_bytes: 0,
                launch_falling: NO_LAUNCH,
                so: false,
                edges_while_deselected: 0,
                settles: 0,
            }
        }

        /// A part that is absent, or unpowered, or simply not wired to
        /// these pins: the wire keeps whatever level it idles at.
        fn silent(idle: bool) -> Self {
            let mut part = Self::new([0, 0, 0]);
            part.present = false;
            part.idle = idle;
            part.so = idle;
            part
        }

        /// A part with a program or erase in progress. `05h` answers with
        /// WIP set; `9Fh` is not decoded (rev. 1.6, §10-3).
        fn busy(jedec: [u8; 3]) -> Self {
            let mut part = Self::new(jedec);
            part.busy = true;
            part.status = STATUS_WIP;
            part
        }

        /// A part parked in a state that answers nothing until `66h`/`99h`.
        fn asleep(jedec: [u8; 3]) -> Self {
            let mut part = Self::new(jedec);
            part.asleep = true;
            part
        }

        fn response_bit(&self, index: usize) -> bool {
            self.response[index / 8] & (0x80 >> (index % 8)) != 0
        }

        /// The eighth rising edge has arrived: the command byte is
        /// complete. Decode it and stage whatever it answers with.
        fn begin_response(&mut self) {
            let cmd = self.command;
            if self.seen_len < self.seen.len() {
                self.seen[self.seen_len] = cmd;
                self.seen_len += 1;
            }

            // How many bytes the master sends before the answer is a
            // property of the opcode, and the part knows it even in states
            // where it answers nothing.
            self.address_len = if cmd == CMD_READ_MANUFACTURER_DEVICE_ID {
                3
            } else {
                0
            };

            // The reset pair, and the datasheet's rule that any
            // instruction between the two makes the reset ignored.
            let armed = self.reset_armed;
            self.reset_armed = cmd == CMD_RESET_ENABLE;
            if cmd == CMD_RESET_MEMORY && armed {
                self.asleep = false;
            }

            self.response = [0; 4];
            self.response_bytes = 0;
            self.launch_falling = NO_LAUNCH;
            if !self.present || self.asleep {
                return;
            }
            match cmd {
                CMD_READ_STATUS => {
                    self.response[0] = self.status;
                    self.response_bytes = 1;
                }
                // §10-3: a part in program/erase does not decode this one.
                CMD_READ_JEDEC_ID if !self.busy => {
                    self.response[..3].copy_from_slice(&self.jedec);
                    self.response_bytes = 3;
                }
                CMD_READ_MANUFACTURER_DEVICE_ID => {
                    self.response[..2].copy_from_slice(&self.rems);
                    self.response_bytes = 2;
                }
                _ => return,
            }
            // The falling edge that ends the last byte the master sends
            // carries the first answer bit.
            self.launch_falling = 8 + self.address_len * 8;
        }
    }

    impl Bus for FakePart {
        fn set_sck(&mut self, high: bool) {
            if high == self.sck_high {
                return;
            }
            self.sck_high = high;
            if self.cs_high {
                self.edges_while_deselected += 1;
                return;
            }
            if high {
                // Rising: the part samples SI. The first eight bits are
                // the command; the next `address_len` bytes are address;
                // anything after is the master's business.
                if self.rising < 8 {
                    self.command = (self.command << 1) | u8::from(self.io0);
                    self.rising += 1;
                    if self.rising == 8 {
                        self.begin_response();
                    }
                    return;
                }
                let index = self.rising - 8;
                if index < self.address_len * 8 {
                    let byte = &mut self.address[index / 8];
                    *byte = (*byte << 1) | u8::from(self.io0);
                }
                self.rising += 1;
            } else {
                self.falling += 1;
                // Falling: the part launches.
                if self.launch_falling != NO_LAUNCH && self.falling >= self.launch_falling {
                    let index = self.falling - self.launch_falling;
                    self.so = if index < self.response_bytes * 8 {
                        self.response_bit(index)
                    } else {
                        self.idle
                    };
                }
            }
        }

        fn set_cs(&mut self, high: bool) {
            if high != self.cs_high {
                // Either edge resets the part's view of the transfer and
                // releases the wire; a deselect also ends the transaction.
                self.rising = 0;
                self.falling = 0;
                self.launch_falling = NO_LAUNCH;
                self.so = self.idle;
            }
            self.cs_high = high;
        }

        fn set_io0(&mut self, high: bool) {
            self.io0 = high;
        }

        fn read_io1(&mut self) -> bool {
            if self.cs_high {
                self.idle
            } else {
                self.so
            }
        }

        fn settle(&mut self) {
            self.settles += 1;
        }
    }

    #[test]
    fn reads_back_the_id_the_part_replays() {
        let mut part = FakePart::new([0xC2, 0x28, 0x15]);
        assert_eq!(read_jedec_id(&mut part), [0xC2, 0x28, 0x15]);
    }

    #[test]
    fn sends_the_jedec_opcode_msb_first() {
        let mut part = FakePart::new([0x9D, 0x60, 0x14]);
        read_jedec_id(&mut part);
        assert_eq!(part.command, 0x9F);
    }

    /// The ISSI part too, so the test is on the shifter and not on one
    /// byte pattern that happens to survive an off-by-one.
    #[test]
    fn reads_back_the_other_boards_part() {
        let mut part = FakePart::new([0x9D, 0x60, 0x14]);
        assert_eq!(read_jedec_id(&mut part), [0x9D, 0x60, 0x14]);
    }

    /// Every bit position, alone. A shifter that samples one edge early or
    /// late, or fills the bytes in the wrong order, moves the set bit and
    /// this catches it wherever it lands — including at the seam between
    /// the command byte and the answer, which is where the edge that
    /// matters is.
    #[test]
    fn every_single_bit_lands_where_it_was_sent() {
        for bit in 0..RESPONSE_BITS {
            let mut answer = [0u8; 3];
            answer[bit / 8] = 0x80 >> (bit % 8);
            let mut part = FakePart::new(answer);
            assert_eq!(
                read_jedec_id(&mut part),
                answer,
                "bit {bit} did not come back in its own place"
            );
        }
    }

    /// A part that answers all-ones is not the same reading as a bus at
    /// rest, and both have to survive the shifter unchanged: `ff:ff:ff` is
    /// what a pulled-up undriven wire looks like, and telling it from a
    /// real id is the whole point of the line this feeds.
    #[test]
    fn saturated_answers_survive() {
        let mut part = FakePart::new([0xFF, 0xFF, 0xFF]);
        assert_eq!(read_jedec_id(&mut part), [0xFF, 0xFF, 0xFF]);
    }

    #[test]
    fn a_bus_nobody_drives_reads_as_its_idle_level() {
        let mut low = FakePart::silent(false);
        assert_eq!(read_jedec_id(&mut low), [0x00, 0x00, 0x00]);

        let mut high = FakePart::silent(true);
        assert_eq!(read_jedec_id(&mut high), [0xFF, 0xFF, 0xFF]);
    }

    /// The part is selected for the whole transfer and for nothing else.
    /// A clock edge outside the selection is a bug the fake counts, and
    /// the transfer has to end deselected so the next driver on these pins
    /// starts from the state this one did.
    #[test]
    fn clocks_only_while_selected_and_leaves_the_bus_idle() {
        let mut part = FakePart::new([0xC2, 0x28, 0x15]);
        read_jedec_id(&mut part);
        assert_eq!(part.edges_while_deselected, 0);
        assert!(part.cs_high, "transfer must end with the part deselected");
        assert!(
            !part.sck_high,
            "transfer must end with the clock parked low"
        );
    }

    /// Exactly 32 clocks: eight of command, 24 of answer. One more or one
    /// fewer is the off-by-one this whole file exists to catch, and the
    /// count is checked before the deselect resets it.
    #[test]
    fn clocks_the_transfer_exactly_once() {
        struct Counting<'a> {
            part: &'a mut FakePart,
            rising_at_last_sample: usize,
        }

        impl Bus for Counting<'_> {
            fn set_sck(&mut self, high: bool) {
                self.part.set_sck(high);
            }
            fn set_cs(&mut self, high: bool) {
                if high && !self.part.cs_high {
                    self.rising_at_last_sample = self.part.rising;
                }
                self.part.set_cs(high);
            }
            fn set_io0(&mut self, high: bool) {
                self.part.set_io0(high);
            }
            fn read_io1(&mut self) -> bool {
                self.part.read_io1()
            }
            fn settle(&mut self) {
                self.part.settle();
            }
        }

        let mut part = FakePart::new([0xC2, 0x28, 0x15]);
        let mut counting = Counting {
            part: &mut part,
            rising_at_last_sample: 0,
        };
        read_jedec_id(&mut counting);
        assert_eq!(counting.rising_at_last_sample, 8 + RESPONSE_BITS);
    }

    /// Every edge is separated from the next by a half period. Without
    /// this the shifter would still be arithmetically right and would
    /// still fail on a board, because the levels would never settle.
    #[test]
    fn waits_between_every_edge() {
        let mut part = FakePart::new([0xC2, 0x28, 0x15]);
        read_jedec_id(&mut part);
        // 2 for the idle-then-select preamble, 3 per command bit,
        // 2 per answer bit, 1 for the deselect.
        assert_eq!(part.settles, 2 + 8 * 3 + RESPONSE_BITS * 2 + 1);
    }

    #[test]
    fn reads_the_status_register() {
        let mut part = FakePart::new([0xC2, 0x28, 0x15]);
        part.status = 0x42;
        assert_eq!(read_status(&mut part), 0x42);
        assert_eq!(part.command, CMD_READ_STATUS);
    }

    /// Every status bit in its own place. One byte instead of three, but
    /// the same defect it catches: the status read shares the seam between
    /// command and answer with the id read, and a WIP bit that arrived one
    /// position off would read as a part that is idle.
    #[test]
    fn every_status_bit_lands_where_it_was_sent() {
        for bit in 0..8 {
            let mut part = FakePart::new([0xC2, 0x28, 0x15]);
            part.status = 0x80 >> bit;
            assert_eq!(
                read_status(&mut part),
                0x80 >> bit,
                "status bit {bit} did not come back in its own place"
            );
        }
    }

    /// `90h` sends three address bytes before it reads. Get that count
    /// wrong and the two id bytes come back shifted by a whole byte, which
    /// is exactly the silent-plausible-answer failure this crate exists
    /// for — so the address is asserted as well as the answer.
    #[test]
    fn rems_sends_three_zero_address_bytes_then_reads_two() {
        let mut part = FakePart::new([0xC2, 0x28, 0x15]);
        part.rems = [0xC2, 0x14];
        assert_eq!(read_manufacturer_device_id(&mut part), [0xC2, 0x14]);
        assert_eq!(part.command, CMD_READ_MANUFACTURER_DEVICE_ID);
        assert_eq!(part.address, [0x00, 0x00, 0x00]);
    }

    /// Every REMS bit, alone — the same sweep as the id read, across the
    /// longer address phase where an off-by-one has more room to hide.
    #[test]
    fn every_rems_bit_lands_where_it_was_sent() {
        for bit in 0..16 {
            let mut answer = [0u8; 2];
            answer[bit / 8] = 0x80 >> (bit % 8);
            let mut part = FakePart::new([0xC2, 0x28, 0x15]);
            part.rems = answer;
            assert_eq!(
                read_manufacturer_device_id(&mut part),
                answer,
                "REMS bit {bit} did not come back in its own place"
            );
        }
    }

    /// The first pre-registered reading in `leviculum_nrf::qspi`'s PROBE
    /// table: `rdsr` answers and `rdid` does not, which is a part busy
    /// with a program or erase rather than an absent one (MX25R1635F
    /// rev. 1.6, §10-3). Without this the firmware's busy row would be
    /// decoration.
    #[test]
    fn a_busy_part_answers_the_status_read_and_not_the_id_read() {
        let mut part = FakePart::busy([0xC2, 0x28, 0x15]);
        let status = read_status(&mut part);
        assert_eq!(
            status & STATUS_WIP,
            STATUS_WIP,
            "a busy part has to report WIP, or the row cannot be recognised"
        );
        assert_eq!(
            read_jedec_id(&mut part),
            [0x00, 0x00, 0x00],
            "§10-3: a part in program/erase does not decode 9Fh"
        );
        // And the other identity opcode still answers, so "silent to 9Fh"
        // is not read as "silent".
        part.rems = [0xC2, 0x14];
        assert_eq!(read_manufacturer_device_id(&mut part), [0xC2, 0x14]);
    }

    /// The second pre-registered reading: a part that answers nothing at
    /// all until `66h`/`99h`, and answers afterwards. This is the row the
    /// whole reset stage exists for.
    #[test]
    fn a_part_that_only_wakes_after_the_reset_pair() {
        let mut part = FakePart::asleep([0xC2, 0x28, 0x15]);
        assert_eq!(read_status(&mut part), 0x00, "silent before the reset");
        assert_eq!(read_jedec_id(&mut part), [0x00, 0x00, 0x00]);
        assert_eq!(read_manufacturer_device_id(&mut part), [0x00, 0x00]);

        reset(&mut part);

        part.status = 0x02;
        part.rems = [0xC2, 0x14];
        assert_eq!(read_status(&mut part), 0x02, "awake after the reset");
        assert_eq!(read_jedec_id(&mut part), [0xC2, 0x28, 0x15]);
        assert_eq!(read_manufacturer_device_id(&mut part), [0xC2, 0x14]);
    }

    /// `66h` and `99h` have to be adjacent: the datasheet says any
    /// instruction between them makes the reset ignored, so a [`reset`]
    /// that grew a status read in the middle would look like it worked on
    /// a board that quietly stayed asleep.
    #[test]
    fn a_command_between_the_reset_pair_makes_the_reset_ignored() {
        let mut part = FakePart::asleep([0xC2, 0x28, 0x15]);
        transfer(&mut part, CMD_RESET_ENABLE, &[], &mut []);
        read_status(&mut part);
        transfer(&mut part, CMD_RESET_MEMORY, &[], &mut []);
        assert_eq!(
            read_jedec_id(&mut part),
            [0x00, 0x00, 0x00],
            "an instruction between 66h and 99h makes the reset ignored"
        );
    }

    /// `99h` on its own does nothing: without the `66h` that arms it the
    /// part stays exactly as it was.
    #[test]
    fn the_reset_opcode_alone_does_nothing() {
        let mut part = FakePart::asleep([0xC2, 0x28, 0x15]);
        transfer(&mut part, CMD_RESET_MEMORY, &[], &mut []);
        assert_eq!(read_jedec_id(&mut part), [0x00, 0x00, 0x00]);
    }

    /// The reset pair is two transactions, each properly framed: no clock
    /// edge outside a selection, and the bus left idle.
    #[test]
    fn the_reset_pair_is_two_framed_transactions() {
        let mut part = FakePart::new([0xC2, 0x28, 0x15]);
        reset(&mut part);
        assert_eq!(
            &part.seen[..part.seen_len],
            &[CMD_RESET_ENABLE, CMD_RESET_MEMORY][..]
        );
        assert_eq!(part.edges_while_deselected, 0);
        assert!(part.cs_high);
        assert!(!part.sck_high);
    }

    /// The read-only guarantee, asserted rather than asserted-about: run
    /// every public function this crate has and look at every opcode that
    /// reached the wire. A write, erase or write-enable opcode appearing
    /// here is the failure.
    #[test]
    fn only_read_and_reset_opcodes_ever_reach_the_wire() {
        let mut part = FakePart::new([0xC2, 0x28, 0x15]);
        reset(&mut part);
        read_status(&mut part);
        read_jedec_id(&mut part);
        read_manufacturer_device_id(&mut part);
        assert_eq!(
            &part.seen[..part.seen_len],
            &[
                CMD_RESET_ENABLE,
                CMD_RESET_MEMORY,
                CMD_READ_STATUS,
                CMD_READ_JEDEC_ID,
                CMD_READ_MANUFACTURER_DEVICE_ID,
            ][..]
        );
        // Named explicitly, because these are the ones that would destroy
        // a part that has somebody else's firmware on it.
        for &opcode in &part.seen[..part.seen_len] {
            assert!(
                !matches!(
                    opcode,
                    0x01 | 0x02 | 0x06 | 0x20 | 0x52 | 0x60 | 0xC7 | 0xD8
                ),
                "opcode {opcode:#04x} writes, erases or arms a write"
            );
        }
    }
}
