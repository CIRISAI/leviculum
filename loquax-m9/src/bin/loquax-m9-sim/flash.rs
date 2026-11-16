//! The board's flash in the sim (1530): the `messages` partition as a
//! [`MemFlash`] of its 1024 sectors, and in place of the client record the
//! two things of it the screen reads, the home's LXMF address and the
//! reader's type size (1532, the record's byte 6). Saved to a
//! file so a second run boots what the first one left, as a power cycle
//! does; `--reboot-after` boots it again inside one run.
//!
//! What it cannot reproduce: the flash's timing (an erase holds the board's
//! interrupts for tens of milliseconds, here it is a `fill`), the
//! partition table's other slots (identity, the record's two copies), and
//! a write cut by a power loss (`MemFlash::cut_at` exists, nothing sets it
//! here).

use std::io::{Read, Write};
use std::path::Path;

use loquax::home::board::store::{MemFlash, SECTOR};

/// The board's `messages` partition: `0x400000` bytes at `0x812000`
/// (`leviculum-esp/partitions-m9.csv`), in 4 KiB sectors.
pub const SECTORS: u32 = 0x40_0000 / SECTOR as u32;

/// The file's first bytes, and its version.
const MAGIC: &[u8; 8] = b"LQXM9FL1";

/// What the board keeps across a power cycle, as far as the sim needs it.
pub struct Flash {
    /// The home whose device the board is; `None` on a board without a
    /// client record, which opens on the pairing.
    pub home: Option<[u8; 16]>,
    /// The record's type size (1532, 1561), `font::Size::record`'s two
    /// bits; kept only with a home, as the board keeps it only in its
    /// client record.
    pub type_size: u8,
    pub messages: MemFlash,
}

/// The header byte's bits: a home follows (bit 0), and its record's type
/// size (bits 1-2; bit 1 alone was 1532's large flag, the same value).
const HAS_HOME: u8 = 1;
const TYPE_SHIFT: u8 = 1;
const TYPE_SIZE: u8 = 0b11 << TYPE_SHIFT;

impl Flash {
    /// A board fresh off the flasher: no record, every sector erased.
    pub fn blank() -> Self {
        Self {
            home: None,
            type_size: 0,
            messages: MemFlash::new(SECTORS),
        }
    }

    /// The image a run saved.
    pub fn load(path: &Path) -> Result<Self, String> {
        let mut file = std::fs::File::open(path).map_err(|e| format!("{}: {e}", path.display()))?;
        let mut head = [0u8; 8 + 1 + 16 + 4];
        file.read_exact(&mut head)
            .map_err(|e| format!("{}: header: {e}", path.display()))?;
        if &head[..8] != MAGIC {
            return Err(format!(
                "{}: not a loquax-m9-sim flash image",
                path.display()
            ));
        }
        let type_size = (head[8] & TYPE_SIZE) >> TYPE_SHIFT;
        let home = match head[8] & HAS_HOME {
            0 => None,
            _ => {
                let mut address = [0u8; 16];
                address.copy_from_slice(&head[9..25]);
                Some(address)
            }
        };
        let mut count = [0u8; 4];
        count.copy_from_slice(&head[25..29]);
        let count = u32::from_le_bytes(count);
        if count != SECTORS {
            return Err(format!(
                "{}: {count} sectors, the board has {SECTORS}",
                path.display()
            ));
        }
        let mut messages = MemFlash::new(count);
        for sector in &mut messages.sectors {
            file.read_exact(sector)
                .map_err(|e| format!("{}: sectors: {e}", path.display()))?;
        }
        Ok(Self {
            home,
            type_size: if home.is_some() { type_size } else { 0 },
            messages,
        })
    }

    /// Write the image to `path`, whole.
    pub fn save(&self, path: &Path) -> Result<(), String> {
        let mut out = Vec::with_capacity(29 + self.messages.sectors.len() * SECTOR);
        out.extend_from_slice(MAGIC);
        match self.home {
            Some(address) => {
                out.push(HAS_HOME | ((self.type_size << TYPE_SHIFT) & TYPE_SIZE));
                out.extend_from_slice(&address);
            }
            None => {
                out.push(0);
                out.extend_from_slice(&[0; 16]);
            }
        }
        out.extend_from_slice(&(self.messages.sectors.len() as u32).to_le_bytes());
        for sector in &self.messages.sectors {
            out.extend_from_slice(sector);
        }
        let mut file =
            std::fs::File::create(path).map_err(|e| format!("{}: {e}", path.display()))?;
        file.write_all(&out)
            .map_err(|e| format!("{}: {e}", path.display()))
    }
}
