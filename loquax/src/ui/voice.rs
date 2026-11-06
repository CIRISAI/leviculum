//! Voice messages in a chat (1222; Lew 2026-10-31): press and hold,
//! record, release, and it goes compressed, as the other messengers send
//! one. The wire is the LXMF audio field `0x07` holding `[mode, bytes]`
//! (`reference/LXMF/LXMF/LXMF.py:14`), the mode one of the reference's
//! audio modes (`LXMF.py:65-89`).
//!
//! What the others send and play:
//! - Columba sends Opus in Ogg, `AM_OPUS_OGG` (`0x10`), as
//!   `listOf(AM_OPUS_OGG, voiceBytes)`
//!   (`columba/app/src/main/java/network/columba/app/viewmodel/MessagingViewModel.kt:1375`),
//!   and plays nothing else: `isPlayable` is `mode == AM_OPUS_OGG`
//!   (`columba/app/src/main/java/network/columba/app/ui/model/MessageMapper.kt:301`).
//! - Sideband sends Opus in Ogg by default (`hq_ptt` defaults on,
//!   `Sideband/sbapp/sideband/core.py:643`, `Sideband/sbapp/main.py:2475`), Codec2 2400 when
//!   switched to low bandwidth (`main.py:2476`), and plays both
//!   (`main.py:2436-2446`). Its Codec2 bytes are the codec's frames back to
//!   back, without a header (`encode_codec2`, `Sideband/sbapp/sideband/audioproc.py:117-143`).
//!
//! So ours go as Opus in Ogg, the one mode every receiver plays, and ours
//! play both. The codec is the shell's (`crate::voice`, behind the
//! `voice` feature); the model knows the field, reads a recording's length
//! and a level bar off the stream without decoding it, and keeps which
//! message plays.

use alloc::string::String;
use alloc::vec::Vec;

use leviculum_lxmf::attachments::{AudioAttachment, MessageAttachments};
use leviculum_lxmf::constants::FIELD_AUDIO;
use leviculum_lxmf::msgpack;
use leviculum_lxmf::Field;

use super::strings::{Arg, StrId, Text};
use super::{UiEffect, UiModel};

/// The reference's audio modes (`reference/LXMF/LXMF/LXMF.py:65-89`).
pub const AM_CODEC2_450PWB: u8 = 0x01;
pub const AM_CODEC2_450: u8 = 0x02;
pub const AM_CODEC2_700C: u8 = 0x03;
pub const AM_CODEC2_1200: u8 = 0x04;
pub const AM_CODEC2_1300: u8 = 0x05;
pub const AM_CODEC2_1400: u8 = 0x06;
pub const AM_CODEC2_1600: u8 = 0x07;
pub const AM_CODEC2_2400: u8 = 0x08;
pub const AM_CODEC2_3200: u8 = 0x09;
pub const AM_OPUS_OGG: u8 = 0x10;

/// The bits a second a recording is coded at, on average
/// (`crate::voice::BITRATE` is this).
pub const OPUS_BITRATE: u32 = 12_000;

/// The bytes a second of a recording on the wire, counted high: the
/// average rate and 5 % for the Ogg pages (1222 measured 4.9 %).
pub const OPUS_BYTES_PER_S: u32 = OPUS_BITRATE / 8 * 105 / 100;

/// The longest recording that goes (1276): what a receiver takes,
/// [`super::files::LARGEST`] bytes, at [`OPUS_BYTES_PER_S`], in whole
/// minutes. 990 000 / 1 575 is 628 s, so ten minutes, 945 KB counted
/// high and about 720 KB at the 1 202 B/s 1222 measured. Columba stops
/// its own at five
/// (`columba/app/src/main/java/network/columba/app/audio/VoiceMessageRecorder.kt:214`)
/// and plays a longer one as it plays any.
pub const LONGEST_MS: u32 = (super::files::LARGEST as u32 / OPUS_BYTES_PER_S) / 60 * 60 * 1000;

/// The shortest recording that goes: a tap on the button is no message.
/// Sideband drops one under 0.6 s (`Sideband/sbapp/main.py:2517`).
pub const SHORTEST_MS: u32 = 600;

/// How many bars a bubble's level bar has.
pub const BARS: usize = 32;

/// What a bubble needs of a voice message without its bytes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VoiceRef {
    /// The sender's audio mode.
    pub mode: u8,
    /// The recording's bytes on the wire.
    pub bytes: usize,
    /// How long it plays, in milliseconds; 0 where the stream does not say.
    pub duration_ms: u32,
    /// [`BARS`] heights, 0 to 100, loudness along the recording; empty
    /// where the stream does not tell (Codec2 frames are all one size).
    pub levels: Vec<u8>,
}

impl VoiceRef {
    /// Whether this build plays it: Opus in Ogg and the Codec2 modes the
    /// decoder has (not 700C and the two 450s).
    pub fn playable(&self) -> bool {
        playable(self.mode)
    }
}

/// Whether a voice message of `mode` plays here.
pub fn playable(mode: u8) -> bool {
    matches!(
        mode,
        AM_OPUS_OGG
            | AM_CODEC2_1200
            | AM_CODEC2_1300
            | AM_CODEC2_1400
            | AM_CODEC2_1600
            | AM_CODEC2_2400
            | AM_CODEC2_3200
    )
}

/// The mode and bytes of the audio field `fields` carry; none for no
/// field. Read leniently: the mode a positive integer, the bytes a bin or
/// a str, an element past the second skipped.
pub fn voice_parts(fields: &[Field]) -> Option<(u8, &[u8])> {
    let (_, value) = fields.iter().find(|(key, _)| *key == FIELD_AUDIO)?;
    let mut at = 0;
    if msgpack::array_len(value, &mut at).ok()? < 2 {
        return None;
    }
    let mode = u8::try_from(msgpack::read_uint(value, &mut at).ok()?).ok()?;
    let start = at;
    let data = match msgpack::read_bin(value, &mut at) {
        Ok(data) => data,
        Err(_) => {
            at = start;
            msgpack::read_str(value, &mut at).ok()?.as_bytes()
        }
    };
    Some((mode, data))
}

/// The voice message `fields` carry.
pub fn voice_of(fields: &[Field]) -> Option<VoiceRef> {
    let (mode, data) = voice_parts(fields)?;
    let (duration_ms, levels) = match mode {
        AM_OPUS_OGG => ogg_shape(data),
        _ => (codec2_ms(mode, data.len()), Vec::new()),
    };
    Some(VoiceRef {
        mode,
        bytes: data.len(),
        duration_ms,
        levels,
    })
}

/// The bytes of the voice message `fields` carry.
pub fn voice_bytes(fields: &[Field]) -> Option<&[u8]> {
    voice_parts(fields).map(|(_, data)| data)
}

/// The fields of a voice message: the audio field alone, `[mode, bytes]`,
/// as Columba writes it.
pub fn voice_fields(mode: u8, data: Vec<u8>) -> Vec<Field> {
    MessageAttachments {
        audio: Some(AudioAttachment { mode, data }),
        ..MessageAttachments::default()
    }
    .into_fields()
}

/// The bytes a Codec2 frame of `mode` takes and the milliseconds it
/// holds (`codec2_bits_per_frame`, `codec2_samples_per_frame` of the
/// codec; Sideband packs whole bytes per frame).
pub fn codec2_frame(mode: u8) -> Option<(usize, u32)> {
    match mode {
        AM_CODEC2_3200 => Some((8, 20)),
        AM_CODEC2_2400 => Some((6, 20)),
        AM_CODEC2_1600 => Some((8, 40)),
        AM_CODEC2_1400 => Some((7, 40)),
        AM_CODEC2_1300 => Some((7, 40)),
        AM_CODEC2_1200 => Some((6, 40)),
        AM_CODEC2_700C => Some((4, 40)),
        _ => None,
    }
}

fn codec2_ms(mode: u8, bytes: usize) -> u32 {
    codec2_frame(mode)
        .map(|(size, ms)| {
            u32::try_from(bytes / size)
                .unwrap_or(u32::MAX)
                .saturating_mul(ms)
        })
        .unwrap_or(0)
}

/// The length and level bar of an Ogg Opus stream, read off its pages:
/// the length from the last page's granule position less the header's
/// pre-skip (RFC 7845, 4.2 and 4.4), the bar from the packets' sizes,
/// which in a variable rate stream follow the speech (a pause codes in a
/// few bytes, a word in tens): their [`super::recording::LevelTrack`]
/// through the one resampler. A stream that does not parse says 0 and no
/// bar.
pub fn ogg_shape(data: &[u8]) -> (u32, Vec<u8>) {
    let mut at = 0;
    // Packets completed so far, the bytes of the one being read.
    let mut packet = 0usize;
    let mut current = 0usize;
    let mut sizes: Vec<usize> = Vec::new();
    let mut granule: i64 = -1;
    let mut head = Vec::new();
    while at + 27 <= data.len() && &data[at..at + 4] == b"OggS" {
        let mut raw = [0u8; 8];
        raw.copy_from_slice(&data[at + 6..at + 14]);
        let page_granule = i64::from_le_bytes(raw);
        let segments = usize::from(data[at + 26]);
        let table = at + 27;
        let Some(lacing) = data.get(table..table + segments) else {
            return (0, Vec::new());
        };
        let mut body = table + segments;
        for &lace in lacing {
            let Some(chunk) = data.get(body..body + usize::from(lace)) else {
                return (0, Vec::new());
            };
            if packet == 0 {
                head.extend_from_slice(
                    &chunk[..chunk.len().min(19usize.saturating_sub(head.len()))],
                );
            }
            current += chunk.len();
            body += usize::from(lace);
            if lace < 255 {
                // OpusHead and OpusTags are not audio.
                if packet >= 2 {
                    sizes.push(current);
                }
                packet += 1;
                current = 0;
            }
        }
        if page_granule >= 0 {
            granule = page_granule;
        }
        at = body;
    }
    let mut pre_skip: u64 = 0;
    if head.len() >= 12 && head.starts_with(b"OpusHead") {
        pre_skip = u64::from(u16::from_le_bytes([head[10], head[11]]));
    }
    let samples = u64::try_from(granule).unwrap_or(0).saturating_sub(pre_skip);
    let ms = u32::try_from(samples / 48).unwrap_or(u32::MAX);
    let track = super::recording::LevelTrack::from_packets(&sizes, ms);
    (ms, track.bars(BARS, super::recording::Fold::Peak))
}

/// `0:07`, `4:59`: a length as a player says it.
pub fn duration_words(ms: u32) -> String {
    let secs = ms.div_ceil(1000);
    alloc::format!("{}:{:02}", secs / 60, secs % 60)
}

/// What a screen reader says for a voice message: `Voice message, 0:07`.
pub fn voice_words(voice: &VoiceRef) -> Text {
    Text::Key(
        match voice.playable() {
            true => StrId::Voice,
            false => StrId::VoiceUnplayable,
        },
        alloc::vec![Arg::Str(duration_words(voice.duration_ms))],
    )
}

/// Why a recording does not go, in the reader's words: shorter than
/// [`SHORTEST_MS`] (the hint to hold the button), longer than
/// [`LONGEST_MS`]; none for one that goes.
pub fn refusal(ms: u32) -> Option<Text> {
    match ms {
        ..SHORTEST_MS => Some(Text::key(StrId::VoiceHold)),
        ms if ms > LONGEST_MS + 1000 => Some(Text::Key(
            StrId::VoiceTooLong,
            alloc::vec![Arg::Str(duration_words(LONGEST_MS))],
        )),
        _ => None,
    }
}

/// Which voice message plays, if one does: its peer and key.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Playing {
    pub peer: [u8; 16],
    pub key: u64,
}

impl<B> UiModel<B> {
    /// [`super::UiEvent::SendVoice`]: the recording goes as a message of
    /// its own in `peer`'s conversation, and the view follows it.
    pub(super) fn send_voice(&mut self, peer: [u8; 16], data: Vec<u8>) -> Vec<UiEffect> {
        if data.is_empty() {
            return Vec::new();
        }
        let index = self.conversation(peer);
        let effects = self.send_fields(index, String::new(), voice_fields(AM_OPUS_OGG, data));
        // The recording that was just handed over keeps its track as the
        // bubble's waveform (1276).
        let recorded = &mut self.recording;
        if recorded.peer == peer
            && recorded.phase == super::recording::Phase::Sent
            && !recorded.track.is_empty()
        {
            if let Some(key) = self.conversations[index]
                .messages
                .last()
                .map(|m| m.activity)
            {
                let track = core::mem::take(&mut recorded.track);
                self.voice_tracks.insert((peer, key), track);
            }
        }
        if index == self.selected {
            self.message_cursor = self.conversations[index].messages.len().saturating_sub(1);
            self.scroll_top = None;
        }
        effects
    }

    /// [`super::UiEvent::PlayVoice`]: a message of `peer`'s with a voice
    /// message this build plays starts; one that played before stops.
    pub(super) fn play_voice(&mut self, peer: [u8; 16], key: u64) -> Vec<UiEffect> {
        let carries = self
            .conversations
            .iter()
            .find(|c| c.peer == peer)
            .and_then(|c| c.messages.iter().find(|m| m.activity == key))
            .and_then(|m| voice_of(&m.fields))
            .is_some_and(|v| v.playable());
        if !carries {
            return Vec::new();
        }
        self.playing = Some(Playing { peer, key });
        alloc::vec![UiEffect::PlayVoice { peer, key }]
    }

    /// [`super::UiEvent::PauseVoice`]: the one playing stops; another
    /// does nothing.
    pub(super) fn pause_voice(&mut self, peer: [u8; 16], key: u64) -> Vec<UiEffect> {
        match self.playing == Some(Playing { peer, key }) {
            true => {
                self.playing = None;
                alloc::vec![UiEffect::PauseVoice { peer, key }]
            }
            false => Vec::new(),
        }
    }

    /// [`super::UiEvent::VoiceEnded`]: the player came to its end.
    pub(super) fn voice_ended(&mut self, peer: [u8; 16], key: u64) {
        if self.playing == Some(Playing { peer, key }) {
            self.playing = None;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;
    use leviculum_ui::strings::Lang;

    /// The field is what the reference packs for
    /// `fields[FIELD_AUDIO] = [AM_OPUS_OGG, data]`
    /// (`umsgpack.packb([0x10, b"OggS"])`).
    #[test]
    fn the_field_is_the_references_mode_and_bytes() {
        let fields = voice_fields(AM_OPUS_OGG, b"OggS".to_vec());
        assert_eq!(
            fields,
            vec![(
                FIELD_AUDIO,
                vec![0x92, 0x10, 0xc4, 0x04, b'O', b'g', b'g', b'S']
            )]
        );
        assert_eq!(voice_parts(&fields), Some((AM_OPUS_OGG, &b"OggS"[..])));
    }

    /// Sideband's low bandwidth message: Codec2 2400, six bytes a 20 ms
    /// frame, its length from the frame count; a third element skipped.
    #[test]
    fn codec2_says_its_length_from_its_frames() {
        let mut value = vec![0x93, AM_CODEC2_2400, 0xc5, 0x01, 0x2c];
        value.extend(core::iter::repeat_n(0u8, 300));
        value.push(0xc0);
        let voice = voice_of(&[(FIELD_AUDIO, value)]).expect("a voice message");
        assert_eq!(
            (voice.mode, voice.bytes, voice.duration_ms),
            (AM_CODEC2_2400, 300, 1000)
        );
        assert!(voice.playable() && voice.levels.is_empty());
        assert_eq!(codec2_ms(AM_CODEC2_1200, 60), 400);
        assert!(!playable(AM_CODEC2_700C) && !playable(AM_CODEC2_450) && !playable(0xff));
    }

    #[test]
    fn a_malformed_field_is_no_voice_message() {
        assert_eq!(voice_of(&[(FIELD_AUDIO, vec![0x01])]), None);
        assert_eq!(voice_of(&[(FIELD_AUDIO, vec![0x91, 0x10])]), None);
        assert_eq!(
            voice_of(&[(FIELD_AUDIO, vec![0x92, 0xd0, 0xff, 0xc4, 0x00])]),
            None
        );
        assert_eq!(voice_of(&[]), None);
    }

    /// A hand-made stream: OpusHead (pre-skip 312), OpusTags, then two
    /// pages of packets, quiet then loud; the last granule says 1.5 s.
    #[test]
    fn an_ogg_stream_says_its_length_and_its_loud_part() {
        fn page(out: &mut Vec<u8>, granule: i64, packets: &[usize]) {
            out.extend_from_slice(b"OggS");
            out.extend_from_slice(&[0, 0]);
            out.extend_from_slice(&granule.to_le_bytes());
            out.extend_from_slice(&[0; 12]);
            let mut lacing = Vec::new();
            for &p in packets {
                lacing.extend(core::iter::repeat_n(255u8, p / 255));
                lacing.push((p % 255) as u8);
            }
            out.push(lacing.len() as u8);
            out.extend_from_slice(&lacing);
            for &p in packets {
                out.extend(core::iter::repeat_n(0xaa, p));
            }
        }
        let mut data = Vec::new();
        // OpusHead with its pre-skip at bytes 10 and 11.
        let mut head = b"OpusHead".to_vec();
        head.extend_from_slice(&[1, 1, 0x38, 0x01, 0x80, 0x3e, 0, 0, 0, 0, 0]);
        data.extend_from_slice(b"OggS\0\x02");
        data.extend_from_slice(&0i64.to_le_bytes());
        data.extend_from_slice(&[0; 12]);
        data.push(1);
        data.push(head.len() as u8);
        data.extend_from_slice(&head);
        page(&mut data, 0, &[20]);
        page(&mut data, 30_000, &[4; 40]);
        page(&mut data, 72_312, &[40; 40]);
        let (ms, bars) = ogg_shape(&data);
        assert_eq!(ms, 1500);
        assert_eq!(bars.len(), BARS);
        assert!(bars[..BARS / 2].iter().all(|&b| b == 8), "{bars:?}");
        assert!(bars[BARS / 2..].iter().all(|&b| b == 100), "{bars:?}");
        // A packet longer than one segment counts whole.
        let mut long = data.clone();
        page(&mut long, 80_000, &[600]);
        assert_eq!(ogg_shape(&long).0, 1660);
        assert_eq!(ogg_shape(b"not ogg"), (0, Vec::new()));
        assert_eq!(ogg_shape(&data[..data.len() - 5]), (0, Vec::new()));
    }

    #[test]
    fn a_voice_message_says_its_length() {
        assert_eq!(duration_words(0), "0:00");
        assert_eq!(duration_words(6_200), "0:07");
        assert_eq!(duration_words(LONGEST_MS), "10:00");
        let voice = VoiceRef {
            mode: AM_OPUS_OGG,
            bytes: 9_000,
            duration_ms: 7_000,
            levels: Vec::new(),
        };
        assert_eq!(voice_words(&voice).resolve(Lang::En), "Voice message, 0:07");
        assert_eq!(
            voice_words(&voice).resolve(Lang::De),
            "Sprachnachricht, 0:07"
        );
        let old = VoiceRef {
            mode: AM_CODEC2_700C,
            ..voice
        };
        assert_eq!(
            voice_words(&old).resolve(Lang::En),
            "Voice message, 0:07, which cannot be played here"
        );
    }

    #[test]
    fn a_tap_is_no_message_and_ten_minutes_is_the_most() {
        assert_eq!(
            refusal(300).map(|t| t.resolve(Lang::En)),
            Some("Hold the microphone to record, release to send.".into())
        );
        assert_eq!(refusal(SHORTEST_MS), None);
        assert_eq!(refusal(LONGEST_MS + 1000), None);
        assert_eq!(
            refusal(LONGEST_MS + 1001).map(|t| t.resolve(Lang::De)),
            Some("Eine Sprachnachricht dauert höchstens 10:00.".into())
        );
    }
}
