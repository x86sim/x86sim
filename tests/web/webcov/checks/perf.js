// performance: small benchmarks whose numbers are compared with Apple's simulator on the Mac (a speed ratio, not a
// pass / fail: each returns {score, unit}; higher is better). They run last among the automatic checks.
'use strict';
const P = (score, unit) => ({ score: +score.toFixed(2), unit });
let sink = 0;
check('perf.js.numeric-loop', async () => P(await bench(() => { let s = 0; for (let i = 0; i < 100000; i++) s = (s + i * 31) | 0; sink ^= s; }, 1500), 'runs/s'), { perf: true });
check('perf.js.float-math', async () => P(await bench(() => { let s = 0; for (let i = 1; i < 20000; i++) s += Math.sin(i) * Math.sqrt(i) / Math.log(i + 1); sink += s; }, 1500), 'runs/s'), { perf: true });
check('perf.js.object-alloc', async () => P(await bench(() => { const a = []; for (let i = 0; i < 5000; i++) a.push({ i, s: 'x' + i, v: [i, i + 1] }); sink += a.length; }, 1500), 'runs/s'), { perf: true });
check('perf.js.string-regex', async () => { const txt = 'The quick brown fox jumps over the lazy dog 12345 '.repeat(200); return P(await bench(() => { sink += txt.replace(/\b(\w)(\w*)/g, (m, a, b) => a.toUpperCase() + b).split(' ').length + (txt.match(/\d+/g) || []).length; }, 1500), 'runs/s'); }, { perf: true });
check('perf.js.json', async () => { const o = { a: Array.from({ length: 500 }, (_, i) => ({ id: i, name: 'item' + i, tags: ['a', 'b'], v: i / 3 })) }; return P(await bench(() => { sink += JSON.parse(JSON.stringify(o)).a.length; }, 1500), 'runs/s'); }, { perf: true });
check('perf.js.array-sort', async () => { const base = Array.from({ length: 20000 }, (_, i) => (i * 2654435761) % 1000003); return P(await bench(() => { sink += base.slice().sort((a, b) => a - b)[0]; }, 1500), 'runs/s'); }, { perf: true });
check('perf.js.typed-array', async () => { const f = new Float64Array(100000); return P(await bench(() => { for (let i = 0; i < f.length; i++) f[i] = f[i] * 0.5 + i; sink += f[99]; }, 1500), 'runs/s'); }, { perf: true });
check('perf.js.closures-calls', async () => { const add = (a) => (b) => a + b; return P(await bench(() => { let s = 0; for (let i = 0; i < 50000; i++) s = add(i)(s) % 1000; sink += s; }, 1500), 'runs/s'); }, { perf: true });
check('perf.wasm.loop', async () => {
	// (func (param i32) (result i32) (local i32 i32) loop: acc = acc*31 + i; i++ < n) -> runs/s of n = 1e6
	const b = wasm(WASM_HDR + '01060160017f017f 03020100 070501016600 00 0a21011f 01027f 0340 2002 411f 6c 2001 6a 2102 2001 4101 6a 2201 2000 48 0d00 0b 2002 0b');
	need(WebAssembly.validate(b), 'bench module does not validate'); const { instance } = await WebAssembly.instantiate(b); const f = instance.exports.f;
	return P(await bench(() => { sink += f(1000000); }, 1500), 'runs/s');
}, { perf: true });
check('perf.crypto.sha256-1mb', async () => { const d = new Uint8Array(1 << 20); let n = 0; const t0 = performance.now(); while (performance.now() - t0 < 1500 && alive()) { await crypto.subtle.digest('SHA-256', d); n++; } return P(n / ((performance.now() - t0) / 1000), 'MB/s'); }, { perf: true });
check('perf.canvas2d.draw-fps', async () => {
	const c = el('canvas'); c.width = 360; c.height = 360; const g = c.getContext('2d'); let n = 0; const t0 = performance.now();
	while (performance.now() - t0 < 2000 && alive()) { for (let i = 0; i < 400; i++) { g.fillStyle = 'hsl(' + (i * 7 + n) % 360 + ' 80% 50%)'; g.beginPath(); g.arc((i * 37) % 360, (i * 91 + n) % 360, 12, 0, 6.283); g.fill(); } g.fillText('frame ' + n, 10, 10); await frames(1); n++; }
	return P(n / ((performance.now() - t0) / 1000), 'fps');
}, { perf: true, timeout: 15000 });
check('perf.canvas2d.getImageData', async () => { const c = document.createElement('canvas'); c.width = c.height = 512; const g = c.getContext('2d'); return P(await bench(() => { g.fillRect(0, 0, 1, 1); sink += g.getImageData(0, 0, 512, 512).data[0]; }, 1500), 'runs/s'); }, { perf: true });
check('perf.webgl.draw-fps', async () => {
	const c = el('canvas'); c.width = 360; c.height = 360; const gl = c.getContext('webgl2'); need(gl, 'no webgl2'); glTriangle(gl, true); let n = 0; const t0 = performance.now();
	while (performance.now() - t0 < 2000 && alive()) { gl.clearColor((n % 60) / 60, 0, 0, 1); gl.clear(gl.COLOR_BUFFER_BIT); for (let i = 0; i < 200; i++) gl.drawArrays(gl.TRIANGLES, 0, 3); await frames(1); n++; }
	return P(n / ((performance.now() - t0) / 1000), 'fps');
}, { perf: true, timeout: 15000 });
check('perf.dom.layout-loop', async () => {
	const host = el('div'); host.style.width = '300px'; return P(await bench(() => {
		const d = document.createElement('div'); d.innerHTML = '<p style="margin:2px">Lorem <b>ipsum</b> dolor sit amet, consectetur</p>'.repeat(10); host.appendChild(d);
		sink += d.offsetHeight; d.style.width = (100 + (sink % 100)) + 'px'; sink += d.getBoundingClientRect().height; host.removeChild(d);
	}, 1500), 'runs/s');
}, { perf: true });
check('perf.dom.query-style', async () => { const host = el('div'); host.innerHTML = '<ul>' + '<li class="a"><span>x</span></li>'.repeat(500) + '</ul>'; return P(await bench(() => { sink += host.querySelectorAll('li.a > span').length; sink += getComputedStyle(host.firstChild.children[(sink % 500) | 0]).display.length; }, 1500), 'runs/s'); }, { perf: true });
check('perf.raf-fps-idle', async () => { let n = 0; const t0 = performance.now(); while (performance.now() - t0 < 1500 && alive()) { await frames(1); n++; } return P(n / ((performance.now() - t0) / 1000), 'fps'); }, { perf: true });
