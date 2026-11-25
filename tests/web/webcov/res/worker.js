// dedicated worker: echoes, sums a SharedArrayBuffer with Atomics, answers feature probes from a worker scope
onmessage = async (e) => {
	const m = e.data;
	if (m.op === 'echo') postMessage({ echo: m.v, isWorker: typeof WorkerGlobalScope !== 'undefined' });
	else if (m.op === 'atomics') { const ia = new Int32Array(m.sab); for (let i = 0; i < 10000; i++) Atomics.add(ia, 0, 1); Atomics.notify(ia, 1); Atomics.store(ia, 1, 1); Atomics.notify(ia, 1); postMessage('ok'); }
	else if (m.op === 'probe') postMessage({ fetch: typeof fetch, idb: typeof indexedDB, osc: typeof OffscreenCanvas, ws: typeof WebSocket, crypto: typeof crypto.subtle, caches: typeof caches, storage: typeof navigator.storage });
	else if (m.op === 'offscreen') { const g = m.canvas.getContext('2d'); g.fillStyle = '#00ff00'; g.fillRect(0, 0, 8, 8); postMessage('drawn'); }
	else if (m.op === 'webgl') { const gl = new OffscreenCanvas(4, 4).getContext('webgl2'); if (!gl) return postMessage(null); gl.clearColor(0, 1, 0, 1); gl.clear(gl.COLOR_BUFFER_BIT); const p = new Uint8Array(4); gl.readPixels(0, 0, 1, 1, gl.RGBA, gl.UNSIGNED_BYTE, p); postMessage(Array.from(p)); }
	else if (m.op === 'opfs-sync') {
		try { const root = await navigator.storage.getDirectory(); const fh = await root.getFileHandle('sync.bin', { create: true });
			const h = await fh.createSyncAccessHandle(); h.truncate(0); h.write(new Uint8Array([1, 2, 3]), { at: 0 }); h.flush(); const b = new Uint8Array(3); h.read(b, { at: 0 }); const sz = h.getSize(); h.close(); postMessage({ data: Array.from(b), size: sz }); }
		catch (err) { postMessage({ error: String(err) }); }
	}
	else if (m.op === 'wasm-thread') { const ia = new Int32Array(m.mem.buffer); Atomics.add(ia, 0, 7); postMessage('ok'); }
};
