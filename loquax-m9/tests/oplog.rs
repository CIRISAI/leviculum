//! 1622: the bench image's `[FLASH]` ring says every op of a forget's
//! turn, and counts what it cannot. Window 24's forget (128 erases)
//! came out as 7 `[FLASH]` lines against 551 `park_ops`: the ring of 32
//! is drained once per loop turn, and a forget and the reopen's rebuild
//! are one turn, so everything past the turn's 32nd op was overwritten
//! unsaid.
//!
//! Here the real forget and the real rebuild run over a store shaped like
//! the 1489 fixture (300 conversations, 4000 messages) in a flash of the
//! board's 896 sectors, every op kept as the board keeps it (an erase is
//! one op, a write one op per 512-byte bounce chunk, `leviculum_esp::
//! flash::Messages`), and the turn drained once: the old ring loses most
//! of it and says so, the bench image's ring loses none.

use std::cell::Cell;

use leviculum_core::Identity;
use leviculum_lxmf::{DeliveryMethod, Message};
use loquax::home::board::store::{MemFlash, Sectors};
use loquax::home::wire::Entry;
use loquax::home::{lxmf_address, KIND_RECEIVED, KIND_SENT};
use loquax_m9::bench::{self, Arrivals};
use loquax_m9::home;
use loquax_m9::oplog::{Op, OpLog, KEEP};
use rand_core::OsRng;

/// The board's message store (`[STORE] sectors=896`).
const SECTORS: u32 = 896;
const CONVERSATIONS: usize = 300;
const MESSAGES: u64 = 4000;
/// The bytes one flash call moves on the board (`flash.rs`, `BOUNCE`).
const BOUNCE: usize = 512;

fn pack(from: &Identity, to: [u8; 16], body: &str, at: f64) -> Vec<u8> {
    let source = lxmf_address(&from.public_key_bytes()).expect("address");
    Message::create(
        to,
        source,
        from,
        at,
        Vec::new(),
        body.as_bytes().to_vec(),
        Vec::new(),
        DeliveryMethod::Direct,
    )
    .expect("create")
    .pack()
}

/// The ring before 1622 and the bench image's, fed the same ops.
struct Logs {
    old: OpLog<32>,
    now: OpLog<KEEP>,
    clock: Cell<u32>,
}

impl Logs {
    fn keep(&self, op: Op) {
        let at = self.clock.get();
        self.clock.set(at + 1);
        self.old.keep(op, 1, at);
        self.now.keep(op, 1, at);
    }
}

/// The store as the board's flash ops see it: every erase and every
/// bounce chunk of a write one record.
struct Counted<'a> {
    flash: &'a mut MemFlash,
    logs: &'a Logs,
}

impl Sectors for Counted<'_> {
    type Error = <MemFlash as Sectors>::Error;

    fn count(&self) -> u32 {
        self.flash.count()
    }

    fn read(&mut self, sector: u32, offset: usize, buf: &mut [u8]) -> Result<(), Self::Error> {
        self.flash.read(sector, offset, buf)
    }

    fn write(&mut self, sector: u32, offset: usize, data: &[u8]) -> Result<(), Self::Error> {
        for _ in data.chunks(BOUNCE) {
            self.logs.keep(Op::Write);
        }
        self.flash.write(sector, offset, data)
    }

    fn erase(&mut self, sector: u32) -> Result<(), Self::Error> {
        self.logs.keep(Op::Erase);
        self.flash.erase(sector)
    }
}

/// Drain `log` once, as the loop's next turn does: (said, erases said,
/// lost).
fn drain<const N: usize>(log: &OpLog<N>) -> (u32, u32, u32) {
    let mut said = 0;
    let mut erases = 0;
    let lost = log.take(|op, _, _| {
        said += 1;
        if op == Op::Erase {
            erases += 1;
        }
    });
    (said, erases, lost)
}

#[test]
fn a_forget_and_its_rebuild_fit_the_bench_ring_and_the_old_ring_counts_its_loss() {
    let me = Identity::generate(&mut OsRng);
    let address = lxmf_address(&me.public_key_bytes()).expect("address");
    let friends: Vec<Identity> = (0..CONVERSATIONS)
        .map(|_| Identity::generate(&mut OsRng))
        .collect();
    let entries: Vec<Entry> = (1..=MESSAGES)
        .map(|seq| {
            let friend = &friends[seq as usize % CONVERSATIONS];
            let theirs = lxmf_address(&friend.public_key_bytes()).expect("address");
            let at = 1_762_000_000.0 + seq as f64;
            let (kind, packed) = match seq % 5 {
                0 => (KIND_SENT, pack(&me, theirs, &format!("Antwort {seq}"), at)),
                _ => (
                    KIND_RECEIVED,
                    pack(friend, address, &format!("Nachricht {seq}"), at),
                ),
            };
            Entry {
                seq,
                kind,
                at: at as i64,
                packed,
            }
        })
        .collect();

    // The board before the walk's forget: the pull kept page by page,
    // then the bench's arrivals.
    let mut flash = MemFlash::new(SECTORS);
    let home::Boot {
        mut shelf,
        mut model,
        ..
    } = home::restore_shown(&mut flash, address, &|| 0, Arrivals::Shown).expect("boot");
    for page in entries.chunks(72) {
        home::keep(&mut shelf, &mut flash, page, &address).expect("keep");
    }
    for n in 1..=50 {
        let arrival = bench::arrival(&model);
        let arrived = bench::entries(address, arrival, n, shelf.cursor());
        home::keep(&mut shelf, &mut flash, &arrived, &address).expect("keep");
        let _ = home::take(&mut model, &arrived, &address);
    }
    shelf.flush(&mut flash, Vec::new).expect("flush");
    drop(shelf);
    drop(model);

    // `bench forget`: the forget and the reopen's rebuild, one turn.
    let logs = Logs {
        old: OpLog::new(),
        now: OpLog::new(),
        clock: Cell::new(0),
    };
    let mut dev = Counted {
        flash: &mut flash,
        logs: &logs,
    };
    let forgot = home::forget(&mut dev, address, &|| ()).expect("forget");
    let forget_ops = logs.clock.get();
    let reopened = home::restore_chunked(&mut dev, address, &|| 0, Arrivals::Shown, &mut |_| {})
        .expect("reopen");
    assert!(
        matches!(reopened.how, home::How::Rebuilt { .. }),
        "the forget erased the summary: {:?}",
        reopened.how
    );
    let ops = logs.clock.get();

    let (old_said, old_erases, old_lost) = drain(&logs.old);
    let (said, erases, lost) = drain(&logs.now);
    eprintln!(
        "forget sectors={} summary_erased={} forget_ops={forget_ops} turn_ops={ops} \
         old: said={old_said} erases={old_erases} lost={old_lost} \
         keep={KEEP}: said={said} erases={erases} lost={lost}",
        forgot.store.sectors, forgot.summary_erased
    );

    // The positive control: the old ring says its last 32 and counts the
    // rest, where it said nothing of them before.
    assert_eq!(old_said, 32);
    assert_eq!(old_lost, ops - 32);
    assert_eq!(logs.old.lost(), ops - 32);
    // The bench image's ring holds the whole turn.
    assert!(ops as usize <= KEEP, "a forget turn of {ops} ops");
    assert_eq!((said, lost), (ops, 0));
    assert_eq!(logs.now.lost(), 0);
    // Every erase the turn made is a line.
    assert!(erases >= forgot.summary_erased, "{erases} erases said");
}

#[test]
fn a_turn_past_the_ring_says_the_newest_and_counts_the_rest() {
    let log: OpLog<KEEP> = OpLog::new();
    let ops = KEEP as u32 + 5;
    for at in 0..ops {
        log.keep(Op::Erase, 40_000, at);
    }
    let mut first = None;
    let mut said = 0;
    let lost = log.take(|_, _, at| {
        first.get_or_insert(at);
        said += 1;
    });
    assert_eq!((said, lost, first), (KEEP as u32, 5, Some(5)));
    // The next turn starts clean.
    log.keep(Op::Write, 70, ops);
    let mut next = Vec::new();
    assert_eq!(log.take(|op, us, at| next.push((op, us, at))), 0);
    assert_eq!(next, vec![(Op::Write, 70, ops)]);
    assert_eq!(log.lost(), 5);
}

/// 1630: window 25's forget (550 `park_ops`) drained in one call wrote
/// 550 `[FLASH]` lines into a send ring of 8 KiB and 512 records, which
/// the UART empties at 11.5 bytes a millisecond, and the ring dropped
/// 388. Bounded by the ring's room, the drain says the same 550 records
/// over several turns, oldest first, every one of them, and the ring a
/// model of the board's never drops.
#[test]
fn a_forget_drains_whole_over_turns_and_the_send_ring_drops_nothing() {
    use leviculum_serial_mux::tx::{Kind, TxRing};
    use loquax_m9::oplog::{sayable, Body, RECORD_MAX};

    const TX_RING: usize = 8 * 1024;
    const TX_RECORDS: usize = 512;
    let log: OpLog<KEEP> = OpLog::new();
    let ops = 550u32;
    for at in 0..ops {
        let op = if at % 5 == 0 { Op::Erase } else { Op::Write };
        log.keep(op, 40_000 + at, at);
    }
    let mut ring: Box<TxRing<TX_RING, TX_RECORDS>> = Box::default();
    let mut said = Vec::new();
    let mut turns = 0;
    while log.waiting() > 0 {
        turns += 1;
        assert!(turns < 1000, "the drain never finished");
        let free_bytes = TX_RING - ring.len();
        let free_records = TX_RECORDS - ring.records();
        let n = sayable(free_bytes, free_records);
        let lost = log.take_at_most(n, |op, us, at| {
            // The worst-case stamp: the line is at most RECORD_MAX.
            let line = format!(
                "\r\n[FLASH] {} t={}\r\n",
                Body { op, us, at_ms: at },
                u64::MAX
            );
            assert!(line.len() <= RECORD_MAX, "{} bytes", line.len());
            assert!(ring.push(Kind::Line, line.as_bytes()));
            said.push(at);
        });
        assert_eq!(lost, 0);
        // The loop's own lossy lines: a window's `[IDLE]`, `[UART]` and
        // `[LOOP]` lines and a frame, 800 bytes once a second (every 50th
        // turn), under the reserve.
        if turns % 50 == 1 {
            for _ in 0..8 {
                assert!(ring.push(Kind::Line, &[b'x'; 100]));
            }
        }
        // A turn of 20 ms: 230 bytes of the line.
        let mut carried = 0;
        while carried < 230 && !ring.is_empty() {
            let n = ring.peek().len().min(230 - carried);
            ring.consume(n);
            carried += n;
        }
    }
    assert_eq!(said, (0..ops).collect::<Vec<_>>());
    assert_eq!(ring.dropped(), 0);
    assert_eq!(log.lost(), 0);
    // A ring with less room than the reserve gets nothing.
    assert_eq!(sayable(4 * 1024, 512), 0);
    assert_eq!(sayable(8 * 1024, 256), 0);
}

/// The longest body the board can say fits [`RECORD_MAX`] with every
/// field at its widest.
#[test]
fn the_widest_flash_line_fits_the_record_bound() {
    use loquax_m9::oplog::{Body, RECORD_MAX};
    let body = Body {
        op: Op::Erase,
        us: (1 << 31) - 1,
        at_ms: u32::MAX,
    };
    let line = format!("\r\n[FLASH] {body} t={}\r\n", u64::MAX);
    assert_eq!(line.len(), RECORD_MAX);
}
