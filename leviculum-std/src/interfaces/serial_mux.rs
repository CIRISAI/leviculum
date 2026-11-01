//! The host half of the single-UART link (`leviculum-nrf/serial-mux`).
//!
//! A board whose one UART carries both its debug text and its HDLC transport
//! (the ThinkNode M9) runs that line under XON/XOFF software flow control,
//! and its UART obeys a received [`XOFF`] in hardware: one raw 0x13 from the
//! host stops the board's transmitter until a 0x11 arrives, so the board
//! goes silent on its serial port with the CPU still running (1238). The
//! contract (`leviculum-nrf/serial-mux/src/lib.rs`, module doc) is that
//! neither byte travels raw for any other reason: every sender escapes both
//! inside its frames ([`escape_flow`]), and the host strips the board's own
//! flow bytes before anything reads the stream ([`strip_flow`]).
//!
//! [`SerialMuxPort`] is that contract at the port, so every frame the serial
//! interface writes (the radio config, the queries, the media verbs, data)
//! is covered at one site. The board's receiver is the core `Deframer`,
//! which undoes any escaped byte (`Deframer::process_byte`,
//! `leviculum-core/src/framing/hdlc.rs:325-327`), so the escape changes
//! nothing it decodes. The serial interface's own reader is the same
//! `Deframer`, so an escaped flow byte from the board decodes too.
//!
//! Only a board that needs it gets it. A plain `SerialInterface` is
//! Python's `SerialInterface` on the other end, whose reader turns only an
//! escaped FLAG or ESCAPE back (`RNS/Interfaces/SerialInterface.py:180-184`
//! in Reticulum 1.3.5), so `7d 33` would reach it as 0x33; and a USB CDC
//! LNode frames 0x11 and 0x13 raw, so stripping them on that line would cut
//! its frames.
//!
//! Which one is on the port, the board says itself ([`detect_serial_mux`]):
//! with the block's `serial_mux` key unset the bring-up asks for the
//! capability report before any other frame and turns the mux on when the
//! report states `envelope::FACT_SERIAL_MUX`. A CH340 has no USB strings to
//! tell an M9 from any other serial peer, so the port cannot. The key, set
//! either way, overrides the detection.

use std::io;
use std::pin::Pin;
use std::task::{Context, Poll};

use std::time::Duration;

use leviculum_core::envelope;
use leviculum_core::framing::hdlc::{frame, DeframeResult, Deframer};
use leviculum_serial_mux::{escape_flow, strip_flow, Flow, XOFF, XON};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, ReadBuf};

/// A serial port that, when `mux` is set, escapes [`XON`] and [`XOFF`] in
/// everything written and removes them from everything read; when it is
/// not, the bytes pass unchanged in both directions.
///
/// A write that returns `Pending` must be retried with the same bytes, as
/// `write_all` does: the escaped copy of the first call is what goes on the
/// line, and the call reports the caller's length once all of it has.
pub struct SerialMuxPort<S> {
    inner: S,
    mux: bool,
    /// The escaped copy of the write in progress; empty between writes.
    pending: Vec<u8>,
    /// How much of `pending` is on the line.
    sent: usize,
    /// The caller's length for `pending`, reported once it is all sent.
    accepted: usize,
    /// The read path's stripped bytes, reused across reads.
    scratch: Vec<u8>,
    /// The board's last flow request, for the log edge.
    flow: Option<Flow>,
}

impl<S> SerialMuxPort<S> {
    /// Wrap `inner`; `mux` is the block's `serial_mux` key.
    pub fn new(inner: S, mux: bool) -> Self {
        Self {
            inner,
            mux,
            pending: Vec::new(),
            sent: 0,
            accepted: 0,
            scratch: Vec::new(),
            flow: None,
        }
    }

    /// Whether the mux is on.
    pub fn mux(&self) -> bool {
        self.mux
    }

    /// Turn the mux on or off, once [`detect_serial_mux`] has decided.
    /// Only between writes: a write in progress keeps its own copy.
    pub fn set_mux(&mut self, mux: bool) {
        self.mux = mux;
    }
}

/// How long one capability query waits for the board's report.
///
/// The M9 answered 7 ms after the query, 6 of 6 runs on the rig (1238,
/// `attachments/1238-m9-probe/cap-*.log` and `capesc-*.log`: `TX capability
/// query` to `FRAME envelope type=0x83`). The answer is composed by the
/// board's serial task the moment the frame lands, so a second is two
/// orders of magnitude of margin, not a guess at a slow board.
pub const CAPABILITY_ANSWER_TIMEOUT: Duration = Duration::from_secs(1);

/// How many queries the detection sends before it calls the board silent.
///
/// Two, because the port may be opened while the board is still booting:
/// the M9 prints its `BOOT_TRACE` line through the serial writer 561 ms
/// after reset (same logs), so a query that landed before its reader ran is
/// lost, and the second one, a second later, finds it up. The whole bound a
/// peer that never answers costs the bring-up is therefore
/// `CAPABILITY_QUERY_ATTEMPTS * CAPABILITY_ANSWER_TIMEOUT`, 2 s.
pub const CAPABILITY_QUERY_ATTEMPTS: u8 = 2;

/// The most raw bytes the detection keeps for the frames it hears besides
/// the answer. The M9 writes about 400 bytes of debug text a second (1238:
/// 1836 to 1960 bytes in the 5 s after a query), so this holds the whole
/// bound with room; past it the oldest traffic is what is lost, as it would
/// have been in any bring-up wait.
const HEARD_CAP: usize = 16 * 1024;

/// The largest frame the detection deframes: the serial interface's
/// `SERIAL_HW_MTU` (Python `SerialInterface.HW_MTU`).
const DETECT_MAX_FRAME: usize = 564;

/// What the board said about its UART when asked.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MuxDetection {
    /// The capability report states `envelope::FACT_SERIAL_MUX`.
    Stated,
    /// The board answered with a report that does not state it: a USB CDC
    /// LNode, or a single-UART build from before the fact.
    NotStated,
    /// Nothing answered within the bound: a Python `SerialInterface` peer,
    /// firmware from before the envelope, or no board at all.
    NoAnswer,
    /// The port failed while asking; the caller finds it dead on its own.
    PortLost,
}

impl MuxDetection {
    /// Whether the mux goes on.
    pub fn mux(self) -> bool {
        self == MuxDetection::Stated
    }

    /// The value of the `source=` key in the bring-up's log line.
    pub fn as_str(self) -> &'static str {
        match self {
            MuxDetection::Stated => "report",
            MuxDetection::NotStated => "report-without-fact",
            MuxDetection::NoAnswer => "no-answer",
            MuxDetection::PortLost => "port-lost",
        }
    }
}

/// Ask the board on `port` whether it shares one UART under XON/XOFF.
///
/// Sends the capability query (`envelope::encode_capability_query`) up to
/// [`CAPABILITY_QUERY_ATTEMPTS`] times, each waiting
/// [`CAPABILITY_ANSWER_TIMEOUT`] for the report. Safe raw on any line: the
/// framed query is `7e a4 a5 04 00 00 7e`, which carries no flow byte, and
/// is five bytes with the IFAC bit set, shorter than any Reticulum packet,
/// so a Python peer drops it (`envelope.rs`, "Distinguishability"). Run it
/// before any other frame, on a port whose mux is still off: the radio
/// config that follows may carry a 0x13 (19 dBm), and only the answer says
/// whether that byte would mute the board.
///
/// The report is looked for twice over the same bytes: as they came, which
/// is how a CDC LNode writes its 0x11/0x13 type bytes, and with raw flow
/// bytes removed, which is how a single-UART board's own XON lands even
/// inside a frame. Either one decoding is the answer.
///
/// Returns the verdict and every other frame heard meanwhile, deframed the
/// way the verdict says the line works (flow bytes stripped when the mux
/// goes on), so a Python peer's packets during the wait reach the
/// transport instead of being dropped.
pub async fn detect_serial_mux<S>(port: &mut S, name: &str) -> (MuxDetection, Vec<Vec<u8>>)
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let mut query = Vec::new();
    frame(&envelope::encode_capability_query(), &mut query);
    let mut raw_deframer = Deframer::with_max_frame(DETECT_MAX_FRAME);
    let mut stripped_deframer = Deframer::with_max_frame(DETECT_MAX_FRAME);
    let mut heard: Vec<u8> = Vec::new();
    let mut buf = [0u8; 1024];
    let mut verdict = MuxDetection::NoAnswer;

    'attempts: for attempt in 1..=CAPABILITY_QUERY_ATTEMPTS {
        if let Err(e) = async {
            port.write_all(&query).await?;
            port.flush().await
        }
        .await
        {
            tracing::warn!("Serial {}: capability query write failed: {}", name, e);
            verdict = MuxDetection::PortLost;
            break;
        }
        let deadline = tokio::time::Instant::now() + CAPABILITY_ANSWER_TIMEOUT;
        loop {
            let n = match tokio::time::timeout_at(deadline, port.read(&mut buf)).await {
                Err(_) => {
                    tracing::debug!(
                        "Serial {}: no capability report within {} ms of query {}",
                        name,
                        CAPABILITY_ANSWER_TIMEOUT.as_millis(),
                        attempt
                    );
                    continue 'attempts;
                }
                Ok(Ok(0)) => {
                    verdict = MuxDetection::PortLost;
                    break 'attempts;
                }
                Ok(Err(e)) => {
                    tracing::warn!(
                        "Serial {}: read failed during capability query: {}",
                        name,
                        e
                    );
                    verdict = MuxDetection::PortLost;
                    break 'attempts;
                }
                Ok(Ok(n)) => n,
            };
            let bytes = &buf[..n];
            let mut stripped = Vec::with_capacity(n);
            strip_flow(bytes, &mut |run| stripped.extend_from_slice(run));
            let answer = report_facts(raw_deframer.process(bytes))
                .or_else(|| report_facts(stripped_deframer.process(&stripped)));
            if let Some(facts) = answer {
                verdict = if facts.contains(&envelope::FACT_SERIAL_MUX) {
                    MuxDetection::Stated
                } else {
                    MuxDetection::NotStated
                };
                break 'attempts;
            }
            heard.extend_from_slice(bytes);
            if heard.len() > HEARD_CAP {
                heard.drain(..heard.len() - HEARD_CAP);
            }
        }
    }

    let mut line = Vec::with_capacity(heard.len());
    if verdict.mux() {
        strip_flow(&heard, &mut |run| line.extend_from_slice(run));
    } else {
        line = heard;
    }
    let others = Deframer::with_max_frame(DETECT_MAX_FRAME)
        .process(&line)
        .into_iter()
        .filter_map(|r| match r {
            DeframeResult::Frame(data) if envelope::decode_frame(&data).is_err() => Some(data),
            _ => None,
        })
        .collect();
    (verdict, others)
}

/// Settle the mux on `port` before any other frame goes out: the block's
/// `configured` key if it has one, otherwise what [`detect_serial_mux`]
/// reads off the board. Logs one `SERIAL_MUX mux=<on|off> source=<...>`
/// line and returns the frames heard meanwhile, for the transport.
///
/// The serial interface's bring-up calls this first and
/// [`super::radio_bring_up`] after it, on the same port.
pub async fn bring_up<S>(
    port: &mut SerialMuxPort<S>,
    configured: Option<bool>,
    name: &str,
) -> Vec<Vec<u8>>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let on_off = |mux: bool| if mux { "on" } else { "off" };
    if let Some(mux) = configured {
        port.set_mux(mux);
        tracing::info!(
            "Serial {}: SERIAL_MUX mux={} source=config",
            name,
            on_off(mux)
        );
        return Vec::new();
    }
    port.set_mux(false);
    let asked = tokio::time::Instant::now();
    let (verdict, heard) = detect_serial_mux(port, name).await;
    port.set_mux(verdict.mux());
    tracing::info!(
        "Serial {}: SERIAL_MUX mux={} source={} wait_ms={}",
        name,
        on_off(verdict.mux()),
        verdict.as_str(),
        asked.elapsed().as_millis()
    );
    heard
}

/// The facts of the first capability report among `results`, if any.
fn report_facts(results: Vec<DeframeResult>) -> Option<Vec<u8>> {
    results.into_iter().find_map(|r| {
        let DeframeResult::Frame(data) = r else {
            return None;
        };
        let f = envelope::decode_frame(&data).ok()?;
        if f.frame_type != envelope::TYPE_CAPABILITY_REPORT {
            return None;
        }
        envelope::decode_capability_report_facts(f.payload).map(<[u8]>::to_vec)
    })
}

impl<S: AsyncWrite + Unpin> AsyncWrite for SerialMuxPort<S> {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        let this = &mut *self;
        if !this.mux {
            return Pin::new(&mut this.inner).poll_write(cx, buf);
        }
        if this.pending.is_empty() {
            if !buf.iter().any(|&b| b == XON || b == XOFF) {
                return Pin::new(&mut this.inner).poll_write(cx, buf);
            }
            escape_flow(buf, &mut |run| this.pending.extend_from_slice(run));
            this.sent = 0;
            this.accepted = buf.len();
        }
        while this.sent < this.pending.len() {
            match Pin::new(&mut this.inner).poll_write(cx, &this.pending[this.sent..]) {
                Poll::Ready(Ok(0)) => {
                    this.pending.clear();
                    return Poll::Ready(Err(io::ErrorKind::WriteZero.into()));
                }
                Poll::Ready(Ok(n)) => this.sent += n,
                Poll::Ready(Err(e)) => {
                    this.pending.clear();
                    return Poll::Ready(Err(e));
                }
                Poll::Pending => return Poll::Pending,
            }
        }
        this.pending.clear();
        Poll::Ready(Ok(this.accepted))
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.inner).poll_flush(cx)
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.inner).poll_shutdown(cx)
    }
}

impl<S: AsyncRead + Unpin> AsyncRead for SerialMuxPort<S> {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        let this = &mut *self;
        if !this.mux {
            return Pin::new(&mut this.inner).poll_read(cx, buf);
        }
        loop {
            let before = buf.filled().len();
            match Pin::new(&mut this.inner).poll_read(cx, buf) {
                Poll::Ready(Ok(())) => {}
                other => return other,
            }
            if buf.filled().len() == before {
                // End of stream.
                return Poll::Ready(Ok(()));
            }
            this.scratch.clear();
            let scratch = &mut this.scratch;
            let flow = strip_flow(&buf.filled()[before..], &mut |run| {
                scratch.extend_from_slice(run)
            });
            if flow.is_some() && flow != this.flow {
                tracing::trace!("serial mux: board flow request {:?}", flow);
                this.flow = flow;
            }
            let kept = this.scratch.len();
            buf.filled_mut()[before..before + kept].copy_from_slice(&this.scratch);
            buf.set_filled(before + kept);
            // A read of nothing but flow bytes is not the end of the stream:
            // read again rather than hand the caller an empty read.
            if kept > 0 {
                return Poll::Ready(Ok(()));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use leviculum_core::framing::hdlc::{frame, DeframeResult, Deframer};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    /// Write `payload` HDLC-framed through a mux port and return the bytes
    /// that reached the line.
    async fn on_the_line(payload: &[u8], mux: bool) -> Vec<u8> {
        let (near, mut far) = tokio::io::duplex(4096);
        let mut port = SerialMuxPort::new(near, mux);
        let mut framed = Vec::new();
        frame(payload, &mut framed);
        port.write_all(&framed).await.expect("write");
        drop(port);
        let mut line = Vec::new();
        far.read_to_end(&mut line).await.expect("read");
        line
    }

    fn deframe(line: &[u8]) -> Vec<Vec<u8>> {
        Deframer::new()
            .process(line)
            .into_iter()
            .filter_map(|r| match r {
                DeframeResult::Frame(data) => Some(data),
                _ => None,
            })
            .collect()
    }

    /// A frame with XOFF in every position a frame has (the envelope's
    /// length byte, the payload, and wherever a checksum would sit: the
    /// last byte) and XON beside it leaves no raw flow byte on a mux line,
    /// and the board's decoder gets the frame back unchanged.
    #[tokio::test]
    async fn a_mux_line_carries_no_raw_flow_byte_and_round_trips() {
        for len in 1..=24usize {
            for at in 0..len {
                let mut payload: Vec<u8> = (0..len as u8).map(|i| 0x40 + i).collect();
                payload[at] = XOFF;
                if at + 1 < len {
                    payload[at + 1] = XON;
                }
                let line = on_the_line(&payload, true).await;
                assert!(
                    !line.iter().any(|&b| b == XON || b == XOFF),
                    "len {len} at {at}: raw flow byte on the line {line:02x?}"
                );
                assert_eq!(deframe(&line), vec![payload], "len {len} at {at}");
            }
        }
    }

    /// The envelope's own bytes: the levels config of 1238, the frame that
    /// muted the M9, whose length field `00 13` must reach the line as
    /// `00 7d 33`, and `TYPE_MGMT_ALLOW`, which is 0x13 in the type byte.
    /// HDLC here carries no checksum, so length, type and payload are every
    /// position a frame has.
    #[tokio::test]
    async fn the_envelope_header_is_escaped() {
        use leviculum_core::envelope::{encode_frame, TYPE_MGMT_ALLOW, TYPE_RADIO_CONFIG};
        for (frame_type, len) in [(TYPE_RADIO_CONFIG, 19usize), (TYPE_MGMT_ALLOW, 1)] {
            let payload = encode_frame(frame_type, &vec![0u8; len]);
            assert!(payload.contains(&XOFF), "the fixture carries the XOFF");
            let line = on_the_line(&payload, true).await;
            assert!(!line.contains(&XOFF), "type {frame_type:#x}: {line:02x?}");
            assert_eq!(deframe(&line), vec![payload]);
        }
        let levels = on_the_line(&encode_frame(TYPE_RADIO_CONFIG, &[0u8; 19]), true).await;
        assert!(levels.windows(3).any(|w| w == [0x00, 0x7d, 0x33]));
    }

    /// Without the key nothing changes: a Python `SerialInterface` peer
    /// reads the frame byte for byte as before.
    #[tokio::test]
    async fn a_plain_line_keeps_its_bytes() {
        let payload = [XOFF, 0x01, XON, 0x7e, 0x02];
        let mut framed = Vec::new();
        frame(&payload, &mut framed);
        assert_eq!(on_the_line(&payload, false).await, framed);
    }

    /// The board's flow bytes, raw at any byte boundary and inside a frame,
    /// are gone before the deframer sees the stream; a read made of nothing
    /// but flow bytes does not look like the end of the stream.
    #[tokio::test]
    async fn the_boards_flow_bytes_are_stripped_on_read() {
        let (near, mut far) = tokio::io::duplex(4096);
        let mut port = SerialMuxPort::new(near, true);
        let payload = [0x01u8, XON, 0x02, XOFF, 0x03];
        let mut escaped = Vec::new();
        escape_flow(
            &{
                let mut f = Vec::new();
                frame(&payload, &mut f);
                f
            },
            &mut |run| escaped.extend_from_slice(run),
        );
        let mut line = vec![XOFF];
        line.extend_from_slice(&escaped[..3]);
        line.push(XON);
        line.extend_from_slice(&escaped[3..]);
        far.write_all(&[XOFF, XON]).await.expect("flow only");
        far.flush().await.expect("flush");
        far.write_all(&line).await.expect("line");
        drop(far);
        let mut got = Vec::new();
        port.read_to_end(&mut got).await.expect("read");
        assert!(!got.iter().any(|&b| b == XON || b == XOFF));
        assert_eq!(deframe(&got), vec![payload.to_vec()]);
    }
}
