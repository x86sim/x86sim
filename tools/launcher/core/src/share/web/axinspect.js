// x86Sim share page: the Accessibility Inspector (as Xcode's): point at an element on the device's screen to see its
// frame and properties, perform its accessibility actions, run the audit. Routes: ../axapi.rs. app.js's phoneView calls
// AXI.attach() and puts panel() in its side bar.
"use strict";

window.AXI = (() => {
  const SEV = { error: "Error", warning: "Warning", info: "Info" };
  function attach(o) {
    const { id, wrap, side, control, api, h, toast } = o;
    const base = `/api/phones/${encodeURIComponent(id)}/ax/`;
    let on = false, tree = null, sel = null, hover = null, busy = false, timer = 0, audit = null, mark = null, showAll = false, alive = true;
    const layer = h("div", { class: "axlayer", hidden: true });
    wrap.append(layer);
    const props = h("div", { class: "axprops" });
    const list = h("div", { class: "axaudit" });
    const status = h("div", { class: "axstatus" });
    const inspectBtn = h("button", { class: "btn small", type: "button", text: "Inspect", title: "Point at an element on the screen to see its accessibility properties (touches are off while inspecting)" });
    const allBox = h("input", { type: "checkbox" });
    const dynBox = h("input", { type: "checkbox" });
    const auditBtn = h("button", { class: "btn small", type: "button", text: "Run audit", title: "Accessibility audit of the app in front" });
    const panel = h("div", { class: "axpanel" },
      h("h4", { text: "Accessibility" }),
      h("div", { class: "btns" }, inspectBtn, auditBtn),
      h("div", { class: "axopts" },
        h("label", { title: "Draw every element's frame" }, allBox, " all frames"),
        control ? h("label", { title: "Also re-check at the largest accessibility text size (the device's text size changes for a few seconds, then goes back)" }, dynBox, " Dynamic Type") : null),
      status, props, list);

    const isXr = () => tree?.platform === "xros";
    function screenPts() { return tree?.screen || [1, 1]; }
    function box(f, cls, label) {
      const [sw, sh] = screenPts();
      const b = h("div", { class: "axbox " + cls });
      b.style.left = (f[0] / sw * 100) + "%";
      b.style.top = (f[1] / sh * 100) + "%";
      b.style.width = (f[2] / sw * 100) + "%";
      b.style.height = (f[3] / sh * 100) + "%";
      if (label) b.append(h("span", { text: label }));
      return b;
    }
    function draw() {
      const kids = [];
      if (tree && !isXr()) {
        if (showAll) for (const e of tree.elements) if (!e.offscreen && e.role !== "webview") kids.push(box(e.frame, "all"));
        if (mark) kids.push(box(mark.frame, "mark " + mark.severity, mark.rule));
        if (hover && hover !== sel) kids.push(box(hover.frame, "hover", `${hover.role}${hover.label ? " · " + hover.label.slice(0, 40) : ""}`));
        if (sel) kids.push(box(sel.frame, "sel", "@" + sel.ref));
      }
      layer.replaceChildren(...kids);
    }
    const rank = (l) => ({ system: 0, status: 1, window: 2, app: 3 })[l] ?? 4;
    function at(x, y) {
      let best = null, bk = null;
      for (const e of tree?.elements || []) {
        const f = e.frame;
        if (e.offscreen || e.covered || x < f[0] || y < f[1] || x > f[0] + f[2] || y > f[1] + f[3]) continue;
        const k = [rank(e.layer), e.role === "webview" ? 1 : 0, f[2] * f[3]];
        if (!bk || k[0] < bk[0] || (k[0] === bk[0] && (k[1] < bk[1] || (k[1] === bk[1] && k[2] < bk[2])))) { best = e; bk = k; }
      }
      return best;
    }
    function pt(ev) {
      const r = layer.getBoundingClientRect(), [sw, sh] = screenPts();
      return [(ev.clientX - r.left) / r.width * sw, (ev.clientY - r.top) / r.height * sh];
    }
    layer.addEventListener("pointermove", (ev) => { const e = at(...pt(ev)); if (e !== hover) { hover = e; draw(); } });
    layer.addEventListener("pointerleave", () => { hover = null; draw(); });
    layer.addEventListener("pointerdown", (ev) => { ev.preventDefault(); ev.stopPropagation(); const e = at(...pt(ev)); select(e); });
    layer.addEventListener("wheel", (ev) => ev.stopPropagation(), { passive: true });
    layer.addEventListener("contextmenu", (ev) => ev.preventDefault());

    function select(e) {
      sel = e; mark = null;
      draw();
      showProps();
    }
    function act(action, arg) {
      if (!sel && action !== "escape" && action !== "magic_tap") return;
      status.textContent = action + "…";
      api(base + "action", { target: sel ? "@" + sel.ref : "", action, arg }).then((r) => { status.textContent = r.did; setTimeout(refresh, 300); })
        .catch((err) => { status.textContent = err.message; });
    }
    function showProps() {
      if (!sel) { props.replaceChildren(on ? h("p", { class: "hint", text: isXr() ? "Vision Pro: windows float in the room, frames are each window's own points; pick an element from the list." : "Point at an element on the screen." }) : ""); return; }
      const rows = sel.properties.map(([k, v]) => h("tr", {}, h("th", { text: k }), h("td", { text: v })));
      const acts = [];
      if (control) {
        for (const a of sel.actions) {
          if (a.startsWith("custom:")) acts.push(h("button", { class: "btn small", type: "button", text: a.slice(7), title: "Custom action", onclick: () => act("custom", a.slice(7)) }));
          else if (a === "scroll") for (const [d, t] of [["up", "↑"], ["down", "↓"], ["left", "←"], ["right", "→"]]) acts.push(h("button", { class: "btn small", type: "button", text: "Scroll " + t, title: "VoiceOver three-finger swipe " + d, onclick: () => act("scroll", d) }));
          else if (!a.includes(" ")) acts.push(h("button", { class: "btn small", type: "button", text: a.replace("_", " "), onclick: () => act(a) }));
        }
      }
      props.replaceChildren(
        h("div", { class: "axtitle" }, h("b", { text: sel.label || "(no label)" }), " ", h("span", { class: "pill", text: sel.role })),
        h("table", { class: "axtable" }, ...rows),
        acts.length ? h("div", {}, h("h4", { text: "Actions" }), h("div", { class: "btns" }, ...acts)) : null);
    }
    function elementsList() {
      if (!tree || !isXr()) return null;
      // Vision Pro: no frames on the display; pick from the list
      return h("div", { class: "axlist" }, ...tree.elements.filter((e) => e.role !== "webview").slice(0, 300).map((e) =>
        h("div", { class: "axrow" + (sel && sel.ref === e.ref ? " on" : ""), onclick: () => select(e) }, h("code", { text: "@" + e.ref }), " " + e.role + " " + (e.label || ""))));
    }
    async function refresh() {
      if (!alive || !on || busy) return;
      busy = true;
      try {
        tree = await api(base + "tree");
        if (sel) sel = tree.elements.find((e) => e.ref === sel.ref) || null;
        if (hover) hover = tree.elements.find((e) => e.ref === hover.ref) || null;
        status.textContent = `${tree.elements.length} elements · ${tree.front.join(", ")} · ${tree.ms} ms` + (tree.hidden.length ? ` · ${tree.hidden.length} hidden from VoiceOver` : "");
        draw(); showProps();
        const l = elementsList();
        if (l) props.append(l);
      } catch (err) { status.textContent = err.message; }
      busy = false;
    }
    function setOn(v) {
      on = v;
      inspectBtn.classList.toggle("on", on);
      layer.hidden = !on && !mark;
      layer.classList.toggle("live", on);
      side.classList.toggle("ax-wide", on || !!audit);
      clearInterval(timer);
      if (on) { refresh(); timer = setInterval(refresh, 2500); }
      else { sel = null; hover = null; draw(); showProps(); status.textContent = ""; }
    }
    inspectBtn.onclick = () => setOn(!on);
    allBox.onchange = () => { showAll = allBox.checked; if (showAll && !on) setOn(true); draw(); };
    auditBtn.onclick = async () => {
      auditBtn.disabled = true;
      list.replaceChildren(h("p", { class: "hint", text: dynBox.checked ? "Auditing (with the largest text size: about 10 s)…" : "Auditing…" }));
      try {
        audit = await api(base + "audit" + (dynBox.checked ? "?dynamic_type=1" : ""));
        if (!tree) tree = await api(base + "tree");
        showAudit();
      } catch (err) { list.replaceChildren(h("p", { class: "hint", text: err.message })); }
      auditBtn.disabled = false;
    };
    function showAudit() {
      side.classList.add("ax-wide");
      const a = audit;
      const head = h("div", { class: "axsum" },
        h("b", { text: `${a.errors} errors · ${a.warnings} warnings · ${a.infos} info` }), ` (${a.checked} elements of ${a.apps.join(", ")})`,
        h("button", { class: "btn small", type: "button", text: "Copy JSON", onclick: () => navigator.clipboard?.writeText(JSON.stringify(a, null, 1)).then(() => toast("Copied")) }),
        h("button", { class: "btn small", type: "button", text: "Close", onclick: () => { audit = null; mark = null; list.replaceChildren(); layer.hidden = !on; side.classList.toggle("ax-wide", on); draw(); } }));
      const rows = a.findings.map((f) => h("div", { class: "axfind " + f.severity, onclick: () => {
        mark = f; layer.hidden = false;
        if (f.ref && tree) { const e = tree.elements.find((x) => x.ref === f.ref); if (e) { sel = e; showProps(); } }
        draw();
      } },
        h("div", {}, h("span", { class: "sev", text: SEV[f.severity] || f.severity }), " ", h("code", { text: f.rule }), f.ref ? h("code", { text: " @" + f.ref }) : null),
        h("div", { text: f.message }),
        h("div", { class: "fix", text: "Fix: " + f.fix })));
      list.replaceChildren(head, ...(rows.length ? rows : [h("p", { class: "hint", text: "No issues found." })]), ...a.notes.map((n) => h("p", { class: "hint", text: n })));
    }
    showProps();
    return {
      panel: () => panel,
      close: () => { alive = false; clearInterval(timer); layer.remove(); },
    };
  }
  return { attach };
})();
