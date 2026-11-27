import Foundation
import XCTest

final class CodecVectors: XCTestCase {
    func testChangeAndEnds() throws {
        let change = Change(
            invalidated: false, replaced: true,
            value: ValueChange(old: nil, new: Data([1, 2])),
            rows: [RowChange(slot: 0, key: Data([0xaa]), old: nil, new: Data([0xbb]))]
        )
        try check(ChangeCodec, change, "00 01 01000000 00000000 02000000 0102 01000000 00000000 01000000 aa 00000000 01000000 01000000 bb")
        try check(SessionEndCodec, .dialRefused(503), "03000000 f701")
        try check(SubEndCodec, .clientCapacity, "01000000")
        try check(SubEndCodec, .server(hex("02000000 02000000")), "00000000 08000000 02000000 02000000")
        try check(WireOutcomeCodec, .refused(.overloaded), "02000000 02000000")
        try check(WireOutcomeCodec, .returned(hex("dead")), "00000000 02000000 dead")
    }

    func testMixedPartAppliesStateBeforeMessageAndLeave() throws {
        // sdk/client/room.ts applies joined and state before messages, then left and snapshot.
        let incoming = [
            "01000000 01000000 10000000 01000000000000000200000000000000 00000000 01 01000000 0200000000000000 01000000 0200000000000000 06000000 0101000000aa 01000000 01000000 11 01000000 0200000000000000 01000000 01000000 11 00000000",
            "02000000 0200000000000000 01000000 22 01",
            "01000000 01000000 10000000 01000000000000000200000000000000 01000000 0700000000000000 00 01000000 0200000000000000 00000000 00000000 01000000 0200000000000000",
            "03000000 0300000000000000",
            "04000000 0400000000000000",
        ]
        let incarnation = hex("01000000000000000200000000000000")
        let expected: [RoomEvent] = [
            .peers(PeersChange(
                incarnation: incarnation, revision: nil, renewed: true, me: 2,
                joined: [Peer(peer: 2, caller: hex("0101000000aa"), state: Data([0x11]))],
                states: [PeerState(peer: 2, state: Data([0x11]))], left: []
            )),
            .message(MessageIn(from: 2, message: Data([0x22]), direct: true)),
            .peers(PeersChange(
                incarnation: incarnation, revision: 7, renewed: false, me: 2,
                joined: [], states: [], left: [2]
            )),
            .gap(3),
            .lost(4),
        ]
        XCTAssertEqual(try incoming.map { try RoomEventCodec.decode(hex($0)) }, expected)
        XCTAssertEqual(try expected.map(RoomEventCodec.encode), incoming.map(hex))
    }

    func testFirstFullPartHasNoPeersEvent() throws {
        let incoming = [hex("05000000")]
        XCTAssertEqual(try incoming.map(RoomEventCodec.decode), [.full])
        XCTAssertEqual(try RoomEventCodec.encode(.full), incoming[0])
    }

    func testNestedSomeNoneAndMalformedBytes() throws {
        let codec = HirselCodec(HirselNestedOption<Optional<UInt32>>.self)
        try check(codec, .some(nil), "01000000 00000000")
        try check(codec, .none, "00000000")
        for (bytes, error) in [
            ("01", HirselCodecError.truncated),
            ("07000000", .invalidTag),
            ("05000000 00", .trailingBytes),
        ] {
            XCTAssertThrowsError(try RoomEventCodec.decode(hex(bytes))) {
                XCTAssertEqual($0 as? HirselCodecError, error)
            }
        }
    }

    private func check<Value>(_ codec: HirselCodec<Value>, _ value: Value, _ bytes: String) throws {
        XCTAssertEqual(try codec.decode(hex(bytes)), value)
        XCTAssertEqual(try codec.encode(value), hex(bytes))
    }

    private func hex(_ text: String) -> Data {
        let digits = Array(text.filter { !$0.isWhitespace })
        return Data(stride(from: 0, to: digits.count, by: 2).map {
            UInt8(String(digits[$0...($0 + 1)]), radix: 16)!
        })
    }
}
