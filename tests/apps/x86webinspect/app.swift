// x86webinspect: two WKWebViews for Web Inspector checks (x86sim inspect, MCP web_*): one with isInspectable = true
// (listed by webinspectord, as in Apple's simulator), one without (not listed). The inspectable page logs to the
// console, fetches X86WI_URL's data.json when given (default http://127.0.0.1:28743/), and sets its title.
// tvOS has no WebKit (no WKWebView, no Safari): there the app has an inspectable JSContext named "x86webinspect"
// (Web Inspector lists JSContexts of tvOS apps in Apple's simulator too) that logs and sets appValue every second.
import SwiftUI
#if canImport(WebKit)
import WebKit
#else
import JavaScriptCore
#endif

let page = """
<!doctype html><html><head><meta name="viewport" content="width=device-width"><title>%TITLE%</title></head>
<body style="font: 20px -apple-system"><h1 id="hello">%TITLE%</h1><p>x86webinspect</p>
<script>
console.log("x86webinspect: %TITLE% loaded");
window.appValue = 6 * 7;
fetch("data.json").then(r => r.json()).then(j => console.log("x86webinspect fetched", JSON.stringify(j))).catch(e => console.warn("x86webinspect fetch failed", String(e)));
</script></body></html>
"""

#if canImport(WebKit)
struct Web: UIViewRepresentable {
    let title: String
    let inspectable: Bool
    final class Coord: NSObject, WKNavigationDelegate {
        let check: String
        init(_ c: String) { check = c }
        func webView(_ w: WKWebView, didFinish _: WKNavigation!) {
            w.evaluateJavaScript("document.title") { t, e in X86Test.log(self.check, e == nil, "title=\(t ?? "nil") inspectable=\(w.isInspectable)") }
        }
    }
    func makeCoordinator() -> Coord { Coord("load-" + title.lowercased()) }
    func makeUIView(context: Context) -> WKWebView {
        let w = WKWebView()
        w.isInspectable = inspectable
        w.navigationDelegate = context.coordinator
        let base = URL(string: ProcessInfo.processInfo.environment["X86WI_URL"] ?? "http://127.0.0.1:28743/")
        w.loadHTMLString(page.replacingOccurrences(of: "%TITLE%", with: title), baseURL: base)
        return w
    }
    func updateUIView(_: WKWebView, context: Context) {}
}

#else
final class JSHost {
    static let shared = JSHost()
    let ctx = JSContext()!
    var n = 0
    init() {
        ctx.name = "x86webinspect"
        ctx.isInspectable = true
        ctx.evaluateScript("var appValue = 6 * 7; function tick(n) { console.log('x86webinspect tick ' + n); return n; }")
        Timer.scheduledTimer(withTimeInterval: 1, repeats: true) { [weak self] _ in
            guard let self else { return }
            self.n += 1
            self.ctx.evaluateScript("tick(\(self.n))")
        }
        X86Test.log("jscontext", true, "inspectable=\(ctx.isInspectable)")
    }
}
#endif

@main
struct X86WebInspectApp: App {
    init() { X86Test.reset() }
    var body: some Scene {
        WindowGroup {
            VStack(alignment: .leading) {
                Text("x86webinspect").font(.largeTitle).bold()
                #if canImport(WebKit)
                Web(title: "Inspectable", inspectable: true).border(.gray)
                Web(title: "Hidden", inspectable: false).border(.gray)
                #else
                Text("JSContext x86webinspect").onAppear { _ = JSHost.shared }
                #endif
            }.padding()
        }
    }
}
