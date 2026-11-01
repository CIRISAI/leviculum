//! The sound of a voice message (1222): what the shell does with a
//! recording before it goes and after it came, in Rust on every target.
//! The phone records and plays PCM and hands it over; this encodes and
//! decodes it.
//!
//! Before it goes ([`encode`]): mono 16-bit PCM at the rate the
//! microphone gave, coded as Opus in Ogg (RFC 7845) at [`BITRATE`],
//! speech mode, 20 ms frames, ended exactly so the far side plays the
//! recording and no padding. That is `AM_OPUS_OGG`, the mode Columba and
//! Sideband both play ([`crate::ui::voice`]).
//!
//! After it came ([`decode`]): Opus in Ogg, from Columba, Sideband or
//! ourselves, and the Codec2 modes Sideband sends on slow links, as mono
//! PCM for the phone's player, or as a WAV file ([`wav`]) for the
//! desktop's. A received recording is the sender's: [`LONGEST_DECODE_MS`]
//! bounds what decoding it may allocate.

use std::io::Cursor;

use opus_pure::{Application, OggOpusReader, OggOpusWriter, OpusEncoder, OpusHead, Trim};

use crate::ui::voice::{codec2_frame, AM_OPUS_OGG};

pub use crate::ui::voice::{LONGEST_MS, SHORTEST_MS};

/// The rate a recording is coded at when the microphone's is not one
/// Opus takes: wideband, what speech needs.
pub const RATE: u32 = 16_000;

/// The bits a second a recording is coded at, on average: wideband speech
/// in SILK (opus-pure chooses SILK below about 20 kb/s). A minute is about
/// 90 KB, and the longest message, ten minutes, stays under what a
/// receiver takes ([`LONGEST_MS`]).
pub const BITRATE: i32 = crate::ui::voice::OPUS_BITRATE as i32;

/// The rate an Opus message is decoded at for the player: every Opus
/// rate divides it, and it keeps all of a wideband or super-wideband
/// recording.
pub const PLAY_RATE: u32 = 24_000;

/// The most a received recording is decoded to: twice the longest that
/// goes. A hostile stream of one byte packets would otherwise ask for
/// hours.
pub const LONGEST_DECODE_MS: u32 = 2 * LONGEST_MS;

/// The rates the Opus encoder takes as they are.
const OPUS_RATES: [u32; 5] = [8_000, 12_000, 16_000, 24_000, 48_000];

/// Mono 16-bit PCM and its rate.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Pcm {
    pub rate: u32,
    pub samples: Vec<i16>,
}

impl Pcm {
    /// How long it plays, in milliseconds.
    pub fn ms(&self) -> u32 {
        match self.rate {
            0 => 0,
            rate => u32::try_from(self.samples.len() as u64 * 1000 / u64::from(rate))
                .unwrap_or(u32::MAX),
        }
    }
}

/// `samples` at `rate`, coded as Opus in Ogg: the bytes of an
/// `AM_OPUS_OGG` message.
pub fn encode(samples: &[i16], rate: u32) -> Result<Vec<u8>, String> {
    if rate == 0 {
        return Err("a recording needs a sample rate".to_string());
    }
    let (rate, resampled) = match OPUS_RATES.contains(&rate) {
        true => (rate, None),
        false => (RATE, Some(resample(samples, rate, RATE))),
    };
    let samples = resampled.as_deref().unwrap_or(samples);
    let opus_rate = i32::try_from(rate).map_err(|e| e.to_string())?;
    let mut encoder =
        OpusEncoder::new(opus_rate, 1, Application::Voip).map_err(|e| e.to_string())?;
    encoder.bitrate_bps = BITRATE;
    let head = OpusHead::for_encoder(&encoder, rate);
    let frame = (rate / 50) as usize;
    let ticks = (48_000 / rate) as usize;
    // The encoder runs `pre_skip` behind its input: that much silence more
    // flushes the recording's end out of it, and the last page's granule
    // says where the audio stopped (RFC 7845, 4.4), as opus-pure's
    // `encode_gapless` does.
    let frames = (samples.len() + usize::from(head.pre_skip).div_ceil(ticks)).div_ceil(frame);
    let final_granule = u64::from(head.pre_skip) + (samples.len() * ticks) as u64;
    let mut writer = OggOpusWriter::new(Vec::new(), head).map_err(|e| e.to_string())?;
    let mut packet = vec![0u8; opus_pure::MAX_PACKET_BYTES];
    let mut block = vec![0i16; frame];
    for i in 0..frames {
        let start = (i * frame).min(samples.len());
        let end = (start + frame).min(samples.len());
        block[..end - start].copy_from_slice(&samples[start..end]);
        block[end - start..].fill(0);
        let n = encoder
            .encode_s16(&block, frame, &mut packet)
            .map_err(|e| e.to_string())?;
        if i + 1 == frames {
            let duration = final_granule.saturating_sub(writer.granule().max(0) as u64);
            writer
                .write_packet_with_duration(&packet[..n], u32::try_from(duration).unwrap_or(0))
                .map_err(|e| e.to_string())?;
        } else {
            writer
                .write_packet(&packet[..n])
                .map_err(|e| e.to_string())?;
        }
    }
    writer.finish().map_err(|e| e.to_string())
}

/// A voice message of `mode` decoded to mono PCM: Opus at [`PLAY_RATE`],
/// Codec2 at its 8 kHz. An error for a mode this build does not play or
/// a stream that does not decode.
pub fn decode(mode: u8, data: &[u8]) -> Result<Pcm, String> {
    match mode {
        AM_OPUS_OGG => decode_opus(data),
        _ => decode_codec2(mode, data),
    }
}

fn decode_opus(data: &[u8]) -> Result<Pcm, String> {
    let mut reader = OggOpusReader::new(Cursor::new(data)).map_err(|e| e.to_string())?;
    let channels = usize::from(reader.head().channel_count).max(1);
    let rate = PLAY_RATE as i32;
    let mut decoder = reader.head().decoder(rate).map_err(|e| e.to_string())?;
    let mut trim = Trim::new(reader.head(), rate, channels).map_err(|e| e.to_string())?;
    // In u64: on a 32-bit phone the product overflows a usize.
    let most = (u64::from(PLAY_RATE) * u64::from(LONGEST_DECODE_MS) / 1000) as usize;
    let mut block = vec![0i16; opus_pure::MAX_PACKET_SAMPLES * channels];
    let mut samples = Vec::new();
    for packet in reader.packets() {
        let packet = packet.map_err(|e| e.to_string())?;
        let n = decoder
            .decode_s16(&packet.data, opus_pure::MAX_PACKET_SAMPLES, &mut block)
            .map_err(|e| e.to_string())?;
        let kept = trim.keep(&packet, &block[..n * channels]);
        // Stereo (Sideband's music profiles) is played as its mean.
        samples.extend(kept.chunks(channels).map(|frame| {
            (frame.iter().map(|&s| i32::from(s)).sum::<i32>() / channels as i32) as i16
        }));
        if samples.len() >= most {
            samples.truncate(most);
            break;
        }
    }
    Ok(Pcm {
        rate: PLAY_RATE,
        samples,
    })
}

fn decode_codec2(mode: u8, data: &[u8]) -> Result<Pcm, String> {
    use codec2::{Codec2, Codec2Mode};
    let codec_mode = match mode {
        crate::ui::voice::AM_CODEC2_3200 => Codec2Mode::MODE_3200,
        crate::ui::voice::AM_CODEC2_2400 => Codec2Mode::MODE_2400,
        crate::ui::voice::AM_CODEC2_1600 => Codec2Mode::MODE_1600,
        crate::ui::voice::AM_CODEC2_1400 => Codec2Mode::MODE_1400,
        crate::ui::voice::AM_CODEC2_1300 => Codec2Mode::MODE_1300,
        crate::ui::voice::AM_CODEC2_1200 => Codec2Mode::MODE_1200,
        other => return Err(format!("audio mode {other:#04x} does not play here")),
    };
    let (bytes, _) = codec2_frame(mode).ok_or("no frame size")?;
    let mut codec = Codec2::new(codec_mode);
    let per_frame = codec.samples_per_frame();
    let most = 8 * LONGEST_DECODE_MS as usize;
    let mut samples = Vec::with_capacity((data.len() / bytes * per_frame).min(most));
    let mut frame = vec![0i16; per_frame];
    for bits in data.chunks_exact(bytes) {
        codec.decode(&mut frame, bits);
        samples.extend_from_slice(&frame);
        if samples.len() >= most {
            break;
        }
    }
    Ok(Pcm {
        rate: 8_000,
        samples,
    })
}

/// `samples` at `from` taken to `to` by linear interpolation, each output
/// sample the mean of the input it covers when going down so the speech
/// band does not fold.
pub fn resample(samples: &[i16], from: u32, to: u32) -> Vec<i16> {
    if from == to || from == 0 || to == 0 || samples.is_empty() {
        return samples.to_vec();
    }
    let out_len = (samples.len() as u64 * u64::from(to) / u64::from(from)) as usize;
    let step = f64::from(from) / f64::from(to);
    (0..out_len)
        .map(|i| {
            let at = i as f64 * step;
            match step > 1.0 {
                true => {
                    let lo = at as usize;
                    let hi = ((at + step) as usize).clamp(lo + 1, samples.len());
                    let part = &samples[lo.min(samples.len() - 1)..hi];
                    (part.iter().map(|&s| i64::from(s)).sum::<i64>() / part.len() as i64) as i16
                }
                false => {
                    let lo = (at as usize).min(samples.len() - 1);
                    let hi = (lo + 1).min(samples.len() - 1);
                    let frac = at - lo as f64;
                    (f64::from(samples[lo]) * (1.0 - frac) + f64::from(samples[hi]) * frac) as i16
                }
            }
        })
        .collect()
}

/// A mono 16-bit WAV file of `pcm`, for a desktop's player.
pub fn wav(pcm: &Pcm) -> Vec<u8> {
    let data_len = u32::try_from(pcm.samples.len() * 2).unwrap_or(u32::MAX);
    let mut out = Vec::with_capacity(44 + data_len as usize);
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&(36u32.saturating_add(data_len)).to_le_bytes());
    out.extend_from_slice(b"WAVEfmt ");
    out.extend_from_slice(&16u32.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&pcm.rate.to_le_bytes());
    out.extend_from_slice(&(pcm.rate * 2).to_le_bytes());
    out.extend_from_slice(&2u16.to_le_bytes());
    out.extend_from_slice(&16u16.to_le_bytes());
    out.extend_from_slice(b"data");
    out.extend_from_slice(&data_len.to_le_bytes());
    for s in &pcm.samples {
        out.extend_from_slice(&s.to_le_bytes());
    }
    out
}

/// The folder under the home a desktop writes a voice message's WAV file
/// in, for the system's player.
pub const DIR: &str = "voice";

/// The voice message `fields` carry, decoded and written under `home`'s
/// [`DIR`] as `<peer>-<key>.wav`, once; its path.
pub fn save_wav(
    home: &std::path::Path,
    peer: [u8; 16],
    key: u64,
    fields: &[leviculum_lxmf::Field],
) -> Result<std::path::PathBuf, String> {
    let (mode, data) = crate::ui::voice::voice_parts(fields)
        .ok_or_else(|| "that message has no voice message".to_string())?;
    let dir = home.join(DIR);
    let path = dir.join(format!("{}-{key}.wav", crate::address::to_hex(&peer)));
    if std::fs::metadata(&path).is_err() {
        let pcm = decode(mode, data)?;
        std::fs::create_dir_all(&dir)
            .and_then(|()| std::fs::write(&path, wav(&pcm)))
            .map_err(|e| format!("{}: {e}", path.display()))?;
    }
    Ok(path)
}

/// A spoken-sounding test signal: a voice of rising and falling pitch
/// through two formants, in syllables with pauses between. For the tests
/// and the device walk's fixture; deterministic.
pub fn test_speech(rate: u32, ms: u32) -> Vec<i16> {
    let n = (u64::from(rate) * u64::from(ms) / 1000) as usize;
    let mut phase = 0.0f64;
    (0..n)
        .map(|i| {
            let t = i as f64 / f64::from(rate);
            // Syllables of 250 ms, one in four silent.
            let syllable = (t / 0.25) as u64;
            let within = t / 0.25 - syllable as f64;
            let envelope = match syllable % 4 {
                3 => 0.0,
                _ => (std::f64::consts::PI * within).sin().powi(2),
            };
            let pitch = 140.0 + 40.0 * (t * 2.1).sin();
            phase += 2.0 * std::f64::consts::PI * pitch / f64::from(rate);
            let formant = |f: f64, width: f64| {
                (1..=20)
                    .map(|h| {
                        let freq = pitch * h as f64;
                        let gain = (-((freq - f) / width).powi(2)).exp();
                        gain * (phase * h as f64).sin()
                    })
                    .sum::<f64>()
            };
            let f1 = 500.0 + 200.0 * (t * 3.0).sin();
            let f2 = 1500.0 + 400.0 * (t * 1.7).cos();
            let s = envelope * (formant(f1, 150.0) + 0.6 * formant(f2, 250.0));
            (s * 6_000.0).clamp(-32_000.0, 32_000.0) as i16
        })
        .collect()
}

/// How alike two recordings sound, -1 to 1: the best normalised
/// correlation of `b` against `a` within `shift` samples either way. A
/// lossy codec's output against its input reads above 0.8 for speech.
pub fn likeness(a: &[i16], b: &[i16], shift: usize) -> f64 {
    let mut best = -1.0f64;
    for lag in 0..=2 * shift {
        let (x, y): (&[i16], &[i16]) = match lag < shift {
            true => (&a[(shift - lag).min(a.len())..], b),
            false => (a, &b[(lag - shift).min(b.len())..]),
        };
        let n = x.len().min(y.len());
        if n == 0 {
            continue;
        }
        let (mut xy, mut xx, mut yy) = (0.0f64, 0.0f64, 0.0f64);
        for i in 0..n {
            let (p, q) = (f64::from(x[i]), f64::from(y[i]));
            xy += p * q;
            xx += p * p;
            yy += q * q;
        }
        if xx > 0.0 && yy > 0.0 {
            best = best.max(xy / (xx.sqrt() * yy.sqrt()));
        }
    }
    best
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::voice::{ogg_shape, AM_CODEC2_2400, AM_CODEC2_700C};

    /// A recording coded and decoded again sounds like itself, plays as
    /// long as it was, and its length and bar read off the stream as the
    /// model reads them.
    #[test]
    fn a_recording_comes_back_as_itself() {
        let speech = test_speech(16_000, 3_000);
        let ogg = encode(&speech, 16_000).expect("encoded");
        assert!(ogg.starts_with(b"OggS"));
        // 12 kb/s for 3 s is 4.5 KB; the headers and pages add a little.
        assert!((3_000..7_000).contains(&ogg.len()), "{} bytes", ogg.len());
        let (ms, bars) = ogg_shape(&ogg);
        assert_eq!(ms, 3_000);
        assert_eq!(bars.len(), crate::ui::voice::BARS);
        let back = decode(AM_OPUS_OGG, &ogg).expect("decoded");
        assert_eq!(back.rate, PLAY_RATE);
        assert_eq!(back.ms(), 3_000);
        let back16 = resample(&back.samples, PLAY_RATE, 16_000);
        let like = likeness(&speech, &back16, 80);
        assert!(like > 0.8, "likeness {like}");
    }

    /// The microphone's rate is taken as it comes: 48 kHz as it is, 44.1
    /// kHz (the one Android guarantees) through the resampler to 16 kHz.
    #[test]
    fn any_microphone_rate_goes() {
        for rate in [48_000, 44_100, 8_000] {
            let speech = test_speech(rate, 1_000);
            let ogg = encode(&speech, rate).expect("encoded");
            assert_eq!(ogg_shape(&ogg).0, 1_000, "at {rate}");
        }
        assert!(encode(&[], 0).is_err());
    }

    #[test]
    fn codec2_modes_decode_and_700c_does_not() {
        let pcm = decode(AM_CODEC2_2400, &[0u8; 60]).expect("decoded");
        assert_eq!((pcm.rate, pcm.samples.len(), pcm.ms()), (8_000, 1_600, 200));
        assert!(decode(AM_CODEC2_700C, &[0u8; 8]).is_err());
        assert!(decode(AM_OPUS_OGG, b"OggS but not").is_err());
    }

    /// One byte packets claiming the longest frames do not decode to
    /// hours: the decode stops at twice the longest message.
    #[test]
    fn a_hostile_stream_decodes_to_a_bounded_length() {
        let codec2 = decode(AM_CODEC2_2400, &vec![0u8; 990_000]).expect("decoded");
        assert!(codec2.ms() <= LONGEST_DECODE_MS + 20, "{}", codec2.ms());
    }

    #[test]
    fn the_wav_file_is_mono_sixteen_bit() {
        let file = wav(&Pcm {
            rate: 8_000,
            samples: vec![1, -1],
        });
        assert_eq!(&file[..4], b"RIFF");
        assert_eq!(file.len(), 48);
        assert_eq!(&file[24..28], &8_000u32.to_le_bytes());
        assert_eq!(&file[44..], &[1, 0, 0xff, 0xff]);
    }

    #[test]
    fn resampling_keeps_the_length_in_time() {
        let x = test_speech(44_100, 500);
        assert_eq!(resample(&x, 44_100, 16_000).len(), 8_000);
        assert_eq!(resample(&x[..10], 8_000, 16_000).len(), 20);
    }
}
