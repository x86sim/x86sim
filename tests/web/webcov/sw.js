// service worker: caches res/data.json at install, answers /sw-virtual with a synthesized response, serves
// cache hits first; skipWaiting + clients.claim so the page is controlled at once
const C = 'webcov-v1';
self.addEventListener('install', (e) => { e.waitUntil(caches.open(C).then((c) => c.add('/res/data.json')).then(() => self.skipWaiting())); });
self.addEventListener('activate', (e) => { e.waitUntil(self.clients.claim()); });
self.addEventListener('fetch', (e) => {
	const u = new URL(e.request.url);
	if (u.pathname === '/sw-virtual') return e.respondWith(new Response(JSON.stringify({ fromSW: true }), { headers: { 'Content-Type': 'application/json', 'Cross-Origin-Resource-Policy': 'cross-origin' } }));
	if (u.pathname === '/res/data.json') return e.respondWith(caches.match(e.request).then((r) => r ? new Response(r.body, { headers: { 'Content-Type': 'application/json', 'X-From-Cache': '1', 'Cross-Origin-Resource-Policy': 'cross-origin' } }) : fetch(e.request)));
});
self.addEventListener('message', (e) => { e.source.postMessage({ pong: e.data, sw: true }); });
self.addEventListener('push', (e) => {});
