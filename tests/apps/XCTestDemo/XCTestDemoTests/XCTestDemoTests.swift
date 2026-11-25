// x86Sim: unit tests hosted in XCTestDemo.app. Two tests fail on purpose (test runner fixture: failures must be
// reported with their message and file:line), one is skipped.
import XCTest
@testable import XCTestDemo
#if os(watchOS)
import WatchKit
#endif

final class CalcTests: XCTestCase {
    func testAdd() { XCTAssertEqual(Calc.add(2, 3), 5) }
    func testGreeting() {
        XCTAssertEqual(Calc.greeting(""), "Hello!")
        XCTAssertEqual(Calc.greeting("Ada"), "Hello, Ada!")
    }
    func testDeliberateFailure() { XCTAssertEqual(Calc.add(2, 2), 5, "deliberate failure") }
    func testSkipped() throws { throw XCTSkip("deliberately skipped") }
    func testAsyncExpectation() {
        let e = expectation(description: "main queue")
        DispatchQueue.main.asyncAfter(deadline: .now() + 0.1) { e.fulfill() }
        wait(for: [e], timeout: 5)
    }
}

final class HostTests: XCTestCase {
    func testRunsInsideHostApp() {
        XCTAssertEqual(Bundle.main.bundleIdentifier, "com.x86sim.test.XCTestDemo")
        #if os(watchOS)
        XCTAssertNotNil(WKApplication.shared())
        #else
        XCTAssertNotNil(UIApplication.shared)
        #endif
    }
    func testDeliberateThrow() throws {
        struct Oops: Error {}
        throw Oops()
    }
}
