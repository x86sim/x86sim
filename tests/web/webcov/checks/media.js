// media: <video>/<audio> playback per codec (muted autoplay: time advances, the decoded frame's pixels are the clip's
// red-over-blue), MSE / ManagedMediaSource, native HLS, canPlayType, Web Audio (offline render, decodeAudioData per
// codec, AudioWorklet, a running realtime context), WebCodecs encode + decode, ImageDecoder, MediaRecorder,
// Picture in Picture, getUserMedia (camera / mic: the simulator's fake sources), EME, Media Session
'use strict';
async function playVideo(src, opts) {
	const v = el('video', { muted: true, playsInline: true, autoplay: true, src: src }); v.setAttribute('playsinline', ''); v.width = 160; v.height = 120;
	await once(v, 'loadedmetadata', 8000, 'error');
	await v.play();
	const t0 = performance.now(); while (v.currentTime < 0.6 && performance.now() - t0 < 8000 && alive()) await sleep(100);
	ok(v.currentTime >= 0.6, 'currentTime stuck at ' + v.currentTime);
	return v;
}
function videoPixels(v) { const c = document.createElement('canvas'); c.width = 64; c.height = 64; const g = c.getContext('2d'); g.drawImage(v, 0, 0, 64, 64); return topBottom(g, 64, 64); }
for (const [k, f] of [['h264', 'h264.mp4'], ['hevc', 'hevc.mp4'], ['vp9', 'vp9.webm']]) check('media.video.' + k, async () => {
	const v = await playVideo('/media/' + f); const p = videoPixels(v);
	const r = [v.videoWidth, v.videoHeight, Math.round(v.duration)]; ok(colorIs(p[0], RED, 60) && colorIs(p[1], BLUE, 60), 'frame pixels ' + JSON.stringify(p) + ' ' + JSON.stringify(r));
	return eq(r, [320, 240, 2]);
}, { timeout: 15000 });
check('media.video.requestVideoFrameCallback', async () => { const v = await playVideo('/media/h264.mp4'); need(v.requestVideoFrameCallback, 'no requestVideoFrameCallback'); const m = await withTimeout(new Promise(r => v.requestVideoFrameCallback((n, md) => r(md))), 3000); return eq([m.width, m.height], [320, 240]); }, { timeout: 15000 });
for (const [k, f] of [['aac', 'aac.m4a'], ['mp3', 'mp3.mp3'], ['opus-webm', 'opus.webm'], ['opus-mp4', 'opus.mp4'], ['flac', 'flac.flac'], ['wav', 'pcm.wav']]) check('media.audio.' + k, async () => {
	const a = el('audio', { muted: true, src: '/media/' + f }); await once(a, 'loadedmetadata', 8000, 'error'); await a.play();
	const t0 = performance.now(); while (a.currentTime < 0.5 && performance.now() - t0 < 8000 && alive()) await sleep(100);
	ok(a.currentTime >= 0.5, 'currentTime stuck at ' + a.currentTime); a.pause(); return Math.round(a.duration);
}, { timeout: 15000 });
check('media.audio.unmuted-with-gesture', async () => {
	const a = new Audio('/media/aac.m4a'); document.getElementById('stage').appendChild(a); await a.play();
	const t0 = performance.now(); while (a.currentTime < 0.5 && performance.now() - t0 < 6000 && alive()) await sleep(100);
	ok(a.currentTime >= 0.5, 'currentTime stuck at ' + a.currentTime); a.pause(); return true;
}, { gesture: true, timeout: 12000 });
check('media.canPlayType', () => {
	const v = document.createElement('video'), r = {};
	for (const t of ['video/mp4; codecs="avc1.4d401e"', 'video/mp4; codecs="hvc1.1.6.L93.B0"', 'video/mp4; codecs="av01.0.05M.08"', 'video/webm; codecs="vp9"', 'video/webm; codecs="vp8"', 'audio/mp4; codecs="mp4a.40.2"', 'audio/mpeg', 'audio/ogg; codecs="opus"', 'audio/webm; codecs="opus"', 'audio/flac', 'application/vnd.apple.mpegurl', 'video/mp4; codecs="dvh1.05.01"'])
		r[t] = v.canPlayType(t);
	ok(r['video/mp4; codecs="avc1.4d401e"'] === 'probably', 'no H.264'); return r;
});
check('media.mediacapabilities', async () => { need(navigator.mediaCapabilities, 'no mediaCapabilities'); const r = await navigator.mediaCapabilities.decodingInfo({ type: 'file', video: { contentType: 'video/mp4; codecs="hvc1.1.6.L93.B0"', width: 1920, height: 1080, bitrate: 5e6, framerate: 30 } }); return { supported: r.supported, smooth: r.smooth, powerEfficient: r.powerEfficient }; });
async function mseLoad(MS) {
	const ms = new MS(); const v = el('video', { muted: true, playsInline: true }); if (MS.name === 'ManagedMediaSource') v.disableRemotePlayback = true;
	v.src = URL.createObjectURL(ms); await once(ms, 'sourceopen', 8000);
	const add = async (type, url) => { const sb = ms.addSourceBuffer(type); sb.appendBuffer(await (await fetch(url)).arrayBuffer()); await once(sb, 'updateend', 8000, 'error'); return sb; };
	const vb = await add('video/mp4; codecs="avc1.4d401e"', '/media/frag-v.mp4'); await add('audio/mp4; codecs="mp4a.40.2"', '/media/frag-a.m4a');
	ms.endOfStream(); await v.play(); const t0 = performance.now(); while (v.currentTime < 0.6 && performance.now() - t0 < 8000 && alive()) await sleep(100);
	ok(v.currentTime >= 0.6, 'currentTime ' + v.currentTime); return [vb.buffered.length, Math.round(vb.buffered.end(0)), videoPixels(v)];
}
check('media.mse.managed-media-source', async () => { need(self.ManagedMediaSource, 'no ManagedMediaSource'); const r = await mseLoad(ManagedMediaSource); ok(colorIs(r[2][0], RED, 60), 'pixels ' + JSON.stringify(r[2])); return eq(r.slice(0, 2), [1, 2]); }, { timeout: 20000 });
check('media.mse.media-source', async () => { need(self.MediaSource, 'no MediaSource'); const r = await mseLoad(MediaSource); return eq(r.slice(0, 2), [1, 2]); }, { timeout: 20000 });
check('media.hls-native', async () => { const v = el('video'); need(v.canPlayType('application/vnd.apple.mpegurl'), 'no native HLS'); v.remove(); const p = await playVideo('/media/hls/index.m3u8'); const px2 = videoPixels(p); ok(colorIs(px2[0], RED, 60), 'pixels ' + JSON.stringify(px2)); return Math.round(p.duration); }, { timeout: 20000 });

// Web Audio
check('media.webaudio.offline-render', async () => {
	const ctx = new OfflineAudioContext(1, 4800, 48000); const o = ctx.createOscillator(), g = ctx.createGain(); o.type = 'square'; o.frequency.value = 1000; g.gain.value = 0.5;
	const bq = ctx.createBiquadFilter(); bq.type = 'allpass'; o.connect(g).connect(bq).connect(ctx.destination); o.start();
	const buf = await ctx.startRendering(); const d = buf.getChannelData(0); let mx = 0, zc = 0; for (let i = 1; i < d.length; i++) { mx = Math.max(mx, Math.abs(d[i])); if ((d[i - 1] < 0) !== (d[i] < 0)) zc++; }
	near(mx, 0.75, 0.35, 'peak');   // a band-limited square through an allpass overshoots (Gibbs): ~1.03 on Apple's near(zc, 200, 30, 'zero crossings'); return { peak: +mx.toFixed(3), zc };
});
for (const [k, f] of [['aac', 'aac.m4a'], ['mp3', 'mp3.mp3'], ['opus', 'opus.webm'], ['flac', 'flac.flac'], ['wav', 'pcm.wav']]) check('media.webaudio.decode.' + k, async () => {
	const ctx = new OfflineAudioContext(1, 48000, 48000); const b = await ctx.decodeAudioData(await (await fetch('/media/' + f)).arrayBuffer());
	const d = b.getChannelData(0); let zc = 0; const s = Math.floor(b.sampleRate * 0.5); for (let i = s + 1; i < s + b.sampleRate * 0.5; i++) if ((d[i - 1] < 0) !== (d[i] < 0)) zc++;
	near(b.duration, 2, 0.15, 'duration'); near(zc, 440, 20, 'zero crossings in 0.5 s (440 Hz)'); return [b.sampleRate, b.numberOfChannels];
});
check('media.webaudio.audioworklet', async () => {
	const ctx = new OfflineAudioContext(1, 1280, 48000); need(ctx.audioWorklet, 'no audioWorklet'); await ctx.audioWorklet.addModule('/res/worklet.js');
	const n = new AudioWorkletNode(ctx, 'webcov-const'); n.connect(ctx.destination); const b = await ctx.startRendering(); return eq(b.getChannelData(0)[640], 0.25);
});
check('media.webaudio.analyser-convolver-panner', async () => {
	const ctx = new OfflineAudioContext(2, 4800, 48000); const o = ctx.createOscillator(); const p = ctx.createPanner(); p.positionX.value = 1; const comp = ctx.createDynamicsCompressor();
	const ir = ctx.createBuffer(2, 128, 48000); ir.getChannelData(0)[0] = 1; ir.getChannelData(1)[0] = 1; const cv = ctx.createConvolver(); cv.buffer = ir;
	o.connect(p).connect(cv).connect(comp).connect(ctx.destination); o.start(); const b = await ctx.startRendering();
	const e = ch => b.getChannelData(ch).reduce((a, x) => a + x * x, 0); ok(e(1) > e(0) * 2, 'panned right: ' + e(0) + ' / ' + e(1)); return true;
});
check('media.webaudio.realtime-running', async () => {
	const ctx = new AudioContext(); await ctx.resume(); const o = ctx.createOscillator(); const g = ctx.createGain(); g.gain.value = 0.05; o.connect(g).connect(ctx.destination); o.start();
	const t = ctx.currentTime; await sleep(600); const dt = ctx.currentTime - t; o.stop(); const r = { state: ctx.state, advanced: +dt.toFixed(2), rate: ctx.sampleRate, latency: ctx.baseLatency }; await ctx.close();
	ok(r.state === 'running' && dt > 0.3, JSON.stringify(r)); return r;
}, { gesture: true, timeout: 8000 });
check('media.webaudio.media-element-source', async () => {
	const ctx = new AudioContext(); const a = new Audio('/media/aac.m4a'); a.crossOrigin = 'anonymous'; const src = ctx.createMediaElementSource(a); const an = ctx.createAnalyser(); src.connect(an).connect(ctx.destination);
	await ctx.resume(); await a.play(); await sleep(700); const d = new Float32Array(an.fftSize); an.getFloatTimeDomainData(d); a.pause(); await ctx.close();
	const peak = Math.max(...d.map(Math.abs)); ok(peak > 0.05, 'analyser peak ' + peak); return +peak.toFixed(2);
}, { gesture: true, timeout: 8000 });

// WebCodecs
function frameCanvas(i) { const c = new OffscreenCanvas(320, 240), g = c.getContext('2d'); g.fillStyle = '#f00'; g.fillRect(0, 0, 320, 120); g.fillStyle = '#00f'; g.fillRect(0, 120, 320, 120); g.fillStyle = '#fff'; g.fillRect(i * 10, 0, 10, 10); return c; }
async function encodeDecode(codec) {
	need(self.VideoEncoder, 'no VideoEncoder'); const cfg = { codec, width: 320, height: 240, bitrate: 500000, framerate: 30 };
	const s = await VideoEncoder.isConfigSupported(cfg); if (!s.supported) throw new Unsupported('encoder config ' + codec + ' not supported');
	const chunks = []; let desc = null; let err = null;
	const enc = new VideoEncoder({ output: (ch, md) => { chunks.push(ch); if (md && md.decoderConfig && md.decoderConfig.description) desc = md.decoderConfig.description; }, error: e => err = e });
	enc.configure(cfg); for (let i = 0; i < 10; i++) { const f = new VideoFrame(frameCanvas(i), { timestamp: i * 33333 }); enc.encode(f, { keyFrame: i === 0 }); f.close(); }
	await enc.flush(); if (err) throw err; ok(chunks.length === 10, chunks.length + ' chunks'); ok(chunks[0].type === 'key', 'first not key');
	const frames = []; const dec = new VideoDecoder({ output: f => { frames.push(f); }, error: e => err = e });
	dec.configure(Object.assign({ codec, codedWidth: 320, codedHeight: 240 }, desc ? { description: desc } : {}));
	for (const ch of chunks) dec.decode(ch); await dec.flush(); if (err) throw err;
	const c = document.createElement('canvas'); c.width = 64; c.height = 64; const g = c.getContext('2d'); g.drawImage(frames[5], 0, 0, 64, 64); const p = topBottom(g, 64, 64); frames.forEach(f => f.close());
	ok(colorIs(p[0], RED, 60) && colorIs(p[1], BLUE, 60), 'decoded pixels ' + JSON.stringify(p)); return { chunks: chunks.length, frames: frames.length, bytes: chunks.reduce((a, c) => a + c.byteLength, 0) };
}
check('media.webcodecs.h264', () => encodeDecode('avc1.42001f'), { timeout: 20000 });
check('media.webcodecs.hevc', () => encodeDecode('hvc1.1.6.L93.B0'), { timeout: 20000 });
check('media.webcodecs.vp9', () => encodeDecode('vp09.00.10.08'), { timeout: 20000 });
check('media.webcodecs.av1', () => encodeDecode('av01.0.04M.08'), { timeout: 20000 });
check('media.webcodecs.audio-encode', async () => {
	need(self.AudioEncoder, 'no AudioEncoder'); const r = {};
	for (const codec of ['opus', 'mp4a.40.2']) {
		const cfg = { codec, sampleRate: 48000, numberOfChannels: 1, bitrate: 64000 }; if (!(await AudioEncoder.isConfigSupported(cfg)).supported) { r[codec] = 'unsupported'; continue; }
		let n = 0, err = null; const e = new AudioEncoder({ output: () => n++, error: x => err = x }); e.configure(cfg);
		const d = new Float32Array(48000).map((_, i) => Math.sin(i / 48000 * 2 * Math.PI * 440) * 0.5);
		e.encode(new AudioData({ format: 'f32', sampleRate: 48000, numberOfFrames: 48000, numberOfChannels: 1, timestamp: 0, data: d })); await e.flush(); if (err) throw err; r[codec] = n;
	}
	ok(Object.values(r).some(x => x > 10), JSON.stringify(r)); return r;
}, { timeout: 15000 });
check('media.webcodecs.image-decoder', async () => { need(self.ImageDecoder, 'no ImageDecoder'); const d = new ImageDecoder({ data: (await fetch('/media/img.png')).body, type: 'image/png' }); const { image } = await d.decode(); return eq([image.displayWidth, image.displayHeight], [64, 64]); });

check('media.mediarecorder-canvas', async () => {
	need(self.MediaRecorder, 'no MediaRecorder'); const c = el('canvas'); c.width = 160; c.height = 120; const g = c.getContext('2d'); const st = c.captureStream(30);
	const mr = new MediaRecorder(st); const parts = []; mr.ondataavailable = e => e.data.size && parts.push(e.data); mr.start();
	for (let i = 0; i < 30; i++) { g.fillStyle = i % 2 ? '#f00' : '#00f'; g.fillRect(0, 0, 160, 120); await frames(1); }
	const stopped = once(mr, 'stop', 5000); mr.stop(); await stopped; const b = new Blob(parts, { type: mr.mimeType }); ok(b.size > 1000, 'recorded ' + b.size + ' bytes'); return mr.mimeType.split(';')[0];
}, { timeout: 15000 });
check('media.picture-in-picture', async () => {
	const v = await playVideo('/media/h264.mp4');
	if (document.pictureInPictureEnabled && v.requestPictureInPicture) { await v.requestPictureInPicture(); const inPip = !!document.pictureInPictureElement; await document.exitPictureInPicture(); return eq(inPip, true); }
	need(v.webkitSupportsPresentationMode && v.webkitSupportsPresentationMode('picture-in-picture'), 'no PiP'); v.webkitSetPresentationMode('picture-in-picture'); await sleep(1500); const m = v.webkitPresentationMode; v.webkitSetPresentationMode('inline'); return eq(m, 'picture-in-picture');
}, { gesture: true, timeout: 20000 });
check('media.fullscreen-video', async () => { const v = await playVideo('/media/h264.mp4'); need(v.webkitEnterFullscreen, 'no webkitEnterFullscreen'); v.webkitEnterFullscreen(); await sleep(1500); const f = v.webkitDisplayingFullscreen; v.webkitExitFullscreen(); return eq(f, true); }, { gesture: true, timeout: 20000 });
check('media.enumerate-devices', async () => { need(navigator.mediaDevices, 'no mediaDevices'); const d = await navigator.mediaDevices.enumerateDevices(); return d.map(x => x.kind).sort(); });
check('media.getusermedia-camera', async () => {
	need(navigator.mediaDevices && navigator.mediaDevices.getUserMedia, 'no getUserMedia'); const s = await navigator.mediaDevices.getUserMedia({ video: true });
	const t = s.getVideoTracks()[0]; const v = el('video', { muted: true, playsInline: true, srcObject: s }); await v.play(); await sleep(800); const r = { label: t.label, w: v.videoWidth, h: v.videoHeight, settings: t.getSettings().frameRate }; s.getTracks().forEach(x => x.stop());
	ok(r.w > 0, JSON.stringify(r)); return r;
}, { gesture: true, timeout: 30000 });
check('media.getusermedia-mic', async () => {
	need(navigator.mediaDevices && navigator.mediaDevices.getUserMedia, 'no getUserMedia'); const s = await navigator.mediaDevices.getUserMedia({ audio: true }); const t = s.getAudioTracks()[0];
	const ctx = new AudioContext(); const an = ctx.createAnalyser(); ctx.createMediaStreamSource(s).connect(an); await ctx.resume(); await sleep(800);
	const d = new Float32Array(an.fftSize); an.getFloatTimeDomainData(d); const r = { label: t.label, state: t.readyState, peak: +Math.max(...d.map(Math.abs)).toFixed(3) }; s.getTracks().forEach(x => x.stop()); await ctx.close(); return r;
}, { gesture: true, timeout: 30000 });
check('media.getdisplaymedia', () => { need(navigator.mediaDevices && navigator.mediaDevices.getDisplayMedia, 'no getDisplayMedia'); return true; });
check('media.eme-fairplay', async () => { need(navigator.requestMediaKeySystemAccess, 'no EME'); try { const a = await navigator.requestMediaKeySystemAccess('com.apple.fps', [{ initDataTypes: ['sinf', 'skd'], videoCapabilities: [{ contentType: 'video/mp4; codecs="avc1.42E01E"' }] }]); return a.keySystem; } catch (e) { throw new Unsupported('com.apple.fps: ' + e.name); } });
check('media.eme-clearkey', async () => { need(navigator.requestMediaKeySystemAccess, 'no EME'); const a = await navigator.requestMediaKeySystemAccess('org.w3.clearkey', [{ initDataTypes: ['keyids'], videoCapabilities: [{ contentType: 'video/mp4; codecs="avc1.42E01E"' }] }]); const mk = await a.createMediaKeys(); return !!mk; });
check('media.media-session', () => { need(navigator.mediaSession, 'no mediaSession'); navigator.mediaSession.metadata = new MediaMetadata({ title: 'webcov', artist: 'x' }); navigator.mediaSession.setActionHandler('play', () => {}); return eq(navigator.mediaSession.metadata.title, 'webcov'); });
check('media.remote-playback-airplay', () => { const v = document.createElement('video'); return { remote: !!v.remote, airplay: typeof v.webkitShowPlaybackTargetPicker === 'function', availability: typeof window.WebKitPlaybackTargetAvailabilityEvent === 'function' }; });
