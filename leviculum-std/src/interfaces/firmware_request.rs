//! Commanded firmware requests, out of band.
//!
//! The vocabulary and the broadcast a caller (lnsd's signal handlers) uses to
//! ask every serial interface in this process for a frame on its board's data
//! port; each serial interface subscribes when its port opens. Kept
//! outside the `serial` feature so a build without it still answers each
//! signal, with "0 interfaces reached", instead of dying of the signal's
//! default action (#463).

/// What a caller can ask this daemon to put on its boards' data ports.
///
/// One frame per variant, and nothing else: this is the escape hatch for
/// the ports the daemon holds exclusively, not a general control channel.
/// A variant earns its place by being impossible to do any other way
/// while the daemon is up.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FirmwareRequest {
    /// Reboot the board ([`leviculum_core::rnode::RADIO_RESET_FRAME`]).
    Reset,
    /// Announce now ([`leviculum_core::envelope::TYPE_ANNOUNCE`]): the
    /// board makes every announce it would make on its own cadence,
    /// immediately. Reaches the daemon as
    /// [`FIRMWARE_ANNOUNCE_SIGNAL`].
    ///
    /// Same standing as the reset, and for the same reason. A harness
    /// that has to observe a client learning a board's destination
    /// cannot wait out the board's announce interval and call the result
    /// a measurement — a 300 s interval against a 300 s step budget is a
    /// race the run loses roughly half the time (periculum's
    /// `ble_pn_board_upload`, 2026-09-14). The daemon is the only
    /// process that can ask, because it holds the port.
    Announce,
    /// Take every carrier off the air: read the board's media profile
    /// ([`leviculum_core::envelope::TYPE_MEDIA_QUERY`]), remember it, then
    /// write a profile with both carriers clear
    /// ([`leviculum_core::envelope::TYPE_MEDIA_PROFILE`]). Reaches the
    /// daemon as [`FIRMWARE_MEDIA_SILENCE_SIGNAL`].
    ///
    /// Earned against the sentence above, and the alternatives are what
    /// earn it. A scenario that measures how a mesh behaves when one node
    /// stops being heard has to stop it being heard *mid-run*: cutting its
    /// power takes its clock, its queues and its links with it, a
    /// [`FirmwareRequest::Reset`] reboots it into a state the run did not
    /// produce, and stopping this daemon takes the port with it — so
    /// nothing would be left that could put the carriers back afterwards.
    /// Silencing leaves the node running and the port answering, which is
    /// the only shape in which "the mesh routed around a node that went
    /// quiet" is a statement about the mesh rather than about the harness.
    /// It is impossible any other way while the daemon is up, for the same
    /// reason the reset is: the daemon holds the port.
    MediaSilence,
    /// Put back exactly the profile [`FirmwareRequest::MediaSilence`] read
    /// off this board before it silenced it. Reaches the daemon as
    /// [`FIRMWARE_MEDIA_RESTORE_SIGNAL`].
    ///
    /// A second variant rather than a parameter on the first, because the
    /// two arrive as two signals and a signal carries no payload. It earns
    /// its place for the reason the silence does — the port is held — plus
    /// one of its own: a silence is only honest if it is reversible, and
    /// the only process that can reverse it is the one holding the port.
    /// Half a verb would leave every silenced board silent until someone
    /// stopped the daemon and opened the port by hand.
    ///
    /// **The daemon owns what "restore" means.** The profile written back
    /// is the one this daemon read off that board, never a value a caller
    /// supplied and never a default: a scenario file that could name a
    /// profile would be making a configuration change wearing a restore's
    /// name, and the run after it would be measuring a board the run
    /// itself reconfigured. [`remembered_media_profile`](super::remembered_media_profile) is the whole
    /// vocabulary, and a restore with nothing remembered is the documented
    /// no-op there.
    MediaRestore,
}

/// The signal a caller sends lnsd to ask for [`FirmwareRequest::Announce`].
///
/// A NUMBER and not a name, which is the whole point. The named spares are
/// gone — SIGUSR1 is the diagnostics dump, SIGUSR2 the firmware reset, and
/// SIGHUP has to keep terminating a daemon run from a terminal — so this
/// lives in the real-time range Linux reserves for signals with no prior
/// meaning. But `SIGRTMIN` is a libc opinion, not a constant: measured on
/// this host, glibc answers 34 and musl 35 (musl keeps 32-34 for its own
/// `__synccall`), and lnsd ships in both flavours — the workspace builds
/// x86_64-unknown-linux-musl by default (`.cargo/config.toml`) while
/// `docker kill --signal=SIGRTMIN` and `/usr/bin/kill -RTMIN` both resolve
/// against the SENDER's glibc. Naming the signal would have sent 34 to a
/// daemon listening on 35, and 34 in a musl process is a signal musl
/// reserves.
///
/// 40 instead: inside the real-time range of both (glibc 34-64, musl
/// 35-64) and clear of the low end, so a libc that reserves another one or
/// two still leaves it free. Senders write the number too — periculum's
/// `announce_board` step is the other end of this contract.
pub const FIRMWARE_ANNOUNCE_SIGNAL: i32 = 40;

/// The signal a caller sends lnsd to ask for
/// [`FirmwareRequest::MediaSilence`].
///
/// Numbers and not names, for the reason [`FIRMWARE_ANNOUNCE_SIGNAL`] gives
/// in full — and re-measured rather than inherited, because the whole point
/// of that reasoning is that `SIGRTMIN` is a libc's opinion and an opinion
/// can change under us. Measured again on this host on 2026-09-22, by
/// compiling one C program that prints `SIGRTMIN` and `SIGRTMAX` with each
/// libc's own compiler: `gcc` (Debian GLIBC 2.41-12+deb13u4) answers
/// `SIGRTMIN=34 SIGRTMAX=64`, `musl-gcc` (musl 1.2.5-3.1~deb13u1) answers
/// `SIGRTMIN=35 SIGRTMAX=64`. So the range that is real-time under both
/// libcs is 35-64, and the announce's 40 still sits inside it.
///
/// 41 and 42 continue upward from 40 rather than starting a new band: they
/// are clear of the low end by the same margin, so a libc that reserves
/// another one or two leaves all three free, and consecutive numbers make
/// the three verbs one surface to read. Senders write the numbers too —
/// periculum's steps are the other end of this contract, exactly as
/// `announce_board` is for 40.
pub const FIRMWARE_MEDIA_SILENCE_SIGNAL: i32 = 41;

/// The signal a caller sends lnsd to ask for
/// [`FirmwareRequest::MediaRestore`]; see
/// [`FIRMWARE_MEDIA_SILENCE_SIGNAL`] for the measurement behind the number.
pub const FIRMWARE_MEDIA_RESTORE_SIGNAL: i32 = 42;

/// Broadcast of a [`FirmwareRequest`] to every attached board.
///
/// The daemon holds each firmware board's data port for its whole run, and
/// the serial crate opens it `TIOCEXCL`, so nothing else on the host can
/// open it while the daemon is up. That is the right default — two writers
/// interleaving mid-frame is a corrupted link — but it leaves no way to
/// reboot a board that a running daemon is attached to. A harness that
/// measures whether a mesh re-forms after a node goes away has to take the
/// BOARD out, and the only process that can speak to it is this one
/// (periculum #255).
///
/// Broadcast rather than addressed because the request arrives as a
/// process signal, which is addressed to the process and not to one
/// interface. A daemon holds the boards of one node; "every board this
/// daemon has" is the same set either way, and an addressed variant would
/// need a name mapping that nothing else in this file has.
///
/// The frame put on the wire is byte-identical to the one an unattached
/// reset writes ([`leviculum_core::rnode::RADIO_RESET_FRAME`], HDLC-framed
/// exactly as an outgoing packet is), so a board rebooted through here
/// comes back from the same defined state — which is what makes the two
/// resets comparable at all.
static FIRMWARE_REQUESTS: std::sync::OnceLock<tokio::sync::broadcast::Sender<FirmwareRequest>> =
    std::sync::OnceLock::new();

pub(crate) fn firmware_request_channel() -> &'static tokio::sync::broadcast::Sender<FirmwareRequest>
{
    FIRMWARE_REQUESTS.get_or_init(|| tokio::sync::broadcast::channel(4).0)
}

/// Ask every serial interface in this process to put `request` on the wire
/// to the board behind it. Returns the number of interfaces that were
/// listening — 0 means this daemon holds no firmware board, which is a
/// finding for the caller rather than an error here.
pub fn request_firmware(request: FirmwareRequest) -> usize {
    firmware_request_channel().send(request).unwrap_or(0)
}

/// [`FirmwareRequest::Reset`], by its own name.
pub fn request_firmware_reset() -> usize {
    request_firmware(FirmwareRequest::Reset)
}

/// [`FirmwareRequest::Announce`], by its own name.
pub fn request_firmware_announce() -> usize {
    request_firmware(FirmwareRequest::Announce)
}

/// [`FirmwareRequest::MediaSilence`], by its own name.
pub fn request_firmware_media_silence() -> usize {
    request_firmware(FirmwareRequest::MediaSilence)
}

/// [`FirmwareRequest::MediaRestore`], by its own name.
pub fn request_firmware_media_restore() -> usize {
    request_firmware(FirmwareRequest::MediaRestore)
}
