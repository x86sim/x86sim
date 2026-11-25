# webcov: web platform coverage, x86Sim vs Apple's simulator

What web-platform features work in Safari on an x86Sim iPhone, compared with Safari on Apple's own simulator
(Xcode 27, iOS 27.2 24B8117, iPhone 17 Pro on both sides; Safari 27.2, the same WebKit). 261 checks in
[checks/](checks/), each with expected values (pixels read back, decoded audio's zero crossings, round-tripped
data), not just "the API exists". Machine-readable: [coverage.json](coverage.json).

Run 2026-11-24: x86Sim `r3w-iphone` (one clean full run), Apple: three runs on the Mac mini (a full one, then
`device` / `media.getusermedia` / `workers` and the gesture checks again; per check the best Apple result counts, so
an Apple-side flake is not credited to x86Sim).

## Summary

| class | count | meaning |
|---|---|---|
| same | 223 | same status on both (PASS, or the same UNSUPPORTED / FAIL) |
| gap | 9 | Apple PASS, x86Sim not: **all one cause** (below) |
| apple-also-fails | 3 | neither passes, differently (MP3 / FLAC `<audio>` stall at 0 s on Apple; PiP) |
| better | 1 | `workers.shared` passes on x86Sim; timed out in both Apple runs that reached it |
| unverified | 9 | permission / gesture checks Apple's side never answered (the Mac runner's taps don't reach Safari's website prompts) |

Perf (x86Sim / Apple, higher = faster; geometric mean **0.37**): rendering is at par (canvas 2D draw 0.99,
requestAnimationFrame 1.00, WebGL draw 1.09, WebAssembly loop 1.12); DOM and JS run at 0.1-0.5 (layout loop 0.19,
query + style 0.50, JS numeric 0.14, float math 0.10, typed arrays 0.08, JSON 0.23, regex 0.32): JavaScriptCore's
JIT output is arm64 code that the translator runs on x86-64, while the WebAssembly loop and the GPU paths are not
bound by it. Single runs; host load moves them ±10%.

Since this run (2026-11-24) JavaScriptCore has its FTL tier again (the translator had set `JSC_useFTLJIT=false` for
every guest, so hot JS stayed in DFG code): means of 4 interleaved boots each, FTL off -> on, as ratio to Apple: JS
numeric 0.15 -> 0.41, typed arrays 0.08 -> 0.21, closures 0.52 -> 0.56, the rest unchanged (geometric mean of all 16:
0.39 -> 0.44). Float math stays at 0.10: ~86% of it is `sin` / `log` in libsystem_m, translated arm64 code.

### The one x86Sim gap: media elements after the first

Every gap is a `<video>` / `<audio>` element (H.264 / HEVC / AAC / WAV / Opus-in-MP4, native HLS, full screen,
requestVideoFrameCallback, MediaElementSource) that never reaches `loadedmetadata`. Only the **first**
AVFoundation-backed load in a WebKit GPU process works (`media.video.h264` passes because it is the first in the
suite; in every short run the first load played and the next one hung); VP9 WebM (WebKit's own parser), MSE / ManagedMediaSource,
WebCodecs and `decodeAudioData` are unaffected. Layer: **Darling** (system calls), seen in the media stack.

Evidence: `bt all` of `com.apple.WebKit.GPU` during the hang (`x86sim debug <phone> <pid> --batch -o "bt all"`):
the first asset's thread sits in `FigAIOIssueWrite` called by `crabsRebalanceWorkingSet` (MediaToolbox's CRABS
byte cache writing its backing store with POSIX AIO), the next asset in `crabsEnsureInitialized`, the ones after
it in `FigByteStreamFactoryCreateStreamFromURLWithOptions`. The phone's `private/var/run/x86sim-unsupported.log`
has `com.apple.WebKi … syscall 319` (`aio_write`): Darling implements none of the AIO system calls (314-320:
`aio_return`, `aio_suspend`, `aio_cancel`, `aio_error`, `aio_read`, `aio_write`, `lio_listio` -> ENOSYS), so the
write never completes and (as the stacks show) the cache never lets the next asset initialize.

Fix (not made this round: Darling's `libsystem_kernel` is installed into the shared prefix by every boot):
synchronous AIO in Darling's BSD syscall table (`bsd_syscall_table.c` 314-320): `aio_read` / `aio_write` do the
`pread` / `pwrite` at submission and keep the result per aiocb, `aio_error` answers 0 / the errno, `aio_return`
the byte count (once), `aio_suspend` returns at once (every request is complete), `aio_cancel` answers
`AIO_ALLDONE`, `lio_listio` runs the list; `SIGEV_SIGNAL` completion raises the signal after the I/O. This is
also the "video playback in Safari is broken" report: the first video of a Safari session plays, the rest don't.

### Notes on other results (same on both)

- Not in Safari 27.2 on either: WebGPU adapter (none in the simulator), `ctx.filter`, `requestIdleCallback`,
  `scheduler.yield`, Temporal, `using` declarations, WebP canvas encoding, ImageDecoder, AV1 encoding,
  `MediaSource` in an iPhone tab (ManagedMediaSource works), request-body streams, WebSocketStream, paint
  worklets, file pickers, Battery, Vibration, Push in a tab, Notifications in a tab (home-screen web apps only).
- `media.getusermedia-camera`: OverconstrainedError on both (the simulator has no camera).
- Apple Pay on `http://127.0.0.1`: SecurityError on both (needs https).
- Apple's side is flaky in `workers.service-worker.*` (the page died at `install` in both full Apple runs, passed in
  the third) and `workers.atomics-cross-thread` / speech voices (one run each); x86Sim passed them in every run.
- Prompts x86Sim showed and the runner allowed: microphone (`media.getusermedia-mic`), location
  (`device.geolocation`; remembered for the origin after the first run), motion (`device.orientation-motion`), the
  clipboard's Paste callout. Location comes from `x86sim location set` (Apple Park), as `simctl location`.

## Running it

```sh
tests/web/webcov/run.sh PHONE                       # Safari on a running x86Sim phone; results + report under $X86SIM_SCRATCH/web
tests/web/webcov/run.sh PHONE --only media,dom.has  # check id prefixes (--skip too; --nogesture)
tests/web/webcov/run.sh --ref-mac ios --prompts X86.jsonl   # Apple's simulator on $X86SIM_MAC (temporary device)
tests/web/webcov/report.py X86.jsonl --ref APPLE.jsonl [--ref APPLE2.jsonl] --md OUT.md --json OUT.json --notes notes.json
```

`serve.py` (stdlib Python, also the Mac's 3.9) serves the suite on `127.0.0.1` (a secure context; COOP / COEP for
SharedArrayBuffer; WebSocket echo, EventSource, CORS from `localhost`) and collects one JSON event per check. The
harness gives every check a hard time limit (`alive()` stops its loops when it is cut off) and fixed-size
allocations; a check whose page died is recorded as CRASH and the run goes on after it. Media fixtures are made by
`gen-media.sh` (ffmpeg, every source with a duration, each run capped at 120 s / 3 GB). Gesture checks run from a
full-page "tap" button the runner taps (x86Sim: by its element in `x86sim snapshot`). Never run the suite in a host
browser outside a memory-capped scope (`systemd-run --user --scope -p MemoryMax=3G`).

## Results

| | count |
|---|---|
| same | 223 |
| gap | 9 |
| apple-also-fails | 3 |
| better | 1 |
| unverified | 9 |

### Gaps (Apple PASS, x86Sim not)

| check | x86Sim | detail | diagnosis |
|---|---|---|---|
| `media.audio.aac` | TIMEOUT | Error: timeout: loadedmetadata | as media.video.hevc (AIO) |
| `media.audio.opus-mp4` | TIMEOUT | Error: timeout: loadedmetadata | as media.video.hevc (AIO) |
| `media.audio.unmuted-with-gesture` | TIMEOUT | Error: timeout: 12000 ms | as media.video.hevc (AIO) |
| `media.audio.wav` | TIMEOUT | Error: timeout: loadedmetadata | as media.video.hevc (AIO) |
| `media.fullscreen-video` | TIMEOUT | Error: timeout: loadedmetadata | as media.video.hevc (AIO) |
| `media.hls-native` | TIMEOUT | Error: timeout: loadedmetadata | as media.video.hevc (AIO) |
| `media.video.hevc` | TIMEOUT | Error: timeout: loadedmetadata | media stack / Darling: only the first AVFoundation-backed media load of a GPU-process lifetime completes. The GPU process's CRABS byte cache (MediaToolbox crabsRebalanceWorkingSet) issues aio_write; Darling has no AIO syscalls (314-320: aio_return / aio_suspend / aio_cancel / aio_error / aio_read / aio_write / lio_listio -> ENOSYS, x86sim-unsupported.log: "com.apple.WebKi … syscall 319"), the cac… |
| `media.video.requestVideoFrameCallback` | TIMEOUT | Error: timeout: loadedmetadata | as media.video.hevc (AIO: the second AVFoundation load hangs) |
| `media.webaudio.media-element-source` | TIMEOUT | Error: timeout: 8000 ms | as media.video.hevc (AIO): the <audio> element never loads |

### Performance (higher is better; ratio = x86Sim / Apple)

| benchmark | x86Sim | Apple | ratio |
|---|---|---|---|
| `perf.canvas2d.draw-fps` | 58.81 | 59.39 | 0.99 |
| `perf.canvas2d.getImageData` | 350.31 | 683.54 | 0.51 |
| `perf.crypto.sha256-1mb` | 690.78 | 1892.44 | 0.37 |
| `perf.dom.layout-loop` | 801.15 | 4276.38 | 0.19 |
| `perf.dom.query-style` | 63362.98 | 126833.33 | 0.5 |
| `perf.js.array-sort` | 174.58 | 548.07 | 0.32 |
| `perf.js.closures-calls` | 1367.1 | 2844.02 | 0.48 |
| `perf.js.float-math` | 652.54 | 6758.31 | 0.1 |
| `perf.js.json` | 1608.84 | 7116.1 | 0.23 |
| `perf.js.numeric-loop` | 4145.94 | 29924.27 | 0.14 |
| `perf.js.object-alloc` | 2001.97 | 5386.09 | 0.37 |
| `perf.js.string-regex` | 1321.21 | 4176.44 | 0.32 |
| `perf.js.typed-array` | 2065.03 | 24503.67 | 0.08 |
| `perf.raf-fps-idle` | 60.66 | 60.74 | 1.0 |
| `perf.wasm.loop` | 1158.9 | 1037.83 | 1.12 |
| `perf.webgl.draw-fps` | 53.16 | 48.93 | 1.09 |
| geometric mean | | | **0.37** |

### All checks

| check | x86Sim | Apple | class | x86Sim detail | Apple detail |
|---|---|---|---|---|---|
| `device.battery` | UNSUPPORTED | UNSUPPORTED | same | Error: no getBattery | Error: no getBattery |
| `device.clipboard-item` | PASS | PASS | same | ["text/plain", "image/png", "text/html"] | ["text/plain", "image/png", "text/html"] |
| `device.clipboard-text` (tap) | PASS | TIMEOUT | unverified | {"written": true, "readBack": "webcov-clip"} | Error: timeout: 10000 ms |
| `device.gamepad` | PASS | PASS | same | {"pads": 0} | {"pads": 0} |
| `device.geolocation` (tap) | PASS | TIMEOUT | unverified | {"lat": 37.3349, "lon": -122.009, "acc": 5} | Error: timeout: 30000 ms |
| `device.geolocation-permission-state` | PASS | PASS | same | "prompt" | "prompt" |
| `device.idle-detection-contacts` | PASS | PASS | same | {"idle": false, "contacts": false, "badge": false} | {"idle": false, "contacts": false, "badge": false} |
| `device.navigator-facts` | PASS | PASS | same | {"platform": "iPhone", "maxTouchPoints": 5, "hw": 8, "mem": null, "standalone": false, "c… | {"platform": "iPhone", "maxTouchPoints": 5, "hw": 8, "mem": null, "standalone": false, "c… |
| `device.notifications` (tap) | UNSUPPORTED | TIMEOUT | unverified | Error: no Notification (Safari tab: only home-screen web apps have it) | Error: timeout: 20000 ms |
| `device.orientation-motion` (tap) | PASS | TIMEOUT | unverified | {"requestPermission": true, "motionEvent": false, "interval": null} | Error: timeout: 15000 ms |
| `device.payment-request-applepay` | PASS | PASS | same | {"applePaySession": "InvalidAccessError", "canMakePayment": "SecurityError"} | {"applePaySession": "InvalidAccessError", "canMakePayment": "SecurityError"} |
| `device.permissions-query` | PASS | PASS | same | {"geolocation": "prompt", "camera": "prompt", "microphone": "prompt", "notifications": "p… | {"geolocation": "prompt", "camera": "prompt", "microphone": "prompt", "notifications": "p… |
| `device.push-manager` | UNSUPPORTED | UNSUPPORTED | same | Error: no PushManager | Error: no PushManager |
| `device.screen-metrics` | PASS | PASS | same | {"w": 402, "h": 874, "dpr": 3, "inner": [402, 714], "colorGamut": "p3", "hdr": false, "po… | {"w": 402, "h": 874, "dpr": 3, "inner": [402, 714], "colorGamut": "p3", "hdr": false, "po… |
| `device.screen-orientation` | PASS | PASS | same | {"type": "portrait-primary", "angle": 0, "lock": "undefined"} | {"type": "portrait-primary", "angle": 0, "lock": "undefined"} |
| `device.speech-recognition` | PASS | PASS | same | {"continuous": true, "onDevice": false} | {"continuous": true, "onDevice": false} |
| `device.speech-synthesis-speak` (tap) | PASS | TIMEOUT | unverified |  | Error: timeout: 25000 ms |
| `device.speech-synthesis-voices` | PASS | PASS | same (values differ) | {"voices": 69, "en": 26, "def": "Samantha"} | {"voices": 68, "en": 25, "def": "Samantha"} |
| `device.vibrate` | UNSUPPORTED | UNSUPPORTED | same | Error: no navigator.vibrate | Error: no navigator.vibrate |
| `device.wake-lock` (tap) | PASS | TIMEOUT | unverified | ["screen", false] | Error: timeout: 10000 ms |
| `device.web-nfc-bluetooth-usb-serial-hid` | PASS | PASS | same | {"nfc": false, "bluetooth": false, "usb": false, "serial": false, "hid": false} | {"nfc": false, "bluetooth": false, "usb": false, "serial": false, "hid": false} |
| `device.web-share` (tap) | PASS | TIMEOUT | unverified | {"canShare": true, "sheet": "opened"} | Error: timeout: 10000 ms |
| `device.webauthn-create` (tap) | PASS | TIMEOUT | unverified | "SecurityError" | Error: timeout: 20000 ms |
| `device.webauthn-platform` | PASS | PASS | same | {"uv": false, "conditional": false, "caps": 12} | {"uv": false, "conditional": false, "caps": 12} |
| `dom.anchor-positioning` | PASS | PASS | same | [0, 0] | [0, 0] |
| `dom.at-property` | PASS | PASS | same | 7 | 7 |
| `dom.cascade-layers-scope` | PASS | PASS | same | [10, 25, true] | [10, 25, true] |
| `dom.container-queries` | PASS | PASS | same | [50, 10] | [50, 10] |
| `dom.container-style-queries` | PASS | PASS | same | 40 | 40 |
| `dom.content-visibility-details-name` | PASS | PASS | same | [true, false] | [true, false] |
| `dom.css-nesting` | PASS | PASS | same | [20, 33] | [20, 33] |
| `dom.css-typed-om` | PASS | PASS | same | [33, 3] | [33, 3] |
| `dom.custom-elements-shadow` | PASS | PASS | same | ["open", 42, 1] | ["open", 42, 1] |
| `dom.declarative-shadow-dom` | PASS | PASS | same |  |  |
| `dom.dialog` | PASS | PASS | same | [true, true, "ok"] | [true, true, "ok"] |
| `dom.has-selector` | PASS | PASS | same | [70, 10, 1] | [70, 10, 1] |
| `dom.iframe-srcdoc-postmessage` | PASS | PASS | same | 7 | 7 |
| `dom.intersection-observer` | PASS | PASS | same | [true, 1] | [true, 1] |
| `dom.mutation-observer` | PASS | PASS | same | 1 | 1 |
| `dom.navigation-api` | PASS | PASS | same | "function" | "function" |
| `dom.popover` | PASS | PASS | same | [true, false] | [true, false] |
| `dom.requestIdleCallback` | UNSUPPORTED | UNSUPPORTED | same | Error: no requestIdleCallback | Error: no requestIdleCallback |
| `dom.resize-observer` | PASS | PASS | same | [50, 80] | [50, 80] |
| `dom.scheduler-yield` | UNSUPPORTED | UNSUPPORTED | same | Error: no scheduler | Error: no scheduler |
| `dom.scroll-driven-animation` | PASS | PASS | same | 50 | 50 |
| `dom.selection-range-editing` | PASS | PASS | same | ["hello", true, "string"] | ["hello", true, "string"] |
| `dom.subgrid` | PASS | PASS | same | 70 | 70 |
| `dom.text-features` | PASS | PASS | same | {"lines": 4, "balance": true, "pretty": true} | {"lines": 4, "balance": true, "pretty": true} |
| `dom.view-transitions` | PASS | PASS | same | ["b", "function"] | ["b", "function"] |
| `dom.web-animations` | PASS | PASS | same | [50, 100, true] | [50, 100, true] |
| `graphics.canvas-encode.jpeg` | PASS | PASS | same |  |  |
| `graphics.canvas-encode.png` | PASS | PASS | same |  |  |
| `graphics.canvas-encode.webp` | UNSUPPORTED | UNSUPPORTED | same | Error: encoded as image/png | Error: encoded as image/png |
| `graphics.canvas2d.cjk-arabic-text` | PASS | PASS | same | ["cjk", "arabic", "devanagari", "thai"] | ["cjk", "arabic", "devanagari", "thai"] |
| `graphics.canvas2d.composite-blend` | PASS | PASS | same | [100, 50, 25, 255] | [100, 50, 25, 255] |
| `graphics.canvas2d.emoji` | PASS | PASS | same |  |  |
| `graphics.canvas2d.filter-shadow` | UNSUPPORTED | UNSUPPORTED | same | Error: no ctx.filter | Error: no ctx.filter |
| `graphics.canvas2d.getImageData-big` | PASS | PASS | same | [0, 255, 0, 255] | [0, 255, 0, 255] |
| `graphics.canvas2d.p3-colorspace` | PASS | PASS | same | ["display-p3", 255] | ["display-p3", 255] |
| `graphics.canvas2d.shapes` | PASS | PASS | same | [[255, 0, 0, 255], [0, 0, 255, 255], 0, true] | [[255, 0, 0, 255], [0, 0, 255, 255], 0, true] |
| `graphics.canvas2d.text` | PASS | PASS | same | {"width": 68, "ascent": 22, "ink": 609} | {"width": 68, "ascent": 22, "ink": 609} |
| `graphics.createImageBitmap` | PASS | PASS | same | [[255, 0, 0], [0, 0, 255]] | [[255, 0, 0], [0, 0, 255]] |
| `graphics.css-backdrop-filter-computed` | PASS | PASS | same | "blur(5px)" | "blur(5px)" |
| `graphics.css-support` | PASS | PASS | same | 11 | 11 |
| `graphics.fonts.emoji-sequences` | PASS | PASS | same |  |  |
| `graphics.fonts.system-families` | PASS | PASS | same | {"present": 13, "missing": []} | {"present": 13, "missing": []} |
| `graphics.fonts.webfont-css` | PASS | PASS | same |  |  |
| `graphics.fonts.webfont-fontface` | PASS | PASS | same | ["loaded", true, true] | ["loaded", true, true] |
| `graphics.image-decode.avif` | PASS | PASS | same | [64, 64] | [64, 64] |
| `graphics.image-decode.gif` | PASS | PASS | same | [64, 64] | [64, 64] |
| `graphics.image-decode.heic` | PASS | PASS | same | [64, 64] | [64, 64] |
| `graphics.image-decode.jpg` | PASS | PASS | same | [64, 64] | [64, 64] |
| `graphics.image-decode.png` | PASS | PASS | same | [64, 64] | [64, 64] |
| `graphics.image-decode.webp` | PASS | PASS | same | [64, 64] | [64, 64] |
| `graphics.offscreen-canvas` | PASS | PASS | same | [[0, 0, 255, 255], "image/png"] | [[0, 0, 255, 255], "image/png"] |
| `graphics.offscreen-canvas-worker` | PASS | PASS | same | [0, 255, 0] | [0, 255, 0] |
| `graphics.offscreen-webgl2-worker` | PASS | PASS | same | [0, 255, 0, 255] | [0, 255, 0, 255] |
| `graphics.svg-dom-geometry` | PASS | PASS | same | [50, true, true] | [50, true, true] |
| `graphics.svg-raster-filter` | PASS | PASS | same | [[255, 0, 0], [0, 255, 0]] | [[255, 0, 0], [0, 255, 0]] |
| `graphics.webgl1.draw` | PASS | PASS | same | [[255, 0, 0, 255], [0, 0, 255, 255]] | [[255, 0, 0, 255], [0, 0, 255, 255]] |
| `graphics.webgl2.canvas-composited` | PASS | PASS | same | [255, 0, 0] | [255, 0, 0] |
| `graphics.webgl2.draw` | PASS | PASS | same | [[255, 0, 0, 255], [0, 0, 255, 255]] | [[255, 0, 0, 255], [0, 0, 255, 255]] |
| `graphics.webgl2.float-render-texture` | PASS | PASS | same | [0.25, 2.5, -1, 1] | [0.25, 2.5, -1, 1] |
| `graphics.webgl2.info` | PASS | PASS | same | {"renderer": "Apple GPU", "maxTex": 8192, "exts": 26, "samples": 4} | {"renderer": "Apple GPU", "maxTex": 8192, "exts": 26, "samples": 4} |
| `graphics.webgl2.texture-upload-video-image` | PASS | PASS | same | [[255, 0, 0], [0, 0, 255]] | [[255, 0, 0], [0, 0, 255]] |
| `graphics.webgpu.adapter` | UNSUPPORTED | UNSUPPORTED | same | Error: no GPU adapter | Error: no GPU adapter |
| `graphics.webgpu.compute` | UNSUPPORTED | UNSUPPORTED | same | Error: no GPU adapter | Error: no GPU adapter |
| `graphics.webgpu.render-canvas` | UNSUPPORTED | UNSUPPORTED | same | Error: no GPU adapter | Error: no GPU adapter |
| `js.async-generators-await` | PASS | PASS | same | 3 | 3 |
| `js.bigint-math` | PASS | PASS | same | ["10000000000000000000000000", "1", "-3"] | ["10000000000000000000000000", "1", "-3"] |
| `js.cross-origin-isolated` | PASS | PASS | same | [true, true] | [true, true] |
| `js.crypto.aes-gcm` | PASS | PASS | same | [22, "webcov"] | [22, "webcov"] |
| `js.crypto.ecdsa-p256` | PASS | PASS | same |  |  |
| `js.crypto.ed25519-x25519` | PASS | PASS | same | [true, true] | [true, true] |
| `js.crypto.pbkdf2-hkdf` | PASS | PASS | same | "0c60c80f961f0e71f3a9b524af6012062fe037a6" | "0c60c80f961f0e71f3a9b524af6012062fe037a6" |
| `js.crypto.random-uuid` | PASS | PASS | same |  |  |
| `js.crypto.rsa-pss` | PASS | PASS | same |  |  |
| `js.crypto.sha` | PASS | PASS | same | ["ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad", "a9993e364706816aba3… | ["ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad", "a9993e364706816aba3… |
| `js.date-tz` | PASS | PASS | same | {"tz": "America/Denver", "offset": 360} | {"tz": "America/Denver", "offset": 360} |
| `js.es2022.at-hasOwn-cause` | PASS | PASS | same | [3, "b", true, 7] | [3, "b", true, 7] |
| `js.es2022.class-private-static` | PASS | PASS | same | [44, true, false] | [44, true, false] |
| `js.es2022.regexp-d-flag` | PASS | PASS | same | [2, 3] | [2, 3] |
| `js.es2023.change-array-by-copy` | PASS | PASS | same | [[1, 2, 3], [2, 1, 3], [9, 1, 2], [3, 2], [3, 1, 2], 2, -1] | [[1, 2, 3], [2, 1, 3], [9, 1, 2], [3, 2], [3, 1, 2], 2, -1] |
| `js.es2024.array-fromAsync` | PASS | PASS | same | [1, 2] | [1, 2] |
| `js.es2024.arraybuffer-resize-transfer` | PASS | PASS | same | [8, true, true, 2] | [8, true, true, 2] |
| `js.es2024.atomics-waitAsync` | PASS | PASS | same | [true, "ok"] | [true, "ok"] |
| `js.es2024.groupBy` | PASS | PASS | same | [[1, 3], [2, 4], [2, 3]] | [[1, 3], [2, 4], [2, 3]] |
| `js.es2024.promise-withResolvers` | PASS | PASS | same | 5 | 5 |
| `js.es2024.regexp-v-flag` | PASS | PASS | same | [false, true, true] | [false, true, true] |
| `js.es2024.string-wellformed` | PASS | PASS | same | [false, 65533] | [false, 65533] |
| `js.es2025.dynamic-import-module` | PASS | PASS | same | 42 | 42 |
| `js.es2025.float16` | PASS | PASS | same | [1.3369140625, 1.3369140625, 0] | [1.3369140625, 1.3369140625, 0] |
| `js.es2025.iterator-helpers` | PASS | PASS | same | [20, 40, 5] | [20, 40, 5] |
| `js.es2025.json-modules` | PASS | PASS | same | 42 | 42 |
| `js.es2025.promise-try` | PASS | PASS | same | 5 | 5 |
| `js.es2025.regexp-dup-named-groups` | PASS | PASS | same | "2025" | "2025" |
| `js.es2025.regexp-escape` | PASS | PASS | same | "\\x61\\.b\\*c" | "\\x61\\.b\\*c" |
| `js.es2025.regexp-modifiers` | PASS | PASS | same | [true, false] | [true, false] |
| `js.es2025.set-methods` | PASS | PASS | same | [[1, 2, 3, 4], [2, 3], [1], [1, 4], true, true] | [[1, 2, 3, 4], [2, 3], [1], [1, 4], true, true] |
| `js.es2026.array-fromAsync-iterator-sequencing` | PASS | PASS | same | [1, 2, 3] | [1, 2, 3] |
| `js.es2026.error-isError` | PASS | PASS | same | [true, false] | [true, false] |
| `js.es2026.explicit-resource-mgmt` | UNSUPPORTED | UNSUPPORTED | same | Error: syntax: Unexpected identifier 'r' | Error: syntax: Unexpected identifier 'r' |
| `js.es2026.math-sumPrecise` | PASS | PASS | same | 0.1 | 0.1 |
| `js.es2026.temporal` | UNSUPPORTED | UNSUPPORTED | same | Error: no Temporal | Error: no Temporal |
| `js.es2026.uint8array-base64` | PASS | PASS | same | ["aGk=", [104, 105], "ff01"] | ["aGk=", [104, 105], "ff01"] |
| `js.eval-jit-hot-loop` | PASS | PASS | same | 999912 | 999912 |
| `js.intl.collator-plural-relative` | PASS | PASS | same | ["aäz", "azä", "few", "ayer"] | ["aäz", "azä", "few", "ayer"] |
| `js.intl.datetime` | PASS | PASS | same | "Jan 2, 2025 at 3:04 PM" | "Jan 2, 2025 at 3:04 PM" |
| `js.intl.datetime-tzdata` | PASS | PASS | same | "05:30" | "05:30" |
| `js.intl.displaynames-list` | PASS | PASS | same | ["Allemagne", "a, b, and c", "Euro"] | ["Allemagne", "a, b, and c", "Euro"] |
| `js.intl.durationformat` | PASS | PASS | same | "1 hr, 5 min" | "1 hr, 5 min" |
| `js.intl.locale-info` | PASS | PASS | same | [7, true] | [7, true] |
| `js.intl.number` | PASS | PASS | same | ["1.234.567,891", "12,34,567", "¥5,000", "12K"] | ["1.234.567,891", "12,34,567", "¥5,000", "12K"] |
| `js.intl.segmenter` | PASS | PASS | same | [2, true] | [2, true] |
| `js.intl.supported-values` | PASS | PASS | same | {"zones": 445, "currencies": 307, "calendars": 26} | {"zones": 445, "currencies": 307, "calendars": 26} |
| `js.shared-array-buffer` | PASS | PASS | same | [5, 7, 9, "wait"] | [5, 7, 9, "wait"] |
| `js.structuredClone` | PASS | PASS | same | [0, 2, 1, "g", true] | [0, 2, 1, "g", true] |
| `js.textencoder-decoders` | PASS | PASS | same | [[195, 169], "あ", "€", "你"] | [[195, 169], "あ", "€", "你"] |
| `js.url-urlpattern` | PASS | PASS | same | ["1", true, null, "7"] | ["1", true, null, "7"] |
| `js.wasm.exceptions-exnref` | PASS | PASS | same | 7 | 7 |
| `js.wasm.exceptions-legacy` | PASS | PASS | same | 42 | 42 |
| `js.wasm.gc-struct` | PASS | PASS | same | 9 | 9 |
| `js.wasm.i64-bigint` | PASS | PASS | same | "42" | "42" |
| `js.wasm.js-exception-tag` | PASS | PASS | same | [true, 5] | [true, 5] |
| `js.wasm.jspi` | PASS | PASS | same |  |  |
| `js.wasm.memory-grow-big` | PASS | PASS | same | 268435456 | 268435456 |
| `js.wasm.mvp` | PASS | PASS | same | 42 | 42 |
| `js.wasm.simd128` | PASS | PASS | same | 42 | 42 |
| `js.wasm.streaming` | PASS | PASS | same | 3 | 3 |
| `js.wasm.tail-call` | PASS | PASS | same | 3 | 3 |
| `js.wasm.threads-shared-memory` | PASS | PASS | same | [0, 5] | [0, 5] |
| `js.weakref-finalization` | PASS | PASS | same | "object" | "object" |
| `media.audio.aac` | TIMEOUT | PASS | gap | Error: timeout: loadedmetadata | 2 |
| `media.audio.flac` | TIMEOUT | FAIL | apple-also-fails | Error: timeout: loadedmetadata | Error: currentTime stuck at 0.000445292 |
| `media.audio.mp3` | TIMEOUT | FAIL | apple-also-fails | Error: timeout: loadedmetadata | Error: currentTime stuck at 0.00063875 |
| `media.audio.opus-mp4` | TIMEOUT | PASS | gap | Error: timeout: loadedmetadata | 2 |
| `media.audio.opus-webm` | PASS | PASS | same | 2 | 2 |
| `media.audio.unmuted-with-gesture` (tap) | TIMEOUT | PASS | gap | Error: timeout: 12000 ms |  |
| `media.audio.wav` | TIMEOUT | PASS | gap | Error: timeout: loadedmetadata | 2 |
| `media.canPlayType` | PASS | PASS | same | {"video/mp4; codecs=\"avc1.4d401e\"": "probably", "video/mp4; codecs=\"hvc1.1.6.L93.B0\""… | {"video/mp4; codecs=\"avc1.4d401e\"": "probably", "video/mp4; codecs=\"hvc1.1.6.L93.B0\""… |
| `media.eme-clearkey` | PASS | PASS | same |  |  |
| `media.eme-fairplay` | PASS | PASS | same | "com.apple.fps" | "com.apple.fps" |
| `media.enumerate-devices` | PASS | PASS | same | ["audioinput"] | ["audioinput"] |
| `media.fullscreen-video` (tap) | TIMEOUT | PASS | gap | Error: timeout: loadedmetadata |  |
| `media.getdisplaymedia` | PASS | PASS | same |  |  |
| `media.getusermedia-camera` (tap) | FAIL | FAIL | same | OverconstrainedError: Invalid constraint | OverconstrainedError: Invalid constraint |
| `media.getusermedia-mic` (tap) | PASS | TIMEOUT | unverified | {"label": "MicrophoneBuiltIn", "state": "ended", "peak": 0} | Error: timeout: 30000 ms |
| `media.hls-native` | TIMEOUT | PASS | gap | Error: timeout: loadedmetadata | 2 |
| `media.media-session` | PASS | PASS | same | "webcov" | "webcov" |
| `media.mediacapabilities` | PASS | PASS | same | {"supported": true, "smooth": false, "powerEfficient": false} | {"supported": true, "smooth": false, "powerEfficient": false} |
| `media.mediarecorder-canvas` | PASS | PASS | same | "video/mp4" | "video/mp4" |
| `media.mse.managed-media-source` | PASS | PASS | same | [1, 2] | [1, 2] |
| `media.mse.media-source` | UNSUPPORTED | UNSUPPORTED | same | Error: no MediaSource | Error: no MediaSource |
| `media.picture-in-picture` (tap) | TIMEOUT | UNSUPPORTED | apple-also-fails | Error: timeout: loadedmetadata | Error: no PiP |
| `media.remote-playback-airplay` | PASS | PASS | same | {"remote": true, "airplay": true, "availability": true} | {"remote": true, "airplay": true, "availability": true} |
| `media.video.h264` | PASS | PASS | same | [320, 240, 2] | [320, 240, 2] |
| `media.video.hevc` | TIMEOUT | PASS | gap | Error: timeout: loadedmetadata | [320, 240, 2] |
| `media.video.requestVideoFrameCallback` | TIMEOUT | PASS | gap | Error: timeout: loadedmetadata | [320, 240] |
| `media.video.vp9` | PASS | PASS | same | [320, 240, 2] | [320, 240, 2] |
| `media.webaudio.analyser-convolver-panner` | PASS | PASS | same |  |  |
| `media.webaudio.audioworklet` | PASS | PASS | same | 0.25 | 0.25 |
| `media.webaudio.decode.aac` | PASS | PASS | same | [48000, 1] | [48000, 1] |
| `media.webaudio.decode.flac` | PASS | PASS | same | [48000, 1] | [48000, 1] |
| `media.webaudio.decode.mp3` | PASS | PASS | same | [48000, 1] | [48000, 1] |
| `media.webaudio.decode.opus` | PASS | PASS | same | [48000, 1] | [48000, 1] |
| `media.webaudio.decode.wav` | PASS | PASS | same | [48000, 1] | [48000, 1] |
| `media.webaudio.media-element-source` (tap) | TIMEOUT | PASS | gap | Error: timeout: 8000 ms | 0.13 |
| `media.webaudio.offline-render` | PASS | PASS | same |  |  |
| `media.webaudio.realtime-running` (tap) | PASS | PASS | same (values differ) | {"state": "running", "advanced": 0.55, "rate": 48000, "latency": 0.0026666666666666666} | {"state": "running", "advanced": 0.58, "rate": 48000, "latency": 0.0026666666666666666} |
| `media.webcodecs.audio-encode` | PASS | PASS | same | {"opus": 51, "mp4a.40.2": 49} | {"opus": 51, "mp4a.40.2": 49} |
| `media.webcodecs.av1` | UNSUPPORTED | UNSUPPORTED | same | Error: encoder config av01.0.04M.08 not supported | Error: encoder config av01.0.04M.08 not supported |
| `media.webcodecs.h264` | PASS | PASS | same | {"chunks": 10, "frames": 10, "bytes": 1216} | {"chunks": 10, "frames": 10, "bytes": 1216} |
| `media.webcodecs.hevc` | PASS | PASS | same | {"chunks": 10, "frames": 10, "bytes": 653} | {"chunks": 10, "frames": 10, "bytes": 653} |
| `media.webcodecs.image-decoder` | UNSUPPORTED | UNSUPPORTED | same | Error: no ImageDecoder | Error: no ImageDecoder |
| `media.webcodecs.vp9` | PASS | PASS | same | {"chunks": 10, "frames": 10, "bytes": 732} | {"chunks": 10, "frames": 10, "bytes": 732} |
| `network.abort-signal-timeout-any` | PASS | PASS | same | [true, "TimeoutError"] | [true, "TimeoutError"] |
| `network.compression-brotli-zstd` | PASS | PASS | same | ["brotli"] | ["brotli"] |
| `network.compression-streams` | PASS | PASS | same | ["gzip", "deflate", "deflate-raw"] | ["gzip", "deflate", "deflate-raw"] |
| `network.cors-allowed` | PASS | PASS | same | {"cors": true} | {"cors": true} |
| `network.cors-blocked` | PASS | PASS | same | "TypeError" | "TypeError" |
| `network.cors-preflight` | PASS | PASS | same | ["{\"p\":1}", "7"] | ["{\"p\":1}", "7"] |
| `network.eventsource` | PASS | PASS | same | [0, 1, 2] | [0, 1, 2] |
| `network.external-https` | PASS | PASS | same | [200, true] | [200, true] |
| `network.fetch-abort` | PASS | PASS | same | "AbortError" | "AbortError" |
| `network.fetch-gzip` | PASS | PASS | same | [1200, "webcov-gzip "] | [1200, "webcov-gzip "] |
| `network.fetch-json` | PASS | PASS | same | [200, "application/json", 42] | [200, "application/json", 42] |
| `network.fetch-stream-read` | PASS | PASS | same | "part0;part1;part2;part3;part4;" | "part0;part1;part2;part3;part4;" |
| `network.fetch-upload-stream` | UNSUPPORTED | UNSUPPORTED | same | Error: request streams: ReadableStream uploading is not supported | Error: request streams: ReadableStream uploading is not supported |
| `network.online-connection` | PASS | PASS | same | {"onLine": true, "connection": false} | {"onLine": true, "connection": false} |
| `network.request-headers` | PASS | PASS | same | {"ua": "Mozilla/5.0 (iPhone; CPU iPhone OS N like Mac OS X) AppleWebKit/605.1.15 (KHTML, … | {"ua": "Mozilla/5.0 (iPhone; CPU iPhone OS N like Mac OS X) AppleWebKit/605.1.15 (KHTML, … |
| `network.sendBeacon` | PASS | PASS | same |  |  |
| `network.streams-transform-pipe` | PASS | PASS | same | [["A", "B"], 1, 1, "xy"] | [["A", "B"], 1, 1, "xy"] |
| `network.websocket` | PASS | PASS | same | ["hello", [1, 2, 3], 70000] | ["hello", [1, 2, 3], 70000] |
| `network.websocket-stream` | UNSUPPORTED | UNSUPPORTED | same | Error: no WebSocketStream | Error: no WebSocketStream |
| `network.webtransport` | PASS | PASS | same |  |  |
| `network.xhr` | PASS | PASS | same | [200, 42] | [200, 42] |
| `storage.blob-filereader-objecturl` | PASS | PASS | same | ["data:text/plain;base64,YWJj", "abc", 3, "x"] | ["data:text/plain;base64,YWJj", "abc", 3, "x"] |
| `storage.cache-api` | PASS | PASS | same | ["cached!", 42, 2, false] | ["cached!", 42, 2, false] |
| `storage.cookie-store` | PASS | PASS | same | "v" | "v" |
| `storage.cookies-document` | PASS | PASS | same | [true, false] | [true, false] |
| `storage.cookies-server-httponly` | PASS | PASS | same | [true, true, false] | [true, true, false] |
| `storage.file-system-access-pickers` | UNSUPPORTED | UNSUPPORTED | same | Error: no showOpenFilePicker | Error: no showOpenFilePicker |
| `storage.indexeddb` | PASS | PASS | same | [100, 10, 10, "blobdata", 5] | [100, 10, 10, "blobdata", 5] |
| `storage.indexeddb-databases` | PASS | PASS | same |  |  |
| `storage.localStorage` | PASS | PASS | same | ["é✓", null, "1"] | ["é✓", null, "1"] |
| `storage.localStorage-5mb` | PASS | PASS | same | 1048576 | 1048576 |
| `storage.opfs` | PASS | PASS | same | ["hello opfs", ["a.txt"]] | ["hello opfs", ["a.txt"]] |
| `storage.opfs-sync-access-handle` | PASS | PASS | same | {"data": [1, 2, 3], "size": 3} | {"data": [1, 2, 3], "size": 3} |
| `storage.persist` | PASS | PASS | same | {"persisted": false} | {"persisted": false} |
| `storage.quota-estimate` | PASS | PASS | same (values differ) | {"quotaMB": 41232, "usageKB": 10} | {"quotaMB": 41232, "usageKB": 1058} |
| `workers.atomics-cross-thread` | PASS | PASS | same | [10000, 1] | [10000, 1] |
| `workers.blob-url` | PASS | PASS | same | 42 | 42 |
| `workers.broadcast-channel` | PASS | PASS | same | {"x": 1} | {"x": 1} |
| `workers.dedicated` | PASS | PASS | same | {"echo": [1, "x"], "isWorker": true} | {"echo": [1, "x"], "isWorker": true} |
| `workers.dedicated-api-surface` | PASS | PASS | same | {"fetch": "function", "idb": "object", "osc": "function", "ws": "function", "crypto": "ob… | {"fetch": "function", "idb": "object", "osc": "function", "ws": "function", "crypto": "ob… |
| `workers.message-channel-transfer` | PASS | PASS | same | [0, 16] | [0, 16] |
| `workers.module` | PASS | PASS | same | {"answer": 42, "module": true} | {"answer": 42, "module": true} |
| `workers.service-worker.cache-hit` | PASS | PASS | same | ["1", 42] | ["1", 42] |
| `workers.service-worker.fetch-intercept` | PASS | PASS | same | {"fromSW": true} | {"fromSW": true} |
| `workers.service-worker.install` | PASS | PASS | same | [true, "activated", true] | [true, "activated", true] |
| `workers.service-worker.message` | PASS | PASS | same | {"pong": "ping", "sw": true} | {"pong": "ping", "sw": true} |
| `workers.service-worker.unregister` | PASS | PASS | same |  |  |
| `workers.shared` | PASS | TIMEOUT | better | ["a", "b", 1] | Error: timeout: shared worker |
| `workers.wasm-shared-memory-thread` | PASS | PASS | same | 7 | 7 |
| `workers.worklet-paint` | UNSUPPORTED | UNSUPPORTED | same | Error: no CSS.paintWorklet | Error: no CSS.paintWorklet |
