// x86a11ybad: a deliberately inaccessible UIKit screen for the Accessibility Inspector's audit (x86sim ax-audit, the
// MCP ax_audit tool) and its actions (ax_action). Every view below breaks one rule on purpose; the comment says which.
import UIKit

final class BadVC: UIViewController {
    let status = UILabel()
    var level = 3

    func note(_ s: String) { status.text = "Did: \(s)" }

    func dot(_ c: UIColor, _ s: CGFloat) -> UIImage {
        UIGraphicsImageRenderer(size: CGSize(width: s, height: s)).image { ctx in
            c.setFill(); ctx.cgContext.fillEllipse(in: CGRect(x: 0, y: 0, width: s, height: s))
        }
    }

    override func viewDidLoad() {
        super.viewDidLoad()
        view.backgroundColor = .white
        let w = view.bounds.width
        var y: CGFloat = 70

        // missing-label + hit-region: an image-only button, 30 x 30
        let b1 = UIButton(type: .custom)
        b1.setImage(dot(.systemBlue, 26), for: .normal)
        b1.frame = CGRect(x: 20, y: y, width: 30, height: 30)
        b1.addAction(UIAction { [weak self] _ in self?.note("image button") }, for: .touchUpInside)
        view.addSubview(b1)

        // label-not-readable: the asset's name as its label
        let b2 = UIButton(type: .custom)
        b2.setImage(dot(.systemGreen, 40), for: .normal)
        b2.accessibilityLabel = "ic_settings_gear"
        b2.frame = CGRect(x: 70, y: y - 7, width: 44, height: 44)
        view.addSubview(b2)

        // hit-region: a labelled 20 x 20 close button
        let b3 = UIButton(type: .system)
        b3.setTitle("x", for: .normal)
        b3.accessibilityLabel = "Close"
        b3.frame = CGRect(x: 140, y: y + 5, width: 20, height: 20)
        view.addSubview(b3)
        y += 50

        // contrast: light grey text on white (#BBBBBB: 1.9:1)
        let low = UILabel(frame: CGRect(x: 20, y: y, width: w - 40, height: 24))
        low.text = "Low contrast text"
        low.textColor = UIColor(white: 0.733, alpha: 1)
        low.font = .systemFont(ofSize: 15)
        view.addSubview(low)
        y += 34

        // hidden-interactive: an image view that takes taps but is no accessibility element
        let iv = UIImageView(image: dot(.systemOrange, 48))
        iv.frame = CGRect(x: 20, y: y, width: 48, height: 48)
        iv.isUserInteractionEnabled = true
        iv.addGestureRecognizer(UITapGestureRecognizer(target: self, action: #selector(tappedImage)))
        view.addSubview(iv)

        // trait-mismatch: a label that takes taps, read as plain text
        let tl = UILabel(frame: CGRect(x: 90, y: y + 10, width: 160, height: 30))
        tl.text = "Tap to continue"
        tl.textColor = .black
        tl.isUserInteractionEnabled = true
        tl.addGestureRecognizer(UITapGestureRecognizer(target: self, action: #selector(tappedLabel)))
        view.addSubview(tl)
        y += 64

        // duplicate-label: two "Delete" buttons
        for (i, x) in [20, 150].enumerated() {
            let d = UIButton(type: .system)
            d.setTitle("Delete", for: .normal)
            d.frame = CGRect(x: CGFloat(x), y: y, width: 110, height: 44)
            d.addAction(UIAction { [weak self] _ in self?.note("delete \(i + 1)") }, for: .touchUpInside)
            view.addSubview(d)
        }
        y += 54

        // text-clipped + dynamic-type (fixed size): one line, too narrow
        let clip = UILabel(frame: CGRect(x: 20, y: y, width: 150, height: 22))
        clip.text = "This sentence does not fit in its label"
        clip.font = .systemFont(ofSize: 15)
        clip.textColor = .black
        view.addSubview(clip)
        y += 30

        // dynamic-type: scales, but its fixed frame cuts it off at large sizes
        let dyn = UILabel(frame: CGRect(x: 20, y: y, width: w - 40, height: 22))
        dyn.text = "Body text in a fixed-height frame"
        dyn.font = .preferredFont(forTextStyle: .body)
        dyn.adjustsFontForContentSizeCategory = true
        dyn.textColor = .black
        view.addSubview(dyn)
        y += 34

        // hidden-interactive: a button under accessibilityElementsHidden
        let box = UIView(frame: CGRect(x: 20, y: y, width: w - 40, height: 44))
        box.accessibilityElementsHidden = true
        let hb = UIButton(type: .system)
        hb.setTitle("Hidden button", for: .normal)
        hb.frame = box.bounds
        box.addSubview(hb)
        view.addSubview(box)
        y += 54

        // fine: an adjustable slider (ax_action increment / decrement; tvOS has no UISlider) and an element with custom actions
        #if !os(tvOS)
        let s = UISlider(frame: CGRect(x: 20, y: y, width: w - 40, height: 44))
        s.minimumValue = 0; s.maximumValue = 10; s.value = 5
        s.accessibilityLabel = "Volume"
        s.addAction(UIAction { [weak self, weak s] _ in self?.note("volume \(Int(s?.value ?? 0))") }, for: .valueChanged)
        view.addSubview(s)
        y += 54
        #endif

        let msg = UIView(frame: CGRect(x: 20, y: y, width: w - 40, height: 50))
        msg.backgroundColor = UIColor(white: 0.95, alpha: 1)
        msg.isAccessibilityElement = true
        msg.accessibilityLabel = "Message from Anna"
        msg.accessibilityHint = "Swipe up or down for actions"
        msg.accessibilityCustomActions = [
            UIAccessibilityCustomAction(name: "Reply") { [weak self] _ in self?.note("Reply"); return true },
            UIAccessibilityCustomAction(name: "Archive") { [weak self] _ in self?.note("Archive"); return true },
        ]
        let ml = UILabel(frame: msg.bounds.insetBy(dx: 10, dy: 0))
        ml.text = "Anna: see you at 6?"
        ml.textColor = .black
        msg.addSubview(ml)
        view.addSubview(msg)
        y += 60

        status.frame = CGRect(x: 20, y: y, width: w - 40, height: 30)
        status.text = "Did: nothing"
        status.textColor = .black
        status.accessibilityIdentifier = "status"
        view.addSubview(status)
    }

    @objc func tappedImage() { note("image view") }
    @objc func tappedLabel() { note("label") }
    override func accessibilityPerformMagicTap() -> Bool { note("magic tap"); return true }
    override func accessibilityPerformEscape() -> Bool { note("escape"); return true }
}

final class SceneDelegate: UIResponder, UIWindowSceneDelegate {
    var window: UIWindow?
    func scene(_ scene: UIScene, willConnectTo session: UISceneSession, options connectionOptions: UIScene.ConnectionOptions) {
        guard let ws = scene as? UIWindowScene else { return }
        let w = UIWindow(windowScene: ws)
        w.rootViewController = BadVC()
        w.makeKeyAndVisible()
        window = w
    }
}

@main
final class AppDelegate: UIResponder, UIApplicationDelegate {
    func application(_ application: UIApplication, configurationForConnecting s: UISceneSession, options: UIScene.ConnectionOptions) -> UISceneConfiguration {
        let c = UISceneConfiguration(name: nil, sessionRole: s.role)
        c.delegateClass = SceneDelegate.self
        return c
    }
}
