// CorralCheckpointWireTests.swift: checkpoint notifications and Touch ID over the bridge
// (Spike S1 step 2; TP-BRIDGE-02/03 for the new payloads). Same shared fixtures as Rust.
import Testing
import Foundation
@testable import CorralNative

private func fixture(_ name: String) throws -> Data {
    let root = URL(fileURLWithPath: #filePath)
        .deletingLastPathComponent().deletingLastPathComponent().deletingLastPathComponent()
    return try Data(contentsOf: root.appendingPathComponent("Fixtures/bridge/\(name)"))
}

private func sameJSON(_ a: Data, _ b: Data) throws -> Bool {
    let x = try JSONSerialization.jsonObject(with: a) as? NSDictionary
    let y = try JSONSerialization.jsonObject(with: b) as? NSDictionary
    return x != nil && x == y
}

private func note(_ json: String) -> Result<CorralNotificationRequest, CorralWireError> {
    CorralNotificationRequest.decode(Data(json.utf8))
}

private func auth(_ json: String) -> Result<CorralAuthRequest, CorralWireError> {
    CorralAuthRequest.decode(Data(json.utf8))
}

// MARK: Rust → Swift

@Test("a checkpoint notification from Rust keeps its IDs, text and the buttons Rust chose")
func notifyFixtureDecodes() throws {
    let n = try CorralNotificationRequest.decode(fixture("notify-v1.json")).get()
    #expect(n.checkpoint == "cp-1")
    #expect(n.nonce == "n-1")
    #expect(n.agent == "scout")
    #expect(n.title == "Scout needs you")
    #expect(n.body == "Send the weekly summary email to 3 people?")
    #expect(n.actions == [.approve, .deny, .open])
}

@Test("Swift draws only the buttons Rust sends: no Approve unless Rust offers it")
func noImplicitApprove() throws {
    let n = try note(#"{"v":1,"checkpoint":"cp-2","nonce":"n-2","agent":"scout","title":"t","body":"b","actions":["deny","open"]}"#).get()
    #expect(n.actions == [.deny, .open])
    #expect(CorralNotificationRequest.categoryID(for: n.actions) == "corral.cp.deny-open")
}

@Test("duplicate buttons collapse, keeping the first")
func duplicateActions() throws {
    let n = try note(#"{"v":1,"checkpoint":"c","nonce":"n","agent":"a","title":"t","body":"b","actions":["open","open","deny"]}"#).get()
    #expect(n.actions == [.open, .deny])
}

@Test("title and body are stripped of control characters and truncated")
func notifyTextBounded() throws {
    let long = String(repeating: "y", count: 5_000)
    let n = try note(#"{"v":1,"checkpoint":"c","nonce":"n","agent":"a","title":"Hi‮\u0000 \#(long)","body":"\#(long)","actions":["open"]}"#).get()
    #expect(n.title.count == CorralNotificationRequest.maxTitleLength)
    #expect(n.title.hasPrefix("Hi y"))
    #expect(n.body.count == CorralNotificationRequest.maxBodyLength)
}

@Test("bad notifications are rejected, never trapped on")
func badNotifications() {
    let ok = #""v":1,"checkpoint":"c","nonce":"n","agent":"a","title":"t","body":"b""#
    #expect(note("{") == .failure(.malformed))
    #expect(note("{\(ok)}") == .failure(.malformed))                                   // actions missing
    #expect(note(#"{\#(ok),"actions":[]}"#) == .failure(.invalid("actions")))
    #expect(note(#"{\#(ok),"actions":["approveAll"]}"#) == .failure(.invalid("actions")))
    #expect(note(#"{\#(ok),"actions":["open"],"extra":1}"#) == .failure(.malformed))
    #expect(note(#"{"v":2,"checkpoint":"c","nonce":"n","agent":"a","title":"t","body":"b","actions":["open"]}"#)
            == .failure(.unsupportedVersion(2)))
    #expect(note(#"{"v":1,"checkpoint":"","nonce":"n","agent":"a","title":"t","body":"b","actions":["open"]}"#)
            == .failure(.invalid("checkpoint")))
    #expect(note(#"{"v":1,"checkpoint":"c","nonce":"n\u0000","agent":"a","title":"t","body":"b","actions":["open"]}"#)
            == .failure(.invalid("nonce")))
    #expect(note(#"{"v":1,"checkpoint":"c","nonce":"n","agent":"a","title":" \u0000 ","body":"b","actions":["open"]}"#)
            == .failure(.invalid("title")))
    let big = Data(repeating: UInt8(ascii: " "), count: CorralNotificationRequest.maxBytes + 1)
    #expect(CorralNotificationRequest.decode(big) == .failure(.tooLarge(CorralNotificationRequest.maxBytes + 1)))
    #expect(CorralNotificationRequest.decode(Data()) == .failure(.empty))
}

@Test("a Touch ID request from Rust keeps its ID and reason")
func authFixtureDecodes() throws {
    let a = try CorralAuthRequest.decode(fixture("authenticate-v1.json")).get()
    #expect(a.request == "auth-1")
    #expect(a.reason == "approve Scout spending $4.00")
}

@Test("bad Touch ID requests are rejected, never trapped on")
func badAuthRequests() {
    #expect(auth(#"{"v":1,"request":"","reason":"r"}"#) == .failure(.invalid("request")))
    #expect(auth(#"{"v":1,"request":"a","reason":"\u0000"}"#) == .failure(.invalid("reason")))
    #expect(auth(#"{"v":1,"request":"a","reason":"r","extra":true}"#) == .failure(.malformed))
    #expect(auth(#"{"v":3,"request":"a","reason":"r"}"#) == .failure(.unsupportedVersion(3)))
    let long = String(repeating: "z", count: 1_000)
    #expect((try? auth(#"{"v":1,"request":"a","reason":"\#(long)"}"#).get().reason.count) == CorralAuthRequest.maxReasonLength)
}

// MARK: Swift → Rust

@Test("notification buttons and Touch ID results are encoded exactly as the fixtures Rust decodes")
func checkpointIntentsMatchFixtures() throws {
    let action = try #require(CorralIntentWire.encodeNotificationAction(checkpoint: "cp-1", nonce: "n-1", action: .approve))
    #expect(try sameJSON(action, fixture("intent-notification-action-v1.json")))
    let failed = try #require(CorralIntentWire.encodeNotificationFailed(checkpoint: "cp-1", reason: .notAuthorized))
    #expect(try sameJSON(failed, fixture("intent-notification-failed-v1.json")))
    let result = try #require(CorralIntentWire.encodeAuthResult(request: "auth-1", outcome: .ok))
    #expect(try sameJSON(result, fixture("intent-auth-result-v1.json")))
}

@Test("only Corral's own notification buttons map to an action")
func responseMapping() {
    #expect(CorralNotificationResponse.action(for: "corral.approve") == .approve)
    #expect(CorralNotificationResponse.action(for: "corral.deny") == .deny)
    #expect(CorralNotificationResponse.action(for: "corral.open") == .open)
    #expect(CorralNotificationResponse.action(for: "com.apple.UNNotificationDefaultActionIdentifier") == .open)
    #expect(CorralNotificationResponse.action(for: "com.apple.UNNotificationDismissActionIdentifier") == .dismiss)
    #expect(CorralNotificationResponse.action(for: "corral.approveAll") == nil)
}
