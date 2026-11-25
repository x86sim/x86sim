//! Share: frames of a phone's screen for the browser. A `Streamer` per (phone, codec, width, quality, fps) reads the
//! phone's frame ring (sleeping on its futex while the screen is still), turns the frame upright as the launcher's
//! viewer does (the device orientation, a two-screen phone's inner panel), scales it with an area filter and encodes
//! it once for every viewer of that stream: H.264 (OpenH264, built from source into the binary: the AppImage carries
//! no ffmpeg) or JPEG. A viewer that falls behind skips to the next key frame instead of queueing.
use crate::hid;
use crate::instance::Instance;
use crate::ring::{Frame, Ring};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{SyncSender, TrySendError};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

/// What a browser sees of a phone and where its touches go (the launcher's Viewer::set_display rule)
#[derive(Clone, Debug, PartialEq)]
pub struct Target {
    pub ring: PathBuf,
    /// the screen's size upright, device pixels
    pub size: (u32, u32),
    /// the picture's turn (hid::unrotate / Frame::png convention)
    pub rot: u32,
    /// for hid: a second display (SFB id, its turn) and the main screen's turn
    pub display: Option<(u32, u32)>,
    pub main_rot: u32,
}

/// The screen shown: a two-screen phone's inner one while the hinge is open; an iPhone / iPad turned to landscape is
/// shown turned (`orient` keeps the last turn for faceUp / faceDown, as Apple's Simulator does)
pub fn target(inst: &Instance, orient: &mut u32) -> Target {
    if inst.platform == "ios" {
        if let Some(o) = inst.orientation().and_then(|o| hid::orientation_rot(&o)) {
            *orient = o;
        }
    }
    let o = *orient;
    let (ring, screen, panel) = match &inst.inner {
        Some(d) if inst.inner_active() => (d.ring.clone(), d.upright(), Some((d.id, d.rotation))),
        _ => (inst.ring.clone(), inst.screen, None),
    };
    let size = if o % 180 == 90 { (screen.1, screen.0) } else { screen };
    Target {
        ring,
        size,
        rot: (panel.map(|p| p.1).unwrap_or(0) + o) % 360,
        display: panel.map(|(id, r)| (id, (r + o) % 360)),
        main_rot: o,
    }
}

/// An RGB24 picture
pub struct Rgb {
    pub w: u32,
    pub h: u32,
    pub px: Vec<u8>,
}

/// The output size for a frame `fw` x `fh` (framebuffer) turned by `rot`, at most `max_w` wide (and never larger than the
/// frame), both even (4:2:0)
pub fn out_size(fw: u32, fh: u32, rot: u32, max_w: u32) -> (u32, u32) {
    let (uw, uh) = if rot % 180 == 90 { (fh, fw) } else { (fw, fh) };
    let w = max_w.clamp(16, uw.max(16));
    let h = ((uh as u64 * w as u64) / uw.max(1) as u64) as u32;
    (w & !1, (h & !1).max(2))
}

/// The frame upright, scaled to `ow` x `oh` with an area filter (up to 4 x 4 samples a pixel: small text stays legible)
pub fn scale(f: &Frame, px: &[u8], rot: u32, ow: u32, oh: u32) -> Rgb {
    let (fw, fh, stride) = (f.w as usize, f.h as usize, f.stride as usize);
    let (ow_, oh_) = (ow as usize, oh as usize);
    // upright width / height in framebuffer pixels
    let (uw, uh) = if rot % 180 == 90 { (fh, fw) } else { (fw, fh) };
    let kx = uw.div_ceil(ow_).clamp(1, 4);
    let ky = uh.div_ceil(oh_).clamp(1, 4);
    // the upright sample positions along each axis (pixel indices in the upright picture)
    let axis = |n_out: usize, n_up: usize, k: usize| -> Vec<usize> {
        let mut v = Vec::with_capacity(n_out * k);
        for o in 0..n_out {
            for s in 0..k {
                let p = ((o as f64 + (s as f64 + 0.5) / k as f64) * n_up as f64 / n_out as f64) as usize;
                v.push(p.min(n_up - 1));
            }
        }
        v
    };
    let ux = axis(ow_, uw, kx);
    let uy = axis(oh_, uh, ky);
    // byte offsets: an upright x / y becomes a framebuffer column or row depending on the turn (Frame::png's mapping)
    let (col, row): (Vec<usize>, Vec<usize>) = match rot % 360 {
        270 => (ux.iter().map(|&x| (fh - 1 - x) * stride).collect(), uy.iter().map(|&y| y * 4).collect()),
        90 => (ux.iter().map(|&x| x * stride).collect(), uy.iter().map(|&y| (fw - 1 - y) * 4).collect()),
        180 => (ux.iter().map(|&x| (fw - 1 - x) * 4).collect(), uy.iter().map(|&y| (fh - 1 - y) * stride).collect()),
        _ => (ux.iter().map(|&x| x * 4).collect(), uy.iter().map(|&y| y * stride).collect()),
    };
    let n = (kx * ky) as u32;
    let mut out = vec![0u8; ow_ * oh_ * 3];
    for yo in 0..oh_ {
        let rows = &row[yo * ky..yo * ky + ky];
        let line = &mut out[yo * ow_ * 3..(yo + 1) * ow_ * 3];
        for xo in 0..ow_ {
            let cols = &col[xo * kx..xo * kx + kx];
            let (mut b, mut g, mut r) = (0u32, 0u32, 0u32);
            for &ro in rows {
                for &co in cols {
                    let o = ro + co;
                    b += px[o] as u32;
                    g += px[o + 1] as u32;
                    r += px[o + 2] as u32;
                }
            }
            let d = &mut line[xo * 3..xo * 3 + 3];
            d[0] = ((r + n / 2) / n) as u8;
            d[1] = ((g + n / 2) / n) as u8;
            d[2] = ((b + n / 2) / n) as u8;
        }
    }
    Rgb { w: ow, h: oh, px: out }
}

impl Rgb {
    pub fn jpeg(&self, quality: u8) -> Vec<u8> {
        let mut out = Vec::new();
        let e = jpeg_encoder::Encoder::new(&mut out, quality);
        let _ = e.encode(&self.px, self.w as u16, self.h as u16, jpeg_encoder::ColorType::Rgb);
        out
    }

    /// BT.601 limited range 4:2:0 planes (what a decoder assumes for a stream without colour info)
    pub fn i420(&self) -> (Vec<u8>, Vec<u8>, Vec<u8>) {
        let (w, h) = (self.w as usize, self.h as usize);
        let mut y = vec![0u8; w * h];
        let mut u = vec![0u8; w / 2 * (h / 2)];
        let mut v = vec![0u8; w / 2 * (h / 2)];
        let p = &self.px;
        for (i, yy) in y.iter_mut().enumerate() {
            let (r, g, b) = (p[i * 3] as i32, p[i * 3 + 1] as i32, p[i * 3 + 2] as i32);
            *yy = (((66 * r + 129 * g + 25 * b + 128) >> 8) + 16) as u8;
        }
        for cy in 0..h / 2 {
            for cx in 0..w / 2 {
                let (mut r, mut g, mut b) = (0i32, 0i32, 0i32);
                for (dx, dy) in [(0, 0), (1, 0), (0, 1), (1, 1)] {
                    let o = ((cy * 2 + dy) * w + cx * 2 + dx) * 3;
                    r += p[o] as i32;
                    g += p[o + 1] as i32;
                    b += p[o + 2] as i32;
                }
                let (r, g, b) = (r / 4, g / 4, b / 4);
                u[cy * (w / 2) + cx] = (((-38 * r - 74 * g + 112 * b + 128) >> 8) + 128).clamp(0, 255) as u8;
                v[cy * (w / 2) + cx] = (((112 * r - 94 * g - 18 * b + 128) >> 8) + 128).clamp(0, 255) as u8;
            }
        }
        (y, u, v)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Codec {
    H264,
    Jpeg,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct StreamKey {
    pub phone: String,
    pub codec: Codec,
    /// the largest width wanted (rounded to 16)
    pub width: u32,
    /// 0 low, 1 medium, 2 high
    pub quality: u8,
    pub fps: u32,
}

/// A message to a viewer's socket writer
pub enum Out {
    /// a binary video packet (header + payload); `key`: a viewer can start here
    Video(Arc<Vec<u8>>, bool),
    Text(String),
    Bin(Vec<u8>),
    Pong(Vec<u8>),
    Close,
}

/// The binary header of a video packet: kind (1 H.264 Annex B access unit, 2 JPEG; audio.rs: 3 Opus, 4 PCM), flags (1 key frame), width, height
/// (u16 LE each), the capture time (u64 LE microseconds, unix), then the payload
pub const HDR_LEN: usize = 14;
pub fn packet(kind: u8, key: bool, w: u32, h: u32, t_us: u64, payload: &[u8]) -> Vec<u8> {
    let mut v = Vec::with_capacity(HDR_LEN + payload.len());
    v.push(kind);
    v.push(key as u8);
    v.extend_from_slice(&(w as u16).to_le_bytes());
    v.extend_from_slice(&(h as u16).to_le_bytes());
    v.extend_from_slice(&t_us.to_le_bytes());
    v.extend_from_slice(payload);
    v
}

struct Sub {
    id: u64,
    tx: SyncSender<Out>,
    /// dropped a packet: skips to the next key frame
    waiting_key: bool,
}

pub struct Streamer {
    key: StreamKey,
    subs: Mutex<Vec<Sub>>,
    want_key: AtomicBool,
    /// frames sent / encoded (for the status page)
    pub stats: Mutex<StreamStats>,
}

#[derive(Default, Clone, serde::Serialize)]
pub struct StreamStats {
    pub frames: u64,
    pub bytes: u64,
    pub last_encode_ms: f64,
    pub viewers: usize,
    pub size: (u32, u32),
}

fn hub() -> &'static Mutex<HashMap<StreamKey, Arc<Streamer>>> {
    static H: OnceLock<Mutex<HashMap<StreamKey, Arc<Streamer>>>> = OnceLock::new();
    H.get_or_init(Default::default)
}

/// Every running stream's key and stats (status page / MCP)
pub fn streams() -> Vec<(StreamKey, StreamStats)> {
    hub().lock().unwrap().iter().map(|(k, s)| (k.clone(), s.stats.lock().unwrap().clone())).collect()
}

/// Subscribes `tx` to the stream `key` of `inst`, starting its encoder thread if needed. Returns an id for `leave`.
pub fn join(inst: &Instance, key: StreamKey, tx: SyncSender<Out>) -> (Arc<Streamer>, u64) {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
    let id = NEXT.fetch_add(1, Ordering::Relaxed);
    let mut h = hub().lock().unwrap();
    let s = h
        .entry(key.clone())
        .or_insert_with(|| {
            let s = Arc::new(Streamer { key: key.clone(), subs: Mutex::new(vec![]), want_key: AtomicBool::new(true), stats: Default::default() });
            let (s2, inst2) = (s.clone(), inst.clone());
            std::thread::Builder::new().name(format!("stream-{}", key.phone)).spawn(move || s2.run(inst2)).ok();
            s
        })
        .clone();
    s.subs.lock().unwrap().push(Sub { id, tx, waiting_key: true });
    s.want_key.store(true, Ordering::Relaxed);
    (s, id)
}

impl Streamer {
    pub fn leave(&self, id: u64) {
        self.subs.lock().unwrap().retain(|s| s.id != id);
    }

    /// A viewer asks for a key frame (its decoder lost its place)
    pub fn request_key(&self) {
        self.want_key.store(true, Ordering::Relaxed);
    }

    fn broadcast(&self, pkt: Vec<u8>, key: bool) -> usize {
        let pkt = Arc::new(pkt);
        let mut subs = self.subs.lock().unwrap();
        let mut lost = false;
        subs.retain_mut(|s| {
            if s.waiting_key && !key {
                return true;
            }
            match s.tx.try_send(Out::Video(pkt.clone(), key)) {
                Ok(()) => {
                    s.waiting_key = false;
                    true
                }
                Err(TrySendError::Full(_)) => {
                    // H.264 frames depend on the ones before: this viewer waits for the next key frame
                    s.waiting_key = self.key.codec == Codec::H264;
                    lost = true;
                    true
                }
                Err(TrySendError::Disconnected(_)) => false,
            }
        });
        if lost {
            self.want_key.store(true, Ordering::Relaxed);
        }
        subs.len()
    }

    fn run(self: Arc<Self>, inst: Instance) {
        let mut orient = 0u32;
        let mut tgt = target(&inst, &mut orient);
        let mut ring = Ring::new(&tgt.ring);
        let mut enc: Option<(openh264::encoder::Encoder, (u32, u32))> = None;
        let mut sent_seq = 0u64;
        let mut last_enc = Instant::now() - Duration::from_secs(1);
        let mut last_check = Instant::now();
        let mut idle_since: Option<Instant> = None;
        let mut refines = 0u32;
        let mut empty_since: Option<Instant> = None;
        let t0 = Instant::now();
        let min_gap = Duration::from_micros(1_000_000 / self.key.fps.clamp(1, 60) as u64);
        loop {
            // no viewer for a while: the stream ends (a new viewer starts a new one)
            {
                let mut h = hub().lock().unwrap();
                let n = self.subs.lock().unwrap().len();
                self.stats.lock().unwrap().viewers = n;
                if n == 0 {
                    let since = *empty_since.get_or_insert_with(Instant::now);
                    if since.elapsed() > Duration::from_secs(5) {
                        h.remove(&self.key);
                        return;
                    }
                } else {
                    empty_since = None;
                }
            }
            if last_check.elapsed() > Duration::from_millis(400) {
                last_check = Instant::now();
                let t = target(&inst, &mut orient);
                if t != tgt {
                    if t.ring != tgt.ring {
                        ring = Ring::new(&t.ring);
                    }
                    tgt = t;
                    sent_seq = 0;
                    self.want_key.store(true, Ordering::Relaxed);
                }
            }
            let want_key = self.want_key.load(Ordering::Relaxed);
            let seq = ring.seq();
            let pending = seq != 0 && (seq != sent_seq || want_key);
            if !pending {
                // a still screen: after a motion burst, the last picture is encoded again (an H.264 P frame of the
                // same input sharpens what the bit budget blurred), then nothing runs until the next frame
                if self.key.codec == Codec::H264 && refines < 2 && idle_since.is_some_and(|t| t.elapsed() > Duration::from_millis(250)) && sent_seq != 0 {
                    refines += 1;
                    idle_since = Some(Instant::now());
                } else {
                    if seq != 0 {
                        idle_since.get_or_insert_with(Instant::now);
                    }
                    ring.wait(seq, Duration::from_millis(100));
                    continue;
                }
            } else {
                let gap = last_enc.elapsed();
                if gap < min_gap {
                    std::thread::sleep(min_gap - gap);
                }
                refines = 0;
                idle_since = Some(Instant::now());
            }
            let codec = self.key.codec;
            let (max_w, rot) = (self.key.width, tgt.rot);
            let started = Instant::now();
            let Some((rgb, fseq, t_ns)) = ring.latest_with(|f, px| {
                let (ow, oh) = out_size(f.w, f.h, rot, max_w);
                (scale(f, px, rot, ow, oh), f.seq, f.t_ns)
            }) else {
                std::thread::sleep(Duration::from_millis(5));
                continue;
            };
            last_enc = Instant::now();
            let t_us = if t_ns > 0 { t_ns / 1000 } else { t0.elapsed().as_micros() as u64 };
            let (pkt, key) = match codec {
                Codec::Jpeg => {
                    let q = [55, 72, 88][self.key.quality.min(2) as usize];
                    (packet(2, true, rgb.w, rgb.h, t_us, &rgb.jpeg(q)), true)
                }
                Codec::H264 => {
                    if enc.as_ref().map(|e| e.1) != Some((rgb.w, rgb.h)) {
                        enc = new_encoder(rgb.w, rgb.h, self.key.quality, self.key.fps).map(|e| (e, (rgb.w, rgb.h)));
                        self.want_key.store(true, Ordering::Relaxed);
                    }
                    let Some((e, _)) = enc.as_mut() else {
                        std::thread::sleep(Duration::from_millis(200));
                        continue;
                    };
                    if self.want_key.swap(false, Ordering::Relaxed) {
                        e.force_intra_frame();
                    }
                    let (y, u, v) = rgb.i420();
                    let yuv = openh264::formats::YUVSlices::new((&y, &u, &v), (rgb.w as usize, rgb.h as usize), (rgb.w as usize, rgb.w as usize / 2, rgb.w as usize / 2));
                    match e.encode_at(&yuv, openh264::Timestamp::from_millis(t0.elapsed().as_millis() as u64)) {
                        Ok(bs) => {
                            let ft = bs.frame_type();
                            if matches!(ft, openh264::encoder::FrameType::Skip | openh264::encoder::FrameType::Invalid) {
                                sent_seq = fseq;
                                continue;
                            }
                            let key = matches!(ft, openh264::encoder::FrameType::IDR | openh264::encoder::FrameType::I);
                            (packet(1, key, rgb.w, rgb.h, t_us, &bs.to_vec()), key)
                        }
                        Err(_) => {
                            enc = None;
                            continue;
                        }
                    }
                }
            };
            if codec == Codec::Jpeg {
                self.want_key.store(false, Ordering::Relaxed);
            }
            sent_seq = fseq;
            let len = pkt.len();
            self.broadcast(pkt, key);
            let mut st = self.stats.lock().unwrap();
            st.frames += 1;
            st.bytes += len as u64;
            st.last_encode_ms = started.elapsed().as_secs_f64() * 1000.0;
            st.size = (rgb.w, rgb.h);
        }
    }
}

fn new_encoder(w: u32, h: u32, quality: u8, fps: u32) -> Option<openh264::encoder::Encoder> {
    use openh264::encoder::*;
    // bits per pixel per second, scaled to the picture: a phone UI at 720 x 1560 gets ~1.5 / 3.5 / 8 Mbit/s
    let bpp = [1.4f64, 3.2, 7.0][quality.min(2) as usize];
    let bps = ((w as f64 * h as f64) / (720.0 * 1560.0) * bpp * 1e6).clamp(400e3, 20e6) as u32;
    let cfg = EncoderConfig::new()
        .usage_type(UsageType::ScreenContentRealTime)
        .bitrate(BitRate::from_bps(bps))
        .max_frame_rate(FrameRate::from_hz(fps as f32))
        .rate_control_mode(RateControlMode::Bitrate)
        .skip_frames(false)
        .intra_frame_period(IntraFramePeriod::from_num_frames(0));
    Encoder::with_api_config(openh264::OpenH264API::from_source(), cfg).ok()
}

/// A still JPEG of a phone's newest frame, upright, at most `max_w` wide (dashboard thumbnails)
pub fn snapshot_jpeg(inst: &Instance, max_w: u32, quality: u8) -> Option<Vec<u8>> {
    let mut o = 0;
    let t = target(inst, &mut o);
    let mut ring = Ring::new(&t.ring);
    ring.latest_with(|f, px| {
        let (ow, oh) = out_size(f.w, f.h, t.rot, max_w);
        scale(f, px, t.rot, ow, oh).jpeg(quality)
    })
}

/// The newest frame as a full-size PNG, upright
pub fn snapshot_png(inst: &Instance) -> Option<Vec<u8>> {
    let mut o = 0;
    let t = target(inst, &mut o);
    Ring::new(&t.ring).latest()?.png(t.rot).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scale_turns_like_png() {
        // 2x1 BGRA: left red, right blue; 270 shows framebuffer x running down the picture (Frame::png)
        let f = Frame { w: 2, h: 1, stride: 8, seq: 1, t_ns: 0, bgra: vec![] };
        let px = vec![0, 0, 255, 255, 255, 0, 0, 255];
        let r = scale(&f, &px, 0, 2, 1);
        assert_eq!(r.px, vec![255, 0, 0, 0, 0, 255]);
        let r = scale(&f, &px, 270, 1, 2);
        assert_eq!(r.px, vec![255, 0, 0, 0, 0, 255]);
        let r = scale(&f, &px, 90, 1, 2);
        assert_eq!(r.px, vec![0, 0, 255, 255, 0, 0]);
        let r = scale(&f, &px, 180, 2, 1);
        assert_eq!(r.px, vec![0, 0, 255, 255, 0, 0]);
    }

    #[test]
    fn scale_averages_and_sizes() {
        let f = Frame { w: 4, h: 2, stride: 16, seq: 1, t_ns: 0, bgra: vec![] };
        // checkerboard black / white: halved, every pixel is grey
        let mut px = vec![0u8; 32];
        for y in 0..2 {
            for x in 0..4 {
                let v = if (x + y) % 2 == 0 { 0 } else { 255 };
                px[y * 16 + x * 4..y * 16 + x * 4 + 3].copy_from_slice(&[v, v, v]);
            }
        }
        let r = scale(&f, &px, 0, 2, 1);
        assert!(r.px.iter().all(|&v| v == 128), "{:?}", r.px);
        assert_eq!(out_size(1206, 2622, 0, 720), (720, 1564));
        assert_eq!(out_size(1206, 2622, 90, 720), (720, 330));
        assert_eq!(out_size(100, 200, 0, 720), (100, 200));
    }

    #[test]
    fn h264_encodes_a_key_frame() {
        let rgb = Rgb { w: 64, h: 128, px: (0..64 * 128 * 3).map(|i| (i % 251) as u8).collect() };
        let mut e = new_encoder(64, 128, 1, 30).unwrap();
        e.force_intra_frame();
        let (y, u, v) = rgb.i420();
        let yuv = openh264::formats::YUVSlices::new((&y, &u, &v), (64, 128), (64, 32, 32));
        let bs = e.encode(&yuv).unwrap();
        assert_eq!(bs.frame_type(), openh264::encoder::FrameType::IDR);
        let b = bs.to_vec();
        // Annex B with an SPS first (what the page reads the codec string from)
        assert_eq!(&b[..4], &[0, 0, 0, 1]);
        assert_eq!(b[4] & 0x1f, 7);
    }
}
