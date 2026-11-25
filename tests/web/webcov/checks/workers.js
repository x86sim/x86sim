// workers: dedicated (classic + module), shared, service worker (install, cache, fetch interception, messaging),
// BroadcastChannel, MessageChannel, Atomics across threads, a worker's own API surface
'use strict';
function workerCall(w, msg, transfer, ms) {
	return withTimeout(new Promise((res, rej) => { w.onmessage = (e) => res(e.data); w.onerror = (e) => rej(new Error('worker error: ' + (e.message || e.type))); w.postMessage(msg, transfer || []); }), ms || 8000, 'worker answer');
}
check('workers.dedicated', async () => { const w = new Worker('res/worker.js'); try { return eq(await workerCall(w, { op: 'echo', v: [1, 'x'] }), { echo: [1, 'x'], isWorker: true }); } finally { w.terminate(); } });
check('workers.dedicated-api-surface', async () => { const w = new Worker('res/worker.js'); try { return eq(await workerCall(w, { op: 'probe' }), { fetch: 'function', idb: 'object', osc: 'function', ws: 'function', crypto: 'object', caches: 'object', storage: 'object' }); } finally { w.terminate(); } });
check('workers.module', async () => { const w = new Worker('res/module-worker.mjs', { type: 'module' }); try { return eq(await withTimeout(new Promise((res, rej) => { w.onmessage = e => res(e.data); w.onerror = e => rej(new Error(e.message)); }), 8000), { answer: 42, module: true }); } finally { w.terminate(); } });
check('workers.blob-url', async () => { const u = URL.createObjectURL(new Blob(['onmessage = e => postMessage(e.data * 2)'], { type: 'text/javascript' })); const w = new Worker(u); try { return eq(await workerCall(w, 21), 42); } finally { w.terminate(); } });
check('workers.shared', async () => {
	need(typeof SharedWorker === 'function', 'no SharedWorker');
	const a = new SharedWorker('res/shared-worker.js'), b = new SharedWorker('res/shared-worker.js');
	const ask = (sw, v) => withTimeout(new Promise(res => { sw.port.onmessage = e => res(e.data); sw.port.postMessage(v); }), 8000, 'shared worker');
	const r1 = await ask(a, 'a'), r2 = await ask(b, 'b');
	return eq([r1.got, r2.got, r2.n - r1.n], ['a', 'b', 1]);
});
check('workers.atomics-cross-thread', async () => {
	need(self.crossOriginIsolated && typeof SharedArrayBuffer === 'function', 'no SharedArrayBuffer');
	const sab = new SharedArrayBuffer(8), ia = new Int32Array(sab); const w = new Worker('res/worker.js');
	try { await workerCall(w, { op: 'atomics', sab }); return eq([Atomics.load(ia, 0), Atomics.load(ia, 1)], [10000, 1]); } finally { w.terminate(); }
});
check('workers.wasm-shared-memory-thread', async () => {
	need(self.crossOriginIsolated && typeof SharedArrayBuffer === 'function', 'no SharedArrayBuffer');
	const mem = new WebAssembly.Memory({ initial: 1, maximum: 2, shared: true }); const w = new Worker('res/worker.js');
	try { await workerCall(w, { op: 'wasm-thread', mem }); return eq(new Int32Array(mem.buffer)[0], 7); } finally { w.terminate(); }
});
check('workers.message-channel-transfer', async () => {
	const { port1, port2 } = new MessageChannel(); const buf = new ArrayBuffer(16);
	const p = new Promise(r => port2.onmessage = e => r(e.data)); port1.postMessage(buf, [buf]);
	const got = await withTimeout(p, 3000); return eq([buf.byteLength, got.byteLength], [0, 16]);
});
check('workers.broadcast-channel', async () => {
	const a = new BroadcastChannel('webcov'), b = new BroadcastChannel('webcov');
	try { const p = new Promise(r => b.onmessage = e => r(e.data)); a.postMessage({ x: 1 }); return eq(await withTimeout(p, 3000), { x: 1 }); } finally { a.close(); b.close(); }
});
async function swReg() {
	need('serviceWorker' in navigator, 'no navigator.serviceWorker');
	const reg = await navigator.serviceWorker.register('/sw.js', { scope: '/' });
	const sw = reg.installing || reg.waiting || reg.active;
	if (sw && sw.state !== 'activated') await withTimeout(new Promise(r => sw.addEventListener('statechange', () => sw.state === 'activated' && r())), 10000, 'sw activation');
	await withTimeout(navigator.serviceWorker.ready, 10000, 'sw ready');
	if (!navigator.serviceWorker.controller) await withTimeout(new Promise(r => navigator.serviceWorker.addEventListener('controllerchange', r, { once: true })), 5000, 'controllerchange');
	return reg;
}
check('workers.service-worker.install', async () => { const reg = await swReg(); return eq([!!reg.active, reg.active.state, reg.scope.endsWith('/')], [true, 'activated', true]); }, { timeout: 20000 });
check('workers.service-worker.fetch-intercept', async () => { await swReg(); return eq(await (await fetch('/sw-virtual')).json(), { fromSW: true }); }, { timeout: 20000 });
check('workers.service-worker.cache-hit', async () => { await swReg(); const r = await fetch('/res/data.json'); return eq([r.headers.get('X-From-Cache'), (await r.json()).webcov], ['1', 42]); }, { timeout: 20000 });
check('workers.service-worker.message', async () => {
	const reg = await swReg();
	const p = new Promise(r => navigator.serviceWorker.addEventListener('message', e => r(e.data), { once: true }));
	reg.active.postMessage('ping'); return eq(await withTimeout(p, 5000), { pong: 'ping', sw: true });
}, { timeout: 20000 });
check('workers.service-worker.unregister', async () => { const reg = await swReg(); return eq(await reg.unregister(), true); }, { timeout: 20000 });
check('workers.worklet-paint', () => { need(CSS.paintWorklet, 'no CSS.paintWorklet'); return true; });
