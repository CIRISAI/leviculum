//! Helpers the store benchmarks and the randomised tests share.

use std::path::Path;
use std::sync::Arc;

use leviculum_lxmf::propagation_store::MIN_BODY_LEN;
use leviculum_lxmf::PropagationStore;
use leviculum_std::durable::Fsync;
use leviculum_std::FilePropagationStore;

/// A deterministic xorshift64 for the randomised tests.
pub(crate) fn next_random(state: &mut u64) -> u64 {
    *state ^= *state << 13;
    *state ^= *state >> 7;
    *state ^= *state << 17;
    *state
}

/// No device under the store: the benchmarks fill hundreds of thousands
/// of files, and the walks they price read none of them.
pub(crate) struct NoFsync;

impl Fsync for NoFsync {
    fn sync(&self, _handle: &std::fs::File) -> std::io::Result<()> {
        Ok(())
    }
}

/// A file store in `dir` holding at least `messages` entries received at
/// random over the `days` before `now`, with random transient ids and
/// sizes around the field's mean of 330 bytes; a reopen tops it up. Runs
/// the production writer, as lnpnd does.
pub(crate) fn spread_store(
    dir: &Path,
    messages: usize,
    days: u64,
    now: u64,
) -> FilePropagationStore {
    let fsync: Arc<dyn Fsync> = Arc::new(NoFsync);
    let mut store =
        FilePropagationStore::open_with(dir, u64::MAX / 2, fsync).expect("a store directory");
    let mut state = 0x6a09_e667_f3bc_c908 ^ store.len() as u64;
    while store.len() < messages {
        let mut transient_id = [0u8; 32];
        for chunk in transient_id.chunks_mut(8) {
            chunk.copy_from_slice(&next_random(&mut state).to_le_bytes());
        }
        let received_at = now - next_random(&mut state) % (days * 24 * 60 * 60);
        let len = MIN_BODY_LEN + 200 + (next_random(&mut state) % 260) as usize;
        store
            .append(&transient_id, received_at, 13, &vec![0x5a; len])
            .expect("the store has room");
    }
    store.write_behind().expect("the store's writer")
}
