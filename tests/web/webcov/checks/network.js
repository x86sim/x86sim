// network: fetch (JSON, streams, upload streams, abort, gzip), WebSocket, EventSource, Compression Streams, CORS
// (http://localhost:PORT and 127.0.0.1:PORT are two origins of one server), XHR, sendBeacon, WebTransport presence
'use strict';
check('network.fetch-json', async () => { const r = await fetch('/res/data.json'); return eq([r.status, r.headers.get('content-type'), (await r.json()).webcov], [200, 'application/json', 42]); });
check('network.fetch-gzip', async () => { const t = await (await fetch('/gzip')).text(); return eq([t.length, t.slice(0, 12)], [1200, 'webcov-gzip ']); });
check('network.fetch-stream-read', async () => {
	const r = await fetch('/chunked'); const rd = r.body.getReader(); const dec = new TextDecoder(); let s = '', n = 0;
	for (;;) { const { done, value } = await rd.read(); if (done || !alive()) break; n++; s += dec.decode(value, { stream: true }); }
	ok(n >= 1, 'no chunks'); return eq(s, 'part0;part1;part2;part3;part4;');
});
check('network.fetch-upload-stream', async () => {
	const body = new ReadableStream({ start(c) { c.enqueue(new TextEncoder().encode('up')); c.close(); } });
	try { const r = await fetch('/echo', { method: 'POST', body, duplex: 'half' }); return eq(await r.text(), 'up'); }
	catch (e) { throw new Unsupported('request streams: ' + e.message); }
});
check('network.fetch-abort', async () => { const ac = new AbortController(); const p = fetch('/slow?ms=3000', { signal: ac.signal }); setTimeout(() => ac.abort(), 50); try { await p; } catch (e) { return eq(e.name, 'AbortError'); } throw new Error('not aborted'); });
check('network.abort-signal-timeout-any', async () => { const s = AbortSignal.any([AbortSignal.timeout(50)]); await sleep(150); return eq([s.aborted, s.reason && s.reason.name], [true, 'TimeoutError']); });
check('network.xhr', async () => { const x = new XMLHttpRequest(); x.open('GET', '/res/data.json'); x.responseType = 'json'; const p = once(x, 'load', 5000); x.send(); await p; return eq([x.status, x.response.webcov], [200, 42]); });
check('network.cors-allowed', async () => { const r = await fetch(ORIGIN2 + '/cors/data'); return eq(await r.json(), { cors: true }); });
check('network.cors-blocked', async () => { try { await fetch(ORIGIN2 + '/cors/nocors'); } catch (e) { return eq(e.name, 'TypeError'); } throw new Error('cross-origin read without CORS headers succeeded'); });
check('network.cors-preflight', async () => { const r = await fetch(ORIGIN2 + '/cors/echo', { method: 'PUT', headers: { 'X-Webcov': '1', 'Content-Type': 'application/json' }, body: '{"p":1}' }); return eq([await r.text(), r.headers.get('X-Webcov-Echo')], ['{"p":1}', '7']); });
check('network.websocket', async () => {
	const ws = new WebSocket('ws://' + location.host + '/ws'); ws.binaryType = 'arraybuffer';
	await once(ws, 'open', 5000, 'error');
	const got = []; const p = new Promise(r => ws.onmessage = e => { got.push(typeof e.data === 'string' ? e.data : Array.from(new Uint8Array(e.data))); if (got.length === 3) r(); });
	ws.send('hello'); ws.send(new Uint8Array([1, 2, 3])); ws.send('x'.repeat(70000)); await withTimeout(p, 5000, 'ws echo');
	ws.close(); return eq([got[0], got[1], got[2].length], ['hello', [1, 2, 3], 70000]);
});
check('network.websocket-stream', () => { need(self.WebSocketStream, 'no WebSocketStream'); return true; });
check('network.eventsource', async () => {
	const es = new EventSource('/sse'); const got = [];
	try { await withTimeout(new Promise((res, rej) => { es.addEventListener('tick', e => { got.push(JSON.parse(e.data).n); if (got.length === 3) res(); }); es.onerror = () => got.length < 3 && es.readyState === 2 && rej(new Error('eventsource error')); }), 6000, 'sse'); }
	finally { es.close(); }
	return eq(got, [0, 1, 2]);
});
check('network.compression-streams', async () => {
	const src = new TextEncoder().encode('webcov '.repeat(1000)); const out = {};
	for (const f of ['gzip', 'deflate', 'deflate-raw']) {
		const c = await new Response(new Blob([src]).stream().pipeThrough(new CompressionStream(f))).arrayBuffer();
		const d = await new Response(new Blob([c]).stream().pipeThrough(new DecompressionStream(f))).text();
		ok(d.length === 7000 && c.byteLength < 200, f); out[f] = c.byteLength;
	}
	return Object.keys(out);
});
check('network.compression-brotli-zstd', async () => { const r = []; for (const f of ['brotli', 'zstd']) { try { new DecompressionStream(f); r.push(f); } catch (e) {} } need(r.length, 'neither brotli nor zstd'); return r; });
check('network.streams-transform-pipe', async () => {
	const t = new TransformStream({ transform(ch, c) { c.enqueue(ch.toUpperCase()); } });
	const rs = new ReadableStream({ start(c) { c.enqueue('a'); c.enqueue('b'); c.close(); } });
	const out = []; await rs.pipeThrough(t).pipeTo(new WritableStream({ write(x) { out.push(x); } }));
	const [b1, b2] = new ReadableStream({ start(c) { c.enqueue(1); c.close(); } }).tee();
	const fromIt = ReadableStream.from ? (await new Response(ReadableStream.from(['x', 'y']).pipeThrough(new TextEncoderStream())).text()) : 'nofrom';
	return eq([out, (await b1.getReader().read()).value, (await b2.getReader().read()).value, fromIt], [['A', 'B'], 1, 1, 'xy']);
});
check('network.sendBeacon', () => eq(navigator.sendBeacon('/echo', 'beacon'), true));
check('network.webtransport', () => { need(self.WebTransport, 'no WebTransport'); return true; });
check('network.online-connection', () => ({ onLine: navigator.onLine, connection: !!navigator.connection }));
check('network.request-headers', async () => { const h = await (await fetch('/headers')).json(); const ua = h['User-Agent'] || h['user-agent'] || ''; return { ua: ua.replace(/\d+_\d+(_\d+)?/g, 'N'), lang: h['Accept-Language'] || h['accept-language'] }; });
// a public CORS-enabled CDN file over HTTPS (TLS, DNS, the host's network through the simulator)
check('network.external-https', async () => { const r = await fetch('https://cdnjs.cloudflare.com/ajax/libs/jquery/3.7.1/jquery.min.js'); const t = await r.text(); return eq([r.status, t.length > 80000], [200, true]); }, { timeout: 20000 });
