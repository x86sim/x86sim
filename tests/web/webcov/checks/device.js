// device APIs: geolocation (the runner sets the simulated location to Apple Park 37.3349,-122.0090), device
// orientation / motion, vibrate, battery, screen orientation, Gamepad, WebAuthn / passkeys, Payment Request / Apple
// Pay, Web Share, Clipboard, Notifications / Push, Wake Lock, Web Speech, misc navigator facts
// Checks that bring up a permission prompt are gesture checks; the runner answers "Allow" and records the prompt.
'use strict';
check('device.geolocation', async () => {
	need(navigator.geolocation, 'no geolocation');
	const p = await withTimeout(new Promise((res, rej) => navigator.geolocation.getCurrentPosition(res, e => rej(new Error('geo error ' + e.code + ' ' + e.message)), { timeout: 20000, maximumAge: 0 })), 25000, 'position');
	near(p.coords.latitude, 37.3349, 0.01, 'lat'); near(p.coords.longitude, -122.009, 0.01, 'lon'); return { lat: +p.coords.latitude.toFixed(4), lon: +p.coords.longitude.toFixed(4), acc: p.coords.accuracy };
}, { gesture: true, timeout: 30000 });
check('device.geolocation-permission-state', async () => { need(navigator.permissions, 'no permissions API'); return (await navigator.permissions.query({ name: 'geolocation' })).state; });
check('device.orientation-motion', async () => {
	need(self.DeviceOrientationEvent, 'no DeviceOrientationEvent');
	if (typeof DeviceOrientationEvent.requestPermission === 'function') { const r = await DeviceOrientationEvent.requestPermission(); if (r !== 'granted') throw new Error('permission ' + r); }
	const e = await withTimeout(new Promise(r => addEventListener('devicemotion', r, { once: true })), 4000, 'devicemotion event').catch(e => null);
	return { requestPermission: typeof DeviceOrientationEvent.requestPermission === 'function', motionEvent: !!e, interval: e && e.interval };
}, { gesture: true, timeout: 15000 });
check('device.vibrate', () => { need(navigator.vibrate, 'no navigator.vibrate'); return navigator.vibrate(10); });
check('device.battery', async () => { need(navigator.getBattery, 'no getBattery'); const b = await navigator.getBattery(); return { level: b.level, charging: b.charging }; });
check('device.screen-orientation', () => { need(screen.orientation, 'no screen.orientation'); return { type: screen.orientation.type, angle: screen.orientation.angle, lock: typeof screen.orientation.lock }; });
check('device.screen-metrics', () => ({ w: screen.width, h: screen.height, dpr: devicePixelRatio, inner: [innerWidth, innerHeight], colorGamut: matchMedia('(color-gamut: p3)').matches ? 'p3' : 'srgb', hdr: matchMedia('(dynamic-range: high)').matches, pointer: matchMedia('(pointer: coarse)').matches ? 'coarse' : 'fine', hover: matchMedia('(hover: hover)').matches }));
check('device.gamepad', () => { need(navigator.getGamepads, 'no getGamepads'); return { pads: Array.from(navigator.getGamepads()).filter(Boolean).length }; });
check('device.webauthn-platform', async () => {
	need(self.PublicKeyCredential, 'no PublicKeyCredential'); const uv = await PublicKeyCredential.isUserVerifyingPlatformAuthenticatorAvailable();
	const cm = PublicKeyCredential.isConditionalMediationAvailable ? await PublicKeyCredential.isConditionalMediationAvailable() : null;
	const caps = PublicKeyCredential.getClientCapabilities ? await PublicKeyCredential.getClientCapabilities() : null; return { uv, conditional: cm, caps: caps && Object.keys(caps).filter(k => caps[k]).length };
});
check('device.webauthn-create', async () => { // a passkey create from a tap: the system sheet comes up (the runner records it), cancelled -> NotAllowedError
	need(self.PublicKeyCredential, 'no PublicKeyCredential');
	try { await withTimeout(navigator.credentials.create({ publicKey: { challenge: new Uint8Array(32), rp: { name: 'webcov' }, user: { id: new Uint8Array(8), name: 'w', displayName: 'w' }, pubKeyCredParams: [{ type: 'public-key', alg: -7 }], authenticatorSelection: { residentKey: 'required', userVerification: 'required' }, timeout: 10000 } }), 15000, 'create');
		return 'created'; } catch (e) { if (/timeout/.test(e.message)) throw e; return e.name; }
}, { gesture: true, timeout: 20000 });
check('device.payment-request-applepay', async () => {
	need(self.PaymentRequest, 'no PaymentRequest'); let can = null; if (self.ApplePaySession) { try { can = { canMakePayments: ApplePaySession.canMakePayments(), version: ApplePaySession.supportsVersion(14) }; } catch (e) { can = e.name; } }   // http://127.0.0.1: InvalidAccessError (Apple Pay needs https) on Apple's too
	let cmp; try {
	const pr = new PaymentRequest([{ supportedMethods: 'https://apple.com/apple-pay', data: { version: 3, merchantIdentifier: 'merchant.webcov', merchantCapabilities: ['supports3DS'], supportedNetworks: ['visa'], countryCode: 'US' } }], { total: { label: 'webcov', amount: { currency: 'USD', value: '1.00' } } });
	cmp = await pr.canMakePayment(); } catch (e) { cmp = e.name; }   // an insecure document: SecurityError at new PaymentRequest
	return { applePaySession: can, canMakePayment: cmp };
});
check('device.web-share', async () => { need(navigator.share, 'no navigator.share'); const can = navigator.canShare({ text: 'webcov' }); const p = navigator.share({ text: 'webcov' }).then(() => 'shared', e => e.name); await sleep(2500); return { canShare: can, sheet: 'opened' }; }, { gesture: true, timeout: 10000, last: true });   // last: its sheet stays up
check('device.clipboard-text', async () => {
	need(navigator.clipboard, 'no navigator.clipboard'); await navigator.clipboard.writeText('webcov-clip');
	let back = null; try { back = await withTimeout(navigator.clipboard.readText(), 4000, 'readText'); } catch (e) { back = e.name || e.message; } return { written: true, readBack: back };
}, { gesture: true, timeout: 10000 });
check('device.clipboard-item', () => { need(self.ClipboardItem, 'no ClipboardItem'); return ClipboardItem.supports ? ['text/plain', 'image/png', 'text/html'].filter(t => ClipboardItem.supports(t)) : true; });
check('device.notifications', async () => {
	need(self.Notification, 'no Notification (Safari tab: only home-screen web apps have it)');
	const p = await Notification.requestPermission(); return { permission: p };
}, { gesture: true, timeout: 20000 });
check('device.push-manager', async () => { need(self.PushManager, 'no PushManager'); const reg = await swReg(); const st = await reg.pushManager.permissionState({ userVisibleOnly: true }); await reg.unregister(); return st; }, { timeout: 20000 });
check('device.wake-lock', async () => { need(navigator.wakeLock, 'no wakeLock'); const l = await navigator.wakeLock.request('screen'); const r = [l.type, l.released]; await l.release(); return eq(r, ['screen', false]); }, { gesture: true, timeout: 10000 });
check('device.speech-synthesis-voices', async () => {
	need(self.speechSynthesis, 'no speechSynthesis'); let v = speechSynthesis.getVoices();
	if (!v.length) await withTimeout(new Promise(r => speechSynthesis.addEventListener('voiceschanged', r, { once: true })), 5000, 'voiceschanged').catch(() => {});
	v = speechSynthesis.getVoices(); ok(v.length > 0, 'no voices'); return { voices: v.length, en: v.filter(x => x.lang.startsWith('en')).length, def: (v.find(x => x.default) || {}).name };
}, { timeout: 8000 });
check('device.speech-synthesis-speak', async () => {
	need(self.speechSynthesis, 'no speechSynthesis'); const u = new SpeechSynthesisUtterance('web coverage'); u.volume = 0.1;
	const st = once(u, 'start', 15000), en = once(u, 'end', 20000, 'error'); speechSynthesis.speak(u); await st; await en; return true;
}, { gesture: true, timeout: 25000 });
check('device.speech-recognition', () => { const SR = self.SpeechRecognition || self.webkitSpeechRecognition; need(SR, 'no SpeechRecognition'); const r = new SR(); r.lang = 'en-US'; return { continuous: 'continuous' in r, onDevice: 'processLocally' in r }; });
check('device.navigator-facts', () => ({ platform: navigator.platform, maxTouchPoints: navigator.maxTouchPoints, hw: navigator.hardwareConcurrency, mem: navigator.deviceMemory || null, standalone: navigator.standalone, cookieEnabled: navigator.cookieEnabled, pdfViewer: navigator.pdfViewerEnabled, webdriver: navigator.webdriver, langs: navigator.languages }));
check('device.permissions-query', async () => { need(navigator.permissions, 'no permissions API'); const r = {}; for (const n of ['geolocation', 'camera', 'microphone', 'notifications', 'push', 'screen-wake-lock']) { try { r[n] = (await navigator.permissions.query(n === 'push' ? { name: n, userVisibleOnly: true } : { name: n })).state; } catch (e) { r[n] = 'error'; } } return r; });
check('device.web-nfc-bluetooth-usb-serial-hid', () => ({ nfc: 'NDEFReader' in self, bluetooth: 'bluetooth' in navigator, usb: 'usb' in navigator, serial: 'serial' in navigator, hid: 'hid' in navigator }));
check('device.idle-detection-contacts', () => ({ idle: 'IdleDetector' in self, contacts: 'contacts' in navigator, badge: 'setAppBadge' in navigator }));
