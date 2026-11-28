// J0, the walking skeleton, on the bundled app (docs/04-test-plan.md §6, TP-E2E-05): add an xAI
// key → click Grok's drop → type a question → send it → the ring turns and the hover card counts
// time and spend → the card shows the answer → Send back. The app is the e2e build: Grok is a
// scripted fake that answers `answer` after `scriptedWait`, so nothing here needs a key or the
// network. Each test runs the app in a fresh home folder, so it starts from a first launch.
import XCTest

final class J0Tests: XCTestCase {
    /// The e2e build's bundle ID (`apps/desktop/src-tauri/tauri.e2e.conf.json`).
    static let bundleID = "com.studioacas.corral.e2e"
    /// A made-up key: the e2e build accepts any key without asking xAI.
    static let key = "xai-e2eNightlyNotARealKey5Rk3Tz8Wq" // gitleaks:allow (made-up test key)
    static let question = "What is 2 + 2?"
    /// The start of every scripted agent's answer (`e2e::ANSWER`).
    static let answer = "Four."

    override func setUp() {
        continueAfterFailure = false
    }

    /// The app, launched from a first launch: its own empty home folder, so no keys, settings or
    /// event store from an earlier run (or from the real Corral on a developer's Mac).
    @MainActor
    private func launch() throws -> XCUIApplication {
        let home = FileManager.default.temporaryDirectory
            .appendingPathComponent("corral-e2e-\(UUID().uuidString)", isDirectory: true)
        try FileManager.default.createDirectory(at: home, withIntermediateDirectories: true)
        addTeardownBlock {
            // Cleanup only: a leftover folder in the temporary folder harms nothing.
            try? FileManager.default.removeItem(at: home)
        }
        let app = XCUIApplication(bundleIdentifier: Self.bundleID)
        app.launchEnvironment["HOME"] = home.path
        // The corral sits on the desktop by default, behind the main window; on a small runner
        // screen the window can cover it. Its "Always on top" level (saved as `corral.level`)
        // keeps every drop clickable, and changes nothing else.
        app.launchArguments += ["-corral.level", "floating"]
        app.launch()
        addTeardownBlock { @MainActor in app.terminate() }
        return app
    }

    /// The first element of `query` whose label or value matches `format` (`%@` is `text`).
    @MainActor
    private func element(_ query: XCUIElementQuery, _ format: String, _ text: String) -> XCUIElement {
        let either = "(label \(format)) OR (value \(format))"
        return query.matching(NSPredicate(format: either, text, text)).firstMatch
    }

    /// A drop, by its agent's name and status ("Grok, working, number 1 by use").
    @MainActor
    private func drop(_ app: XCUIApplication, _ name: String, _ status: String) -> XCUIElement {
        element(app.buttons, "BEGINSWITH %@", "\(name), \(status),")
    }

    /// What an element says: its value when it has one, else its label.
    @MainActor
    private func text(_ element: XCUIElement) -> String {
        if let value = element.value as? String, !value.isEmpty { return value }
        return element.label
    }

    @MainActor
    func testJ0AKeyAQuestionTheAnswerAndSendBack() throws {
        let app = try launch()

        // A first launch: no keys, and an empty corral.
        XCTAssertTrue(element(app.descendants(matching: .any), "== %@", "Corral, empty")
            .waitForExistence(timeout: 30), "the corral starts empty")

        // Settings → API keys: paste the key; the field knows it's xAI's, and Connect stores it.
        let keyField = app.secureTextFields.firstMatch
        XCTAssertTrue(keyField.waitForExistence(timeout: 30), "the key field shows")
        keyField.click()
        keyField.typeText(Self.key)
        XCTAssertTrue(element(app.staticTexts, "== %@", "Looks like an xAI key.")
            .waitForExistence(timeout: 10), "the field suggests xAI")
        let connect = app.buttons["Connect"]
        XCTAssertTrue(connect.waitForExistence(timeout: 10))
        connect.click()

        // Grok drops into the corral.
        let available = drop(app, "Grok", "available")
        XCTAssertTrue(available.waitForExistence(timeout: 20), "Grok's drop shows once the key connects")

        // Click the drop: its card is pinned, with a task field. Type the question and send it.
        available.click()
        let task = app.textFields.matching(NSPredicate(format: "placeholderValue == %@", "Give Grok a task"))
            .firstMatch
        XCTAssertTrue(task.waitForExistence(timeout: 10), "the pinned card has a task field")
        task.click()
        task.typeText(Self.question)
        app.buttons["Send task"].click()

        // The ring turns (TP-SKEL-03).
        let working = drop(app, "Grok", "working")
        XCTAssertTrue(working.waitForExistence(timeout: 5), "the drop shows the working ring")

        // Close the pinned card, then hover the drop: the hover card counts time and spend from
        // the core (TP-SKEL-04), at least once a second.
        app.typeKey(.escape, modifierFlags: [])
        let status = element(app.staticTexts, "BEGINSWITH %@", "Working · ")
        if !status.waitForNonExistence(timeout: 3) {
            // Esc may have gone to the task field; the card's × closes it too.
            element(app.buttons, "== %@", "Close").click()
        }
        XCTAssertTrue(status.waitForNonExistence(timeout: 5), "the pinned card closes")
        working.hover()
        XCTAssertTrue(status.waitForExistence(timeout: 5), "the hover card shows the run")
        let first = text(status)
        XCTAssertTrue(first.contains("$"), "it shows the spend: \(first)")
        Thread.sleep(forTimeInterval: 2.2)
        let later = text(status)
        XCTAssertNotEqual(first, later, "the elapsed time moves on")

        // The run finishes; the card shows the answer (TP-SKEL-05).
        let done = drop(app, "Grok", "done")
        XCTAssertTrue(done.waitForExistence(timeout: 30), "the run finishes")
        done.click()
        XCTAssertTrue(element(app.staticTexts, "BEGINSWITH %@", Self.answer)
            .waitForExistence(timeout: 10), "the card shows the answer")

        // Send back: the drop is idle again.
        let sendBack = app.buttons["Send back"]
        XCTAssertTrue(sendBack.waitForExistence(timeout: 5))
        sendBack.click()
        XCTAssertTrue(drop(app, "Grok", "available").waitForExistence(timeout: 10),
                      "Grok is back in the corral")
    }
}
