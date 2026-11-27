import CryptoKit
import Foundation
import SimGate
import XCTest

final class TLSTests: XCTestCase {
    func testPublicSystemAndUnanchoredCA() async throws {
        let fixture = try Fixture()
        try await withRuntime(fixture.grants) { runtime in
            await probe(runtime, "www.apple.com", 443, lookup: true, expected: 0, "Public HTTPS under System")
            await probe(runtime, "hirsel-ci.test", fixture.unanchored, expected: 1, "UnknownIssuer under System")
        }
    }

    func testCustomRootsAndDigest() async throws {
        let fixture = try Fixture()
        try await withRuntime(fixture.grants) { runtime in
            await probe(runtime, "hirsel-ci.test", fixture.valid, fixture.roots, expected: 0, "matching Roots")
            await probe(runtime, "wrong.test", fixture.valid, fixture.roots, expected: 2, "Roots NameMismatch")
            await probe(runtime, "hirsel-ci.test", fixture.expired, fixture.roots, expected: 3, "Roots Expired")
            await probe(runtime, "hirsel-ci.test", fixture.future, fixture.roots, expected: 4, "Roots NotYetValid")
            await probe(runtime, "hirsel-ci.test", fixture.valid, fixture.other, expected: 5, "Roots digest Denied")
        }
    }

    #if targetEnvironment(simulator)
    func testProvisionedSimulatorSystemTrust() async throws {
        let fixture = try Fixture()
        try await withRuntime(fixture.grants) { runtime in
            await probe(runtime, "hirsel-ci.test", fixture.valid, expected: 0, "installed CA")
            await probe(runtime, "wrong.test", fixture.valid, expected: 2, "System NameMismatch")
            await probe(runtime, "hirsel-ci.test", fixture.expired, expected: 3, "System Expired")
            await probe(runtime, "hirsel-ci.test", fixture.future, expected: 4, "System NotYetValid")
        }
    }
    #endif

    private func probe(_ runtime: Runtime, _ name: String, _ port: UInt16, _ roots: Data? = nil,
                       lookup: Bool = false, expected: UInt32, _ row: String) async {
        do {
            let session = try runtime.open()
            let result = try await session.api.call.main.handshake(
                .init(item0: name, item1: port, item2: roots, item3: lookup), deadline: 10_000)
            XCTAssertEqual(result, .returned(expected), row)
            try await session.close()
        } catch {
            XCTFail("\(row): \(error)")
        }
    }

    private struct Fixture {
        let roots: Data
        let other: Data
        let valid: UInt16
        let expired: UInt16
        let future: UInt16
        let unanchored: UInt16

        init() throws {
            let bundle = Bundle(for: TLSTests.self)
            func resource(_ name: String, _ ext: String) throws -> Data {
                let url = try XCTUnwrap(bundle.url(forResource: name, withExtension: ext))
                return try Data(contentsOf: url)
            }
            roots = try resource("primary", "pem")
            other = try resource("unknown", "pem")
            let ports = try JSONDecoder().decode([String: UInt16].self, from: resource("ports", "json"))
            valid = try XCTUnwrap(ports["valid"])
            expired = try XCTUnwrap(ports["expired"])
            future = try XCTUnwrap(ports["future"])
            unanchored = try XCTUnwrap(ports["unanchored"])
        }

        var grants: String {
            let pin = SHA256.hash(data: roots).map { String(format: "%02x", $0) }.joined()
            var rules = [
                "Resolve www.apple.com",
                "Connect 0.0.0.0/0 443-443 public",
                "Connect 2000::/3 443-443 public",
                "Tls www.apple.com 443-443 System",
            ]
            for port in Set([valid, expired, future, unanchored]).sorted() {
                rules.append("Connect 127.0.0.1/32 \(port)-\(port)")
                rules.append("Tls hirsel-ci.test,wrong.test \(port)-\(port) System")
                rules.append("Tls hirsel-ci.test,wrong.test \(port)-\(port) Roots=sha256:\(pin)")
            }
            return "Clock\nRandom\nTasks\nNet=" + rules.joined(separator: ";")
        }
    }
}
