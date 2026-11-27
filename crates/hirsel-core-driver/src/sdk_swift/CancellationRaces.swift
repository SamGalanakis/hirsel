import Dispatch
import Foundation
import XCTest

final class CancellationRaces: XCTestCase {
    func testCancellationBeforeIssueSkipsC() async {
        let audit = Calls()
        let call = CallBox(abandon: audit.abandon)
        call.cancel()
        do {
            _ = try await call.run(register: audit.register)
            XCTFail("CancellationError expected")
        } catch { XCTAssertTrue(error is CancellationError) }
        XCTAssertEqual(audit.counts(), [0, 0])
    }

    func testCancellationHeldAtCRegistrationBoundary() async {
        let audit = Calls()
        let call = CallBox(abandon: audit.abandon)
        let entered = DispatchSemaphore(value: 0)
        let release = DispatchSemaphore(value: 0)
        let task = Task.detached {
            try await call.run {
                entered.signal()
                release.wait()
                return audit.register()
            }
        }
        XCTAssertEqual(entered.wait(timeout: .now() + 5), .success)
        task.cancel()
        XCTAssertEqual(audit.counts(), [0, 0])
        release.signal()
        do {
            _ = try await task.value
            XCTFail("CancellationError expected")
        } catch { XCTAssertTrue(error is CancellationError) }
        XCTAssertEqual(audit.counts(), [1, 1])
    }

    func testEarlyOutcomeBeforeCReturnsWins() async throws {
        let audit = Calls()
        let call = CallBox(abandon: audit.abandon)
        let reply = Data([7])
        let result = try await call.run {
            call.cancel()
            call.resolve(.success(reply))
            return audit.register()
        }
        XCTAssertEqual(result, reply)
        call.cancel()
        XCTAssertEqual(audit.counts(), [1, 0])
    }

    func testAbandonNotLiveLeavesDeliveryInCharge() async throws {
        let audit = Calls()
        let delivering = DispatchSemaphore(value: 0)
        let abandon = DispatchSemaphore(value: 0)
        let call = CallBox {
            _ = audit.abandon()
            abandon.signal()
            delivering.wait()
            return HIRSEL_NOT_LIVE
        }
        let reply = Data([8])
        let task = Task.detached {
            try await call.run {
                call.cancel()
                return audit.register()
            }
        }
        XCTAssertEqual(abandon.wait(timeout: .now() + 5), .success)
        call.resolve(.success(reply))
        delivering.signal()
        let result = try await task.value
        XCTAssertEqual(result, reply)
        call.cancel()
        XCTAssertEqual(audit.counts(), [1, 1])
    }

    func testBreakKeepsCallLiveUntilStreamRelease() async throws {
        let call = SubscriptionCall()
        var retained: AsyncThrowingStream<UInt8, Error>?
        let sink: StreamSink<UInt8>
        (retained, sink) = try StreamSink<UInt8>.make(buffer: 1, onTermination: call.unsubscribe)
        sink.yield(7)

        try await breakAfterFirst(XCTUnwrap(retained))
        withExtendedLifetime(retained) {
            XCTAssertEqual(call.results(), [])
        }

        retained = nil
        XCTAssertEqual(call.results(), [HIRSEL_OK])
        sink.finish()
        XCTAssertEqual(call.results(), [HIRSEL_OK])
    }

    private func breakAfterFirst(_ stream: AsyncThrowingStream<UInt8, Error>) async throws {
        for try await value in stream {
            XCTAssertEqual(value, 7)
            break
        }
    }

    private final class SubscriptionCall: @unchecked Sendable {
        private let lock = NSLock()
        private var completions: [hirsel_status] = []

        func unsubscribe() { lock.withLock { completions.append(HIRSEL_OK) } }
        func results() -> [hirsel_status] { lock.withLock { completions } }
    }

    private final class Calls: @unchecked Sendable {
        private let lock = NSLock()
        private var registered = 0
        private var abandoned = 0

        func register() -> hirsel_status {
            lock.withLock { registered += 1 }
            return HIRSEL_OK
        }

        func abandon() -> hirsel_status {
            lock.withLock { abandoned += 1 }
            return HIRSEL_OK
        }

        func counts() -> [Int] { lock.withLock { [registered, abandoned] } }
    }
}
