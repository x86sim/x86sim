// storage: Web Storage, IndexedDB (versions, indexes, cursors, blobs), Cache API, OPFS (async + sync access
// handle in a worker), File System Access pickers (presence), cookies (document + server + HttpOnly), quota
'use strict';
function idbReq(r) { return new Promise((res, rej) => { r.onsuccess = () => res(r.result); r.onerror = () => rej(r.error); }); }
check('storage.localStorage', () => { localStorage.setItem('wc', 'é✓'); const v = localStorage.getItem('wc'); localStorage.removeItem('wc'); sessionStorage.setItem('s', '1'); return eq([v, localStorage.getItem('wc'), sessionStorage.getItem('s')], ['é✓', null, '1']); });
check('storage.localStorage-5mb', () => { const big = 'x'.repeat(1024 * 1024); try { for (let i = 0; i < 4; i++) localStorage.setItem('big' + i, big); return eq(localStorage.getItem('big3').length, 1048576); } finally { for (let i = 0; i < 4; i++) localStorage.removeItem('big' + i); } });
check('storage.indexeddb', async () => {
	await idbReq(indexedDB.deleteDatabase('webcov'));
	const open = indexedDB.open('webcov', 2);
	open.onupgradeneeded = () => { const s = open.result.createObjectStore('items', { keyPath: 'id' }); s.createIndex('byName', 'name'); };
	const db = await idbReq(open);
	const tx = db.transaction('items', 'readwrite'); const st = tx.objectStore('items');
	for (let i = 0; i < 100; i++) st.put({ id: i, name: 'n' + (i % 10), blob: i === 5 ? new Blob(['blobdata']) : null, d: new Date(i) });
	await new Promise((r, j) => { tx.oncomplete = r; tx.onerror = () => j(tx.error); });
	const rt = db.transaction('items').objectStore('items');
	const count = await idbReq(rt.count()), byName = await idbReq(rt.index('byName').getAll('n3')), five = await idbReq(rt.get(5));
	let cur = 0; await new Promise(res => { const c = rt.openCursor(IDBKeyRange.bound(10, 19)); c.onsuccess = () => { if (c.result) { cur++; c.result.continue(); } else res(); }; });
	const blobText = await five.blob.text(); db.close(); await idbReq(indexedDB.deleteDatabase('webcov'));
	return eq([count, byName.length, cur, blobText, five.d.getTime()], [100, 10, 10, 'blobdata', 5]);
});
check('storage.indexeddb-databases', async () => { need(indexedDB.databases, 'no indexedDB.databases'); const db = await idbReq(indexedDB.open('webcov-list', 1)); db.close(); const l = await indexedDB.databases(); await idbReq(indexedDB.deleteDatabase('webcov-list')); return eq(l.some(d => d.name === 'webcov-list'), true); });
check('storage.cache-api', async () => {
	need(self.caches, 'no CacheStorage');
	const c = await caches.open('webcov-t'); await c.put('/virtual/x', new Response('cached!', { headers: { 'Content-Type': 'text/plain' } })); await c.add('/res/data.json');
	const a = await (await c.match('/virtual/x')).text(), b = (await (await c.match('/res/data.json')).json()).webcov, keys = (await c.keys()).length;
	await caches.delete('webcov-t'); return eq([a, b, keys, await caches.has('webcov-t')], ['cached!', 42, 2, false]);
});
check('storage.opfs', async () => {
	need(navigator.storage && navigator.storage.getDirectory, 'no OPFS');
	const root = await navigator.storage.getDirectory(); const dir = await root.getDirectoryHandle('wc', { create: true });
	const fh = await dir.getFileHandle('a.txt', { create: true }); const w = await fh.createWritable(); await w.write('hello opfs'); await w.close();
	const text = await (await fh.getFile()).text(); const names = []; for await (const [n] of dir.entries()) names.push(n);
	await root.removeEntry('wc', { recursive: true }); return eq([text, names], ['hello opfs', ['a.txt']]);
});
check('storage.opfs-sync-access-handle', async () => { const w = new Worker('res/worker.js'); try { const r = await workerCall(w, { op: 'opfs-sync' }); if (r.error) throw new Error(r.error); return eq(r, { data: [1, 2, 3], size: 3 }); } finally { w.terminate(); } });
check('storage.file-system-access-pickers', () => { need(self.showOpenFilePicker, 'no showOpenFilePicker'); return true; });
check('storage.cookies-document', () => { document.cookie = 'wc_doc=1; path=/; SameSite=Lax'; const ok1 = document.cookie.includes('wc_doc=1'); document.cookie = 'wc_doc=; path=/; max-age=0'; return eq([ok1, document.cookie.includes('wc_doc=1')], [true, false]); });
check('storage.cookies-server-httponly', async () => { await fetch('/cookie'); const sent = await (await fetch('/cookies')).text(); return eq([sent.includes('wc_srv=1'), sent.includes('wc_http=1'), document.cookie.includes('wc_http')], [true, true, false]); });
check('storage.cookie-store', async () => { need(self.cookieStore, 'no cookieStore'); await cookieStore.set('wc_cs', 'v'); const c = await cookieStore.get('wc_cs'); await cookieStore.delete('wc_cs'); return eq(c && c.value, 'v'); });
check('storage.quota-estimate', async () => { need(navigator.storage && navigator.storage.estimate, 'no storage.estimate'); const e = await navigator.storage.estimate(); ok(e.quota > 50e6, 'quota ' + e.quota); return { quotaMB: Math.round(e.quota / 1e6), usageKB: Math.round(e.usage / 1e3) }; });
check('storage.persist', async () => { need(navigator.storage && navigator.storage.persisted, 'no storage.persisted'); return { persisted: await navigator.storage.persisted() }; });
check('storage.blob-filereader-objecturl', async () => {
	const b = new Blob(['ab', new Uint8Array([99])], { type: 'text/plain' }); const fr = new FileReader(); const p = new Promise(r => fr.onload = () => r(fr.result)); fr.readAsDataURL(b);
	const u = URL.createObjectURL(b); const t = await (await fetch(u)).text(); URL.revokeObjectURL(u);
	return eq([await p, t, b.size, await new File(['x'], 'f.txt').text()], ['data:text/plain;base64,YWJj', 'abc', 3, 'x']);
});
