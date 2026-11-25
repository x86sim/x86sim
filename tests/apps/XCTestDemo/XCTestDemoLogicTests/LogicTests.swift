// x86Sim: a logic test bundle (no host app: run by the xctest tool). testDeliberateFailure fails on purpose.
import XCTest

final class LogicTests: XCTestCase {
    func testSort() { XCTAssertEqual([3, 1, 2].sorted(), [1, 2, 3]) }
    func testString() { XCTAssertTrue("x86Sim".hasPrefix("x86")) }
    func testNoHostApp() { XCTAssertNil(Bundle.main.bundleIdentifier.flatMap { $0.hasPrefix("com.x86sim.test") ? $0 : nil }) }
    func testDeliberateFailure() { XCTAssertTrue([1, 2].isEmpty, "deliberate logic failure") }
}
