//! 1603: a rebuild of the ring asks the heap for no block that grows with
//! the ring.
//!
//! Window 22 (bench image of 51b80f57): `bench forget` erased the summary
//! log and the reopen rebuilt the ring of 4362 entries; twelve seconds in,
//! core 0 panicked on `memory allocation of 278528 bytes failed`. That
//! block is 2048 x 136 B, the long chat's `Vec<Message>` doubling past its
//! 1024th message (`Message` is 136 B on the image), and it failed because
//! the forget path held a second whole model in the IO pool meanwhile
//! (`Handheld::bench_release`, `Handheld::bench_reopen`). A chat's own
//! length is the model's shape, the live pull's as well; what may not be
//! there is a block that grows with the ring whatever the chats are.
//! Before 1603 the rebuild had two: the batch's note scope, one peer per
//! folded entry (`Batch::fold`, 8192 x 16 B), and the toasts, one per
//! received message, which a board that never ticks never ages out
//! (`UiModel::toast`, 4096 of them).
//!
//! The rebuild here is the board's (`home::restore` with the summary log
//! erased, as `home::forget` leaves it), under the firmware's TLSF heap
//! (`leviculum-heap-lab`), its largest single block counted. Over
//! [`CHATS`] conversations: the model's own `Vec<Conversation>` grows with
//! their count (512 of 560 B on the image for the window's 302, 512 of
//! 808 B here), a block of the model's shape that the bound would
//! otherwise measure instead of the ring's.

use std::sync::Mutex;

use leviculum_core::Identity;
use leviculum_heap_lab::{largest, live, peak, reset_counts, spilled, with, Which};
use leviculum_lxmf::{DeliveryMethod, Message};
use loquax::home::board::rows;
use loquax::home::board::store::Sectors;
use loquax::home::wire::Entry;
use loquax::home::{lxmf_address, KIND_RECEIVED, KIND_SENT};
use loquax_m9::home;
use rand_core::OsRng;

/// The lab's counters are global: one measurement at a time.
static ONE: Mutex<()> = Mutex::new(());

/// The bound on any one block of a rebuild whose chats are short.
const BOUND: usize = 64 * 1024;

/// The conversations of the ring: 64 of the model's 808 B (host) fit the
/// bound, so what breaks it grows with the ring.
const CHATS: usize = 60;

/// The M9's `messages` partition: 4 MiB of 4 KiB sectors
/// (`partitions-m9.csv`), the ring and the summary log.
const SECTORS: u32 = 1024;

/// A flash in memory, as the part takes it.
struct Flash {
    sectors: Vec<Vec<u8>>,
}

impl Flash {
    fn new(count: u32) -> Self {
        Self {
            sectors: vec![vec![0xFF; 4096]; count as usize],
        }
    }
}

impl Sectors for Flash {
    type Error = ();

    fn count(&self) -> u32 {
        self.sectors.len() as u32
    }

    fn read(&mut self, sector: u32, offset: usize, buf: &mut [u8]) -> Result<(), ()> {
        let src = self.sectors.get(sector as usize).ok_or(())?;
        buf.copy_from_slice(src.get(offset..offset + buf.len()).ok_or(())?);
        Ok(())
    }

    fn write(&mut self, sector: u32, offset: usize, data: &[u8]) -> Result<(), ()> {
        let dst = self.sectors.get_mut(sector as usize).ok_or(())?;
        let dst = dst.get_mut(offset..offset + data.len()).ok_or(())?;
        if dst.iter().any(|&b| b != 0xFF) {
            return Err(());
        }
        dst.copy_from_slice(data);
        Ok(())
    }

    fn erase(&mut self, sector: u32) -> Result<(), ()> {
        self.sectors.get_mut(sector as usize).ok_or(())?.fill(0xFF);
        Ok(())
    }
}

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

/// A home's ring of `total` entries over [`CHATS`] conversations, the first
/// `long` of them in one chat, the rest round the others; one in five
/// ours. Kept page by page as the pull keeps it, then the summary log
/// erased, as `bench forget` leaves it: the next boot rebuilds.
fn forgotten(total: u64, long: u64) -> (Flash, [u8; 16]) {
    let me = Identity::generate(&mut OsRng);
    let address = lxmf_address(&me.public_key_bytes()).expect("address");
    let friends: Vec<Identity> = (0..CHATS).map(|_| Identity::generate(&mut OsRng)).collect();
    let entries: Vec<Entry> = (1..=total)
        .map(|seq| {
            let friend = match seq <= long {
                true => &friends[0],
                false => &friends[1 + (seq as usize % (CHATS - 1))],
            };
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
    let mut flash = Flash::new(SECTORS);
    let mut boot = home::restore(&mut flash, address).expect("boot");
    for page in entries.chunks(72) {
        home::keep(&mut boot.shelf, &mut flash, page, &address).expect("keep");
    }
    drop(boot);
    let (ring, summary) = rows::split(SECTORS).expect("split");
    for sector in ring..ring + summary {
        flash.erase(sector).expect("erase");
    }
    (flash, address)
}

/// The rebuild under the TLSF lab heap: its largest single block, the
/// most it held at once, and what the boot it made holds.
fn rebuilt(flash: &mut Flash, address: [u8; 16]) -> (usize, usize, usize) {
    let before = spilled();
    let out = with(Which::Tlsf, || {
        let base = live();
        reset_counts();
        let boot = home::restore(flash, address).expect("restore");
        assert_eq!(boot.how.word(), "rebuilt", "the forgotten summary rebuilds");
        let held = live() - base;
        let out = (largest(), peak() - base, held);
        drop(boot);
        out
    });
    assert_eq!(spilled(), before, "the lab heap overflowed its region");
    out
}

#[test]
fn a_rebuild_of_short_chats_asks_for_no_block_that_grows_with_the_ring() {
    let _one = ONE.lock().unwrap_or_else(|e| e.into_inner());
    let (mut small, address) = forgotten(1100, 0);
    let (small_largest, _, _) = rebuilt(&mut small, address);
    let (mut flash, address) = forgotten(4400, 0);
    let (largest, peak, held) = rebuilt(&mut flash, address);
    println!(
        "[1603] rebuild short chats: 1100 entries largest={small_largest} \
         4400 entries largest={largest} peak={peak} held={held}"
    );
    assert!(
        largest <= BOUND,
        "a rebuild of 4400 entries over {CHATS} chats asked for one block of \
         {largest} B (bar {BOUND}; 1100 entries: {small_largest})"
    );
}

/// The window's shape: the long chat of the 1489 store (1200 messages)
/// in a ring of 4400. Its `Vec<Message>` is the largest block, and only
/// it: the chat's length, not the ring's. Said for the report, with what
/// the boot holds (host sizes, a 64-bit `Message`).
#[test]
fn a_rebuild_with_a_long_chat_asks_for_that_chat_alone() {
    let _one = ONE.lock().unwrap_or_else(|e| e.into_inner());
    let (mut flash, address) = forgotten(4400, 1200);
    let (largest, peak, held) = rebuilt(&mut flash, address);
    let message = std::mem::size_of::<loquax::ui::Message>();
    println!(
        "[1603] rebuild long chat: largest={largest} message={message} \
         peak={peak} held={held}"
    );
    assert_eq!(
        largest,
        2048 * message,
        "the largest block is the long chat's messages, 2048 of {message} B"
    );
}
