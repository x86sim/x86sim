import Testing
import XCTest
@testable import CalcKit

final class CalcKitXCTests: XCTestCase {
    func testAdd() { XCTAssertEqual(CalcKit.add(2, 3), 5) }
    func testMean() { XCTAssertEqual(CalcKit.mean([1, 2, 3]), 2) }
    func testDeliberateFailure() { XCTAssertNil(CalcKit.mean([1]), "deliberate failure") }
}

@Test func meanOfNothing() { #expect(CalcKit.mean([]) == nil) }
@Test func deliberateSwiftTestingFailure() { #expect(CalcKit.add(1, 1) == 3) }
