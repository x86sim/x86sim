let n = 0;
onconnect = (e) => { const p = e.ports[0]; p.onmessage = (m) => p.postMessage({ n: ++n, got: m.data }); };
