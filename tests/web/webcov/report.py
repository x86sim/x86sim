#!/usr/bin/env python3
# webcov report: one run's results (the server's <run>.jsonl) as a table, or x86Sim vs Apple's simulator:
#   report.py X86.jsonl                      summary + every non-PASS check
#   report.py X86.jsonl [X86b.jsonl...] --ref APPLE.jsonl [--ref APPLE2.jsonl...] [--md OUT.md] [--json OUT.json] [--notes notes.json]
# Several runs of one side: per check the best result (PASS over the rest; else the latest), so a flake on Apple's
# side is not counted for x86Sim and one on x86Sim's not against it; checks that disagree between runs of a side are
# listed as flaky.
# Classes per check: same (equal status), gap (Apple PASS, x86Sim not), apple-fails (both fail / unsupported the same
# way is "same"; Apple not PASS and x86Sim not PASS but different: "apple-also-fails"), better (x86Sim PASS, Apple not); unverified: a gesture check (a tap and usually a permission prompt) that timed
# out on Apple's side: the Mac runner's taps did not answer Apple's prompt, so there is no reference.
# Perf checks: ratio = x86Sim score / Apple score (1.0 = as fast as Apple's simulator on the Mac).
# --notes: {"check id": "diagnosis"} merged into the gap rows.
import argparse, json, sys

def load_many(paths):
	res, meta, seen = {}, {}, {}
	for p in paths:
		r1, m1 = load(p); meta = meta or m1
		for k, v in r1.items():
			seen.setdefault(k, set()).add(v['status'])
			if k not in res or res[k]['status'] != 'PASS': res[k] = v   # a PASS stays; otherwise the latest
	return res, meta, sorted(k for k, s in seen.items() if len(s) > 1)

def load(p):
	res, meta = {}, {}
	for line in open(p):
		ev = json.loads(line)
		if ev.get('ev') == 'result': res[ev['id']] = ev          # a rerun of a check replaces its result
		elif ev.get('ev') == 'log' and ev.get('msg') == 'load' and not meta: meta = ev
		elif ev.get('ev') == 'meta': meta.setdefault('runner', {}).update(ev)
	return res, meta

def short(r):
	if r is None: return 'missing'
	s = r['status']
	if s == 'PASS':
		v = r.get('value')
		if isinstance(v, dict) and 'score' in v: return '%s %s' % (v['score'], v.get('unit', ''))
		return 'PASS'
	return s

def detail(r):
	if r is None: return ''
	if r['status'] == 'PASS':
		v = json.dumps(r.get('value'), ensure_ascii=False)
		return '' if v in ('true', 'null') else v
	return r.get('error') or ''

def esc(s, n=160): s = str(s).replace('|', '\\|').replace('\n', ' '); return s if len(s) <= n else s[:n - 1] + '…'

def main():
	ap = argparse.ArgumentParser(); ap.add_argument('x86', nargs='+'); ap.add_argument('--ref', action='append'); ap.add_argument('--md'); ap.add_argument('--json'); ap.add_argument('--notes')
	ap.add_argument('--title', default='x86Sim'); A = ap.parse_args()
	x, xm, xflaky = load_many(A.x86)
	if not A.ref:
		from collections import Counter
		c = Counter(r['status'] for r in x.values()); print('%d checks: %s' % (len(x), ', '.join('%s %d' % kv for kv in sorted(c.items()))))
		for k, r in sorted(x.items()):
			if r['status'] != 'PASS': print('%-11s %-45s %s' % (r['status'], k, esc(detail(r), 140)))
			elif r.get('perf'): print('%-11s %-45s %s' % ('PERF', k, short(r)))
		return
	a, am, aflaky = load_many(A.ref); notes = json.load(open(A.notes)) if A.notes else {}
	rows, cls_count = [], {'same': 0, 'gap': 0, 'apple-also-fails': 0, 'better': 0, 'unverified': 0}
	perf = []
	for k in sorted(set(x) | set(a), key=lambda k: (k.split('.')[0], k)):
		rx, ra = x.get(k), a.get(k)
		if (rx and rx.get('perf')) or (ra and ra.get('perf')):
			sx = rx and rx['status'] == 'PASS' and rx['value']['score']; sa = ra and ra['status'] == 'PASS' and ra['value']['score']
			perf.append({'id': k, 'x86': sx or None, 'apple': sa or None, 'unit': ((rx or ra).get('value') or {}).get('unit') if (rx or ra)['status'] == 'PASS' else '',
				'ratio': round(sx / sa, 2) if sx and sa else None, 'x86_status': rx and rx['status'], 'apple_status': ra and ra['status']})
			continue
		px, pa = (rx and rx['status'] == 'PASS'), (ra and ra['status'] == 'PASS')
		if ra and ra['status'] == 'TIMEOUT' and ra.get('gesture') and not (rx and rx['status'] == 'TIMEOUT'): c = 'unverified'
		elif (rx and rx['status']) == (ra and ra['status']): c = 'same'
		elif pa and not px: c = 'gap'
		elif px and not pa: c = 'better'
		else: c = 'apple-also-fails'
		cls_count[c] += 1
		vdiff = bool(px and pa and json.dumps(rx.get('value'), sort_keys=True) != json.dumps(ra.get('value'), sort_keys=True))
		rows.append({'id': k, 'class': c, 'x86': rx and rx['status'], 'apple': ra and ra['status'], 'x86_detail': detail(rx), 'apple_detail': detail(ra),
			'value_differs': vdiff, 'gesture': bool((rx or ra).get('gesture')), 'note': notes.get(k, '')})
	geo = lambda l: (lambda v: round(v, 2) if v else None)(__import__('math').prod(l) ** (1 / len(l)) if l else None)
	ratios = [p['ratio'] for p in perf if p['ratio']]
	out = {'x86': {'ua': xm.get('ua'), 'screen': xm.get('screen')}, 'apple': {'ua': am.get('ua'), 'screen': am.get('screen')},
		'counts': cls_count, 'flaky': {'x86': xflaky, 'apple': aflaky}, 'checks': rows, 'perf': perf, 'perf_geomean_ratio': geo(ratios)}
	if A.json: json.dump(out, open(A.json, 'w'), indent=1, ensure_ascii=False)
	L = []
	L.append('| | count |\n|---|---|')
	for c in ('same', 'gap', 'apple-also-fails', 'better', 'unverified'): L.append('| %s | %d |' % (c, cls_count[c]))
	L.append('\n### Gaps (Apple PASS, x86Sim not)\n\n| check | x86Sim | detail | diagnosis |\n|---|---|---|---|')
	for r in rows:
		if r['class'] == 'gap': L.append('| `%s` | %s | %s | %s |' % (r['id'], r['x86'], esc(r['x86_detail']), esc(r['note'], 400)))
	L.append('\n### Performance (higher is better; ratio = x86Sim / Apple)\n\n| benchmark | x86Sim | Apple | ratio |\n|---|---|---|---|')
	for p in perf: L.append('| `%s` | %s | %s | %s |' % (p['id'], p['x86'] if p['x86'] else p['x86_status'], p['apple'] if p['apple'] else p['apple_status'], p['ratio'] or '-'))
	L.append('| geometric mean | | | **%s** |' % out['perf_geomean_ratio'])
	L.append('\n### All checks\n\n| check | x86Sim | Apple | class | x86Sim detail | Apple detail |\n|---|---|---|---|---|---|')
	for r in rows:
		L.append('| `%s`%s | %s | %s | %s | %s | %s |' % (r['id'], ' (tap)' if r['gesture'] else '', r['x86'], r['apple'], r['class'] + (' (values differ)' if r['value_differs'] else ''), esc(r['x86_detail'], 90), esc(r['apple_detail'], 90)))
	md = '\n'.join(L) + '\n'
	if A.md: open(A.md, 'w').write(md)
	else: sys.stdout.write(md)
	L.insert(0, '')
	print('same %(same)d, gap %(gap)d, apple-also-fails %(apple-also-fails)d, better %(better)d, unverified %(unverified)d' % cls_count, 'perf geomean', out['perf_geomean_ratio'], file=sys.stderr)

if __name__ == '__main__': main()
