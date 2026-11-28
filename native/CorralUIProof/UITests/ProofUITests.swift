// The UI test proof (Spike S2, step 4). If these pass on a GitHub-hosted macos-26 runner, the
// nightly can drive the bundled Corral app there; if the runner blocks UI automation, they fail
// at launch or at the first click, and the nightly moves to a self-hosted Mac.
import XCTest

final class ProofUITests: XCTestCase {
    override func setUp() {
        continueAfterFailure = false
    }

    /// The label `id` shows, once it says `text`.
    @MainActor
    private func waitFor(_ text: String, in app: XCUIApplication, id: String) -> Bool {
        let status = app.staticTexts[id]
        let said = NSPredicate(format: "value == %@ OR label == %@", text, text)
        let expectation = XCTNSPredicateExpectation(predicate: said, object: status)
        return XCTWaiter.wait(for: [expectation], timeout: 10) == .completed
    }

    @MainActor
    func testAClickInAWindow() {
        let app = XCUIApplication()
        app.launch()
        let button = app.buttons["window-button"]
        XCTAssertTrue(button.waitForExistence(timeout: 20), "the window's button shows")
        button.click()
        XCTAssertTrue(waitFor("Clicked 1", in: app, id: "window-status"), "the click lands")
    }

    @MainActor
    func testAClickInANonActivatingPanel() {
        let app = XCUIApplication()
        app.launch()
        let button = app.buttons["panel-button"]
        XCTAssertTrue(button.waitForExistence(timeout: 20), "the panel's button shows")
        button.click()
        XCTAssertTrue(waitFor("Clicked 1", in: app, id: "panel-status"), "the click lands")
    }
}
