//! A simulated nRF52840 internal-flash page range, driven through the
//! SoftDevice, with the semantics that actually bite.
//!
//! Both store candidates for #384 run over *this* device and no other, so a
//! difference between them is a difference in the store rather than in what
//! it was measured against.
//!
//! # What it models, and why each one is a bug this crate could ship
//!
//! 1. **Erase sets `0xFF`, programming only clears bits.** A program that
//!    would raise a bit panics rather than quietly doing what RAM would do,
//!    because on the real part it would leave the word at its old value and
//!    the store would read back something it never wrote.
//! 2. **Word writes, 4 bytes, sector-aligned erases of 4 KiB.** The
//!    SoftDevice's `sd_flash_write` takes a word count and a word-aligned
//!    address (S140 SDS, SoC library); `nrf_softdevice::Flash` therefore
//!    publishes `WRITE_SIZE = 4` and `ERASE_SIZE = 4096`. Reads have no such
//!    rule — flash is memory-mapped and a read is a `memcpy` — so
//!    [`SimNor`] takes `READ_SIZE = 1` and asserts nothing about a read.
//! 3. **At most two writes per word between erases** (nRF52840 Product
//!    Specification, NVMC chapter). This is the constraint that separates
//!    the internal flash from the QSPI part this crate was first written
//!    for, and [`SimNor::max_word_writes`] is what turns it from an
//!    argument into a number. A store that needs a third write to a word to
//!    withdraw an entry is not a candidate for this part.
//! 4. **Power cuts at a word.** [`SimNor::arm_power_cut`] gives the device a
//!    budget in word-sized units; when it runs out, the words up to that
//!    point are on the part, everything after is not, and every later access
//!    fails until [`SimNor::power_on`]. The word is the unit because it is
//!    the unit the SoftDevice writes in: there is no such thing as a torn
//!    half-word.
//! 5. **A torn erase.** An erase is charged in the same word units, so a cut
//!    can land inside one. See [`SimNor::erase`] for what the page looks
//!    like afterwards and why writing into it is an error rather than a
//!    guess.
//! 6. **An operation that fails and changes nothing.** The SoftDevice
//!    schedules flash work between radio events and *fails the operation
//!    with a timeout* when it finds no gap (S140 SDS, Flash API timing).
//!    [`SimNor::fail_next_op`] injects exactly that: an `Err`, no bytes
//!    moved, and the retry after it succeeds.
//!
//! It also counts erases per page. Round-robin reclaim is only level wear if
//! the counts stay within one of each other, and a store with a fixed
//! metadata page fails that immediately — at 10 000 cycles per page rather
//! than the 100 000 the external part offered, which is the same argument
//! with an order of magnitude less room.
//!
//! # What it does not model
//!
//! Cell-level retention. A word whose erase pulse was cut is out of
//! specification, and no amount of simulation says what it will read back
//! next year. The simulation refuses to write to such a page instead of
//! inventing a value.

use alloc::vec;
use alloc::vec::Vec;

use embedded_storage_async::nor_flash::{
    ErrorType, MultiwriteNorFlash, NorFlash, NorFlashError, NorFlashErrorKind, ReadNorFlash,
};

use crate::{PROGRAM_UNIT, SECTOR_SIZE};

/// The erased state of a flash cell.
pub const ERASED: u8 = 0xFF;

/// Erase granularity of the nRF52840's internal flash, and the unit its
/// endurance figure is stated in.
pub const PAGE_SIZE: u32 = SECTOR_SIZE;

/// Word units in one page. The cost of a page erase in the same currency as
/// a write, so a power cut can land inside an erase.
pub const WORDS_PER_PAGE: usize = (SECTOR_SIZE / PROGRAM_UNIT) as usize;

/// Writes one word accepts between erases (nRF52840 PS, NVMC).
pub const WRITES_PER_WORD: u8 = 2;

/// What a simulated part refuses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SimError {
    /// The access ran past the end of the part.
    OutOfBounds,
    /// The part lost power mid-operation and has not been powered on again.
    PowerCut,
    /// The SoftDevice found no gap between radio events and failed the
    /// operation. Nothing was written or erased.
    Timeout,
    /// A program into a page whose erase was interrupted. The cells never
    /// finished their erase pulse; what they hold is not a value this
    /// simulation is willing to invent.
    TornPage,
}

impl NorFlashError for SimError {
    fn kind(&self) -> NorFlashErrorKind {
        match self {
            SimError::OutOfBounds => NorFlashErrorKind::OutOfBounds,
            SimError::PowerCut | SimError::Timeout | SimError::TornPage => NorFlashErrorKind::Other,
        }
    }
}

/// A page range of the nRF52840's internal flash, in memory.
pub struct SimNor {
    data: Vec<u8>,
    erases: Vec<u32>,
    /// Writes each 4-byte word has taken since its page was last erased.
    word_writes: Vec<u8>,
    /// Pages whose erase was interrupted.
    torn: Vec<bool>,
    /// Word units left before the lights go out. `None` = mains.
    budget: Option<usize>,
    /// Word units this part has programmed or erased since it was made.
    /// What the power-cut sweep uses to learn how many cuts an append has.
    spent: usize,
    dark: bool,
    /// Operations left before one fails without doing anything. `None` =
    /// the SoftDevice always finds its gap.
    fail_in: Option<usize>,
    /// Writes and erases attempted, whether or not they succeeded. The
    /// currency [`SimNor::fail_op_after`] counts in.
    ops: usize,
}

impl SimNor {
    /// A fresh, fully erased range of `pages` × 4 KiB.
    pub fn new(pages: u32) -> Self {
        Self {
            data: vec![ERASED; (pages * SECTOR_SIZE) as usize],
            erases: vec![0; pages as usize],
            word_writes: vec![0; pages as usize * WORDS_PER_PAGE],
            torn: vec![false; pages as usize],
            budget: None,
            spent: 0,
            dark: false,
            fail_in: None,
            ops: 0,
        }
    }

    /// Cut the power once `words` further word units have been programmed
    /// or erased. `0` cuts before the very next one.
    pub fn arm_power_cut(&mut self, words: usize) {
        self.budget = Some(words);
    }

    /// Power the part back on: the budget is gone and the contents are
    /// whatever survived.
    pub fn power_on(&mut self) {
        self.budget = None;
        self.dark = false;
    }

    /// Whether the armed cut has fired.
    pub fn is_dark(&self) -> bool {
        self.dark
    }

    /// Fail the next write or erase with [`SimError::Timeout`], changing
    /// nothing, then go back to working. One shot, like the SoftDevice's
    /// timeout: the retry is expected to succeed.
    pub fn fail_next_op(&mut self) {
        self.fail_op_after(0);
    }

    /// Fail the operation `n` operations from now. `fail_op_after(0)` is
    /// [`Self::fail_next_op`].
    pub fn fail_op_after(&mut self, n: usize) {
        self.fail_in = Some(n);
    }

    /// Whether an injected failure is still pending.
    pub fn failure_armed(&self) -> bool {
        self.fail_in.is_some()
    }

    /// Word units programmed or erased since the part was made.
    pub fn spent(&self) -> usize {
        self.spent
    }

    /// Writes and erases attempted since the part was made. What the
    /// injected-failure sweep uses to learn how many operations a call has.
    pub fn ops(&self) -> usize {
        self.ops
    }

    /// Erases per page, indexed by page. The wear pin reads this.
    pub fn erase_counts(&self) -> &[u32] {
        &self.erases
    }

    /// Writes per 4-byte word since that word's page was last erased.
    pub fn word_writes(&self) -> &[u8] {
        &self.word_writes
    }

    /// The most writes any one word has taken since its page was last
    /// erased. The nRF52840 allows [`WRITES_PER_WORD`].
    pub fn max_word_writes(&self) -> u8 {
        self.word_writes.iter().copied().max().unwrap_or(0)
    }

    /// Whether this page's erase was interrupted and not since redone.
    pub fn is_torn(&self, page: u32) -> bool {
        self.torn[page as usize]
    }

    /// Raw contents, for a byte-exact digest or a deliberate corruption.
    pub fn bytes(&self) -> &[u8] {
        &self.data
    }

    /// Overwrite raw contents, for planting foreign data in the region.
    ///
    /// Not a flash operation: no budget, no counters, no program-once rule.
    /// This is what somebody else's bootloader left behind, not something
    /// the store did.
    pub fn plant(&mut self, offset: u32, bytes: &[u8]) {
        let at = offset as usize;
        self.data[at..at + bytes.len()].copy_from_slice(bytes);
    }

    /// Clear bits in one byte directly, the way a decaying cell would.
    ///
    /// Only clearing is possible, so `value` must be reachable from what is
    /// there — and it must actually change the byte. A corruption that
    /// silently does nothing turns the test that uses it into a test of
    /// nothing, so it panics instead.
    pub fn corrupt(&mut self, offset: u32, value: u8) {
        let cell = &mut self.data[offset as usize];
        let after = *cell & value;
        assert_ne!(
            after, *cell,
            "corrupting {offset:#x} with {value:#04x} over {cell:#04x} changes no bit"
        );
        *cell = after;
    }

    /// Pages on the part.
    pub fn sectors(&self) -> u32 {
        self.erases.len() as u32
    }

    /// Charge `n` word units against the power budget and return how many of
    /// them actually happen. A budget that lands exactly on the end of an
    /// operation lets that operation finish and cuts before the next one.
    fn charge(&mut self, n: usize) -> usize {
        let done = match self.budget {
            None => n,
            Some(left) => core::cmp::min(left, n),
        };
        if let Some(left) = self.budget {
            self.budget = Some(left - done);
        }
        self.spent += done;
        if done < n {
            self.dark = true;
        }
        done
    }

    /// Consume one operation against the injected-failure counter. `true`
    /// means this operation fails and must change nothing.
    fn injected_failure(&mut self) -> bool {
        self.ops += 1;
        match self.fail_in {
            Some(0) => {
                self.fail_in = None;
                true
            }
            Some(left) => {
                self.fail_in = Some(left - 1);
                false
            }
            None => false,
        }
    }

    fn check_live(&self) -> Result<(), SimError> {
        if self.dark {
            return Err(SimError::PowerCut);
        }
        Ok(())
    }

    /// The bits a cut erase lifts in the word it stopped in.
    ///
    /// Deterministic in the address, so a failing sweep is reproducible, and
    /// never zero, so the word is never left reading exactly what it held.
    fn half_erased(offset: u32, byte: u8) -> u8 {
        let lift = 0x11u8.rotate_left(offset % 8) | 0x40;
        byte | lift
    }
}

impl ErrorType for SimNor {
    type Error = SimError;
}

impl ReadNorFlash for SimNor {
    /// Internal flash is memory-mapped; a read is a `memcpy` and has no
    /// alignment or length rule at all.
    const READ_SIZE: usize = 1;

    async fn read(&mut self, offset: u32, bytes: &mut [u8]) -> Result<(), Self::Error> {
        self.check_live()?;
        let end = offset as usize + bytes.len();
        if end > self.data.len() {
            return Err(SimError::OutOfBounds);
        }
        bytes.copy_from_slice(&self.data[offset as usize..end]);
        Ok(())
    }

    fn capacity(&self) -> usize {
        self.data.len()
    }
}

impl NorFlash for SimNor {
    const WRITE_SIZE: usize = PROGRAM_UNIT as usize;
    const ERASE_SIZE: usize = SECTOR_SIZE as usize;

    /// Erase whole pages.
    ///
    /// A cut inside an erase leaves the page **neither erased nor intact**,
    /// and this is how that is modelled: the words the erase had already
    /// reached read `0xFF`, the word it stopped in reads its old content
    /// with some of its bits lifted and some not
    /// ([`SimNor::half_erased`]), and the words it never reached still hold
    /// what they held. The page is then flagged torn, and a program into a
    /// torn page returns [`SimError::TornPage`] until the page has been
    /// erased end to end again — because a cell whose erase pulse was cut
    /// has no defined program behaviour, and a simulation that guessed one
    /// would be teaching the store a lie. Every store therefore has exactly
    /// one legal recovery from a torn erase, which is to redo it.
    async fn erase(&mut self, from: u32, to: u32) -> Result<(), Self::Error> {
        self.check_live()?;
        assert_eq!(from % SECTOR_SIZE, 0, "erase start must be sector-aligned");
        assert_eq!(to % SECTOR_SIZE, 0, "erase end must be sector-aligned");
        assert!(from < to, "erase range must be non-empty");
        if to as usize > self.data.len() {
            return Err(SimError::OutOfBounds);
        }
        if self.injected_failure() {
            return Err(SimError::Timeout);
        }
        for sector in (from..to).step_by(SECTOR_SIZE as usize) {
            let page = (sector / SECTOR_SIZE) as usize;
            self.erases[page] += 1;
            let done = self.charge(WORDS_PER_PAGE);
            let base = sector as usize;
            self.data[base..base + done * PROGRAM_UNIT as usize].fill(ERASED);

            if done < WORDS_PER_PAGE {
                let torn = base + done * PROGRAM_UNIT as usize;
                for i in 0..PROGRAM_UNIT as usize {
                    self.data[torn + i] = Self::half_erased((torn + i) as u32, self.data[torn + i]);
                }
                self.torn[page] = true;
                return Err(SimError::PowerCut);
            }

            self.torn[page] = false;
            let words = page * WORDS_PER_PAGE;
            self.word_writes[words..words + WORDS_PER_PAGE].fill(0);
        }
        Ok(())
    }

    async fn write(&mut self, offset: u32, bytes: &[u8]) -> Result<(), Self::Error> {
        self.check_live()?;
        assert_eq!(
            offset % PROGRAM_UNIT,
            0,
            "program address must be 4-byte aligned"
        );
        assert_eq!(
            bytes.len() % PROGRAM_UNIT as usize,
            0,
            "program length must be a multiple of 4"
        );
        let end = offset as usize + bytes.len();
        if end > self.data.len() {
            return Err(SimError::OutOfBounds);
        }
        let first_page = offset / SECTOR_SIZE;
        let last_page = if bytes.is_empty() {
            first_page
        } else {
            (end as u32 - 1) / SECTOR_SIZE
        };
        for page in first_page..=last_page {
            if self.torn[page as usize] {
                return Err(SimError::TornPage);
            }
        }
        if self.injected_failure() {
            return Err(SimError::Timeout);
        }
        let done = self.charge(bytes.len() / PROGRAM_UNIT as usize);
        let first_word = (offset / PROGRAM_UNIT) as usize;
        for word in first_word..first_word + done {
            self.word_writes[word] = self.word_writes[word].saturating_add(1);
        }
        for (i, byte) in bytes[..done * PROGRAM_UNIT as usize].iter().enumerate() {
            let cell = &mut self.data[offset as usize + i];
            assert!(
                *byte & !*cell == 0,
                "program at {:#x} would raise a bit: {:#04x} over {:#04x}. \
                 Flash cannot do that; the page has to be erased first.",
                offset as usize + i,
                byte,
                cell
            );
            *cell &= *byte;
        }
        if done < bytes.len() / PROGRAM_UNIT as usize {
            return Err(SimError::PowerCut);
        }
        Ok(())
    }
}

/// `nrf_softdevice::Flash` declares this, so the simulation does too, and
/// the stores that need a second write to a word (to withdraw an entry) can
/// ask for it in their bounds. Note that the marker says *more than once*,
/// not *without limit*: the nRF52840 caps it at [`WRITES_PER_WORD`], which
/// the trait has no way to express and [`SimNor::max_word_writes`] is
/// therefore how a candidate gets held to it.
impl MultiwriteNorFlash for SimNor {}

/// Drive a future to completion on the calling thread.
///
/// Nothing over [`SimNor`] waits on an interrupt — the device is a
/// `Vec<u8>` — so polling in a loop with a no-op waker is the whole
/// executor, and it drives [`Yielding`] too. A future that never finishes
/// would spin here, so the loop is bounded and says which assumption broke
/// rather than hanging the test run.
pub fn block_on<F: core::future::Future>(fut: F) -> F::Output {
    let mut fut = core::pin::pin!(fut);
    let mut cx = core::task::Context::from_waker(core::task::Waker::noop());
    for _ in 0..POLL_LIMIT {
        if let core::task::Poll::Ready(value) = fut.as_mut().poll(&mut cx) {
            return value;
        }
    }
    panic!(
        "future still pending after {POLL_LIMIT} polls: nothing over SimNor \
         waits on anything, so this is a busy loop rather than slow progress"
    );
}

/// Return `Pending` exactly once, so a caller can be dropped here.
async fn yield_now() {
    let mut yielded = false;
    core::future::poll_fn(move |cx| {
        if yielded {
            core::task::Poll::Ready(())
        } else {
            yielded = true;
            cx.waker().wake_by_ref();
            core::task::Poll::Pending
        }
    })
    .await;
}

/// A device that returns `Pending` once before every operation.
///
/// [`SimNor`] alone cannot be cancelled: its futures are ready on the first
/// poll, so there is no `.await` for a `select!` to drop the store at. The
/// real `nrf_softdevice::Flash` yields at every operation — it waits for the
/// SoftDevice's flash-operation event — so a wrapper that yields once per
/// operation is the shape a cancellation test needs, and the count of polls
/// a call takes is the count of points it can be dropped at.
pub struct Yielding<F> {
    inner: F,
    /// Operations begun, whether or not they completed. The cancellation
    /// sweep uses it the way the power-cut sweep uses `spent`.
    ops: usize,
}

impl<F> Yielding<F> {
    /// Wrap a device.
    pub fn new(inner: F) -> Self {
        Self { inner, ops: 0 }
    }

    /// The device underneath.
    pub fn inner_mut(&mut self) -> &mut F {
        &mut self.inner
    }

    /// Give the device back.
    pub fn into_inner(self) -> F {
        self.inner
    }

    /// Operations begun since the wrapper was made.
    pub fn ops(&self) -> usize {
        self.ops
    }

    /// Yield once, then let the operation through.
    async fn gate(&mut self) {
        self.ops += 1;
        yield_now().await;
    }
}

impl<F: ErrorType> ErrorType for Yielding<F> {
    type Error = F::Error;
}

impl<F: ReadNorFlash> ReadNorFlash for Yielding<F> {
    const READ_SIZE: usize = F::READ_SIZE;

    async fn read(&mut self, offset: u32, bytes: &mut [u8]) -> Result<(), Self::Error> {
        self.gate().await;
        self.inner.read(offset, bytes).await
    }

    fn capacity(&self) -> usize {
        self.inner.capacity()
    }
}

impl<F: NorFlash> NorFlash for Yielding<F> {
    const WRITE_SIZE: usize = F::WRITE_SIZE;
    const ERASE_SIZE: usize = F::ERASE_SIZE;

    async fn erase(&mut self, from: u32, to: u32) -> Result<(), Self::Error> {
        self.gate().await;
        self.inner.erase(from, to).await
    }

    async fn write(&mut self, offset: u32, bytes: &[u8]) -> Result<(), Self::Error> {
        self.gate().await;
        self.inner.write(offset, bytes).await
    }
}

impl<F: MultiwriteNorFlash> MultiwriteNorFlash for Yielding<F> {}

const POLL_LIMIT: usize = 1_000_000;
