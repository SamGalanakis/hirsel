import Foundation
import SimGateCore
@testable import SimGate
import XCTest

final class AbiTests: XCTestCase {
    func testWelcomeTarget() async throws {
        try await withRuntime { runtime in
            let session = try runtime.open(credential: fixtureCredential)
            let reply = try await session.api.call.main.answer(0, deadline: 0)
            XCTAssertEqual(reply, .returned(7))
            var target = [UInt8](repeating: 0, count: 128)
            let count = hirsel_embed_harness_welcome_target(runtime.handle.generation, &target, target.count)
            XCTAssertGreaterThan(count, 0)
            #if targetEnvironment(simulator)
            let expected = "aarch64-apple-ios-sim"
            #else
            let expected = "aarch64-apple-darwin"
            #endif
            XCTAssertEqual(String(decoding: target.prefix(max(count, 0)), as: UTF8.self), expected)
            try await session.close()
        }
    }

    func testHostClose() async throws {
        try await withRuntime { runtime in
            let observer = Observer()
            let session = try observer.open(runtime)
            let status = await Task.detached { hirsel_embed_v1_close(session) }.value
            XCTAssertEqual(status, HIRSEL_OK)
            XCTAssertEqual(observer.ended.wait(timeout: .now()), .success)
            XCTAssertEqual(hirsel_embed_v1_close(session), HIRSEL_STOPPED)
        }
    }

    func testCloseFromCallback() async throws {
        try await withRuntime { runtime in
            let observer = Observer()
            let session = try observer.open(runtime)
            observer.onCall = {
                observer.setStatus(hirsel_embed_v1_close(session))
            }
            try issue(session, id: 1)
            XCTAssertEqual(observer.ended.wait(timeout: .now() + 5), .success)
            XCTAssertEqual(observer.status, HIRSEL_DEFERRED)
        }
    }

    func testExternalCloseRacesDeferredDrain() async throws {
        try await withRuntime { runtime in
            let observer = Observer()
            let callback = DispatchSemaphore(value: 0)
            let release = DispatchSemaphore(value: 0)
            let session = try observer.open(runtime)
            observer.onCall = {
                observer.setStatus(hirsel_embed_v1_close(session))
                callback.signal()
                _ = release.wait(timeout: .now() + 5)
            }
            try issue(session, id: 1)
            let entered = callback.wait(timeout: .now() + 5)
            XCTAssertEqual(entered, .success)
            XCTAssertEqual(observer.status, HIRSEL_DEFERRED)
            XCTAssertEqual(observer.ended.wait(timeout: .now()), .timedOut)
            let other = hirsel_embed_v1_close(session)
            release.signal()
            XCTAssertEqual(other, HIRSEL_STOPPED)
            XCTAssertEqual(observer.ended.wait(timeout: .now() + 5), .success)
        }
    }

    func testAbandonBeforeDelivery() async throws {
        try await withRuntime { runtime in
            let observer = Observer()
            let callback = DispatchSemaphore(value: 0)
            let release = DispatchSemaphore(value: 0)
            let session = try observer.open(runtime)
            observer.onCall = {
                callback.signal()
                _ = release.wait(timeout: .now() + 5)
            }
            try issue(session, id: 1)
            XCTAssertEqual(callback.wait(timeout: .now() + 5), .success)
            try issue(session, id: 2)
            XCTAssertEqual(hirsel_embed_v1_abandon(session, 2), HIRSEL_OK)
            XCTAssertEqual(hirsel_embed_v1_abandon(session, 2), HIRSEL_NOT_LIVE)
            release.signal()
            XCTAssertEqual(hirsel_embed_v1_close(session), HIRSEL_OK)
            XCTAssertEqual(observer.ids, [1])
        }
    }

    private func issue(_ session: hirsel_session, id: UInt64) throws {
        let projection = try XCTUnwrap(Bundle(for: Self.self).url(forResource: "projection", withExtension: "json"))
        let root = try XCTUnwrap(JSONSerialization.jsonObject(with: Data(contentsOf: projection)) as? [String: Any])
        let entries = try XCTUnwrap(root["entries"] as? [[String: Any]])
        let index = try XCTUnwrap(entries.firstIndex { $0["name"] as? String == "answer" })
        let status = [UInt8](repeating: 0, count: 8).withUnsafeBufferPointer {
            hirsel_embed_v1_call(session, id, UInt32(index), hirsel_bytes(ptr: $0.baseAddress, len: $0.count), 5_000)
        }
        XCTAssertEqual(status, HIRSEL_OK)
    }

    private final class Observer: @unchecked Sendable {
        let ended = DispatchSemaphore(value: 0)
        private let lock = NSLock()
        private var callHook: (@Sendable () -> Void)?
        private var calls: [UInt64] = []
        private var result: hirsel_status?

        var onCall: (@Sendable () -> Void)? {
            get { lock.withLock { callHook } }
            set { lock.withLock { callHook = newValue } }
        }
        var status: hirsel_status? { lock.withLock { result } }
        var ids: [UInt64] { lock.withLock { calls } }
        func setStatus(_ status: hirsel_status) { lock.withLock { result = status } }

        func open(_ runtime: Runtime) throws -> hirsel_session {
            var callbacks = hirsel_embed_callbacks()
            callbacks.size = UInt32(MemoryLayout<hirsel_embed_callbacks>.size)
            callbacks.context = Unmanaged.passRetained(self).toOpaque()
            callbacks.outcome = { context, id, _ in
                let observer = Unmanaged<Observer>.fromOpaque(context!).takeUnretainedValue()
                observer.lock.withLock { observer.calls.append(id) }
                observer.onCall?()
            }
            callbacks.change = { _, _, _ in }
            callbacks.sub_ended = { _, _, _ in }
            callbacks.room_event = { _, _, _ in }
            callbacks.session_ended = { context, _ in
                let observer = Unmanaged<Observer>.fromOpaque(context!).takeRetainedValue()
                observer.onCall = nil
                observer.ended.signal()
            }
            var session = hirsel_session()
            let status = fixtureCredential.withUnsafeBytes { credential in
                let bytes = credential.bindMemory(to: UInt8.self)
                return hirsel_embed_v1_open(runtime.handle, &callbacks,
                                            hirsel_bytes(ptr: bytes.baseAddress, len: bytes.count), &session)
            }
            if status != HIRSEL_OK {
                Unmanaged<Observer>.fromOpaque(callbacks.context!).release()
                throw EmbedError.status(status.rawValue)
            }
            return session
        }
    }
}
