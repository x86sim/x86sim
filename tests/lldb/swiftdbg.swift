// x86sim debug test app for Swift (tests/lldb/run-swift.sh): a UIKit app whose timer calls swiftTick() twice a second
// on the main thread. swiftTick has Swift locals of every common kind (struct, class, array, string, dictionary,
// optional, enum) for `frame variable`, `po` and `expression`. Built with -g -Onone and a .dSYM by build-swift-app.sh.
import UIKit

struct Point: Equatable {
    var x: Int
    var y: Double
    var label: String
}

final class Counter: CustomDebugStringConvertible {
    var name: String
    var hits: Int
    var history: [Int] = []
    init(name: String, hits: Int) { self.name = name; self.hits = hits }
    var debugDescription: String { "Counter(\(name), hits: \(hits))" }
}

enum Mode { case idle, running(speed: Int) }

let counter = Counter(name: "ticks", hits: 0)

@inline(never) func swiftTick(_ n: Int) -> Int {
    let point = Point(x: n, y: Double(n) * 1.5, label: "p\(n)")
    let numbers = Array(1...max(n, 1))
    let greeting = "hello swift \(n)"
    let table: [String: Int] = ["one": 1, "two": 2]
    let maybe: Int? = n % 2 == 0 ? n : nil
    let mode = Mode.running(speed: n * 10)
    counter.hits += 1
    counter.history.append(n)
    if counter.history.count > 8 { counter.history.removeFirst() }
    var acc = 0
    for v in numbers { acc += v * point.x }
    _ = (greeting, table, maybe, mode)
    return acc
}

final class SceneDelegate: UIResponder, UIWindowSceneDelegate {
    var window: UIWindow?
    var label: UILabel?
    var timer: Timer?
    var n = 0
    func scene(_ scene: UIScene, willConnectTo session: UISceneSession, options: UIScene.ConnectionOptions) {
        guard let ws = scene as? UIWindowScene else { return }
        let w = UIWindow(windowScene: ws)
        let vc = UIViewController()
        vc.view.backgroundColor = .systemBackground
        let l = UILabel(frame: CGRect(x: 20, y: 120, width: 320, height: 40))
        l.text = "swiftdbg"
        vc.view.addSubview(l)
        w.rootViewController = vc
        w.makeKeyAndVisible()
        window = w
        label = l
        timer = Timer.scheduledTimer(withTimeInterval: 0.5, repeats: true) { [weak self] _ in
            guard let self else { return }
            self.n += 1
            let v = swiftTick(self.n % 10 + 1)
            self.label?.text = "tick \(counter.hits) -> \(v)"
        }
    }
}

@main
final class AppDelegate: UIResponder, UIApplicationDelegate {
    func application(_ application: UIApplication, configurationForConnecting s: UISceneSession,
                     options: UIScene.ConnectionOptions) -> UISceneConfiguration {
        let c = UISceneConfiguration(name: nil, sessionRole: s.role)
        c.delegateClass = SceneDelegate.self
        return c
    }
}
