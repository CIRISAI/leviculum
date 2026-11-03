//! Helpers the store benchmarks and the randomised tests share.

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

impl leviculum_std::durable::Fsync for NoFsync {
    fn sync(&self, _handle: &std::fs::File) -> std::io::Result<()> {
        Ok(())
    }
}
