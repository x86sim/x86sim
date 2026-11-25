// webcov harness: runs the registered checks one by one and POSTs every start / result to the host server
// (serve.py, /r/<run>). A check is check(id, fn, opts): fn returns a JSON value (PASS, recorded as "value"),
// throws Unsupported (the API is missing: UNSUPPORTED), throws anything else (FAIL), or outlives opts.timeout
// (TIMEOUT). A check that was started but has no result when the page loads again (the WebContent process died,
// the runner reloaded) becomes CRASH and the run goes on after it.
// opts.gesture: the check runs from a tap on the full-page "tap" button (user activation: video with sound, PiP,
//   clipboard write, speech, Web Share, permission requests); the runner taps the middle of the screen.
// opts.last: (gesture) run after all other gesture checks
// opts.perf: the value is {score, unit, higher} for the speed comparison.
// Budgets: every check has a hard time limit (opts.timeout, default 10 s; the harness moves on and records TIMEOUT
//   even when the check's own promise never settles). alive() is false once the running check's time is up: bench()
//   and the checks' polling loops stop then, so a cut-off check stops burning CPU / allocating behind the next one.
//   Allocations in checks are fixed-size (the biggest: a 256 MB WebAssembly.Memory, 1 MB digests, 512x512 canvases).
// URL parameters: run=ID (results file), only=PREFIX[,PREFIX] (just those checks), skip=PREFIX[,...], nogesture=1
'use strict';
const WC = { checks: [], byId: {}, errors: [] };
// a check file that does not load (syntax the engine lacks, a top-level throw) is reported in the load event
window.addEventListener('error', e => WC.errors.push((e.filename || '').split('/').pop() + ':' + e.lineno + ' ' + e.message));
class Unsupported extends Error {}
function check(id, fn, opts) { const c = Object.assign({ id, fn, timeout: 10000 }, opts || {}); WC.checks.push(c); WC.byId[id] = c; }
function need(cond, what) { if (!cond) throw new Unsupported(what || 'missing'); }
function eq(actual, expected, label) {
	const a = JSON.stringify(actual), e = JSON.stringify(expected);
	if (a !== e) throw new Error((label ? label + ': ' : '') + 'got ' + a + ', expected ' + e);
	return actual;
}
function ok(cond, msg) { if (!cond) throw new Error(msg || 'assertion failed'); }
function near(a, b, tol, label) { if (!(Math.abs(a - b) <= tol)) throw new Error((label || '') + ' got ' + a + ', expected ' + b + '±' + tol); return a; }
let DEADLINE = Infinity;   // the running check's end (performance.now() time)
const alive = () => performance.now() < DEADLINE;
const sleep = ms => new Promise(r => setTimeout(r, ms));
function withTimeout(p, ms, what) {
	return Promise.race([p, new Promise((_, rej) => setTimeout(() => rej(new Error('timeout: ' + (what || ms + ' ms'))), ms))]);
}
function once(target, ev, ms, errEv) {
	return withTimeout(new Promise((res, rej) => {
		target.addEventListener(ev, e => res(e), { once: true });
		if (errEv) target.addEventListener(errEv, e => rej(new Error(errEv + ': ' + (e.message || (target.error && (target.error.code + ' ' + target.error.message)) || ''))), { once: true });
	}), ms || 8000, ev);
}
// a pixel of a canvas as [r,g,b,a]
function px(canvas, x, y) {
	const c2 = document.createElement('canvas'); c2.width = canvas.width; c2.height = canvas.height;
	const g = c2.getContext('2d'); g.drawImage(canvas, 0, 0); return Array.from(g.getImageData(x, y, 1, 1).data);
}
function colorIs(p, rgb, tol) { return Math.abs(p[0] - rgb[0]) <= tol && Math.abs(p[1] - rgb[1]) <= tol && Math.abs(p[2] - rgb[2]) <= tol; }
function el(tag, attrs, parent) {
	const e = document.createElement(tag); Object.assign(e, attrs || {}); (parent || document.getElementById('stage')).appendChild(e); return e;
}
function clearStage() { const s = document.getElementById('stage'); s.innerHTML = ''; s.removeAttribute('style'); }
const frames = n => new Promise(r => { let i = 0; const f = () => (++i >= n ? r() : requestAnimationFrame(f)); requestAnimationFrame(f); });
// a benchmark: runs fn repeatedly for ~ms, returns ops/s
async function bench(fn, ms) {
	let n = 0; const t0 = performance.now(); let t = t0;
	while ((t = performance.now()) - t0 < (ms || 1000) && t < DEADLINE && n < 1e7) { fn(); n++; }
	return n / ((t - t0) / 1000);
}

const Q = new URLSearchParams(location.search);
const RUN = Q.get('run') || 'adhoc';
const ORIGIN2 = location.protocol + '//' + (location.hostname === 'localhost' ? '127.0.0.1' : 'localhost') + ':' + location.port;
async function post(ev) {
	ev.run = RUN;
	for (let i = 0; i < 3; i++) {
		try { await fetch('/r/' + RUN, { method: 'POST', body: JSON.stringify(ev), headers: { 'Content-Type': 'application/json' }, keepalive: true }); return; }
		catch (e) { await sleep(300); }
	}
}
function show(id, status, detail) {
	const log = document.getElementById('log'); const d = document.createElement('div');
	d.className = 's-' + status; d.textContent = status + ' ' + id + (detail ? ' — ' + String(detail).slice(0, 140) : '');
	log.prepend(d); document.getElementById('count').textContent = ++WC.ndone + ' / ' + WC.total;
}
function tapButton(label) {   // resolves on the next tap anywhere on the full-page button
	const b = document.getElementById('tap'); b.textContent = label; b.style.display = 'flex';
	return new Promise(res => b.addEventListener('click', ev => { b.style.display = 'none'; res(ev); }, { once: true }));
}
async function runOne(c) {
	await post({ ev: 'start', id: c.id, gesture: !!c.gesture, timeout: c.timeout });
	const t0 = performance.now(); let r;
	// a gesture check waits up to 60 s for the runner's tap; its own time starts at the tap
	DEADLINE = t0 + c.timeout + (c.gesture ? 60000 : 0);
	try {
		let p;
		if (c.gesture) {
			// the check runs inside the click handler (transient activation); its promise is awaited after
			p = new Promise((res, rej) => tapButton('tap: ' + c.id).then(() => { try { Promise.resolve(c.fn()).then(res, rej); } catch (e) { rej(e); } }));
		} else p = Promise.resolve().then(() => c.fn());
		const v = await withTimeout(p, c.timeout + (c.gesture ? 60000 : 0), c.timeout + ' ms');
		r = { status: 'PASS', value: v === undefined ? null : v };
	} catch (e) {
		const msg = (e && (e.name ? e.name + ': ' : '') + e.message) || String(e);
		r = { status: e instanceof Unsupported ? 'UNSUPPORTED' : /^Error: timeout: /.test(msg) ? 'TIMEOUT' : 'FAIL', error: msg };
	}
	DEADLINE = -Infinity;   // a check still running after its timeout: alive() is false from here on
	document.getElementById('tap').style.display = 'none';
	r.ms = Math.round(performance.now() - t0);
	try { JSON.stringify(r.value); } catch (e) { r.value = String(r.value); }
	await post(Object.assign({ ev: 'result', id: c.id, perf: !!c.perf, gesture: !!c.gesture }, r));
	show(c.id, r.status, r.error || (r.value !== null && JSON.stringify(r.value)));
	clearStage();
}
async function main() {
	WC.ndone = 0;
	const pre = s => (Q.get(s) || '').split(',').filter(Boolean);
	const only = pre('only'), skip = pre('skip');
	let list = WC.checks.filter(c => (!only.length || only.some(p => c.id.startsWith(p))) && !skip.some(p => c.id.startsWith(p)));
	// non-gesture checks first, gesture checks at the end (the runner starts tapping once the page asks)
	// opts.last: a gesture check whose system UI stays up (the share sheet) runs after every other one
	const g = list.filter(c => c.gesture); list = list.filter(c => !c.gesture).concat(Q.get('nogesture') ? [] : g.filter(c => !c.last).concat(g.filter(c => c.last)));
	WC.total = list.length;
	const st = await (await fetch('/r/' + RUN)).json();
	if (st.started && !st.done.includes(st.started)) {
		await post({ ev: 'result', id: st.started, status: 'CRASH', error: 'the page died during this check (reloaded)' });
		st.done.push(st.started); show(st.started, 'CRASH');
	}
	const done = new Set(st.done);
	await post({ ev: 'log', msg: 'load', ua: navigator.userAgent, resume: done.size, total: list.length,
		errors: WC.errors, registered: WC.checks.length, screen: [screen.width, screen.height, devicePixelRatio], isolated: self.crossOriginIsolated, secure: self.isSecureContext });
	for (const c of list) {
		if (done.has(c.id)) { WC.ndone++; continue; }
		document.getElementById('cur').textContent = c.id;
		await runOne(c);
	}
	document.getElementById('cur').textContent = 'done';
	await post({ ev: 'done', total: list.length });
}
window.addEventListener('load', () => setTimeout(main, 300));
