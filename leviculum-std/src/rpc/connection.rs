//! Wire protocol for CPython `multiprocessing.connection`
//!
//! Implements length-prefixed framing and bidirectional HMAC handshake
//! as used by Python's `multiprocessing.connection.Listener`/`Client`.
//!
//! Supports both legacy HMAC-MD5 (Python < 3.12) and modern HMAC-SHA256
//! (Python >= 3.12) authentication protocols.

use hmac::{Hmac, Mac};
use md5::Md5;
use sha2::Sha256;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

use super::error::RpcError;

type HmacSha256 = Hmac<Sha256>;
type HmacMd5 = Hmac<Md5>;

const CHALLENGE_PREFIX: &[u8] = b"#CHALLENGE#";
const WELCOME: &[u8] = b"#WELCOME#";
const FAILURE: &[u8] = b"#FAILURE#";
const SHA256_DIGEST_TAG: &[u8] = b"{sha256}";
const CHALLENGE_RANDOM_LEN: usize = 40;

/// HMAC-MD5 digest length (16 bytes). Python < 3.12 sends raw 16-byte
/// HMAC-MD5 responses without a digest tag prefix.
const MD5_DIGEST_LEN: usize = 16;
/// HMAC-SHA256 digest length (32 bytes).
const SHA256_DIGEST_LEN: usize = 32;

/// Ceiling on a message the daemon accepts: requests, and every handshake
/// message on either side.
///
/// The length prefix is read from the socket before anything has been
/// authenticated, and the buffer is allocated from it before a single payload
/// byte arrives — so whatever the ceiling is, a four-byte write is all an
/// attacker needs to spend to make us allocate it. Bounded by `i32::MAX` alone
/// that was two gigabytes per connection, which kills a Raspberry Pi Zero 2W
/// (512 MB shared with the GPU) outright.
///
/// 64 KiB is set from the request vocabulary rather than from a round number:
/// the largest request either our clients or Python `rnsd`'s clients build is
/// under 130 bytes (`rpc_request_vocabulary_fits_under_the_ceiling` in
/// `pickle.rs` measures every verb), and the only free-length field in the
/// whole vocabulary is the `reason` string of `blackhole_identity`. 64 KiB
/// leaves roughly 65_000 bytes for that reason — far past any human-written
/// one — and still costs at most 64 KiB per connection to refuse.
pub(crate) const MAX_REQUEST_LEN: usize = 64 * 1024;

/// Ceiling on a response a client accepts, and on any message we emit.
///
/// Responses are legitimately large where requests are not: `path_table` and
/// `transport_tables` serialise one dict per table row, so their size follows
/// the node's tables and reaches tens of megabytes on a busy transport node.
/// A request-sized ceiling here would break `lnstatus`/`rnpath` against
/// exactly the nodes worth asking, so the two roles get two ceilings.
///
/// This one is not a pre-auth surface: a client only reads a response after
/// [`client_handshake`] has completed, and the handshake itself reads through
/// [`MAX_REQUEST_LEN`]. 128 MiB is ~4x the largest response we can currently
/// produce (a path row costs ~130 bytes encoded, so it admits ~1M rows) and
/// cuts the unauthenticated-to-us worst case by 16x against `i32::MAX`.
pub(crate) const MAX_RESPONSE_LEN: usize = 128 * 1024 * 1024;

/// Read a length-prefixed message from a stream, accepting at most
/// [`MAX_REQUEST_LEN`] bytes.
///
/// Format: `[4-byte big-endian i32 length][payload]`
pub(crate) async fn read_message<R: AsyncRead + Unpin>(
    stream: &mut R,
) -> Result<Vec<u8>, RpcError> {
    read_message_bounded(stream, MAX_REQUEST_LEN).await
}

/// Read a length-prefixed message, refusing any length above `max_len` before
/// allocating for it.
///
/// The order matters and is the whole fix: the length is compared against the
/// ceiling first, so an over-large prefix costs an error message and nothing
/// else. Callers reading a response pass [`MAX_RESPONSE_LEN`]; everything
/// else goes through [`read_message`].
pub(crate) async fn read_message_bounded<R: AsyncRead + Unpin>(
    stream: &mut R,
    max_len: usize,
) -> Result<Vec<u8>, RpcError> {
    let mut len_buf = [0u8; 4];
    stream.read_exact(&mut len_buf).await?;
    let len = i32::from_be_bytes(len_buf);

    if len < 0 {
        // Large message format (>= 2 GB), not expected in RPC
        return Err(RpcError::InvalidFormat(
            "large message format not supported".into(),
        ));
    }

    let len = len as usize;
    if len > max_len {
        return Err(RpcError::InvalidFormat(format!(
            "message length {} exceeds ceiling {}",
            len, max_len
        )));
    }

    let mut buf = vec![0u8; len];
    stream.read_exact(&mut buf).await?;
    Ok(buf)
}

/// Write a length-prefixed message to a stream.
///
/// Refuses above [`MAX_RESPONSE_LEN`] — the ceiling the reading side accepts.
/// Emitting more than that would produce a message our own client refuses on
/// read, so the failure belongs here, where the caller can see it, rather than
/// after the bytes have been shovelled at a peer that will drop them.
pub(crate) async fn write_message<W: AsyncWrite + Unpin>(
    stream: &mut W,
    data: &[u8],
) -> Result<(), RpcError> {
    let len = data.len();
    if len > MAX_RESPONSE_LEN {
        return Err(RpcError::InvalidFormat(format!(
            "message too large: {} bytes exceeds ceiling {}",
            len, MAX_RESPONSE_LEN
        )));
    }
    let len_buf = (len as i32).to_be_bytes();
    stream.write_all(&len_buf).await?;
    stream.write_all(data).await?;
    stream.flush().await?;
    Ok(())
}

/// Server-side: send challenge, verify client's HMAC response, send WELCOME/FAILURE.
///
/// We always send the modern `{sha256}` challenge. The client's response
/// determines which protocol it speaks:
/// - 16 bytes: legacy HMAC-MD5 (Python < 3.12)
/// - `{sha256}` + 32 bytes: modern HMAC-SHA256 (Python >= 3.12)
pub(crate) async fn deliver_challenge<S: AsyncRead + AsyncWrite + Unpin>(
    stream: &mut S,
    authkey: &[u8; 32],
) -> Result<(), RpcError> {
    // Generate challenge: #CHALLENGE#{sha256} + 40 random bytes
    let mut challenge =
        Vec::with_capacity(CHALLENGE_PREFIX.len() + SHA256_DIGEST_TAG.len() + CHALLENGE_RANDOM_LEN);
    challenge.extend_from_slice(CHALLENGE_PREFIX);
    challenge.extend_from_slice(SHA256_DIGEST_TAG);

    let mut random_bytes = [0u8; CHALLENGE_RANDOM_LEN];
    rand_core::OsRng.fill_bytes(&mut random_bytes);
    challenge.extend_from_slice(&random_bytes);

    write_message(stream, &challenge).await?;

    // Read HMAC response from client
    let response = read_message(stream).await?;

    // message = everything after #CHALLENGE# (includes {sha256} prefix)
    let message = &challenge[CHALLENGE_PREFIX.len()..];

    let verified = if response.len() == MD5_DIGEST_LEN {
        // Legacy HMAC-MD5 response (Python < 3.12):
        // Client computed HMAC-MD5(authkey, "{sha256}" + random_bytes)
        let mut mac = HmacMd5::new_from_slice(authkey)
            .map_err(|e| RpcError::InvalidFormat(format!("HMAC-MD5 init: {}", e)))?;
        mac.update(message);
        mac.verify_slice(&response).is_ok()
    } else if response.starts_with(SHA256_DIGEST_TAG)
        && response.len() == SHA256_DIGEST_TAG.len() + SHA256_DIGEST_LEN
    {
        // Modern HMAC-SHA256 response (Python >= 3.12):
        // Client computed HMAC-SHA256(authkey, "{sha256}" + random_bytes)
        let mut mac = HmacSha256::new_from_slice(authkey)
            .map_err(|e| RpcError::InvalidFormat(format!("HMAC-SHA256 init: {}", e)))?;
        mac.update(message);
        let hmac_bytes = &response[SHA256_DIGEST_TAG.len()..];
        mac.verify_slice(hmac_bytes).is_ok()
    } else {
        false
    };

    if !verified {
        write_message(stream, FAILURE).await?;
        return Err(RpcError::AuthFailed);
    }

    write_message(stream, WELCOME).await?;
    Ok(())
}

/// Server-side: receive client's challenge, compute HMAC, send response, read WELCOME/FAILURE.
///
/// Detects whether the client sent a modern `{sha256}`-prefixed challenge
/// or a legacy challenge (no digest prefix), and responds accordingly:
/// - Modern: HMAC-SHA256 with `{sha256}` prefix in response
/// - Legacy: raw HMAC-MD5 (16 bytes, no prefix)
pub(crate) async fn answer_challenge<S: AsyncRead + AsyncWrite + Unpin>(
    stream: &mut S,
    authkey: &[u8; 32],
) -> Result<(), RpcError> {
    let challenge = read_message(stream).await?;

    // Parse: must start with #CHALLENGE#
    if !challenge.starts_with(CHALLENGE_PREFIX) {
        return Err(RpcError::InvalidFormat("missing #CHALLENGE# prefix".into()));
    }

    let message = &challenge[CHALLENGE_PREFIX.len()..];

    let response = if message.starts_with(SHA256_DIGEST_TAG) {
        // Modern protocol: compute HMAC-SHA256 over full message (including {sha256} prefix)
        let mut mac = HmacSha256::new_from_slice(authkey)
            .map_err(|e| RpcError::InvalidFormat(format!("HMAC-SHA256 init: {}", e)))?;
        mac.update(message);
        let digest = mac.finalize().into_bytes();

        let mut resp = Vec::with_capacity(SHA256_DIGEST_TAG.len() + SHA256_DIGEST_LEN);
        resp.extend_from_slice(SHA256_DIGEST_TAG);
        resp.extend_from_slice(&digest);
        resp
    } else {
        // Legacy protocol (Python < 3.12): compute HMAC-MD5 over raw message
        let mut mac = HmacMd5::new_from_slice(authkey)
            .map_err(|e| RpcError::InvalidFormat(format!("HMAC-MD5 init: {}", e)))?;
        mac.update(message);
        mac.finalize().into_bytes().to_vec()
    };

    write_message(stream, &response).await?;

    // Read WELCOME or FAILURE
    let reply = read_message(stream).await?;
    if reply == WELCOME {
        Ok(())
    } else {
        Err(RpcError::AuthFailed)
    }
}

/// Full server-side handshake: deliver_challenge then answer_challenge.
///
/// Matches Python's `Listener.accept()` which calls:
/// 1. `deliver_challenge(conn, authkey)`, server authenticates client
/// 2. `answer_challenge(conn, authkey)`, server answers client's challenge
pub(crate) async fn server_handshake<S: AsyncRead + AsyncWrite + Unpin>(
    stream: &mut S,
    authkey: &[u8; 32],
) -> Result<(), RpcError> {
    deliver_challenge(stream, authkey).await?;
    answer_challenge(stream, authkey).await?;
    Ok(())
}

/// Full client-side handshake: answer_challenge then deliver_challenge.
///
/// Matches Python's `Client()` which calls:
/// 1. `answer_challenge(conn, authkey)`, client answers server's challenge
/// 2. `deliver_challenge(conn, authkey)`, client authenticates server
pub(crate) async fn client_handshake<S: AsyncRead + AsyncWrite + Unpin>(
    stream: &mut S,
    authkey: &[u8; 32],
) -> Result<(), RpcError> {
    answer_challenge(stream, authkey).await?;
    deliver_challenge(stream, authkey).await?;
    Ok(())
}

use rand_core::RngCore;

/// Test-only allocator seam: records the largest single allocation made on a
/// thread while it is armed.
///
/// The defect this file fixes is an allocation, not a read — the length prefix
/// alone made us allocate, whether or not the payload ever arrived. Asserting
/// only that the connection is refused would leave that untested, so the
/// refusal test measures the allocation directly.
#[cfg(test)]
mod alloc_probe {
    use std::alloc::{GlobalAlloc, Layout, System};
    use std::cell::Cell;

    thread_local! {
        /// Largest single allocation on this thread since arming; `None` on a
        /// thread that is not armed — which is every other test in this
        /// binary, for which the probe costs one thread-local read.
        ///
        /// `const`-initialised and `Drop`-free on purpose: a thread-local that
        /// needs lazy initialisation or a destructor would allocate from
        /// inside the allocator.
        static PEAK: Cell<Option<usize>> = const { Cell::new(None) };
    }

    struct PeakProbe;

    fn record(size: usize) {
        let _ = PEAK.try_with(|peak| {
            if let Some(seen) = peak.get() {
                if size > seen {
                    peak.set(Some(size));
                }
            }
        });
    }

    // SAFETY: every method forwards to `System` unchanged; `record` only
    // touches a `Cell<Option<usize>>` that never allocates.
    unsafe impl GlobalAlloc for PeakProbe {
        unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
            record(layout.size());
            unsafe { System.alloc(layout) }
        }

        unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
            record(layout.size());
            unsafe { System.alloc_zeroed(layout) }
        }

        unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
            record(new_size);
            unsafe { System.realloc(ptr, layout, new_size) }
        }

        unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
            unsafe { System.dealloc(ptr, layout) }
        }
    }

    #[global_allocator]
    static PROBE: PeakProbe = PeakProbe;

    /// Run `f` with the probe armed on this thread; return its result and the
    /// largest single allocation made while it ran.
    pub(super) fn measure<T>(f: impl FnOnce() -> T) -> (T, usize) {
        PEAK.with(|peak| peak.set(Some(0)));
        let out = f();
        let peak = PEAK.with(|peak| peak.replace(None)).unwrap_or(0);
        (out, peak)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::pin::Pin;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;
    use std::task::{Context, Poll};
    use tokio::io::{duplex, ReadBuf};

    /// Reader over a fixed byte string that counts how often it is polled.
    ///
    /// The count separates "refused before reading the payload" from
    /// "attempted the payload read and hit EOF": both end in an `Err`, only
    /// the first is the fix.
    struct CountingReader {
        data: Vec<u8>,
        pos: usize,
        polls: Arc<AtomicUsize>,
    }

    impl AsyncRead for CountingReader {
        fn poll_read(
            mut self: Pin<&mut Self>,
            _cx: &mut Context<'_>,
            buf: &mut ReadBuf<'_>,
        ) -> Poll<std::io::Result<()>> {
            self.polls.fetch_add(1, Ordering::Relaxed);
            let available = self.data.len() - self.pos;
            let n = available.min(buf.remaining());
            let from = self.pos;
            buf.put_slice(&self.data[from..from + n]);
            self.pos += n;
            // n == 0 is EOF, which is what `read_exact` needs to see when the
            // prefix promised bytes that never come.
            Poll::Ready(Ok(()))
        }
    }

    /// A length prefix claiming almost two gigabytes must be refused by name,
    /// and nothing of that size may be allocated on the way out. Before the
    /// ceiling existed this test failed on the allocation, not the error: the
    /// `vec![0u8; len]` ran first and `read_exact` then reported
    /// `UnexpectedEof`.
    #[test]
    fn oversize_length_prefix_is_refused_before_allocating() {
        let claimed = i32::MAX as usize;
        let polls = Arc::new(AtomicUsize::new(0));
        let mut reader = CountingReader {
            data: (claimed as i32).to_be_bytes().to_vec(),
            pos: 0,
            polls: Arc::clone(&polls),
        };

        let rt = tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap();
        let (result, peak) =
            alloc_probe::measure(|| rt.block_on(async { read_message(&mut reader).await }));

        // The allocation is the defect, so it is asserted first: without the
        // ceiling this line reports the full 2 GiB.
        assert!(
            peak < 1024 * 1024,
            "a 4-byte prefix claiming {claimed} bytes allocated {peak} bytes"
        );
        assert_eq!(
            polls.load(Ordering::Relaxed),
            1,
            "the payload read must never be attempted"
        );

        match result {
            Err(RpcError::InvalidFormat(msg)) => {
                assert!(
                    msg.contains(&claimed.to_string()) && msg.contains("ceiling"),
                    "refusal must name the length and the ceiling, got: {msg}"
                );
            }
            other => panic!("expected an InvalidFormat refusal, got {:?}", other),
        }
    }

    /// Positive control for the test above: the probe does see an allocation
    /// of the shape `read_message` makes (`vec![0u8; n]`, i.e. `alloc_zeroed`),
    /// so a peak of zero there means absence, not a blind probe.
    #[test]
    fn the_allocation_probe_sees_a_zeroed_vec() {
        let wanted = 8 * 1024 * 1024;
        let (len, peak) = alloc_probe::measure(|| {
            let buf = vec![0u8; wanted];
            std::hint::black_box(&buf);
            buf.len()
        });
        assert_eq!(len, wanted);
        assert!(peak >= wanted, "probe saw {peak} bytes, wanted {wanted}");
    }

    /// The off-by-one that would silently break a legitimate client: a message
    /// of exactly the ceiling is a message, one byte more is not.
    #[tokio::test]
    async fn a_message_at_exactly_the_ceiling_still_arrives() {
        let payload = vec![0x5Au8; MAX_REQUEST_LEN];
        let mut framed = (MAX_REQUEST_LEN as i32).to_be_bytes().to_vec();
        framed.extend_from_slice(&payload);

        let received = read_message(&mut framed.as_slice()).await.unwrap();
        assert_eq!(received.len(), MAX_REQUEST_LEN);
        assert_eq!(received, payload);

        let mut over = ((MAX_REQUEST_LEN + 1) as i32).to_be_bytes().to_vec();
        over.extend_from_slice(&payload);
        over.push(0x5A);
        assert!(
            read_message(&mut over.as_slice()).await.is_err(),
            "one byte above the ceiling must be refused"
        );
    }

    /// Requests and responses get different ceilings on purpose: a response
    /// carries one dict per table row and dwarfs any request. The bounded
    /// reader is what lets the client keep reading those.
    #[tokio::test]
    async fn the_response_ceiling_admits_what_the_request_ceiling_refuses() {
        const { assert!(MAX_RESPONSE_LEN > MAX_REQUEST_LEN) };

        let big = MAX_REQUEST_LEN + 1;
        let mut framed = (big as i32).to_be_bytes().to_vec();
        framed.extend_from_slice(&vec![0x17u8; big]);

        assert!(read_message(&mut framed.as_slice()).await.is_err());
        let received = read_message_bounded(&mut framed.as_slice(), MAX_RESPONSE_LEN)
            .await
            .unwrap();
        assert_eq!(received.len(), big);
    }

    #[tokio::test]
    async fn test_message_round_trip() {
        let (mut client, mut server) = duplex(1024);
        let data = b"hello world";

        write_message(&mut client, data).await.unwrap();
        let received = read_message(&mut server).await.unwrap();
        assert_eq!(received, data);
    }

    #[tokio::test]
    async fn test_empty_message() {
        let (mut client, mut server) = duplex(1024);
        write_message(&mut client, b"").await.unwrap();
        let received = read_message(&mut server).await.unwrap();
        assert!(received.is_empty());
    }

    #[tokio::test]
    async fn test_handshake_success() {
        let authkey = [0x42u8; 32];
        let (mut client, mut server) = duplex(4096);

        let server_task =
            tokio::spawn(async move { server_handshake(&mut server, &authkey).await });
        let client_task =
            tokio::spawn(async move { client_handshake(&mut client, &authkey).await });

        server_task.await.unwrap().unwrap();
        client_task.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn test_handshake_wrong_key() {
        let server_key = [0x42u8; 32];
        let client_key = [0x99u8; 32];
        let (mut client, mut server) = duplex(4096);

        let server_task =
            tokio::spawn(async move { server_handshake(&mut server, &server_key).await });
        let client_task =
            tokio::spawn(async move { client_handshake(&mut client, &client_key).await });

        // At least one side should fail
        let (server_result, client_result) = tokio::join!(server_task, client_task);
        let server_err = server_result.unwrap().is_err();
        let client_err = client_result.unwrap().is_err();
        assert!(
            server_err || client_err,
            "mismatched keys must cause auth failure"
        );
    }

    #[tokio::test]
    async fn test_deliver_challenge_bad_response_length() {
        let authkey = [0x42u8; 32];
        let (mut client, mut server) = duplex(4096);

        // Server sends challenge
        let server_task =
            tokio::spawn(async move { deliver_challenge(&mut server, &authkey).await });

        // Client sends wrong-length response
        let client_task = tokio::spawn(async move {
            let _challenge = read_message(&mut client).await.unwrap();
            write_message(&mut client, b"too short").await.unwrap();
            // Read the FAILURE response
            let reply = read_message(&mut client).await.unwrap();
            assert_eq!(reply, FAILURE);
        });

        let server_result = server_task.await.unwrap();
        assert!(server_result.is_err());
        client_task.await.unwrap();
    }

    #[tokio::test]
    async fn test_full_handshake_then_message() {
        let authkey = [0xABu8; 32];
        let (mut client, mut server) = duplex(4096);

        let server_task = tokio::spawn(async move {
            server_handshake(&mut server, &authkey).await.unwrap();
            // Read a request
            let msg = read_message(&mut server).await.unwrap();
            assert_eq!(msg, b"ping");
            // Send a response
            write_message(&mut server, b"pong").await.unwrap();
        });

        let client_task = tokio::spawn(async move {
            client_handshake(&mut client, &authkey).await.unwrap();
            // Send request
            write_message(&mut client, b"ping").await.unwrap();
            // Read response
            let resp = read_message(&mut client).await.unwrap();
            assert_eq!(resp, b"pong");
        });

        server_task.await.unwrap();
        client_task.await.unwrap();
    }

    /// Test that our server can authenticate a legacy HMAC-MD5 client
    /// (simulates Python < 3.12 behavior).
    #[tokio::test]
    async fn test_deliver_challenge_accepts_legacy_md5_client() {
        let authkey = [0x42u8; 32];
        let (mut client, mut server) = duplex(4096);

        let server_task =
            tokio::spawn(async move { deliver_challenge(&mut server, &authkey).await });

        let authkey_clone = authkey;
        let client_task = tokio::spawn(async move {
            // Read challenge from server
            let challenge = read_message(&mut client).await.unwrap();
            assert!(challenge.starts_with(CHALLENGE_PREFIX));

            // Legacy client: strip #CHALLENGE#, compute HMAC-MD5 over remainder
            let message = &challenge[CHALLENGE_PREFIX.len()..];
            let mut mac = HmacMd5::new_from_slice(&authkey_clone).unwrap();
            mac.update(message);
            let digest = mac.finalize().into_bytes();

            // Send raw 16-byte MD5 digest (no prefix)
            write_message(&mut client, &digest).await.unwrap();

            // Should get WELCOME
            let reply = read_message(&mut client).await.unwrap();
            assert_eq!(reply, WELCOME, "server should accept legacy MD5 response");
        });

        server_task.await.unwrap().unwrap();
        client_task.await.unwrap();
    }

    /// Test that our answer_challenge handles legacy challenges (no {sha256} prefix).
    #[tokio::test]
    async fn test_answer_challenge_handles_legacy_challenge() {
        let authkey = [0x42u8; 32];
        let (mut client, mut server) = duplex(4096);

        let authkey_clone = authkey;
        // Simulate a legacy Python < 3.12 server sending challenge without {sha256}
        let server_task = tokio::spawn(async move {
            // Send challenge WITHOUT {sha256} prefix (legacy format)
            let mut challenge = Vec::new();
            challenge.extend_from_slice(CHALLENGE_PREFIX);
            let mut random = [0u8; 20]; // Python < 3.12 uses 20-byte random
            rand_core::OsRng.fill_bytes(&mut random);
            challenge.extend_from_slice(&random);
            write_message(&mut server, &challenge).await.unwrap();

            // Read response, should be raw 16-byte HMAC-MD5
            let response = read_message(&mut server).await.unwrap();
            assert_eq!(
                response.len(),
                MD5_DIGEST_LEN,
                "response to legacy challenge should be raw MD5"
            );

            // Verify the MD5 HMAC
            let message = &challenge[CHALLENGE_PREFIX.len()..];
            let mut mac = HmacMd5::new_from_slice(&authkey_clone).unwrap();
            mac.update(message);
            assert!(
                mac.verify_slice(&response).is_ok(),
                "MD5 HMAC should verify"
            );

            write_message(&mut server, WELCOME).await.unwrap();
        });

        let client_task =
            tokio::spawn(async move { answer_challenge(&mut client, &authkey).await });

        server_task.await.unwrap();
        client_task.await.unwrap().unwrap();
    }

    /// Test full handshake between Rust server and simulated legacy Python < 3.12 client.
    #[tokio::test]
    async fn test_full_handshake_with_legacy_md5_client() {
        let authkey = [0x55u8; 32];
        let (mut client, mut server) = duplex(4096);

        // Server side: normal Rust server
        let server_task =
            tokio::spawn(async move { server_handshake(&mut server, &authkey).await });

        let authkey_clone = authkey;
        // Client side: simulate Python 3.11 (legacy MD5)
        let client_task = tokio::spawn(async move {
            // Phase 1: answer server's challenge with HMAC-MD5
            let challenge = read_message(&mut client).await.unwrap();
            let message = &challenge[CHALLENGE_PREFIX.len()..];
            let mut mac = HmacMd5::new_from_slice(&authkey_clone).unwrap();
            mac.update(message);
            let digest = mac.finalize().into_bytes();
            write_message(&mut client, &digest).await.unwrap();
            let reply = read_message(&mut client).await.unwrap();
            assert_eq!(reply, WELCOME);

            // Phase 2: send own challenge (legacy: no {sha256} prefix)
            let mut challenge2 = Vec::new();
            challenge2.extend_from_slice(CHALLENGE_PREFIX);
            let mut random = [0u8; 20];
            rand_core::OsRng.fill_bytes(&mut random);
            challenge2.extend_from_slice(&random);
            write_message(&mut client, &challenge2).await.unwrap();

            // Read server's response, should be raw MD5
            let response = read_message(&mut client).await.unwrap();
            assert_eq!(response.len(), MD5_DIGEST_LEN);

            let msg = &challenge2[CHALLENGE_PREFIX.len()..];
            let mut mac2 = HmacMd5::new_from_slice(&authkey_clone).unwrap();
            mac2.update(msg);
            assert!(mac2.verify_slice(&response).is_ok());

            write_message(&mut client, WELCOME).await.unwrap();
        });

        server_task.await.unwrap().unwrap();
        client_task.await.unwrap();
    }
}
