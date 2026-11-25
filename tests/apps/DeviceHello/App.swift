import UIKit

// A device build (iphoneos SDK) for `x86sim install X.ipa` (tests/apps/build-ipa.sh): its text comes from an embedded
// framework (Greeter.framework, fat arm64 + arm64e), so the conversion of nested bundles and fat files is exercised.
@_silgen_name("greeter_text") func greeter_text() -> UnsafePointer<CChar>

@main
final class AppDelegate: UIResponder, UIApplicationDelegate {
    func application(_ application: UIApplication,
                     configurationForConnecting connectingSceneSession: UISceneSession,
                     options: UIScene.ConnectionOptions) -> UISceneConfiguration {
        let c = UISceneConfiguration(name: "Default", sessionRole: connectingSceneSession.role)
        c.delegateClass = SceneDelegate.self
        return c
    }
}

final class SceneDelegate: UIResponder, UIWindowSceneDelegate {
    var window: UIWindow?
    func scene(_ scene: UIScene, willConnectTo session: UISceneSession, options: UIScene.ConnectionOptions) {
        guard let ws = scene as? UIWindowScene else { return }
        let w = UIWindow(windowScene: ws)
        w.rootViewController = HelloViewController()
        w.makeKeyAndVisible()
        window = w
    }
}

final class HelloViewController: UIViewController {
    override func viewDidLoad() {
        super.viewDidLoad()
        view.backgroundColor = .systemOrange
        let l = UILabel()
        l.text = String(cString: greeter_text())
        l.numberOfLines = 0
        l.font = .systemFont(ofSize: 40, weight: .bold)
        l.textColor = .white
        l.textAlignment = .center
        l.translatesAutoresizingMaskIntoConstraints = false
        view.addSubview(l)
        NSLayoutConstraint.activate([
            l.centerXAnchor.constraint(equalTo: view.centerXAnchor),
            l.centerYAnchor.constraint(equalTo: view.centerYAnchor),
            l.widthAnchor.constraint(equalTo: view.widthAnchor, multiplier: 0.9),
        ])
        FileHandle.standardError.write("X86TEST DeviceHello launch PASS \(l.text ?? "")\n".data(using: .utf8)!)
    }
}
