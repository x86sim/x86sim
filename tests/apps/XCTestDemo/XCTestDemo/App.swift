// x86Sim: the app the XCTestDemo unit and UI tests exercise (tests/apps/XCTestDemo, x86sim test).
import SwiftUI

/// Logic the unit tests check (also used by the UI).
public enum Calc {
    public static func add(_ a: Int, _ b: Int) -> Int { a + b }
    public static func greeting(_ name: String) -> String { name.isEmpty ? "Hello!" : "Hello, \(name)!" }
}

@main
struct XCTestDemoApp: App {
    var body: some Scene { WindowGroup { ContentView() } }
}

struct ContentView: View {
    @State private var count = 0
    @State private var name = ""
    var body: some View {
        VStack(spacing: 20) {
            Text("Count: \(count)").font(.title).accessibilityIdentifier("countLabel")
            Button("Increment") { count += 1 }.accessibilityIdentifier("incrementButton")
            Button("Reset") { count = 0 }.accessibilityIdentifier("resetButton")
            #if os(iOS) || os(visionOS)
            TextField("Name", text: $name).textFieldStyle(.roundedBorder).padding(.horizontal)
                .accessibilityIdentifier("nameField")
            #else
            TextField("Name", text: $name).accessibilityIdentifier("nameField")
            #endif
            Text(Calc.greeting(name)).accessibilityIdentifier("greetingLabel")
        }
    }
}
