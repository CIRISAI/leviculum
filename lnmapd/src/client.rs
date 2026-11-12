//! The verifying store client: fetch the signed index, then a pack's
//! pieces coarse before fine inside a time budget, every byte checked
//! before it is used (`docs/src/concepts/map-server.md`, "What a device
//! runs").
//!
//! The planner is transport-blind: a [`PieceFetcher`] is one function,
//! and the HTTP one here is a reference implementation. The Reticulum
//! side plugs in the same way (a request per piece id on the `lev.maps`
//! link); the Loquax ladder owns that wiring.
//!
//! Time is injected ([`Clock`]) so the budget tests run on a fake clock
//! and never on the wall: a throttled link is modelled, not slept.

use std::time::Duration;

use crate::store::{verify_piece, Pack, VerifyError};

/// Why a prefix fetch stopped with an error.
#[derive(Debug, thiserror::Error)]
pub enum FetchError {
    /// The carrier failed to produce the piece at all.
    #[error("piece {id}: {reason}")]
    Transport {
        /// The piece id asked for.
        id: String,
        /// What the carrier said.
        reason: String,
    },
    /// The piece arrived but is not what the index promised. The bytes
    /// were discarded, nothing of them reached the assembly.
    #[error(transparent)]
    Verify(#[from] VerifyError),
    /// The index's piece list does not assemble contiguously (a piece's
    /// offset is not where the previous one ended): the index itself is
    /// broken, however validly signed.
    #[error("piece {id} starts at {offset}, the assembly is at {assembled}")]
    NotContiguous {
        /// The piece id whose offset is wrong.
        id: String,
        /// The offset the index gave.
        offset: u64,
        /// How many bytes are assembled.
        assembled: u64,
    },
}

/// One piece from wherever the client gets its bytes.
pub trait PieceFetcher {
    /// The bytes of the piece named `id` (64 hex characters).
    fn fetch_piece(&mut self, id: &str) -> Result<Vec<u8>, FetchError>;
}

/// Elapsed time since the fetch began, injectable for tests.
pub trait Clock {
    /// How much of the budget is spent.
    fn elapsed(&self) -> Duration;
}

/// The wall clock, from its creation.
pub struct WallClock(std::time::Instant);

impl WallClock {
    /// A clock starting now.
    pub fn start() -> Self {
        Self(std::time::Instant::now())
    }
}

impl Clock for WallClock {
    fn elapsed(&self) -> Duration {
        self.0.elapsed()
    }
}

/// What a prefix fetch produced.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Fetched {
    /// How many pieces landed, from the front of the list.
    pub pieces: usize,
    /// Whether the whole pack is there.
    pub complete: bool,
    /// The deepest zoom complete in the assembly, when any piece landed.
    pub zoom: Option<u8>,
}

/// Fetch `pack`'s pieces in order — coarse before fine — into `out`,
/// stopping cleanly when `budget` is spent. The first piece is always
/// asked for (a budget that allows no bytes at all would make every
/// view empty); each further piece only starts while the clock is
/// inside the budget. Every piece is verified against the index before
/// it is appended; a tampered piece is discarded and the fetch fails
/// with the assembly intact up to the last good piece.
///
/// `out` must be empty or hold an already-assembled prefix of this same
/// pack (a resumed fetch): the next piece is the one whose offset is
/// `out.len()`.
pub fn fetch_prefix<F: PieceFetcher, C: Clock>(
    fetcher: &mut F,
    clock: &C,
    pack: &Pack,
    budget: Duration,
    out: &mut Vec<u8>,
) -> Result<Fetched, FetchError> {
    let mut fetched = Fetched {
        pieces: 0,
        complete: false,
        zoom: None,
    };
    for piece in &pack.pieces {
        let assembled = out.len() as u64;
        if piece.offset + u64::from(piece.len) <= assembled {
            // Already there from an earlier fetch.
            fetched.pieces += 1;
            fetched.zoom = Some(piece.zoom);
            continue;
        }
        if piece.offset != assembled {
            return Err(FetchError::NotContiguous {
                id: piece.id(),
                offset: piece.offset,
                assembled,
            });
        }
        if fetched.pieces > 0 && clock.elapsed() >= budget {
            return Ok(fetched);
        }
        let bytes = fetcher.fetch_piece(&piece.id())?;
        verify_piece(piece, &bytes)?;
        out.extend_from_slice(&bytes);
        fetched.pieces += 1;
        fetched.zoom = Some(piece.zoom);
    }
    fetched.complete = out.len() as u64 == pack.total;
    Ok(fetched)
}

/// The HTTP fetcher: `GET <base>/piece/<id>`, the same piece ids the
/// Reticulum side answers.
pub struct HttpFetcher {
    agent: ureq::Agent,
    base: String,
}

impl HttpFetcher {
    /// A fetcher against `base` (no trailing slash needed).
    pub fn new(base: &str) -> Self {
        let agent: ureq::Agent = ureq::Agent::config_builder()
            .timeout_global(Some(Duration::from_secs(60)))
            .user_agent(concat!("lnmapd/", env!("CARGO_PKG_VERSION")))
            .build()
            .into();
        Self {
            agent,
            base: base.trim_end_matches('/').to_owned(),
        }
    }

    /// `GET <base>/index`: the signed index's bytes, unverified — the
    /// caller runs [`crate::store::Index::verify`] against its pin.
    pub fn fetch_index(&mut self) -> Result<Vec<u8>, FetchError> {
        self.get(&format!("{}/index", self.base), "index")
    }

    fn get(&mut self, url: &str, what: &str) -> Result<Vec<u8>, FetchError> {
        let transport = |reason: String| FetchError::Transport {
            id: what.to_owned(),
            reason,
        };
        let mut response = self
            .agent
            .get(url)
            .call()
            .map_err(|e| transport(e.to_string()))?;
        response
            .body_mut()
            .read_to_vec()
            .map_err(|e| transport(e.to_string()))
    }
}

impl PieceFetcher for HttpFetcher {
    fn fetch_piece(&mut self, id: &str) -> Result<Vec<u8>, FetchError> {
        self.get(&format!("{}/piece/{id}", self.base), id)
    }
}
