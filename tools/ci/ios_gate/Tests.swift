import CryptoKit
import Darwin
import Foundation
import XCTest
import SimGateCore
@testable import SimGate

let baseGrants = "Clock\nRandom\nTasks\nNet=Connect 127.0.0.1/32 1-1"

func withRuntime<Value>(_ grants: String = baseGrants,
                        _ body: (Runtime) async throws -> Value) async throws -> Value {
    let runtime = try Runtime.start(grants: grants)
    let result: Value
    do {
        result = try await body(runtime)
    } catch {
        try await runtime.stop()
        throw error
    }
    try await runtime.stop()
    return result
}

final class RuntimeTests: XCTestCase {
    func testProjection() throws {
        let resource = try XCTUnwrap(Bundle(for: Self.self).url(forResource: "projection", withExtension: "json"))
        let shipped = try Data(contentsOf: resource)
        XCTAssertEqual(Data(SHA256.hash(data: shipped)), manifest_fp)
        let bytes = hirsel_embed_v1_manifest()
        let manifest = Data(bytes: try XCTUnwrap(bytes.ptr), count: bytes.len)
        var linked = try XCTUnwrap(JSONSerialization.jsonObject(with: manifest) as? [String: Any])
        XCTAssertNotNil(linked.removeValue(forKey: "launch"))
        let entries = try XCTUnwrap(linked["entries"] as? [[String: Any]])
        linked["entries"] = entries.map { $0.filter { $0.key != "revision" } }
        let expected = try JSONSerialization.jsonObject(with: shipped) as? NSDictionary
        XCTAssertEqual(linked as NSDictionary, expected)
    }

    func testPlaceCapacity() {
        XCTAssertThrowsError(try Runtime.start(grants: baseGrants, nPlace: 1)) {
            guard case EmbedError.status(let status, let reason) = $0 else {
                return XCTFail("Expected place registration refusal, got \($0)")
            }
            XCTAssertEqual(status, HIRSEL_REFUSED.rawValue)
            XCTAssertTrue(reason?.hasPrefix("PlaceCapacity(Room { requested: ") == true, reason ?? "missing reason")
        }
    }

    func testCallAndTwoSubscriptionChanges() async throws {
        try await withRuntime { runtime in
            let session = try runtime.open()
            let answer = try await session.api.call.main.answer(35, deadline: 0)
            XCTAssertEqual(answer, .returned(42))

            let initial = try await session.api.call.main.write(512, deadline: 0)
            XCTAssertEqual(initial, .returned(512))
            let subscription = session.api.subscribe.main.rows(HirselUnit())
            var updates = subscription.makeAsyncIterator()
            let first = try await updates.next()
            XCTAssertNotNil(first)
            for (value, bytes) in [(UInt64(513), Data([1, 2, 0, 0, 0, 0, 0, 0])),
                                   (UInt64(514), Data([2, 2, 0, 0, 0, 0, 0, 0]))] {
                let written = try await session.api.call.main.write(value, deadline: 0)
                XCTAssertEqual(written, .returned(value))
                let event = try await updates.next()
                let change = try XCTUnwrap(event)
                XCTAssertEqual(change.rows.count, 1)
                XCTAssertEqual(change.rows.first?.new, bytes)
            }
            try await session.close()
        }
    }

    func testRoomSetSendAndLeave() async throws {
        try await withRuntime { runtime in
            let a = try runtime.open()
            let b = try runtime.open()
            let roomA = try a.api.room.main.room(HirselUnit())
            var aEvents = roomA.events.makeAsyncIterator()
            guard case .peers(let selfA)? = try await aEvents.next() else {
                return XCTFail("Expected A to join")
            }
            let roomB = try b.api.room.main.room(HirselUnit())
            var bEvents = roomB.events.makeAsyncIterator()
            guard case .peers(let selfB)? = try await bEvents.next() else {
                return XCTFail("Expected B to join")
            }
            XCTAssertNotEqual(selfA.me, selfB.me)
            let joined = try await aEvents.next()
            guard case .peers(let join)? = joined else { return XCTFail("Expected B to join A") }
            XCTAssertEqual(join.joined.map(\.peer), [try XCTUnwrap(selfB.me)])

            let c = try runtime.open()
            let fullRoom = try c.api.room.main.room(HirselUnit())
            var fullEvents = fullRoom.events.makeAsyncIterator()
            let first = try await fullEvents.next()
            XCTAssertEqual(first, .full)
            try await c.close()

            try roomB.set(513)
            guard case .peers(let set)? = try await aEvents.next() else {
                return XCTFail("Expected B's state on A")
            }
            XCTAssertEqual(set.states, [PeerState(peer: try XCTUnwrap(selfB.me),
                                                 state: Data([1, 2, 0, 0, 0, 0, 0, 0]))])
            try roomB.send(514, to: selfA.me)
            guard case .message(let message)? = try await aEvents.next() else {
                return XCTFail("Expected B's direct message on A")
            }
            XCTAssertEqual(message.from, selfB.me)
            XCTAssertEqual(message.message, Data([2, 2, 0, 0, 0, 0, 0, 0]))
            XCTAssertTrue(message.direct)
            try roomB.leave()
            guard case .peers(let left)? = try await aEvents.next() else {
                return XCTFail("Expected B to leave A")
            }
            XCTAssertEqual(left.left, [try XCTUnwrap(selfB.me)])
            try roomA.leave()
            try await a.close()
            try await b.close()
        }
    }

    func testStopThenStart() async throws {
        let old = try await withRuntime { runtime in
            let session = try runtime.open()
            let result = try await session.api.call.main.answer(0, deadline: 0)
            XCTAssertEqual(result, .returned(7))
            return session
        }
        try await withRuntime { runtime in
            XCTAssertEqual(hirsel_embed_v1_call(old.handle, 99, 0, hirsel_bytes(), 0), HIRSEL_STOPPED)
            let fresh = try runtime.open()
            let result = try await fresh.api.call.main.answer(1, deadline: 0)
            XCTAssertEqual(result, .returned(8))
            try await fresh.close()
        }
    }

    func testMeasurements() async throws {
        let clock = ContinuousClock()
        let start = clock.now
        try await withRuntime { runtime in
            let session = try runtime.open()
            let first = try await session.api.call.main.answer(0, deadline: 0)
            let firstTime = start.duration(to: clock.now)
            let memoryAtStart = memory()
            let workerCount = hirsel_gate_workers()
            let workers = workerCount == UInt32.max ? "unavailable" : String(workerCount)
            XCTAssertEqual(first, .returned(7))
            var calls: [Duration] = []
            for index in 0..<1000 {
                let beforeCall = clock.now
                let result = try await session.api.call.main.answer(UInt64(index), deadline: 0)
                calls.append(beforeCall.duration(to: clock.now))
                XCTAssertEqual(result, .returned(UInt64(index + 7)))
            }
            calls.sort()
            print("HIRSEL_MEASUREMENT p50=\(calls[499]) p99=\(calls[989]) start_to_first=\(firstTime) \(memoryAtStart) worker_threads=\(workers)")
            try await session.close()
        }
        let runtime = try Runtime.start(grants: baseGrants)
        let beforeStop = clock.now
        try await runtime.stop()
        try await withRuntime { _ in
            print("HIRSEL_MEASUREMENT stop_then_start=\(beforeStop.duration(to: clock.now))")
        }
    }

    private func memory() -> String {
        var info = mach_task_basic_info()
        var count = mach_msg_type_number_t(MemoryLayout<mach_task_basic_info>.size / MemoryLayout<integer_t>.size)
        let status = withUnsafeMutablePointer(to: &info) { pointer in
            pointer.withMemoryRebound(to: integer_t.self, capacity: Int(count)) {
                task_info(mach_task_self_, task_flavor_t(MACH_TASK_BASIC_INFO), $0, &count)
            }
        }
        guard status == KERN_SUCCESS else { return "task_info_error=\(status)" }
        return "resident_bytes=\(info.resident_size) virtual_bytes=\(info.virtual_size)"
    }
}
