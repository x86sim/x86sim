// x86Sim: UI tests (XCUITest) driving XCTestDemo.app. testDeliberateUIFailure fails on purpose.
import XCTest

final class XCTestDemoUITests: XCTestCase {
    override func setUp() { continueAfterFailure = false }

    func testIncrement() {
        let app = XCUIApplication()
        app.launch()
        let label = app.staticTexts["countLabel"]
        XCTAssertTrue(label.waitForExistence(timeout: 30))
        XCTAssertEqual(label.label, "Count: 0")
        #if os(tvOS)
        // tvOS: the Siri Remote; up until Increment has the focus (the top focusable control)
        for _ in 0..<4 where !app.buttons["incrementButton"].hasFocus { XCUIRemote.shared.press(.up) }
        XCUIRemote.shared.press(.select)
        XCUIRemote.shared.press(.select)
        XCTAssertEqual(label.label, "Count: 2")
        XCUIRemote.shared.press(.down)
        XCUIRemote.shared.press(.select)
        #else
        app.buttons["incrementButton"].tap()
        app.buttons["incrementButton"].tap()
        XCTAssertEqual(label.label, "Count: 2")
        app.buttons["resetButton"].tap()
        #endif
        XCTAssertEqual(label.label, "Count: 0")
    }

    func testTypeName() {
        let app = XCUIApplication()
        app.launch()
        let field = app.textFields["nameField"]
        XCTAssertTrue(field.waitForExistence(timeout: 30))
        #if os(tvOS)
        XCUIRemote.shared.press(.down)
        XCUIRemote.shared.press(.down)
        XCUIRemote.shared.press(.select)
        #else
        field.tap()
        #endif
        field.typeText("Ada")
        XCTAssertEqual(app.staticTexts["greetingLabel"].label, "Hello, Ada!")
    }

    func testDeliberateUIFailure() {
        let app = XCUIApplication()
        app.launch()
        XCTAssertTrue(app.buttons["noSuchButton"].waitForExistence(timeout: 2), "deliberate UI failure")
    }
}
