// graphics: Canvas 2D (shapes, text, emoji, filters, image decode/encode), OffscreenCanvas (main + worker), WebGL 1/2
// (a drawn triangle read back), WebGPU (compute + render read back), SVG rasterized into a canvas, CSS feature
// support, fonts (system families present, web font loaded and used, CJK / Arabic / emoji glyphs drawn)
'use strict';
const RED = [255, 0, 0], BLUE = [0, 0, 255], GREEN = [0, 255, 0];
function canvas(w, h) { const c = document.createElement('canvas'); c.width = w || 64; c.height = h || 64; return c; }
function ink(g, w, h) { const d = g.getImageData(0, 0, w, h).data; let n = 0, colored = 0; for (let i = 0; i < d.length; i += 4) { if (d[i + 3] > 40) { n++; const mx = Math.max(d[i], d[i + 1], d[i + 2]), mn = Math.min(d[i], d[i + 1], d[i + 2]); if (mx - mn > 60) colored++; } } return { n, colored }; }
function loadImg(src) { const i = new Image(); i.src = src; return i.decode().then(() => i); }
function topBottom(g, w, h) { return [Array.from(g.getImageData(w >> 1, h >> 2, 1, 1).data).slice(0, 3), Array.from(g.getImageData(w >> 1, (3 * h) >> 2, 1, 1).data).slice(0, 3)]; }

check('graphics.canvas2d.shapes', () => {
	const c = canvas(100, 100), g = c.getContext('2d');
	g.fillStyle = '#ff0000'; g.fillRect(0, 0, 50, 100); g.fillStyle = 'rgb(0 0 255)'; g.beginPath(); g.arc(75, 50, 20, 0, Math.PI * 2); g.fill();
	const lg = g.createLinearGradient(0, 90, 100, 90); lg.addColorStop(0, '#000'); lg.addColorStop(1, '#fff'); g.fillStyle = lg; g.fillRect(0, 95, 100, 5);
	g.fill(new Path2D('M 60 0 L 70 0 L 70 10 Z')); g.roundRect && g.roundRect(0, 0, 4, 4, 2);
	const p = (x, y) => Array.from(g.getImageData(x, y, 1, 1).data);
	const grad = p(50, 97)[0];
	return eq([p(10, 10), p(75, 50), p(99, 50)[3], grad > 100 && grad < 160], [[255, 0, 0, 255], [0, 0, 255, 255], 0, true]);
});
check('graphics.canvas2d.text', () => {
	const c = canvas(200, 50), g = c.getContext('2d'); g.font = '30px sans-serif'; g.fillStyle = '#000';
	const m = g.measureText('Hello'); g.fillText('Hello', 5, 35); const i = ink(g, 200, 50);
	ok(m.width > 50 && m.width < 110, 'width ' + m.width); ok(i.n > 200, 'ink ' + i.n);
	return { width: Math.round(m.width), ascent: Math.round(m.actualBoundingBoxAscent), ink: i.n };
});
check('graphics.canvas2d.emoji', () => {
	const c = canvas(64, 64), g = c.getContext('2d'); g.font = '48px "Apple Color Emoji", sans-serif'; g.textBaseline = 'top'; g.fillText('😀', 4, 4);
	const i = ink(g, 64, 64); ok(i.colored > 300, 'colored pixels ' + i.colored + ' (ink ' + i.n + '): no color emoji'); return i.colored > 300;
});
check('graphics.canvas2d.cjk-arabic-text', () => {
	const r = {};
	for (const [k, s] of [['cjk', '漢字かな한글'], ['arabic', 'مرحبا بالعالم'], ['devanagari', 'नमस्ते'], ['thai', 'สวัสดี']]) {
		const c = canvas(300, 50), g = c.getContext('2d'); g.font = '28px sans-serif'; g.fillText(s, 5, 36); r[k] = ink(g, 300, 50).n;
	}
	for (const k in r) ok(r[k] > 150, k + ' ink ' + r[k]); return Object.keys(r);
});
check('graphics.canvas2d.filter-shadow', () => {
	const c = canvas(60, 60), g = c.getContext('2d'); need('filter' in g, 'no ctx.filter');
	g.filter = 'blur(4px)'; g.fillStyle = '#000'; g.fillRect(20, 20, 20, 20); const edge = g.getImageData(17, 30, 1, 1).data[3];
	ok(edge > 10 && edge < 250, 'blur edge alpha ' + edge); return edge;
});
check('graphics.canvas2d.composite-blend', () => {
	const c = canvas(10, 10), g = c.getContext('2d'); g.fillStyle = 'rgb(200,100,50)'; g.fillRect(0, 0, 10, 10); g.globalCompositeOperation = 'multiply'; g.fillStyle = 'rgb(128,128,128)'; g.fillRect(0, 0, 10, 10);
	const p = Array.from(g.getImageData(5, 5, 1, 1).data); ok(Math.abs(p[0] - 100) < 4 && Math.abs(p[1] - 50) < 4, JSON.stringify(p)); return p;
});
check('graphics.canvas2d.getImageData-big', () => { const c = canvas(2048, 2048), g = c.getContext('2d'); g.fillStyle = '#0f0'; g.fillRect(0, 0, 2048, 2048); return eq(Array.from(g.getImageData(2047, 2047, 1, 1).data), [0, 255, 0, 255]); });
check('graphics.canvas2d.p3-colorspace', () => { const c = canvas(4, 4), g = c.getContext('2d', { colorSpace: 'display-p3' }); g.fillStyle = 'color(display-p3 1 0 0)'; g.fillRect(0, 0, 4, 4); return eq([g.getContextAttributes().colorSpace, g.getImageData(0, 0, 1, 1, { colorSpace: 'display-p3' }).data[0]], ['display-p3', 255]); });
for (const f of ['png', 'jpg', 'webp', 'gif', 'avif', 'heic']) check('graphics.image-decode.' + f, async () => {
	const i = await withTimeout(loadImg('/media/img.' + f), 8000, 'decode');
	const c = canvas(64, 64), g = c.getContext('2d'); g.drawImage(i, 0, 0); const [t, b] = topBottom(g, 64, 64);
	ok(colorIs(t, RED, 40) && colorIs(b, BLUE, 40), 'pixels ' + JSON.stringify([t, b])); return [i.naturalWidth, i.naturalHeight];
});
check('graphics.createImageBitmap', async () => { const b = await (await fetch('/media/img.png')).blob(); const bm = await createImageBitmap(b, { resizeWidth: 32, resizeHeight: 32 }); const c = canvas(32, 32), g = c.getContext('2d'); g.drawImage(bm, 0, 0); return eq(topBottom(g, 32, 32), [RED, BLUE]); });
for (const [t, q] of [['image/png'], ['image/jpeg', 0.9], ['image/webp', 0.9]]) check('graphics.canvas-encode.' + t.split('/')[1], async () => {
	const c = canvas(32, 32), g = c.getContext('2d'); g.fillStyle = '#f00'; g.fillRect(0, 0, 32, 16); g.fillStyle = '#00f'; g.fillRect(0, 16, 32, 16);
	const blob = await new Promise(r => c.toBlob(r, t, q)); ok(blob, 'no blob'); if (blob.type !== t) throw new Unsupported('encoded as ' + blob.type);
	const i = await loadImg(URL.createObjectURL(blob)); const c2 = canvas(32, 32), g2 = c2.getContext('2d'); g2.drawImage(i, 0, 0);
	const [a, b] = topBottom(g2, 32, 32); ok(colorIs(a, RED, 40) && colorIs(b, BLUE, 40), JSON.stringify([a, b])); return blob.size > 0;
});
check('graphics.offscreen-canvas', async () => {
	need(self.OffscreenCanvas, 'no OffscreenCanvas'); const oc = new OffscreenCanvas(16, 16), g = oc.getContext('2d'); g.fillStyle = '#00f'; g.fillRect(0, 0, 16, 16);
	const bm = oc.transferToImageBitmap(); const c = canvas(16, 16); c.getContext('bitmaprenderer').transferFromImageBitmap(bm);
	const blob = await new OffscreenCanvas(4, 4).convertToBlob(); return eq([px(c, 8, 8), blob.type], [[0, 0, 255, 255], 'image/png']);
});
check('graphics.offscreen-canvas-worker', async () => {
	const c = canvas(8, 8); el('div').appendChild(c); const oc = c.transferControlToOffscreen(); const w = new Worker('res/worker.js');
	try { await workerCall(w, { op: 'offscreen', canvas: oc }, [oc]); await frames(3); return eq(px(c, 4, 4).slice(0, 3), GREEN); } finally { w.terminate(); }
});
check('graphics.offscreen-webgl2-worker', async () => { const w = new Worker('res/worker.js'); try { const r = await workerCall(w, { op: 'webgl' }); need(r, 'no webgl2 in a worker'); return eq(r, [0, 255, 0, 255]); } finally { w.terminate(); } });

// WebGL: one colored triangle covering the middle; readPixels in the middle + a corner (clear color)
function glTriangle(gl, v2) {
	const vs = (v2 ? '#version 300 es\nin' : 'attribute') + ' vec2 p; void main() { gl_Position = vec4(p, 0.0, 1.0); }';
	const fs = v2 ? '#version 300 es\nprecision mediump float; out vec4 o; uniform vec4 col; void main() { o = col; }' : 'precision mediump float; uniform vec4 col; void main() { gl_FragColor = col; }';
	const sh = (t, s) => { const x = gl.createShader(t); gl.shaderSource(x, s); gl.compileShader(x); if (!gl.getShaderParameter(x, gl.COMPILE_STATUS)) throw new Error('shader: ' + gl.getShaderInfoLog(x)); return x; };
	const pr = gl.createProgram(); gl.attachShader(pr, sh(gl.VERTEX_SHADER, vs)); gl.attachShader(pr, sh(gl.FRAGMENT_SHADER, fs)); gl.linkProgram(pr);
	if (!gl.getProgramParameter(pr, gl.LINK_STATUS)) throw new Error('link: ' + gl.getProgramInfoLog(pr)); gl.useProgram(pr);
	const b = gl.createBuffer(); gl.bindBuffer(gl.ARRAY_BUFFER, b); gl.bufferData(gl.ARRAY_BUFFER, new Float32Array([-0.8, -0.8, 0.8, -0.8, 0, 0.8]), gl.STATIC_DRAW);
	const loc = gl.getAttribLocation(pr, 'p'); gl.enableVertexAttribArray(loc); gl.vertexAttribPointer(loc, 2, gl.FLOAT, false, 0, 0);
	gl.uniform4f(gl.getUniformLocation(pr, 'col'), 1, 0, 0, 1); gl.clearColor(0, 0, 1, 1); gl.clear(gl.COLOR_BUFFER_BIT); gl.drawArrays(gl.TRIANGLES, 0, 3);
	const rd = (x, y) => { const p = new Uint8Array(4); gl.readPixels(x, y, 1, 1, gl.RGBA, gl.UNSIGNED_BYTE, p); return Array.from(p); };
	const r = [rd(32, 28), rd(2, 62)]; const e = gl.getError(); if (e) throw new Error('gl error ' + e); return r;
}
check('graphics.webgl1.draw', () => { const c = canvas(64, 64); const gl = c.getContext('webgl', { preserveDrawingBuffer: true }); need(gl, 'no webgl context'); return eq(glTriangle(gl, false), [[255, 0, 0, 255], [0, 0, 255, 255]]); });
check('graphics.webgl2.draw', () => { const c = canvas(64, 64); const gl = c.getContext('webgl2', { preserveDrawingBuffer: true }); need(gl, 'no webgl2 context'); return eq(glTriangle(gl, true), [[255, 0, 0, 255], [0, 0, 255, 255]]); });
check('graphics.webgl2.canvas-composited', async () => { // the drawn WebGL canvas, read back through a 2D canvas (the compositing path)
	const c = canvas(64, 64); const gl = c.getContext('webgl2', { preserveDrawingBuffer: true }); need(gl, 'no webgl2'); glTriangle(gl, true); el('div').appendChild(c); await frames(2);
	return eq(px(c, 32, 36).slice(0, 3), RED);
});
check('graphics.webgl2.info', () => {
	const gl = canvas(4, 4).getContext('webgl2'); need(gl, 'no webgl2'); const dbg = gl.getExtension('WEBGL_debug_renderer_info');
	return { renderer: dbg ? gl.getParameter(dbg.UNMASKED_RENDERER_WEBGL) : gl.getParameter(gl.RENDERER), maxTex: gl.getParameter(gl.MAX_TEXTURE_SIZE), exts: gl.getSupportedExtensions().length, samples: gl.getParameter(gl.MAX_SAMPLES) };
});
check('graphics.webgl2.float-render-texture', () => {
	const gl = canvas(4, 4).getContext('webgl2'); need(gl, 'no webgl2'); need(gl.getExtension('EXT_color_buffer_float'), 'no EXT_color_buffer_float');
	const t = gl.createTexture(); gl.bindTexture(gl.TEXTURE_2D, t); gl.texImage2D(gl.TEXTURE_2D, 0, gl.RGBA32F, 4, 4, 0, gl.RGBA, gl.FLOAT, null);
	const fb = gl.createFramebuffer(); gl.bindFramebuffer(gl.FRAMEBUFFER, fb); gl.framebufferTexture2D(gl.FRAMEBUFFER, gl.COLOR_ATTACHMENT0, gl.TEXTURE_2D, t, 0);
	eq(gl.checkFramebufferStatus(gl.FRAMEBUFFER), gl.FRAMEBUFFER_COMPLETE, 'fb'); gl.clearColor(0.25, 2.5, -1, 1); gl.clear(gl.COLOR_BUFFER_BIT);
	const p = new Float32Array(4); gl.readPixels(0, 0, 1, 1, gl.RGBA, gl.FLOAT, p); return eq(Array.from(p), [0.25, 2.5, -1, 1]);
});
check('graphics.webgl2.texture-upload-video-image', async () => {
	const gl = canvas(64, 64).getContext('webgl2'); need(gl, 'no webgl2'); const i = await loadImg('/media/img.png');
	const t = gl.createTexture(); gl.bindTexture(gl.TEXTURE_2D, t); gl.texImage2D(gl.TEXTURE_2D, 0, gl.RGBA, gl.RGBA, gl.UNSIGNED_BYTE, i);
	const fb = gl.createFramebuffer(); gl.bindFramebuffer(gl.FRAMEBUFFER, fb); gl.framebufferTexture2D(gl.FRAMEBUFFER, gl.COLOR_ATTACHMENT0, gl.TEXTURE_2D, t, 0);
	const p = new Uint8Array(8); gl.readPixels(32, 16, 1, 1, gl.RGBA, gl.UNSIGNED_BYTE, p.subarray(0, 4)); gl.readPixels(32, 48, 1, 1, gl.RGBA, gl.UNSIGNED_BYTE, p.subarray(4));
	// texture row 0 = the image's top row (no UNPACK_FLIP_Y): y 16 is red, y 48 blue
	return eq([Array.from(p.subarray(0, 3)), Array.from(p.subarray(4, 7))], [RED, BLUE]);
});

// WebGPU
async function gpuDevice() { need(navigator.gpu, 'no navigator.gpu'); const a = await navigator.gpu.requestAdapter(); need(a, 'no GPU adapter'); return [a, await a.requestDevice()]; }
check('graphics.webgpu.adapter', async () => { const [a] = await gpuDevice(); const info = a.info || {}; return { vendor: info.vendor, arch: info.architecture, features: [...a.features].length, maxBuf: a.limits.maxBufferSize }; });
check('graphics.webgpu.compute', async () => {
	const [, d] = await gpuDevice(); const n = 64; const data = new Float32Array(n).map((_, i) => i);
	const buf = d.createBuffer({ size: n * 4, usage: GPUBufferUsage.STORAGE | GPUBufferUsage.COPY_SRC | GPUBufferUsage.COPY_DST }); d.queue.writeBuffer(buf, 0, data);
	const rb = d.createBuffer({ size: n * 4, usage: GPUBufferUsage.MAP_READ | GPUBufferUsage.COPY_DST });
	const m = d.createShaderModule({ code: '@group(0) @binding(0) var<storage, read_write> v: array<f32>; @compute @workgroup_size(64) fn main(@builtin(global_invocation_id) i: vec3u) { v[i.x] = v[i.x] * 2.0 + 1.0; }' });
	const p = d.createComputePipeline({ layout: 'auto', compute: { module: m, entryPoint: 'main' } });
	const bg = d.createBindGroup({ layout: p.getBindGroupLayout(0), entries: [{ binding: 0, resource: { buffer: buf } }] });
	const e = d.createCommandEncoder(); const pass = e.beginComputePass(); pass.setPipeline(p); pass.setBindGroup(0, bg); pass.dispatchWorkgroups(1); pass.end(); e.copyBufferToBuffer(buf, 0, rb, 0, n * 4); d.queue.submit([e.finish()]);
	await rb.mapAsync(GPUMapMode.READ); const out = Array.from(new Float32Array(rb.getMappedRange())); return eq([out[0], out[10], out[63]], [1, 21, 127]);
}, { timeout: 20000 });
check('graphics.webgpu.render-canvas', async () => {
	const [, d] = await gpuDevice(); const c = canvas(64, 64); el('div').appendChild(c); const ctx = c.getContext('webgpu'); const fmt = navigator.gpu.getPreferredCanvasFormat();
	ctx.configure({ device: d, format: fmt, alphaMode: 'opaque' });
	const m = d.createShaderModule({ code: '@vertex fn vs(@builtin(vertex_index) i: u32) -> @builtin(position) vec4f { var p = array<vec2f, 3>(vec2f(-0.8, -0.8), vec2f(0.8, -0.8), vec2f(0.0, 0.8)); return vec4f(p[i], 0.0, 1.0); } @fragment fn fs() -> @location(0) vec4f { return vec4f(1.0, 0.0, 0.0, 1.0); }' });
	const pl = d.createRenderPipeline({ layout: 'auto', vertex: { module: m, entryPoint: 'vs' }, fragment: { module: m, entryPoint: 'fs', targets: [{ format: fmt }] }, primitive: { topology: 'triangle-list' } });
	const e = d.createCommandEncoder(); const pass = e.beginRenderPass({ colorAttachments: [{ view: ctx.getCurrentTexture().createView(), clearValue: { r: 0, g: 0, b: 1, a: 1 }, loadOp: 'clear', storeOp: 'store' }] });
	pass.setPipeline(pl); pass.draw(3); pass.end(); d.queue.submit([e.finish()]); await d.queue.onSubmittedWorkDone();
	return eq([px(c, 32, 36).slice(0, 3), px(c, 2, 2).slice(0, 3)], [RED, BLUE]);
}, { timeout: 20000 });

// SVG rasterized into a canvas (SVG engine + filter effects) and inline SVG geometry
check('graphics.svg-raster-filter', async () => {
	const svg = '<svg xmlns="http://www.w3.org/2000/svg" width="64" height="64"><defs><filter id="f"><feFlood flood-color="#00ff00"/></filter><linearGradient id="g"><stop offset="0" stop-color="red"/><stop offset="1" stop-color="red"/></linearGradient></defs><rect width="64" height="32" fill="url(#g)"/><rect y="32" width="64" height="32" filter="url(#f)"/></svg>';
	const i = await loadImg('data:image/svg+xml,' + encodeURIComponent(svg)); const c = canvas(64, 64), g = c.getContext('2d'); g.drawImage(i, 0, 0);
	return eq(topBottom(g, 64, 64), [RED, GREEN]);
});
check('graphics.svg-dom-geometry', () => {
	const s = document.createElementNS('http://www.w3.org/2000/svg', 'svg'); s.setAttribute('width', '100'); s.setAttribute('height', '100'); document.getElementById('stage').appendChild(s);
	s.innerHTML = '<path id="p" d="M0 0 L30 40"/><text id="t" x="0" y="20" font-size="16">SVG</text>'; const p = s.querySelector('#p'), t = s.querySelector('#t');
	return eq([p.getTotalLength(), t.getComputedTextLength() > 10, Math.round(t.getBBox().height) > 10], [50, true, true]);
});
check('graphics.css-support', () => {
	const f = ['backdrop-filter: blur(4px)', 'filter: drop-shadow(1px 1px 1px red)', 'mask-image: linear-gradient(red, transparent)', 'color: color-mix(in oklch, red, blue)', 'color: oklch(0.7 0.1 200)', 'color: rgb(from red r g b / 50%)', 'background: conic-gradient(red, blue)', 'mix-blend-mode: multiply', 'clip-path: path("M0 0 L10 10 Z")', 'text-wrap: balance', 'contain-intrinsic-size: 10px'];
	const missing = f.filter(x => !CSS.supports(x)); ok(!missing.length, 'unsupported: ' + missing.join('; ')); return f.length;
});
check('graphics.css-backdrop-filter-computed', () => { const d = el('div'); d.style.cssText = '-webkit-backdrop-filter: blur(5px); backdrop-filter: blur(5px)'; const cs = getComputedStyle(d); return eq((cs.backdropFilter || cs.webkitBackdropFilter), 'blur(5px)'); });

// fonts: a family is present when text in it measures differently from a family that does not exist
function famWidth(fam) { const g = canvas(10, 10).getContext('2d'); g.font = '40px ' + fam; return g.measureText('mmmmiiiiWWll漢字0123').width; }
check('graphics.fonts.system-families', () => {
	const base = famWidth('webcovNoSuchFont'); const fams = ['-apple-system', 'system-ui', '"Helvetica Neue"', 'Helvetica', 'Courier', 'Georgia', 'Menlo', '"PingFang SC"', '"Hiragino Sans"', '"Apple SD Gothic Neo"', '"Avenir Next"', 'Futura', '"Noteworthy"'];
	const missing = fams.filter(f => Math.abs(famWidth(f) - base) < 0.01 && !/system|apple-system/.test(f));
	const present = fams.length - missing.length; // (Times, "SF Pro Text" and "New York" are not web-visible names in Safari 27 on Apple's simulator either: ui-serif etc.)
	ok(missing.length === 0, 'missing: ' + missing.join(', ')); return { present, missing };
});
check('graphics.fonts.webfont-fontface', async () => {
	const ff = new FontFace('WebcovMono', 'url(/media/webfont.ttf)'); await withTimeout(ff.load(), 8000, 'font load'); document.fonts.add(ff);
	const g = canvas(10, 10).getContext('2d'); g.font = '20px WebcovMono'; const wi = g.measureText('iiii').width, wm = g.measureText('MMMM').width;
	return eq([ff.status, document.fonts.check('20px WebcovMono'), Math.abs(wi - wm) < 0.5], ['loaded', true, true]);
});
check('graphics.fonts.webfont-css', async () => {
	const st = el('style'); st.textContent = '@font-face { font-family: WebcovCss; src: url(/media/webfont.ttf) format("truetype"); } #wcf { font: 20px WebcovCss, serif; }';
	const s = el('span', { id: 'wcf', textContent: 'iiii' }); await document.fonts.load('20px WebcovCss'); await document.fonts.ready; const w1 = s.getBoundingClientRect().width; s.textContent = 'MMMM'; const w2 = s.getBoundingClientRect().width;
	st.remove(); return eq(Math.abs(w1 - w2) < 1, true);
});
check('graphics.fonts.emoji-sequences', () => {
	const c = canvas(200, 50), g = c.getContext('2d'); g.font = '30px sans-serif'; const one = g.measureText('👨‍👩‍👧‍👦').width, flag = g.measureText('🇯🇵').width, skin = g.measureText('👍🏽').width, base = g.measureText('👍').width;
	ok(Math.abs(one - base) < base * 0.3 && Math.abs(flag - base) < base * 0.3 && Math.abs(skin - base) < base * 0.3, JSON.stringify({ one, flag, skin, base }) + ': sequences not one glyph'); return true;
});
