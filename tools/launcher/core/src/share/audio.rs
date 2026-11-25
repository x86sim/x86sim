//! Share: the phone's sound for the browser, and a browser's microphone for the phone. cli/libexec/simaudio.py (the
//! phone's coreaudiod, one a boot) offers every output stream of the phone on its tap, `<state dir>/simaudio-tap.sock`
//! (protocol there). An `Audio` a phone, started by the first listener, mixes those streams to 48 kHz stereo on a
//! 10 ms clock and encodes each 10 ms once for every listener: Opus (libopus, built from source into the binary) or
//! 16-bit PCM for a browser without WebCodecs. A stream is held back until it has a cycle and 10 ms queued (the phone
//! writes in IO cycles of ~10 ms) and trimmed when it piles up, so the server adds ~10-20 ms. Nothing is sent while
//! the phone is silent (no stream open). A listener's microphone (16-bit mono 48 kHz from the page) goes back on the
//! tap; simaudio.py gives it to the phone's input streams while it arrives.
use super::video::{self, Out};
use crate::instance::Instance;
use std::collections::{HashMap, VecDeque};
use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{SyncSender, TrySendError};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

pub const RATE: u32 = 48000;
/// frames a packet (10 ms)
pub const FRAMES: usize = 480;
pub const KIND_OPUS: u8 = 3;
pub const KIND_PCM: u8 = 4;
/// a browser's microphone frame on the socket (first byte of a binary message, then s16 LE mono 48 kHz)
pub const KIND_MIC: u8 = 5;

const OP_OPEN: u8 = 1;
const OP_DATA: u8 = 2;
const OP_CLOSE: u8 = 3;
const OP_MIC: u8 = 4;
const OP_MIC_OFF: u8 = 5;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Codec {
    Opus,
    Pcm,
}

struct Sub {
    id: u64,
    tx: SyncSender<Out>,
    codec: Codec,
    /// lost a packet or just joined: the next packet is marked as a new start (the page re-anchors its schedule)
    restart: bool,
}

pub struct Audio {
    phone: String,
    tap: PathBuf,
    subs: Mutex<Vec<Sub>>,
    /// the tap's write side (the microphone)
    tap_w: Mutex<Option<UnixStream>>,
    mic_owner: Mutex<Option<u64>>,
    pub connected: AtomicBool,
}

fn hub() -> &'static Mutex<HashMap<String, Arc<Audio>>> {
    static H: OnceLock<Mutex<HashMap<String, Arc<Audio>>>> = OnceLock::new();
    H.get_or_init(Default::default)
}

/// Subscribes `tx` to the phone's sound, starting its mixer if needed. Returns an id for `leave`.
pub fn join(inst: &Instance, tx: SyncSender<Out>, codec: Codec) -> (Arc<Audio>, u64) {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
    let id = NEXT.fetch_add(1, Ordering::Relaxed);
    let mut h = hub().lock().unwrap();
    let a = h
        .entry(inst.id.clone())
        .or_insert_with(|| {
            let a = Arc::new(Audio {
                phone: inst.id.clone(),
                tap: inst.state_dir.join("simaudio-tap.sock"),
                subs: Mutex::new(vec![]),
                tap_w: Mutex::new(None),
                mic_owner: Mutex::new(None),
                connected: AtomicBool::new(false),
            });
            let a2 = a.clone();
            std::thread::Builder::new().name(format!("audio-{}", inst.id)).spawn(move || a2.run()).ok();
            a
        })
        .clone();
    a.subs.lock().unwrap().push(Sub { id, tx, codec, restart: true });
    (a, id)
}

/// The phone's mixer for the microphone of a viewer who is not listening (joins with no sound)
pub fn get(inst: &Instance) -> Arc<Audio> {
    let (a, id) = join(inst, std::sync::mpsc::sync_channel(1).0, Codec::Pcm);
    a.leave(id);
    a
}

/// One output stream of the phone, as 48 kHz stereo
struct Stream {
    rate: u32,
    ch: usize,
    buf: VecDeque<f32>,
    /// resampler (44.1 kHz): the position in the input after the last frame, the last input frame
    frac: f64,
    prev: [f32; 2],
    /// frames in the phone's last write (its IO cycle)
    chunk: usize,
    starved: bool,
    closed: bool,
    /// the least queued after a tick over the last second: what never got used is latency, dropped
    low: usize,
    ticks: u32,
}

impl Stream {
    fn new(rate: u32, ch: usize) -> Stream {
        Stream { rate, ch, buf: VecDeque::new(), frac: 0.0, prev: [0.0; 2], chunk: FRAMES, starved: true, closed: false, low: usize::MAX, ticks: 0 }
    }

    fn push(&mut self, pcm: &[u8]) {
        let n = pcm.len() / 4 / self.ch;
        let frame = |i: usize| -> [f32; 2] {
            let s = |k: usize| f32::from_le_bytes(pcm[(i * self.ch + k) * 4..(i * self.ch + k) * 4 + 4].try_into().unwrap());
            if self.ch == 1 {
                let v = s(0);
                [v, v]
            } else {
                [s(0), s(1)]
            }
        };
        if n == 0 {
            return;
        }
        let before = self.buf.len() / 2;
        if self.rate == RATE {
            for i in 0..n {
                self.buf.extend(frame(i));
            }
        } else {
            // linear: position 0 is the previous frame, 1 the first new one
            let step = self.rate as f64 / RATE as f64;
            let mut t = self.frac;
            while t < n as f64 {
                let i = t.floor() as usize;
                let a = if i == 0 { self.prev } else { frame(i - 1) };
                let b = frame(i);
                let f = (t - i as f64) as f32;
                self.buf.extend([a[0] + (b[0] - a[0]) * f, a[1] + (b[1] - a[1]) * f]);
                t += step;
            }
            self.frac = t - n as f64;
            self.prev = frame(n - 1);
        }
        self.chunk = self.buf.len() / 2 - before;
    }

    /// adds this tick's frames to `mix`; false while the stream is held back
    fn mix_into(&mut self, mix: &mut [f32]) -> bool {
        let have = self.buf.len() / 2;
        let chunk = self.chunk.max(FRAMES);
        if self.starved {
            if have < FRAMES + chunk && !(self.closed && have > 0) {
                return false;
            }
            self.starved = false;
        }
        // piled up (the phone's clock ran ahead, or we were descheduled): back to the start level
        if have > chunk + FRAMES * 10 {
            let drop = have - (chunk + FRAMES * 2);
            self.buf.drain(..drop * 2);
        }
        let take = FRAMES.min(self.buf.len() / 2);
        for (k, v) in self.buf.drain(..take * 2).enumerate() {
            mix[k] += v;
        }
        if take < FRAMES {
            self.starved = true;
            self.low = usize::MAX;
            self.ticks = 0;
        } else {
            // a hiccup (the phone's IO thread or ours late, then catching up) leaves frames queued for good: a second
            // in which the queue never went below 10 ms gives back all but 5 ms of it
            self.low = self.low.min(self.buf.len() / 2);
            self.ticks += 1;
            if self.ticks >= 100 {
                if self.low > FRAMES {
                    self.buf.drain(..(self.low - FRAMES / 2) * 2);
                }
                self.low = usize::MAX;
                self.ticks = 0;
            }
        }
        true
    }
}

fn read_msg(s: &mut UnixStream) -> std::io::Result<(u8, u32, Vec<u8>)> {
    let mut h = [0u8; 9];
    s.read_exact(&mut h)?;
    let n = u32::from_le_bytes(h[5..9].try_into().unwrap()) as usize;
    if n > 1 << 22 {
        return Err(std::io::Error::other("tap message too large"));
    }
    let mut p = vec![0u8; n];
    s.read_exact(&mut p)?;
    Ok((h[0], u32::from_le_bytes(h[1..5].try_into().unwrap()), p))
}

fn tap_msg(op: u8, payload: &[u8]) -> Vec<u8> {
    let mut v = Vec::with_capacity(9 + payload.len());
    v.push(op);
    v.extend_from_slice(&0u32.to_le_bytes());
    v.extend_from_slice(&(payload.len() as u32).to_le_bytes());
    v.extend_from_slice(payload);
    v
}

fn now_us() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_micros() as u64).unwrap_or(0)
}

impl Audio {
    pub fn leave(&self, id: u64) {
        self.subs.lock().unwrap().retain(|s| s.id != id);
        self.mic_off(id);
    }

    /// 16-bit mono 48 kHz frames from viewer `id`'s microphone (the last viewer to send one owns the microphone)
    pub fn mic(&self, id: u64, s16: &[u8]) {
        *self.mic_owner.lock().unwrap() = Some(id);
        let f: Vec<u8> = s16.chunks_exact(2).flat_map(|c| (i16::from_le_bytes([c[0], c[1]]) as f32 / 32768.0).to_le_bytes()).collect();
        self.tap_write(&tap_msg(OP_MIC, &f));
    }

    pub fn mic_off(&self, id: u64) {
        let mut o = self.mic_owner.lock().unwrap();
        if *o == Some(id) {
            *o = None;
            drop(o);
            self.tap_write(&tap_msg(OP_MIC_OFF, &[]));
        }
    }

    fn tap_write(&self, m: &[u8]) {
        let mut w = self.tap_w.lock().unwrap();
        if let Some(s) = w.as_mut() {
            if s.write_all(m).is_err() {
                *w = None;
            }
        }
    }

    fn run(self: Arc<Self>) {
        let mut empty_since: Option<Instant> = None;
        loop {
            // no listener and no microphone for a while: the mixer ends (a new listener starts a new one)
            {
                let mut h = hub().lock().unwrap();
                if self.idle() {
                    if empty_since.get_or_insert_with(Instant::now).elapsed() > Duration::from_secs(5) {
                        h.remove(&self.phone);
                        return;
                    }
                } else {
                    empty_since = None;
                }
            }
            match UnixStream::connect(&self.tap) {
                Ok(s) => {
                    self.session(s);
                    // ended for want of listeners: gone unless one joined meanwhile
                    let mut h = hub().lock().unwrap();
                    if self.idle() {
                        h.remove(&self.phone);
                        return;
                    }
                }
                Err(_) => std::thread::sleep(Duration::from_millis(1000)),
            }
        }
    }

    /// One connection to the tap: a reader thread fills the streams, this thread mixes and sends every 10 ms
    fn session(&self, s: UnixStream) {
        let _ = s.set_write_timeout(Some(Duration::from_millis(200)));
        *self.tap_w.lock().unwrap() = s.try_clone().ok();
        self.connected.store(true, Ordering::Relaxed);
        let streams: Arc<Mutex<HashMap<u32, Stream>>> = Default::default();
        let alive = Arc::new(AtomicBool::new(true));
        let reader = {
            let (streams, alive) = (streams.clone(), alive.clone());
            let mut r = match s.try_clone() {
                Ok(r) => r,
                Err(_) => return,
            };
            std::thread::spawn(move || {
                while let Ok((op, sid, p)) = read_msg(&mut r) {
                    let mut st = streams.lock().unwrap();
                    match op {
                        OP_OPEN if p.len() >= 8 => {
                            let rate = u32::from_le_bytes(p[0..4].try_into().unwrap());
                            let ch = u32::from_le_bytes(p[4..8].try_into().unwrap()).clamp(1, 2) as usize;
                            st.insert(sid, Stream::new(rate, ch));
                        }
                        OP_DATA => {
                            if let Some(x) = st.get_mut(&sid) {
                                x.push(&p);
                            }
                        }
                        OP_CLOSE => {
                            if let Some(x) = st.get_mut(&sid) {
                                x.closed = true;
                            }
                        }
                        _ => {}
                    }
                }
                alive.store(false, Ordering::Relaxed);
            })
        };
        let mut opus: Option<opus::Encoder> = None;
        let mut mix = vec![0f32; FRAMES * 2];
        let mut out = vec![0u8; 4000];
        let tick = Duration::from_micros(1_000_000 * FRAMES as u64 / RATE as u64);
        let mut next = Instant::now();
        let mut quiet = 0u32;
        while alive.load(Ordering::Relaxed) {
            next += tick;
            let now = Instant::now();
            if next > now {
                std::thread::sleep(next - now);
            } else if now - next > Duration::from_millis(200) {
                next = now;
            }
            if self.subs.lock().unwrap().is_empty() {
                // the microphone alone keeps the connection; nobody for 5 s ends it
                quiet = if self.idle() { quiet + 1 } else { 0 };
                if quiet > 500 {
                    break;
                }
                continue;
            }
            quiet = 0;
            mix.iter_mut().for_each(|v| *v = 0.0);
            let mut any = false;
            let mut open = false;
            {
                let mut st = streams.lock().unwrap();
                for x in st.values_mut() {
                    any |= x.mix_into(&mut mix);
                    open |= !x.closed;
                }
                st.retain(|_, x| !(x.closed && x.buf.is_empty()));
            }
            if !any && !open {
                // silent: nothing to send; the next sound starts fresh
                if let Some(e) = opus.as_mut() {
                    let _ = e.reset_state();
                }
                for s in self.subs.lock().unwrap().iter_mut() {
                    s.restart = true;
                }
                continue;
            }
            let t = now_us();
            let mut subs = self.subs.lock().unwrap();
            let mut opus_pkt: Option<(Vec<u8>, Vec<u8>)> = None;
            let mut pcm_pkt: Option<(Vec<u8>, Vec<u8>)> = None;
            subs.retain_mut(|sub| {
                let slot = if sub.codec == Codec::Opus { &mut opus_pkt } else { &mut pcm_pkt };
                if slot.is_none() {
                    let payload = match sub.codec {
                        Codec::Opus => {
                            let e = opus.get_or_insert_with(|| {
                                let mut e = opus::Encoder::new(RATE, opus::Channels::Stereo, opus::Application::LowDelay).expect("opus");
                                let _ = e.set_bitrate(opus::Bitrate::Bits(128_000));
                                e
                            });
                            let n = e.encode_float(&mix, &mut out).unwrap_or(0);
                            out[..n].to_vec()
                        }
                        Codec::Pcm => mix.iter().flat_map(|v| ((v.clamp(-1.0, 1.0) * 32767.0) as i16).to_le_bytes()).collect(),
                    };
                    let kind = if sub.codec == Codec::Opus { KIND_OPUS } else { KIND_PCM };
                    *slot = Some((video::packet(kind, false, RATE, 2, t, &payload), video::packet(kind, true, RATE, 2, t, &payload)));
                }
                let (cont, start) = slot.as_ref().unwrap();
                match sub.tx.try_send(Out::Bin(if sub.restart { start.clone() } else { cont.clone() })) {
                    Ok(()) => {
                        sub.restart = false;
                        true
                    }
                    Err(TrySendError::Full(_)) => {
                        sub.restart = true;
                        true
                    }
                    Err(TrySendError::Disconnected(_)) => false,
                }
            });
        }
        let _ = s.shutdown(std::net::Shutdown::Both);
        let _ = reader.join();
        *self.tap_w.lock().unwrap() = None;
        self.connected.store(false, Ordering::Relaxed);
    }

    fn idle(&self) -> bool {
        self.subs.lock().unwrap().is_empty() && self.mic_owner.lock().unwrap().is_none()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pcm(v: &[f32]) -> Vec<u8> {
        v.iter().flat_map(|x| x.to_le_bytes()).collect()
    }

    #[test]
    fn resamples_and_holds_back() {
        let mut s = Stream::new(44100, 1);
        s.push(&pcm(&vec![0.5; 441]));
        // 10 ms at 44.1 kHz is 10 ms at 48 kHz, stereo
        assert!((s.buf.len() / 2).abs_diff(480) <= 1);
        assert!(s.buf.iter().skip(4).all(|v| (*v - 0.5).abs() < 1e-6));
        let mut mix = vec![0f32; FRAMES * 2];
        assert!(!s.mix_into(&mut mix), "held back until a cycle and 10 ms are queued");
        for _ in 0..3 {
            s.push(&pcm(&vec![0.5; 441]));
        }
        assert!(s.mix_into(&mut mix));
        assert!((mix[100] - 0.5).abs() < 1e-6);
    }

    #[test]
    fn trims_a_pile_up() {
        let mut s = Stream::new(48000, 2);
        s.push(&pcm(&vec![0.1; 512 * 2]));
        for _ in 0..30 {
            s.push(&pcm(&vec![0.1; 512 * 2]));
        }
        let mut mix = vec![0f32; FRAMES * 2];
        assert!(s.mix_into(&mut mix));
        assert!(s.buf.len() / 2 <= 512 + FRAMES * 2);
    }

    #[test]
    fn gives_back_unused_queue() {
        let mut s = Stream::new(48000, 2);
        let mut mix = vec![0f32; FRAMES * 2];
        // 60 ms more than the ticks use, steadily
        s.push(&pcm(&vec![0.1; (FRAMES * 6 + FRAMES * 2) * 2]));
        s.chunk = FRAMES;
        for _ in 0..100 {
            s.push(&pcm(&vec![0.1; FRAMES * 2]));
            s.chunk = FRAMES;
            assert!(s.mix_into(&mut mix));
        }
        assert!(s.buf.len() / 2 <= FRAMES, "{}", s.buf.len() / 2);
    }

    #[test]
    fn opus_encodes_10ms() {
        let mut e = opus::Encoder::new(RATE, opus::Channels::Stereo, opus::Application::LowDelay).unwrap();
        let mut out = [0u8; 4000];
        let n = e.encode_float(&[0.0; FRAMES * 2], &mut out).unwrap();
        assert!(n > 0);
    }
}
