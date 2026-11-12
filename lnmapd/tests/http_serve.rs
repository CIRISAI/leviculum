//! The HTTP side end to end: an origin `lnmapd` serves a store the
//! verifying client reads whole; a front-door cache in front of it
//! serves what it holds after the upstream dies, refuses a lying
//! upstream, and keeps its piece cache under the bound.

mod common;

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use leviculum_std::generate_identity;
use lnmapd::client::{fetch_prefix, FetchError, HttpFetcher, PieceFetcher, WallClock};
use lnmapd::serve::{router, CacheConfig, Served};
use lnmapd::store::{add_pack, piece_path, AddOptions, Index, KIND_VECTORS};

/// A served store on an ephemeral loopback port: the base URL and the
/// task to abort when the test kills the server.
async fn spawn(served: Arc<Served>) -> (String, tokio::task::JoinHandle<()>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind loopback");
    let addr = listener.local_addr().expect("local addr");
    let task = tokio::spawn(async move {
        // The test aborts the task; serve itself runs until then.
        let _ = axum::serve(listener, router(served).into_make_service()).await;
    });
    (format!("http://{addr}"), task)
}

/// A store with the grunewald pack, its signing identity's pinned
/// destination, and the verified index.
fn build_store(dir: &Path) -> (leviculum_std::DestinationHash, Index, Vec<u8>) {
    let region = common::grunewald_region();
    let identity = generate_identity();
    let index = add_pack(
        dir,
        &region,
        &identity,
        &AddOptions {
            id: "grunewald".to_owned(),
            kind: KIND_VECTORS.to_owned(),
            http_url: None,
        },
    )
    .expect("add the pack");
    let pinned = Index::destination_of(&identity.public_key_bytes()).expect("own destination");
    (pinned, index, region)
}

#[tokio::test]
async fn the_origin_serves_what_the_client_verifies() {
    let store = tempfile::tempdir().expect("store dir");
    let (pinned, _, region) = build_store(store.path());
    let (base, task) = spawn(Served::origin(store.path().to_path_buf())).await;

    let (index, assembled) = tokio::task::spawn_blocking(move || {
        let mut fetcher = HttpFetcher::new(&base);
        let bytes = fetcher.fetch_index().expect("fetch the index");
        let index = Index::verify(&bytes, &pinned, None).expect("verify against the pin");
        let pack = index.pack("grunewald").expect("pack").clone();
        let mut out = Vec::new();
        let fetched = fetch_prefix(
            &mut fetcher,
            &WallClock::start(),
            &pack,
            Duration::from_secs(600),
            &mut out,
        )
        .expect("fetch every piece");
        assert!(fetched.complete);

        // An id the store does not hold is a clean transport error.
        let unknown = "0".repeat(64);
        assert!(matches!(
            fetcher.fetch_piece(&unknown),
            Err(FetchError::Transport { .. })
        ));
        (index, out)
    })
    .await
    .expect("client task");

    assert_eq!(assembled, region, "the transport is byte-faithful");
    assert_eq!(index.generation, 1);
    task.abort();
}

#[tokio::test]
async fn the_front_door_caches_survives_the_upstream_and_stays_bounded() {
    let origin_store = tempfile::tempdir().expect("origin store");
    let (pinned, index, region) = build_store(origin_store.path());
    let pack = index.pack("grunewald").expect("pack").clone();
    let (origin_base, origin_task) = spawn(Served::origin(origin_store.path().to_path_buf())).await;

    let door_store = tempfile::tempdir().expect("front door store");
    let (door_base, door_task) = spawn(Served::front_door(
        door_store.path().to_path_buf(),
        CacheConfig {
            upstream: origin_base.clone(),
            cap_bytes: 64 * 1024 * 1024,
            index_ttl: Duration::from_secs(3600),
        },
    ))
    .await;

    // Everything through the front door: verified against the same
    // pin — the cache cannot substitute anything.
    let base = door_base.clone();
    let assembled = tokio::task::spawn_blocking(move || {
        let mut fetcher = HttpFetcher::new(&base);
        let bytes = fetcher.fetch_index().expect("index through the front door");
        let index = Index::verify(&bytes, &pinned, None).expect("still the pinned server's index");
        let pack = index.pack("grunewald").expect("pack").clone();
        let mut out = Vec::new();
        fetch_prefix(
            &mut fetcher,
            &WallClock::start(),
            &pack,
            Duration::from_secs(600),
            &mut out,
        )
        .expect("fetch through the front door");
        out
    })
    .await
    .expect("client task");
    assert_eq!(assembled, region);

    // The upstream dies; the front door serves everything it holds —
    // what it fetched once it never fetches again.
    origin_task.abort();
    let base = door_base.clone();
    let again = tokio::task::spawn_blocking(move || {
        let mut fetcher = HttpFetcher::new(&base);
        fetcher.fetch_index().expect("the held index still serves");
        let mut out = Vec::new();
        fetch_prefix(
            &mut fetcher,
            &WallClock::start(),
            &pack,
            Duration::from_secs(600),
            &mut out,
        )
        .expect("held pieces still serve");
        out
    })
    .await
    .expect("client task");
    assert_eq!(again, region);
    door_task.abort();

    // The bound: a cache whose cap is one byte below the pack keeps
    // the newest pieces and deletes the oldest.
    let bounded_store = tempfile::tempdir().expect("bounded store");
    let cap = pieces_total(&index) - 1;
    let (origin_base, origin_task) = spawn(Served::origin(origin_store.path().to_path_buf())).await;
    let (door_base, door_task) = spawn(Served::front_door(
        bounded_store.path().to_path_buf(),
        CacheConfig {
            upstream: origin_base,
            cap_bytes: cap,
            index_ttl: Duration::from_secs(3600),
        },
    ))
    .await;
    let pack = index.pack("grunewald").expect("pack").clone();
    let fetch_pack = pack.clone();
    tokio::task::spawn_blocking(move || {
        let mut fetcher = HttpFetcher::new(&door_base);
        let mut out = Vec::new();
        fetch_prefix(
            &mut fetcher,
            &WallClock::start(),
            &fetch_pack,
            Duration::from_secs(600),
            &mut out,
        )
        .expect("fetch through the bounded front door");
    })
    .await
    .expect("client task");
    let held: Vec<bool> = pack
        .pieces
        .iter()
        .map(|p| piece_path(bounded_store.path(), &p.id()).exists())
        .collect();
    assert!(
        !held[0],
        "one byte over the cap: the least recently served piece left"
    );
    assert!(
        *held.last().expect("pieces"),
        "the piece just written is never the one evicted"
    );
    origin_task.abort();
    door_task.abort();
}

fn pieces_total(index: &Index) -> u64 {
    index
        .pack("grunewald")
        .expect("pack")
        .pieces
        .iter()
        .map(|p| u64::from(p.len))
        .sum()
}

#[tokio::test]
async fn a_lying_upstream_is_refused_and_nothing_is_cached() {
    let origin_store = tempfile::tempdir().expect("origin store");
    let (_, index, _) = build_store(origin_store.path());
    let pack = index.pack("grunewald").expect("pack").clone();

    // The origin's copy of the first piece is corrupted on disk: it
    // serves bytes that no longer hash to the piece's name.
    let poisoned = &pack.pieces[0];
    let path = piece_path(origin_store.path(), &poisoned.id());
    let mut bytes = std::fs::read(&path).expect("read piece");
    bytes[0] ^= 1;
    std::fs::write(&path, &bytes).expect("poison the origin");

    let (origin_base, origin_task) = spawn(Served::origin(origin_store.path().to_path_buf())).await;
    let door_store = tempfile::tempdir().expect("front door store");
    let door_path: PathBuf = door_store.path().to_path_buf();
    let (door_base, door_task) = spawn(Served::front_door(
        door_path.clone(),
        CacheConfig {
            upstream: origin_base,
            cap_bytes: 64 * 1024 * 1024,
            index_ttl: Duration::from_secs(3600),
        },
    ))
    .await;

    let id = poisoned.id();
    let refused = tokio::task::spawn_blocking(move || {
        let mut fetcher = HttpFetcher::new(&door_base);
        fetcher.fetch_piece(&id)
    })
    .await
    .expect("client task");
    assert!(
        matches!(refused, Err(FetchError::Transport { .. })),
        "the front door must answer an error, got {refused:?}"
    );
    assert!(
        !piece_path(&door_path, &poisoned.id()).exists(),
        "bytes that do not hash to their name are never cached"
    );
    origin_task.abort();
    door_task.abort();
}
