//! Compile-fail fixture: a [`CoreProcessor`] may not `.await` (Codeberg #196).
//!
//! Empty unless the internal `__compile_fail_fixtures` feature is on. The
//! check that this does NOT build, and fails with the error code named below,
//! is `scripts/check-processor-compile-fail.sh`, run by `just fast`.
//!
//! Pinned error: `error[E0728]: `await` is only allowed inside `async`
//! functions and blocks`.
//!
//! Why that error and not a neighbouring one: the deadlock the concept page
//! describes (`docs/src/concepts/core-lock-budget.md`) needs the driver's
//! `action_dispatch_tx.send(output).await` to *complete*. Building the future
//! and dropping it sends nothing and blocks nothing. `.await` is the only
//! construct that can complete it, so E0728 is the error that closes the
//! hazard; an E0599 here would only mean the method name was wrong.

#![cfg(feature = "__compile_fail_fixtures")]

use leviculum_core::node::NodeEvent;
use leviculum_core::transport::TickOutput;
use leviculum_core::DestinationHash;
use leviculum_std::driver::{CoreProcessor, ReticulumNode, StdNodeCore};

struct Deadlock {
    node: ReticulumNode,
    dest: DestinationHash,
}

impl CoreProcessor for Deadlock {
    fn on_event(&mut self, _core: &mut StdNodeCore, _event: &NodeEvent) -> TickOutput {
        // `announce_destination` ends in `action_dispatch_tx.send(..).await`
        // on the bounded channel this very loop drains.
        self.node
            .announce_destination(&self.dest, None)
            .await
            .unwrap();
        TickOutput::empty()
    }
}
