#!/usr/bin/env python3
"""x86Sim: end-to-end checks of the agent tools (x86sim-mcp over stdio: snapshot, tap, scroll, open_url, ...) on a
running phone. Covers what host/simax + tools/launcher/core/src/ax.rs promise (docs/protocols/SIMAX.md):

  refs     a ref stays with its element across scrolls, also for repeated web text ("by", "|" on every row)
  iframes  web content inside a same-origin and a cross-origin iframe is listed, placed, and taps reach it
  land     taps land in landscape: Settings turned with the phone, the portrait-only home screen on a phone held sideways
  term     app_terminate is fast (simctl terminate: well under the old ~6 s)
  latency  snapshot time on a heavy screen (Settings search results, a long web page)
  busy     a busy WebContent process is named in the snapshot, the rest still comes
  duo      (an iPhone Duo) snapshot / tap on the outer screen and, unfolded, on the inner screen

Usage: tests/ax/mcp_e2e.py --phone NAME [--mcp PATH] [--only refs,iframes,...] [--out DIR]
The web checks serve test pages from this machine (the phone reaches it at 127.0.0.1). Exit status: failed checks.
Use a phone of your own: the checks rotate, fold, open and terminate apps.
"""
import argparse
import http.server
import json
import os
import re
import socketserver
import subprocess
import sys
import threading
import time

ROOT = os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))


class Mcp:
    def __init__(self, path, phone, log):
        self.p = subprocess.Popen([path], stdin=subprocess.PIPE, stdout=subprocess.PIPE, text=True, bufsize=1)
        self.phone, self.log, self.n = phone, log, 0
        self.rpc("initialize", {"protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": {"name": "mcp_e2e", "version": "1"}})

    def rpc(self, method, params):
        self.n += 1
        self.p.stdin.write(json.dumps({"jsonrpc": "2.0", "id": self.n, "method": method, "params": params}) + "\n")
        while True:
            line = self.p.stdout.readline()
            if not line:
                raise RuntimeError("x86sim-mcp exited")
            m = json.loads(line)
            if m.get("id") == self.n:
                return m

    def call(self, tool, **args):
        """(text, structured, error, seconds)"""
        args.setdefault("phone", self.phone)
        t0 = time.monotonic()
        r = self.rpc("tools/call", {"name": tool, "arguments": args})["result"]
        dt = time.monotonic() - t0
        text = "\n".join(c.get("text", "") for c in r.get("content", []) if c.get("type") == "text")
        self.log.write("### %s %s (%.2f s)\n%s\n\n" % (tool, json.dumps(args), dt, text))
        self.log.flush()
        return text, r.get("structuredContent"), bool(r.get("isError")), dt

    def snap(self, **kw):
        text, st, err, dt = self.call("snapshot", json=True, **kw)
        if err:
            raise RuntimeError("snapshot: " + text)
        return st, dt

    def close(self):
        self.p.stdin.close()
        self.p.wait(10)


def els(s, **kw):
    out = []
    for e in s["elements"]:
        if all((re.search(v, e.get(k) or "") if isinstance(v, str) else e.get(k) == v) for k, v in kw.items()):
            out.append(e)
    return out


def center(e):
    x, y, w, h = e["frame"]
    return x + w / 2, y + h / 2


# ---------------------------------------------------------------------------------------------------------------------
# test pages
ROWS = "".join('<tr><td><a href="#i%d">Item %d</a><br><span class=sub>%d points by <a href="#u%d">user%d</a> | <a href="#h">hide</a> | ' \
               '<span>by</span> <span>|</span></span></td></tr>' % (i, i, i * 3, i, i) for i in range(1, 80))
PAGES = {
    "/rows.html": "<!doctype html><meta name=viewport content='width=device-width'><title>Rows</title><table>%s</table>" % ROWS,
    "/frames.html": """<!doctype html><meta name=viewport content='width=device-width'><title>Frames</title>
<h1>Outer page</h1><p>Before the frames</p>
<iframe src="/inner.html?same" style="width:300px;height:140px;border:4px solid #888;padding:6px;margin-left:20px"></iframe>
<iframe src="http://localhost:%(port2)d/inner.html?cross" style="width:300px;height:140px;border:2px solid #888;margin-top:10px"></iframe>
<p>After the frames</p>""",
    "/long.html": "<!doctype html><meta name=viewport content='width=device-width'><title>Long</title>" +
    "".join("<p><a href='#p%d'>Paragraph %d</a> some text by <b>someone</b> | <i>%d comments</i></p>" % (i, i, i) for i in range(3000)),
    # the WebContent process busy for 20 s, 2 s after the page loaded
    "/busy.html": """<!doctype html><meta name=viewport content='width=device-width'><title>Busy</title><p>Busy page</p>
<script>setTimeout(() => { const t = Date.now(); while (Date.now() - t < 20000) {} document.title = 'Done'; }, 2000)</script>""",
    "/inner.html": """<!doctype html><title>Inner</title><body style="margin:8px">
<button id=b onclick="this.textContent='Clicked ' + location.search.slice(1)">Press inner</button>
<p>Inside frame text</p><script>document.title = 'Inner ' + location.search.slice(1)</script>""",
}


def serve(port_holder):
    class H(http.server.BaseHTTPRequestHandler):
        def do_GET(self):
            path = self.path.split("?")[0]
            body = PAGES.get(path)
            if body is None:
                self.send_error(404)
                return
            body = (body % {"port2": port_holder[1]} if "%(port2)" in body else body).encode()
            self.send_response(200)
            self.send_header("Content-Type", "text/html; charset=utf-8")
            self.send_header("Content-Length", str(len(body)))
            self.send_header("Cache-Control", "no-store")
            self.end_headers()
            self.wfile.write(body)

        def log_message(self, *a):
            pass

    servers = []
    for _ in range(2):
        s = socketserver.ThreadingTCPServer(("0.0.0.0", 0), H)
        s.daemon_threads = True
        threading.Thread(target=s.serve_forever, daemon=True).start()
        servers.append(s)
        port_holder.append(s.server_address[1])
    return servers


# ---------------------------------------------------------------------------------------------------------------------
class Run:
    def __init__(self, m, out):
        self.m, self.out, self.fails, self.notes = m, out, [], []

    def check(self, name, ok, detail=""):
        print("%s %s %s" % ("PASS" if ok else "FAIL", name, detail), flush=True)
        if not ok:
            self.fails.append(name)

    def home(self):
        self.m.call("press", button="home", after="none")
        time.sleep(1.5)

    def portrait(self):
        self.m.call("sim_orientation", set="portrait")
        time.sleep(1.5)

    # -- refs: repeated web text
    def refs(self, port):
        self.m.call("open_url", url="http://127.0.0.1:%d/rows.html" % port, after="none")
        self.m.call("wait_for", target="Item 3", timeout=60)
        s1, _ = self.m.snap()
        # the "by" (own text span) on Item 5's row: the one at the row's height
        it5 = els(s1, role="^link$", label="^Item 5$")
        self.check("refs: Item 5 listed", bool(it5))
        if not it5:
            return
        y5 = it5[0]["frame"][1]
        sub = [e for e in els(s1, role="^text$", label="^by$") if 0 < e["frame"][1] - y5 < 60]
        self.check("refs: a 'by' under Item 5", bool(sub))
        if not sub:
            return
        ref = sub[0]["ref"]
        self.m.call("scroll", direction="down", amount=0.25, after="none")
        s2, _ = self.m.snap()
        it5b = els(s2, role="^link$", label="^Item 5$")
        moved = it5b and abs(it5b[0]["frame"][1] - y5) > 20
        self.check("refs: the page scrolled", bool(moved), "Item 5 at %s -> %s" % (y5, it5b and it5b[0]["frame"][1]))
        if not it5b:
            return
        y5b = it5b[0]["frame"][1]
        same = [e for e in s2["elements"] if e["ref"] == ref]
        ok = bool(same) and 0 < same[0]["frame"][1] - y5b < 60
        self.check("refs: Item 5's 'by' kept its ref after the scroll", ok,
                   "ref %s now at %s, Item 5 at %s" % (ref, same and same[0]["frame"][1], y5b))
        # and no other row's 'by' took a ref of another row: every 'by' ref is unique and sits under one item
        refs = [e["ref"] for e in els(s2, role="^text$", label="^by$")]
        self.check("refs: 'by' refs unique", len(refs) == len(set(refs)), str(len(refs)))

    # -- iframes
    def iframes(self, port):
        self.m.call("open_url", url="http://127.0.0.1:%d/frames.html" % port, after="none")
        self.m.call("wait_for", target="After the frames", timeout=60)
        time.sleep(2)
        s, _ = self.m.snap()
        btns = els(s, label="^Press inner$")
        self.check("iframes: both frames' buttons listed", len(btns) == 2, str(len(btns)))
        urls = sorted((b.get("frame_url") or "") for b in btns)
        self.check("iframes: frame URLs", any("?same" in u for u in urls) and any("?cross" in u for u in urls), str(urls))
        outer = els(s, label="^Before the frames$")
        if len(btns) == 2 and outer:
            # placed inside the page: below the heading, inside the web view, the same-origin one shifted by its
            # border + padding (20 margin + 4 border + 6 padding + 8 body margin)
            b = sorted(btns, key=lambda e: e["frame"][1])
            self.check("iframes: placed below the outer text", b[0]["frame"][1] > outer[0]["frame"][1], "%s vs %s" % (b[0]["frame"], outer[0]["frame"]))
            self.check("iframes: same-origin frame offset", 35 <= b[0]["frame"][0] - outer[0]["frame"][0] <= 45, "%s vs %s" % (b[0]["frame"], outer[0]["frame"]))
            for e in b:
                tag = "same" if "?same" in (e.get("frame_url") or "") else "cross"
                text, _, err, _ = self.m.call("tap", target="@" + e["ref"], after="none")
                time.sleep(1.0)
                s2, _ = self.m.snap()
                self.check("iframes: tap reaches the %s-origin frame" % tag, bool(els(s2, label="^Clicked %s$" % tag)), text.split("\n")[0])

    # -- landscape
    def land(self, port):
        self.home()
        self.m.call("sim_orientation", set="landscapeLeft")
        time.sleep(2)
        # the iPhone home screen stays portrait while the phone is held sideways
        s, _ = self.m.snap()
        self.check("land: home screen stays portrait", s["screen"][0] < s["screen"][1] and not s.get("rot"), "%s rot %s" % (s["screen"], s.get("rot")))
        text, _, err, _ = self.m.call("tap", target="Settings")
        s, _ = self.m.snap()
        self.check("land: tap an icon on the portrait home screen of a sideways phone", "com.apple.Preferences" in s["front"], str(s["front"]))
        # iPhone Settings is portrait-only: it stays portrait on the sideways phone (the portrait-only app case)
        self.check("land: portrait-only Settings stays portrait", s["screen"][0] < s["screen"][1] and not s.get("rot"), "%s rot %s" % (s["screen"], s.get("rot")))
        text, _, err, _ = self.m.call("tap", target="General")
        self.check("land: tap General in portrait-only Settings on a sideways phone", not err and re.search(r"About|Software Update", text) is not None, text.split("\n")[0])
        self.m.call("app_terminate", bundle_id="com.apple.Preferences", after="none")
        # Safari turned with the phone: a link on a web page
        self.m.call("open_url", url="http://127.0.0.1:%d/rows.html" % port, after="none")
        self.m.call("wait_for", target="Item 3", timeout=60)
        s, _ = self.m.snap()
        self.check("land: Safari turned with the phone", s["screen"][0] > s["screen"][1], "%s rot %s" % (s["screen"], s.get("rot")))
        text, _, err, _ = self.m.call("tap", target="Item 2", after="none")
        time.sleep(1.5)
        s, _ = self.m.snap()
        wv = els(s, role="^webview$")
        url = wv[0].get("value", "") if wv else ""
        self.check("land: tap a link in landscape Safari", url.endswith("#i2"), "%s; url %s" % (text.split("\n")[0], url))
        self.portrait()
        self.home()

    # -- terminate
    def term(self):
        self.m.call("app_launch", bundle_id="com.apple.Preferences", after="none")
        self.m.call("wait_for", target="General", timeout=60)
        text, _, err, dt = self.m.call("app_terminate", bundle_id="com.apple.Preferences", after="none")
        self.check("term: app_terminate fast", not err and dt < 2.5, "%.2f s: %s" % (dt, text.split("\n")[0]))
        s, _ = self.m.snap()
        self.check("term: app gone from the front", "com.apple.Preferences" not in s["front"], str(s["front"]))

    # -- latency
    def latency(self, port):
        self.m.call("open_url", url="http://127.0.0.1:%d/rows.html" % port, after="none")
        self.m.call("open_url", url="http://127.0.0.1:%d/long.html" % port, after="none")
        self.m.call("wait_for", target="Paragraph 3", timeout=60)
        ts = [self.m.snap()[1] for _ in range(5)]
        t0 = time.monotonic()
        s, _ = self.m.snap(all=True)
        ta = time.monotonic() - t0
        n = len(s["elements"])
        self.notes.append("latency long web page (3000 paragraphs), visible: %s s; all: %d elements in %.2f s" % (" ".join("%.2f" % t for t in ts), n, ta))
        self.check("latency: long web page snapshot < 1.5 s", sorted(ts)[2] < 1.5, "median %.2f s" % sorted(ts)[2])
        self.check("latency: long web page snapshot --all < 6 s", ta < 6, "%.2f s, %d elements" % (ta, n))
        # Settings from its root (it restores the last page it showed)
        self.m.call("app_terminate", bundle_id="com.apple.Preferences", after="none")
        self.m.call("app_launch", bundle_id="com.apple.Preferences", after="none")
        self.m.call("wait_for", target="General", timeout=60)
        for _ in range(4):
            s, _ = self.m.snap()
            back = els(s, id="^BackButton$")
            if not back:
                break
            self.m.call("tap", target="@" + back[0]["ref"], after="none")
            time.sleep(1)
        s, _ = self.m.snap()
        sf = els(s, role="^searchfield$")
        if sf:
            self.m.call("tap", target="@" + sf[0]["ref"], after="none")
            time.sleep(1.5)
        text, _, err, _ = self.m.call("type", text="a", after="none")
        time.sleep(3)
        ts = [self.m.snap()[1] for _ in range(5)]
        s, _ = self.m.snap()
        self.notes.append("latency Settings search 'a' (%d elements): %s s" % (len(s["elements"]), " ".join("%.2f" % t for t in ts)))
        self.check("latency: Settings search typed", not err, text.split("\n")[0])
        self.check("latency: Settings search snapshot < 1.5 s", sorted(ts)[2] < 1.5, "median %.2f s" % sorted(ts)[2])
        self.m.call("app_terminate", bundle_id="com.apple.Preferences", after="none")

    # -- busy: a WebContent process that does not answer is named, the rest of the screen still comes
    def busy(self, port):
        self.m.call("open_url", url="http://127.0.0.1:%d/busy.html" % port, after="none")
        self.m.call("wait_for", target="Busy page", timeout=60)
        time.sleep(3)
        text, st, err, dt = self.m.call("snapshot", json=True)
        busy = (st or {}).get("busy") or []
        self.check("busy: snapshot answers while the page is busy", not err and dt < 8, "%.2f s" % dt)
        self.check("busy: names the busy web content", any("mobilesafari" in b and "web content" in b for b in busy), str(busy))
        self.check("busy: Safari's own controls still listed", bool(els(st or {"elements": []}, id="^TabOverviewButton$")), "")
        time.sleep(20)

    # -- duo
    def duo(self):
        self.home()
        self.m.call("sim_fold", open=False)
        time.sleep(3)
        s, _ = self.m.snap()
        self.check("duo: outer screen named", s.get("display") == "outer", str(s.get("display")))
        self.m.call("tap", target="Settings", after="none")
        time.sleep(3)
        s, _ = self.m.snap()
        self.check("duo: tap on the outer screen opens Settings", "com.apple.Preferences" in s["front"], str(s["front"]))
        self.m.call("app_terminate", bundle_id="com.apple.Preferences", after="none")
        self.home()
        self.m.call("sim_fold", open=True)
        time.sleep(5)
        s, _ = self.m.snap()
        self.check("duo: inner screen named", s.get("display") == "inner", "%s %s" % (s.get("display"), s["screen"]))
        self.m.call("tap", target="Settings", after="none")
        time.sleep(3)
        s, _ = self.m.snap()
        self.check("duo: tap on the inner screen opens Settings", "com.apple.Preferences" in s["front"], str(s["front"]))
        text, _, err, _ = self.m.call("tap", target="General")
        self.check("duo: tap General on the inner screen", not err and re.search(r"About|Software Update", text) is not None, text.split("\n")[0])
        self.m.call("app_terminate", bundle_id="com.apple.Preferences", after="none")
        self.m.call("sim_fold", open=False)
        time.sleep(3)


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--phone", required=True)
    ap.add_argument("--mcp", default=os.path.join(ROOT, "tools/launcher/target/release/x86sim-mcp"))
    ap.add_argument("--only", default="")
    ap.add_argument("--out", default=os.path.expanduser("~/scratch/x86sim/ax-e2e"))
    a = ap.parse_args()
    os.makedirs(a.out, exist_ok=True)
    log = open(os.path.join(a.out, "mcp-%s.log" % a.phone), "w")
    m = Mcp(a.mcp, a.phone, log)
    r = Run(m, a.out)
    ports = []
    servers = serve(ports)
    only = set(filter(None, a.only.split(",")))
    model = ""
    for p in json.loads(m.call("phones_list")[0])["phones"]:
        if a.phone in (p.get("name"), p.get("id")):
            model = p.get("model", "")
    want = lambda n: (not only or n in only)
    try:
        if want("term"):
            r.term()
        if want("refs"):
            r.refs(ports[0])
        if want("iframes"):
            r.iframes(ports[0])
        if want("latency"):
            r.latency(ports[0])
        if want("busy"):
            r.busy(ports[0])
        if want("land"):
            r.land(ports[0])
        if want("duo") and "Duo" in model:
            r.duo()
    finally:
        for s in servers:
            s.shutdown()
        m.close()
    for n in r.notes:
        print("NOTE " + n)
    print("%d failed: %s" % (len(r.fails), ", ".join(r.fails)))
    sys.exit(len(r.fails))


if __name__ == "__main__":
    main()
