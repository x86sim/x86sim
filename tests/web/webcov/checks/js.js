// JS engine: ECMAScript features (each runs code and checks its result), Intl / ICU data, WebCrypto, WebAssembly
// (hand-assembled modules: MVP, SIMD, threads, exception handling legacy + exnref, GC, tail calls, i64/BigInt)
'use strict';
// syntax the parser may not know stays out of this file: a SyntaxError there means the engine lacks the feature
const js = (src) => { let f; try { f = new Function(src); } catch (e) { if (e instanceof SyntaxError) throw new Unsupported('syntax: ' + e.message); throw e; } return f(); };
const wasm = (hex) => new Uint8Array(hex.replace(/\s+/g, '').match(/../g).map(h => parseInt(h, 16)));
const WASM_HDR = '0061736d01000000';
async function wasmRun(hex, name, args, imports) {
	need(typeof WebAssembly === 'object', 'no WebAssembly');
	const bytes = wasm(WASM_HDR + hex);
	if (!WebAssembly.validate(bytes)) throw new Unsupported('module does not validate');
	const { instance } = await WebAssembly.instantiate(bytes, imports || {});
	return instance.exports[name || 'f'](...(args || []));
}

// --- ES2022..ES2026 ---
check('js.es2022.class-private-static', () => eq(js(`class A { #x = 1; static #n = 2; static { A.s = A.#n + 1; } #m() { return this.#x + 40; } get v() { return this.#m() + A.s; } static has(o) { return #x in o; } } return [new A().v, A.has(new A()), A.has({})];`), [44, true, false]));
check('js.es2022.at-hasOwn-cause', () => eq([[1, 2, 3].at(-1), 'abc'.at(-2), Object.hasOwn({ a: 1 }, 'a'), new Error('x', { cause: 7 }).cause], [3, 'b', true, 7]));
check('js.es2022.regexp-d-flag', () => eq(/b(c)/d.exec('abc').indices[1], [2, 3]));
check('js.es2023.change-array-by-copy', () => { const a = [3, 1, 2]; return eq([a.toSorted(), a.toReversed(), a.with(0, 9), a.toSpliced(1, 1), a, a.findLast(x => x > 1), a.findLastIndex(x => x > 5)], [[1, 2, 3], [2, 1, 3], [9, 1, 2], [3, 2], [3, 1, 2], 2, -1]); });
check('js.es2024.groupBy', () => { const g = Object.groupBy([1, 2, 3, 4], x => x % 2 ? 'o' : 'e'); const m = Map.groupBy([1, 2, 3], x => x > 1); return eq([g.o, g.e, m.get(true)], [[1, 3], [2, 4], [2, 3]]); });
check('js.es2024.promise-withResolvers', async () => { const { promise, resolve } = Promise.withResolvers(); setTimeout(() => resolve(5), 1); return eq(await promise, 5); });
check('js.es2024.arraybuffer-resize-transfer', () => { const b = new ArrayBuffer(4, { maxByteLength: 16 }); b.resize(12); const t = b.transfer(8); return eq([t.byteLength, b.detached, t.resizable, new ArrayBuffer(2).transferToFixedLength().byteLength], [8, true, true, 2]); });
check('js.es2024.string-wellformed', () => eq(['a\uD800'.isWellFormed(), 'a\uD800'.toWellFormed().charCodeAt(1)], [false, 0xFFFD]));
check('js.es2024.regexp-v-flag', () => eq([/[\p{L}--[a-z]]/v.test('a'), /[\p{L}--[a-z]]/v.test('A'), /^\p{RGI_Emoji}$/v.test('👨‍👩‍👧')], [false, true, true]));
check('js.es2024.array-fromAsync', async () => eq(await Array.fromAsync((async function* () { yield 1; yield 2; })()), [1, 2]));
check('js.es2024.atomics-waitAsync', async () => { need(Atomics.waitAsync, 'no Atomics.waitAsync'); need(typeof SharedArrayBuffer === 'function', 'no SharedArrayBuffer'); const ia = new Int32Array(new SharedArrayBuffer(4)); const r = Atomics.waitAsync(ia, 0, 0, 1000); setTimeout(() => Atomics.notify(ia, 0), 10); return eq([r.async, await r.value], [true, 'ok']); });
check('js.es2025.iterator-helpers', () => eq([...[1, 2, 3, 4].values().filter(x => x % 2 === 0).map(x => x * 10)].concat(Iterator.from([5, 6]).take(1).toArray()), [20, 40, 5]));
check('js.es2025.set-methods', () => { const a = new Set([1, 2, 3]), b = new Set([2, 3, 4]); return eq([[...a.union(b)], [...a.intersection(b)], [...a.difference(b)], [...a.symmetricDifference(b)], a.isSubsetOf(new Set([1, 2, 3, 4])), a.isDisjointFrom(new Set([9]))], [[1, 2, 3, 4], [2, 3], [1], [1, 4], true, true]); });
check('js.es2025.promise-try', async () => eq(await Promise.try((a, b) => a + b, 2, 3), 5));
check('js.es2025.regexp-escape', () => eq(RegExp.escape('a.b*c'), '\\x61\\.b\\*c'));
check('js.es2025.regexp-modifiers', () => eq([js(`return /a(?i:b)c/.test('aBc')`), js(`return /a(?i:b)c/.test('aBC')`)], [true, false]));
check('js.es2025.regexp-dup-named-groups', () => eq(js(`const m = /(?<y>\\d{4})-x|x-(?<y>\\d{4})/.exec('x-2025'); return m.groups.y;`), '2025'));
check('js.es2025.float16', () => { need(typeof Float16Array === 'function', 'no Float16Array'); const f = new Float16Array([1.337]); return eq([Math.f16round(1.337), f[0], new DataView(new ArrayBuffer(2)).getFloat16(0)], [1.3369140625, 1.3369140625, 0]); });
check('js.es2025.json-modules', async () => eq((await import('../res/data.json', { with: { type: 'json' } })).default.webcov, 42));
check('js.es2025.dynamic-import-module', async () => eq((await import('../res/mod.mjs')).answer(), 42));
check('js.es2026.error-isError', () => { need(Error.isError, 'no Error.isError'); return eq([Error.isError(new TypeError()), Error.isError({ message: 'x' })], [true, false]); });
check('js.es2026.math-sumPrecise', () => { need(Math.sumPrecise, 'no Math.sumPrecise'); return eq(Math.sumPrecise([1e20, 0.1, -1e20]), 0.1); });
check('js.es2026.uint8array-base64', () => { need(Uint8Array.fromBase64, 'no Uint8Array.fromBase64'); return eq([new Uint8Array([104, 105]).toBase64(), Array.from(Uint8Array.fromBase64('aGk=')), new Uint8Array([255, 1]).toHex()], ['aGk=', [104, 105], 'ff01']); });
check('js.es2026.explicit-resource-mgmt', () => eq(js(`const log = []; { using r = { [Symbol.dispose]() { log.push('d'); } }; log.push('b'); } return log;`), ['b', 'd']));
check('js.es2026.temporal', () => { need(typeof Temporal === 'object', 'no Temporal'); return eq(Temporal.PlainDate.from('2025-01-31').add({ months: 1 }).toString(), '2025-02-28'); });
check('js.es2026.array-fromAsync-iterator-sequencing', () => { need(Iterator.concat, 'no Iterator.concat'); return eq([...Iterator.concat([1], [2, 3])], [1, 2, 3]); });
check('js.weakref-finalization', () => { const w = new WeakRef({}); const f = new FinalizationRegistry(() => {}); f.register({}, 1); return eq(typeof w.deref(), 'object'); });
check('js.async-generators-await', async () => { let s = 0; for await (const x of (async function* () { yield 1; await sleep(1); yield 2; })()) s += x; return eq(s, 3); });
check('js.structuredClone', () => { const o = { d: new Date(0), m: new Map([[1, 2]]), b: new Uint8Array([1]), r: /x/g }; o.self = o; const c = structuredClone(o); return eq([c.d.getTime(), c.m.get(1), c.b[0], c.r.flags, c.self === c], [0, 2, 1, 'g', true]); });
check('js.shared-array-buffer', () => { need(self.crossOriginIsolated, 'not crossOriginIsolated (COOP/COEP)'); need(typeof SharedArrayBuffer === 'function', 'no SharedArrayBuffer'); const ia = new Int32Array(new SharedArrayBuffer(8)); Atomics.add(ia, 0, 5); Atomics.store(ia, 1, 7); return eq([Atomics.load(ia, 0), Atomics.compareExchange(ia, 1, 7, 9), ia[1], Atomics.wait ? 'wait' : 'nowait'], [5, 7, 9, 'wait']); });
check('js.cross-origin-isolated', () => eq([self.crossOriginIsolated, self.isSecureContext], [true, true]));
check('js.eval-jit-hot-loop', () => { // a JIT-tier smoke: a hot numeric loop gives the exact answer (and its speed is in perf.*)
	const f = new Function('n', 'let s = 0; for (let i = 0; i < n; i++) s = (s + i * i) % 1000003; return s;');
	for (let i = 0; i < 50; i++) f(1000); return eq(f(2000000), (() => { let s = 0; for (let i = 0; i < 2000000; i++) s = (s + i * i) % 1000003; return s; })());
});
check('js.bigint-math', () => eq([(2n ** 100n).toString(16), BigInt.asUintN(8, 257n), (-7n) / 2n].map(String), ['10000000000000000000000000', '1', '-3']));
check('js.date-tz', () => { const tz = Intl.DateTimeFormat().resolvedOptions().timeZone; ok(tz && tz.includes('/') || tz === 'UTC', 'time zone ' + tz); return { tz, offset: new Date(2025, 6, 1).getTimezoneOffset() }; });

// --- Intl / ICU data ---
check('js.intl.datetime', () => eq(new Intl.DateTimeFormat('en-US', { timeZone: 'UTC', dateStyle: 'medium', timeStyle: 'short' }).format(Date.UTC(2025, 0, 2, 15, 4)).replace(/[\u202f\u00a0]/g, ' '), 'Jan 2, 2025 at 3:04 PM'));   // Safari 27 (ICU's en-US medium date + short time: "at")
check('js.intl.datetime-tzdata', () => eq(new Intl.DateTimeFormat('en-US', { timeZone: 'Asia/Kolkata', hour: 'numeric', minute: 'numeric', hourCycle: 'h23' }).format(Date.UTC(2025, 0, 1, 0, 0)), '05:30'));
check('js.intl.number', () => eq([new Intl.NumberFormat('de-DE').format(1234567.891), new Intl.NumberFormat('en-IN').format(1234567), new Intl.NumberFormat('ja-JP', { style: 'currency', currency: 'JPY' }).format(5000), new Intl.NumberFormat('en', { notation: 'compact' }).format(12345)], ['1.234.567,891', '12,34,567', '¥5,000', '12K']));   // ja-JP JPY: halfwidth ¥ in Safari 27
check('js.intl.segmenter', () => eq([[...new Intl.Segmenter('en', { granularity: 'grapheme' }).segment('👨‍👩‍👧e\u0301')].length, [...new Intl.Segmenter('ja', { granularity: 'word' }).segment('今日は良い天気')].filter(s => s.isWordLike).length > 1], [2, true]));
check('js.intl.collator-plural-relative', () => eq([['ä', 'z', 'a'].sort(new Intl.Collator('de').compare).join(''), ['ä', 'z', 'a'].sort(new Intl.Collator('sv').compare).join(''), new Intl.PluralRules('ar').select(3), new Intl.RelativeTimeFormat('es', { numeric: 'auto' }).format(-1, 'day')], ['aäz', 'azä', 'few', 'ayer']));
check('js.intl.displaynames-list', () => eq([new Intl.DisplayNames(['fr'], { type: 'region' }).of('DE'), new Intl.ListFormat('en', { type: 'conjunction' }).format(['a', 'b', 'c']), new Intl.DisplayNames(['en'], { type: 'currency' }).of('EUR')], ['Allemagne', 'a, b, and c', 'Euro']));
check('js.intl.durationformat', () => { need(Intl.DurationFormat, 'no Intl.DurationFormat'); return eq(new Intl.DurationFormat('en', { style: 'short' }).format({ hours: 1, minutes: 5 }), '1 hr, 5 min'); });
check('js.intl.locale-info', () => { const l = new Intl.Locale('en-US'); const wi = l.getWeekInfo ? l.getWeekInfo() : l.weekInfo; return eq([wi && wi.firstDay, navigator.language.length >= 2], [7, true]); });
check('js.intl.supported-values', () => { need(Intl.supportedValuesOf, 'no supportedValuesOf'); const tz = Intl.supportedValuesOf('timeZone'); ok(tz.length > 300, tz.length + ' zones'); return { zones: tz.length, currencies: Intl.supportedValuesOf('currency').length, calendars: Intl.supportedValuesOf('calendar').length }; });
check('js.textencoder-decoders', () => eq([Array.from(new TextEncoder().encode('é')), new TextDecoder('shift_jis').decode(new Uint8Array([0x82, 0xa0])), new TextDecoder('windows-1252').decode(new Uint8Array([0x80])), new TextDecoder('gbk').decode(new Uint8Array([0xc4, 0xe3]))], [[195, 169], 'あ', '€', '你']));
check('js.url-urlpattern', () => { const u = new URL('https://a.example/p?q=1#h'); const r = [u.searchParams.get('q'), URL.canParse('x:/y'), URL.parse ? URL.parse('nope') : 'noparse']; need(typeof URLPattern === 'function', 'no URLPattern (URL ok: ' + JSON.stringify(r) + ')'); return eq([...r, new URLPattern({ pathname: '/books/:id' }).exec('https://e.x/books/7').pathname.groups.id], ['1', true, null, '7']); });

// --- WebCrypto ---
const hex = b => Array.from(new Uint8Array(b)).map(x => x.toString(16).padStart(2, '0')).join('');
check('js.crypto.random-uuid', () => { const u = crypto.randomUUID(); ok(/^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/.test(u), u); const a = crypto.getRandomValues(new Uint8Array(32)); ok(a.some(x => x), 'all zero'); return true; });
check('js.crypto.sha', async () => eq([hex(await crypto.subtle.digest('SHA-256', new TextEncoder().encode('abc'))), hex(await crypto.subtle.digest('SHA-1', new TextEncoder().encode('abc')))], ['ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad', 'a9993e364706816aba3e25717850c26c9cd0d89d']));
check('js.crypto.aes-gcm', async () => { const k = await crypto.subtle.generateKey({ name: 'AES-GCM', length: 256 }, true, ['encrypt', 'decrypt']); const iv = new Uint8Array(12); const c = await crypto.subtle.encrypt({ name: 'AES-GCM', iv }, k, new TextEncoder().encode('webcov')); return eq([c.byteLength, new TextDecoder().decode(await crypto.subtle.decrypt({ name: 'AES-GCM', iv }, k, c))], [22, 'webcov']); });
check('js.crypto.ecdsa-p256', async () => { const k = await crypto.subtle.generateKey({ name: 'ECDSA', namedCurve: 'P-256' }, false, ['sign', 'verify']); const d = new TextEncoder().encode('m'); const s = await crypto.subtle.sign({ name: 'ECDSA', hash: 'SHA-256' }, k.privateKey, d); return eq(await crypto.subtle.verify({ name: 'ECDSA', hash: 'SHA-256' }, k.publicKey, s, d), true); });
check('js.crypto.rsa-pss', async () => { const k = await crypto.subtle.generateKey({ name: 'RSA-PSS', modulusLength: 2048, publicExponent: new Uint8Array([1, 0, 1]), hash: 'SHA-256' }, false, ['sign', 'verify']); const d = new TextEncoder().encode('m'); const s = await crypto.subtle.sign({ name: 'RSA-PSS', saltLength: 32 }, k.privateKey, d); return eq(await crypto.subtle.verify({ name: 'RSA-PSS', saltLength: 32 }, k.publicKey, s, d), true); }, { timeout: 20000 });
check('js.crypto.ed25519-x25519', async () => { const k = await crypto.subtle.generateKey({ name: 'Ed25519' }, false, ['sign', 'verify']); const s = await crypto.subtle.sign('Ed25519', k.privateKey, new Uint8Array([1])); const a = await crypto.subtle.generateKey({ name: 'X25519' }, false, ['deriveBits']), b = await crypto.subtle.generateKey({ name: 'X25519' }, false, ['deriveBits']); const s1 = hex(await crypto.subtle.deriveBits({ name: 'X25519', public: b.publicKey }, a.privateKey, 256)), s2 = hex(await crypto.subtle.deriveBits({ name: 'X25519', public: a.publicKey }, b.privateKey, 256)); return eq([await crypto.subtle.verify('Ed25519', k.publicKey, s, new Uint8Array([1])), s1 === s2], [true, true]); });
check('js.crypto.pbkdf2-hkdf', async () => { const k = await crypto.subtle.importKey('raw', new TextEncoder().encode('password'), 'PBKDF2', false, ['deriveBits']); return eq(hex(await crypto.subtle.deriveBits({ name: 'PBKDF2', salt: new TextEncoder().encode('salt'), iterations: 1, hash: 'SHA-1' }, k, 160)), '0c60c80f961f0e71f3a9b524af6012062fe037a6'); });

// --- WebAssembly ---
check('js.wasm.mvp', async () => eq(await wasmRun('0107016002 7f7f017f 03020100 0707010361646400 00 0a09010700200020016a0b', 'add', [40, 2]), 42));
check('js.wasm.streaming', async () => { const r = new Response(wasm(WASM_HDR + '0107016002 7f7f017f 03020100 0707010361646400 00 0a09010700200020016a0b'), { headers: { 'Content-Type': 'application/wasm' } }); const { instance } = await WebAssembly.instantiateStreaming(r); return eq(instance.exports.add(1, 2), 3); });
check('js.wasm.i64-bigint', async () => eq(String(await wasmRun('010601600 17e017e 03020100 070501016600 00 0a0901070020004201 7c0b', 'f', [41n])), '42'));
check('js.wasm.simd128', async () => eq(await wasmRun('0107016002 7f7f017f 03020100 070501016600 00 0a120110 00 2000fd11 2001fd11 fdae01 fd1b02 0b', 'f', [40, 2]), 42));
check('js.wasm.threads-shared-memory', async () => { need(self.crossOriginIsolated && typeof SharedArrayBuffer === 'function', 'no SharedArrayBuffer'); const m = new WebAssembly.Memory({ initial: 1, maximum: 1, shared: true }); ok(m.buffer instanceof SharedArrayBuffer, 'shared memory buffer is not a SharedArrayBuffer'); const b = wasm(WASM_HDR + '010501600 0017f 03020100 0504010301 01 070501016600 00 0a0c010a00 4100 4105 fe1e0200 0b'); if (!WebAssembly.validate(b)) throw new Unsupported('atomics module does not validate'); const { instance } = await WebAssembly.instantiate(b); return eq([instance.exports.f(), instance.exports.f()], [0, 5]); });
check('js.wasm.exceptions-legacy', async () => eq(await wasmRun('0109026001 7f00 6000017f 03020101 0d03010000 070501016600 00 0a0d010b 00 067f 412a 0800 0700 0b 0b'), 42));
check('js.wasm.exceptions-exnref', async () => eq(await wasmRun('0109026001 7f00 6000017f 03020101 0d03010000 070501016600 00 0a1301110 0027f 1f40 01 000000 4107 0800 0b 00 0b 0b'), 7));
check('js.wasm.gc-struct', async () => eq(await wasmRun('0109025f01 7f00 6000017f 03020101 070501016600 00 0a0d010b 00 4109 fb0000 fb020000 0b'), 9));
check('js.wasm.tail-call', async () => eq(await wasmRun('010501600 0017f 0303020000 070501016600 00 0a0b02 04001201 0b 04004103 0b'), 3));
check('js.wasm.js-exception-tag', () => { need(WebAssembly.Tag && WebAssembly.Exception, 'no WebAssembly.Tag/Exception'); const t = new WebAssembly.Tag({ parameters: ['i32'] }); const e = new WebAssembly.Exception(t, [5]); return eq([e.is(t), e.getArg(t, 0)], [true, 5]); });
check('js.wasm.jspi', () => { need(WebAssembly.Suspending, 'no JSPI (WebAssembly.Suspending)'); return true; });
check('js.wasm.memory-grow-big', () => { const m = new WebAssembly.Memory({ initial: 1, maximum: 16384 }); m.grow(4095); return eq(m.buffer.byteLength, 256 * 1024 * 1024); });
