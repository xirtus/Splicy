//! Royalty-free exports from Splicy's own encoders: VP9 (WebM) and AV1 (MP4) pictures, Opus
//! sound, and the WebM writer. All three codecs are pure Rust (craft-libs), so they run wherever
//! Splicy runs, SexOS included, with no hardware encoder.
use super::convert::rgba_to_yuv420_8;
use super::format::{ExportSettings, Format};
use super::registry::{AudioEncoder, EncodedPacket, EncoderFrame, VideoEncoder};
use super::stats::{Stage, timed};
use super::{ExportError, FrameRate, Out, Result, SampleEntry};
use craft_audio_opus::opus::encoder::Encoder as OpusEncoder;
use craft_media::matroska::{MkvWriter, MuxOptions, TrackKind, TrackSpec};
use filmcraft_isobmff::{CodecConfig, FourCc, OpusConfig};
use std::collections::VecDeque;

/// Opus codes 48 kHz only.
pub const OPUS_RATE: u32 = 48_000;
/// Samples per channel in one Opus packet (20 ms).
const OPUS_FRAME: usize = OpusEncoder::FRAME_SIZE;
/// What WebM players decode before a seek point so Opus has settled (RFC 7845 recommends 80 ms).
const OPUS_SEEK_PRE_ROLL_NS: u64 = 80_000_000;

/// The Opus bit rate (kbit/s) for the Audio Bitrate setting: at most 96 kbit/s a channel (Opus is
/// transparent well below that) and at least 16.
pub fn opus_kbps(setting: u32, channels: u32) -> u32 {
    let ch = channels.clamp(1, 2);
    setting.clamp(16 * ch, 96 * ch)
}

fn encode_error(e: impl std::fmt::Display) -> ExportError {
    ExportError::Encode(e.to_string())
}

/// 8-bit limited-range BT.709 4:2:0 planes of an SDR picture (both encoders take SDR only).
fn yuv420(f: &EncoderFrame, y: &mut Vec<u8>, u: &mut Vec<u8>, v: &mut Vec<u8>) {
    timed(Stage::Convert, || rgba_to_yuv420_8(f.rgba, f.width as usize, f.height as usize, y, u, v));
}

fn keyint(s: &ExportSettings, rate: FrameRate) -> u32 {
    s.keyframe_distance.filter(|k| *k > 0).unwrap_or_else(|| (rate.num as f64 / rate.den as f64 * 2.0).round().max(1.0) as u32)
}

/// VP9 profile 0 for WebM. One packet per picture, no reordering.
struct Vp9 {
    enc: craft_video::vp9::Vp9Encoder,
    w: u32,
    h: u32,
    rate: FrameRate,
    planes: (Vec<u8>, Vec<u8>, Vec<u8>),
}

impl VideoEncoder for Vp9 {
    fn sample_entry(&self) -> SampleEntry {
        // WebM needs no codec private data; this entry only describes the stream
        let fourcc = FourCc(*b"vp09");
        SampleEntry::video(fourcc, CodecConfig::Unknown { fourcc, raw: self.enc.vpcc() }, self.w as u16, self.h as u16)
    }
    fn timescale(&self) -> u32 {
        self.rate.num as u32
    }
    fn encode(&mut self, f: &EncoderFrame) -> Result<Vec<EncodedPacket>> {
        let (y, u, v) = &mut self.planes;
        yuv420(f, y, u, v);
        let coded = self.enc.encode_frame(y, u, v, false);
        if coded.is_empty() {
            return Err(ExportError::Encode(format!("VP9: picture {} is not {}x{}", f.index, self.w, self.h)));
        }
        Ok(vec![EncodedPacket { data: coded.data, key: coded.key, duration: self.rate.den as u32, composition_offset: 0 }])
    }
    fn flush(&mut self) -> Result<Vec<EncodedPacket>> {
        Ok(Vec::new())
    }
}

pub(crate) fn vp9_factory(format: Format, w: u32, h: u32, rate: FrameRate, s: &ExportSettings) -> Option<Result<Box<dyn VideoEncoder>>> {
    if format != Format::Webm {
        return None;
    }
    let cfg = craft_video::vp9::EncoderConfig {
        width: w,
        height: h,
        keyframe_interval: keyint(s, rate),
        target_kbps: Some(s.bitrate_kbps.max(100)),
        frame_rate: rate.num as f64 / rate.den.max(1) as f64,
        ..Default::default()
    };
    let enc = craft_video::vp9::Vp9Encoder::new(cfg);
    Some(Ok(Box::new(Vp9 { enc, w, h, rate, planes: Default::default() })))
}

/// AV1 Main (8-bit 4:2:0) for MP4: one temporal unit per picture, the sequence header in `av1C`
/// and in front of every key frame.
struct Av1 {
    enc: craft_video::av1::Encoder,
    w: u32,
    h: u32,
    rate: FrameRate,
    planes: (Vec<u8>, Vec<u8>, Vec<u8>),
    wide: [Vec<u16>; 3],
}

impl VideoEncoder for Av1 {
    fn sample_entry(&self) -> SampleEntry {
        let cfg = filmcraft_isobmff::Av1Config::parse(&self.enc.av1c()).unwrap_or_default();
        SampleEntry::av1(cfg, self.w as u16, self.h as u16)
    }
    fn timescale(&self) -> u32 {
        self.rate.num as u32
    }
    fn encode(&mut self, f: &EncoderFrame) -> Result<Vec<EncodedPacket>> {
        let (y, u, v) = &mut self.planes;
        yuv420(f, y, u, v);
        for (wide, narrow) in self.wide.iter_mut().zip([&*y, &*u, &*v]) {
            wide.clear();
            wide.extend(narrow.iter().map(|&s| u16::from(s)));
        }
        let cw = (self.w as usize).div_ceil(2);
        let frame = craft_video::av1::Frame { y: &self.wide[0], u: &self.wide[1], v: &self.wide[2], y_stride: self.w as usize, uv_stride: cw };
        let p = self.enc.encode(&frame).map_err(encode_error)?;
        Ok(vec![EncodedPacket { data: p.data, key: p.keyframe, duration: self.rate.den as u32, composition_offset: 0 }])
    }
    fn flush(&mut self) -> Result<Vec<EncodedPacket>> {
        Ok(Vec::new())
    }
}

pub(crate) fn av1_factory(format: Format, w: u32, h: u32, rate: FrameRate, s: &ExportSettings) -> Option<Result<Box<dyn VideoEncoder>>> {
    if format != Format::Av1 {
        return None;
    }
    let mut cfg = craft_video::av1::EncoderConfig::new(w, h, rate.num as u32, rate.den as u32);
    cfg.rate = craft_video::av1::RateControl::Bitrate { kbps: s.bitrate_kbps.max(100) };
    cfg.keyint = keyint(s, rate);
    Some(
        craft_video::av1::Encoder::new(cfg)
            .map(|enc| Box::new(Av1 { enc, w, h, rate, planes: Default::default(), wide: Default::default() }) as Box<dyn VideoEncoder>)
            .map_err(encode_error),
    )
}

/// Opus, 20 ms packets, for WebM and for AV1 MP4 (`Opus` + `dOps`).
pub(crate) struct OpusAudio {
    enc: OpusEncoder,
    channels: usize,
    /// Interleaved samples not yet coded (less than one packet after each call).
    held: Vec<f32>,
    /// Samples per channel given, and coded, so far.
    given: u64,
    coded: u64,
}

impl OpusAudio {
    pub(crate) fn new(channels: u32, s: &ExportSettings) -> Result<Self> {
        let ch = channels.clamp(1, 2);
        let enc = OpusEncoder::new(ch as u8, opus_kbps(s.audio.bitrate_kbps, ch) * 1000).map_err(encode_error)?;
        Ok(Self { enc, channels: ch as usize, held: Vec::new(), given: 0, coded: 0 })
    }

    fn code_held(&mut self, out: &mut Vec<Vec<u8>>) -> Result<()> {
        let n = OPUS_FRAME * self.channels;
        let mut at = 0;
        while self.held.len() - at >= n {
            out.push(self.enc.encode_float(&self.held[at..at + n]).map_err(encode_error)?);
            at += n;
            self.coded += OPUS_FRAME as u64;
        }
        self.held.drain(..at);
        Ok(())
    }
}

impl AudioEncoder for OpusAudio {
    fn sample_entry(&self) -> SampleEntry {
        let cfg = OpusConfig { output_channels: self.channels as u8, pre_skip: self.enc.pre_skip(), input_sample_rate: OPUS_RATE, ..Default::default() };
        SampleEntry::audio(FourCc(*b"Opus"), CodecConfig::Opus(cfg), self.channels as u32, f64::from(OPUS_RATE), 16)
    }
    fn encode(&mut self, planar: &[Vec<f32>]) -> Result<Vec<Vec<u8>>> {
        let n = planar.first().map_or(0, Vec::len);
        self.held.reserve(n * self.channels);
        for i in 0..n {
            for c in 0..self.channels {
                // mono export of a stereo mix, or the reverse: take what there is
                let s = planar.get(c).or(planar.first()).and_then(|p| p.get(i)).copied().unwrap_or(0.0);
                self.held.push(s.clamp(-1.0, 1.0));
            }
        }
        self.given += n as u64;
        let mut out = Vec::new();
        self.code_held(&mut out)?;
        Ok(out)
    }
    fn flush(&mut self) -> Result<Vec<Vec<u8>>> {
        // silence until the encoder's delay is coded too, so the last real sample comes out
        let wanted = (self.given + u64::from(self.enc.pre_skip())).div_ceil(OPUS_FRAME as u64) * OPUS_FRAME as u64;
        let missing = wanted.saturating_sub(self.coded) as usize * self.channels;
        self.held.resize(missing, 0.0);
        let mut out = Vec::new();
        self.code_held(&mut out)?;
        Ok(out)
    }
    fn priming(&self) -> u32 {
        u32::from(self.enc.pre_skip())
    }
    fn frame_size(&self) -> u32 {
        OPUS_FRAME as u32
    }
}

/// One packet waiting for the WebM writer, by presentation time.
struct Queued {
    track: usize,
    pts_ns: i64,
    key: bool,
    data: Vec<u8>,
}

/// The WebM writer: VP9 picture and Opus sound, written in time order (the export hands over a
/// group of pictures, then the sound of that group).
pub(crate) struct WebmMux {
    mux: MkvWriter<Out>,
    rate: FrameRate,
    audio: bool,
    pictures: u64,
    packets: u64,
    pre_skip_ns: i64,
    video: VecDeque<Queued>,
    sound: VecDeque<Queued>,
}

fn ns(units: u64, num: i64, den: i64) -> i64 {
    (i128::from(units) * i128::from(den) * 1_000_000_000 / i128::from(num.max(1))) as i64
}

impl WebmMux {
    /// `audio`: channels and the Opus stream's description (its `OpusHead` is the track's codec
    /// private data).
    pub(crate) fn new(out: Out, w: u32, h: u32, rate: FrameRate, audio: Option<(u32, OpusConfig)>, title: Option<String>) -> Result<Self> {
        let frame_ns = ns(1, rate.num, rate.den) as u64;
        let mut video = TrackSpec::new(TrackKind::Video, "V_VP9");
        video.video_size = Some((w, h));
        video.default_duration_ns = Some(frame_ns);
        let mut tracks = vec![video];
        let mut pre_skip_ns = 0;
        let has_audio = audio.is_some();
        if let Some((channels, cfg)) = audio {
            pre_skip_ns = ns(u64::from(cfg.pre_skip), i64::from(OPUS_RATE), 1);
            let mut a = TrackSpec::new(TrackKind::Audio, "A_OPUS");
            a.codec_private = cfg.opus_head();
            a.audio = Some((f64::from(OPUS_RATE), u64::from(channels.clamp(1, 2)), None));
            a.default_duration_ns = Some(ns(OPUS_FRAME as u64, i64::from(OPUS_RATE), 1) as u64);
            a.codec_delay_ns = pre_skip_ns as u64;
            a.seek_pre_roll_ns = OPUS_SEEK_PRE_ROLL_NS;
            tracks.push(a);
        }
        let opts = MuxOptions { doc_type: "webm".into(), title, writing_app: "Splicy".into(), ..Default::default() };
        let mux = MkvWriter::new(out, tracks, opts).map_err(|e| ExportError::Io(e.to_string()))?;
        Ok(Self { mux, rate, audio: has_audio, pictures: 0, packets: 0, pre_skip_ns, video: VecDeque::new(), sound: VecDeque::new() })
    }

    pub(crate) fn write_video(&mut self, packets: Vec<EncodedPacket>) -> Result<()> {
        for p in packets {
            let pts_ns = ns(self.pictures, self.rate.num, self.rate.den);
            self.pictures += 1;
            self.video.push_back(Queued { track: 0, pts_ns, key: p.key, data: p.data });
        }
        self.drain(false)
    }

    pub(crate) fn write_audio(&mut self, packets: Vec<Vec<u8>>) -> Result<()> {
        for data in packets {
            // the stored time adds the codec delay back: the first packet is stored at 0
            let pts_ns = ns(self.packets * OPUS_FRAME as u64, i64::from(OPUS_RATE), 1) - self.pre_skip_ns;
            self.packets += 1;
            self.sound.push_back(Queued { track: 1, pts_ns, key: true, data });
        }
        self.drain(false)
    }

    /// Write every queued packet that nothing still to come can precede (everything at the end).
    fn drain(&mut self, all: bool) -> Result<()> {
        loop {
            let next = match (self.video.front(), self.sound.front()) {
                (Some(v), Some(a)) => {
                    if v.pts_ns <= a.pts_ns + self.pre_skip_ns {
                        self.video.pop_front()
                    } else {
                        self.sound.pop_front()
                    }
                }
                (Some(_), None) if all || !self.audio => self.video.pop_front(),
                (None, Some(_)) if all => self.sound.pop_front(),
                _ => None,
            };
            let Some(q) = next else { return Ok(()) };
            self.mux.write_frame(q.track, q.pts_ns, q.key, &q.data, None).map_err(|e| ExportError::Io(e.to_string()))?;
        }
    }

    pub(crate) fn finish(mut self, settings: &ExportSettings) -> Result<u64> {
        self.drain(true)?;
        let out = self.mux.finish().map_err(|e| ExportError::Io(e.to_string()))?;
        out.finish(settings)
    }
}
