#![cfg(feature = "pow")]

mod common;

use futures::executor::block_on;
use leviculum_core::crypto::full_hash;
use leviculum_lxmf::constants::{WORKBLOCK_EXPAND_ROUNDS, WORKBLOCK_EXPAND_ROUNDS_PN};
use leviculum_lxmf::stamp::{
    valid, value, CooperativeStamper, ReadyYield, StampCancel, StampError, StampExecutor,
    WorkblockStream,
};
use leviculum_lxmf::{DeliveryStampRequest, PropagationStampRequest};
use rand_core::OsRng;
use std::{boxed::Box, future::Future, pin::Pin};

struct RecordingExecutor {
    material: [u8; 32],
    cost: u8,
    rounds: usize,
    result: [u8; 32],
}

impl StampExecutor for RecordingExecutor {
    fn generate<'a>(
        &'a mut self,
        material: &'a [u8],
        cost: u8,
        rounds: usize,
        cancel: &'a StampCancel,
    ) -> Pin<Box<dyn Future<Output = Result<[u8; 32], StampError>> + Send + 'a>> {
        assert_eq!(material, self.material);
        assert_eq!(cost, self.cost);
        assert_eq!(rounds, self.rounds);
        // Observing the caller's handle is what an executor owes; one that
        // ignored it could not be called off (Codeberg #185).
        if cancel.is_cancelled() {
            return Box::pin(core::future::ready(Err(StampError::Cancelled)));
        }
        Box::pin(core::future::ready(Ok(self.result)))
    }

    fn validate<'a>(
        &'a mut self,
        _: &'a [u8],
        _: &'a [u8; 32],
        _: u8,
        _: usize,
    ) -> Pin<Box<dyn Future<Output = Result<Option<u16>, StampError>> + 'a>> {
        unreachable!("this executor only records generation")
    }
}

#[test]
fn python_stamp_workblock_vector() {
    let material: [u8; 32] = hex::decode(common::fixture("VEC-STAMP-1", "material_hex"))
        .unwrap()
        .try_into()
        .unwrap();
    let stamp: [u8; 32] = hex::decode(common::fixture("VEC-STAMP-1", "stamp_hex"))
        .unwrap()
        .try_into()
        .unwrap();
    let rounds = common::fixture("VEC-STAMP-1", "expand_rounds")
        .parse::<usize>()
        .unwrap();
    let cost = common::fixture("VEC-STAMP-1", "target_cost")
        .parse::<u8>()
        .unwrap();
    let expected_workblock_hash = common::fixture("VEC-STAMP-1", "workblock_sha256_hex");
    let mut stamper = CooperativeStamper::new(OsRng, ReadyYield);

    let workblock = block_on(stamper.workblock(&material, rounds));

    assert_eq!(hex::encode(full_hash(&workblock)), expected_workblock_hash);
    assert!(valid(&workblock, &stamp, cost));
    assert_eq!(
        value(&workblock, &stamp),
        common::fixture("VEC-STAMP-1", "stamp_value")
            .parse::<u16>()
            .unwrap()
    );
}

#[test]
fn python_propagation_stamp_uses_the_1000_round_workblock() {
    let material: [u8; 32] = hex::decode(common::fixture("VEC-STAMP-PN", "material_hex"))
        .unwrap()
        .try_into()
        .unwrap();
    let stamp: [u8; 32] = hex::decode(common::fixture("VEC-STAMP-PN", "stamp_hex"))
        .unwrap()
        .try_into()
        .unwrap();
    let rounds = common::fixture("VEC-STAMP-PN", "expand_rounds")
        .parse::<usize>()
        .unwrap();
    let cost = common::fixture("VEC-STAMP-PN", "target_cost")
        .parse::<u8>()
        .unwrap();
    let mut stamper = CooperativeStamper::new(OsRng, ReadyYield);

    assert_eq!(rounds, WORKBLOCK_EXPAND_ROUNDS_PN);
    let workblock = block_on(stamper.workblock(&material, rounds));

    assert_eq!(
        workblock.len().to_string(),
        common::fixture("VEC-STAMP-PN", "workblock_len")
    );
    assert_eq!(
        hex::encode(full_hash(&workblock)),
        common::fixture("VEC-STAMP-PN", "workblock_sha256_hex")
    );
    assert!(valid(&workblock, &stamp, cost));
    assert_eq!(
        value(&workblock, &stamp).to_string(),
        common::fixture("VEC-STAMP-PN", "stamp_value")
    );
}

/// A workblock expanded in slices is the SAME workblock.
///
/// The board validates a propagation stamp a slice at a time so its main loop
/// keeps reaching its radio (Codeberg #425, `leviculum_nrf::pn`). A slicing
/// that changed the digest would make every stamp a Python peer produced
/// invalid, which is a wire-compatibility break dressed as a scheduling fix,
/// so the equivalence is asserted against the same Python vector the
/// unsliced path is asserted against, and at four slice widths including ones
/// that do not divide the round count.
#[test]
fn a_sliced_workblock_agrees_with_the_python_vector() {
    let material: [u8; 32] = hex::decode(common::fixture("VEC-STAMP-PN", "material_hex"))
        .unwrap()
        .try_into()
        .unwrap();
    let stamp: [u8; 32] = hex::decode(common::fixture("VEC-STAMP-PN", "stamp_hex"))
        .unwrap()
        .try_into()
        .unwrap();
    let cost = common::fixture("VEC-STAMP-PN", "target_cost")
        .parse::<u8>()
        .unwrap();
    let expected_value = common::fixture("VEC-STAMP-PN", "stamp_value")
        .parse::<u16>()
        .unwrap();

    // 20 is the firmware's slice; 1 is the pathological narrow one; 7 and 333
    // do not divide 1000, so the final partial slice is exercised too.
    for slice in [1usize, 7, 20, 333] {
        let mut stream = WorkblockStream::new(WORKBLOCK_EXPAND_ROUNDS_PN);
        let mut passes = 0usize;
        while !stream.is_complete() {
            let did = stream.advance(&material, slice);
            assert!(did > 0, "a slice of {slice} made no progress");
            passes += 1;
            assert!(passes <= WORKBLOCK_EXPAND_ROUNDS_PN, "slice {slice} looped");
        }
        assert_eq!(stream.rounds_done(), WORKBLOCK_EXPAND_ROUNDS_PN);
        assert_eq!(
            stream.validated(&stamp, cost),
            Some(expected_value),
            "slice width {slice} disagreed with the Python vector"
        );
        assert_eq!(stream.value(&stamp), expected_value);
    }
}

/// An exhausted stream makes no further progress, so a caller that loops on
/// `advance` terminates instead of spinning.
#[test]
fn a_complete_workblock_stream_advances_no_further() {
    let mut stream = WorkblockStream::new(4);
    assert_eq!(stream.advance(b"material", 10), 4);
    assert!(stream.is_complete());
    assert_eq!(stream.advance(b"material", 10), 0);
    assert_eq!(stream.rounds_total(), 4);

    // A zero-round workblock is complete on arrival, and its digest is the
    // stamp alone, which is what a cost-0 validation amounts to.
    let empty = WorkblockStream::new(0);
    assert!(empty.is_complete());
    assert_eq!(empty.validated(&[0u8; 32], 0), Some(0));
}

#[test]
fn detached_requests_select_delivery_and_propagation_workblocks() {
    let delivery = DeliveryStampRequest {
        message_id: [0x31; 32],
        target_cost: 7,
    };
    let mut executor = RecordingExecutor {
        material: delivery.message_id,
        cost: delivery.target_cost,
        rounds: WORKBLOCK_EXPAND_ROUNDS,
        result: [0x41; 32],
    };
    assert_eq!(
        block_on(delivery.generate_with(&mut executor, &StampCancel::new())).unwrap(),
        [0x41; 32]
    );

    let propagation = PropagationStampRequest {
        message_id: [0x32; 32],
        transient_id: [0x33; 32],
        target_cost: 9,
    };
    executor = RecordingExecutor {
        material: propagation.transient_id,
        cost: propagation.target_cost,
        rounds: WORKBLOCK_EXPAND_ROUNDS_PN,
        result: [0x42; 32],
    };
    assert_eq!(
        block_on(propagation.generate_with(&mut executor, &StampCancel::new())).unwrap(),
        [0x42; 32]
    );

    // The request forwards the caller's handle rather than minting its own,
    // which is the only reason a host can call a grind off from outside.
    let cancelled = StampCancel::new();
    cancelled.cancel();
    assert_eq!(
        block_on(propagation.generate_with(&mut executor, &cancelled)),
        Err(StampError::Cancelled)
    );
}
