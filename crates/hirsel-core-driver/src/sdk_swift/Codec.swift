import Foundation

public enum HirselCodecError: Error, Equatable, Sendable {
    case truncated, trailingBytes, invalidTag, invalidScalar, invalidUTF8
    case invalidEscape, noncanonicalNaN, duplicateMapKey, countOverflow
}

public struct HirselCodec<Value: Sendable & Equatable>: Sendable {
    private let read: @Sendable (inout WireReader) throws -> Value
    private let write: @Sendable (Value, inout WireWriter) throws -> Void
    private let key: Bool

    init(_ type: Value.Type, key: Bool = false) where Value: WireValue {
        self.read = { try Value.read(&$0) }
        self.write = { try $0.write(&$1) }
        self.key = key
    }

    public func decode(_ bytes: Data) throws -> Value {
        var reader = WireReader(data: bytes, key: key)
        let value = try read(&reader)
        guard reader.at == bytes.count else { throw HirselCodecError.trailingBytes }
        return value
    }

    public func encode(_ value: Value) throws -> Data {
        var writer = WireWriter(key: key)
        try write(value, &writer)
        return writer.data
    }
}

protocol WireValue: Sendable, Equatable {
    static func read(_ reader: inout WireReader) throws -> Self
    func write(_ writer: inout WireWriter) throws
}

struct WireReader {
    let data: Data
    let key: Bool
    var at = 0
    var inverted = false

    mutating func take(_ count: Int) throws -> Data.SubSequence {
        guard count <= data.count - at else { throw HirselCodecError.truncated }
        let start = data.startIndex + at
        at += count
        return data[start..<(start + count)]
    }

    mutating func word<T: FixedWidthInteger>(_ type: T.Type, ordered: Bool = false) throws -> T {
        let bytes = try take(T.bitWidth / 8)
        let mask: UInt8 = inverted ? 255 : 0
        var value: T = 0
        for (index, byte) in bytes.enumerated() {
            let shift = (ordered ? bytes.count - 1 - index : index) * 8
            value |= T(truncatingIfNeeded: byte ^ mask) << shift
        }
        if ordered && T.isSigned { value ^= T.min }
        return value
    }

    mutating func count() throws -> Int {
        return Int(try word(UInt32.self))
    }
}

struct WireWriter {
    let key: Bool
    var data = Data()
    var inverted = false

    mutating func word<T: FixedWidthInteger>(_ word: T, ordered: Bool = false) {
        let width = T.bitWidth / 8
        let value = ordered && T.isSigned ? word ^ T.min : word
        let mask: UInt8 = inverted ? 255 : 0
        for index in 0..<width {
            let shift = (ordered ? width - 1 - index : index) * 8
            data.append(UInt8(truncatingIfNeeded: value >> shift) ^ mask)
        }
    }

    mutating func count(_ count: Int) throws {
        guard let value = UInt32(exactly: count) else { throw HirselCodecError.countOverflow }
        word(value)
    }
}

extension WireValue where Self: FixedWidthInteger {
    static func read(_ reader: inout WireReader) throws -> Self {
        return try reader.word(Self.self, ordered: reader.key)
    }

    func write(_ writer: inout WireWriter) {
        writer.word(self, ordered: writer.key)
    }
}

extension Int8: WireValue {}
extension Int16: WireValue {}
extension Int32: WireValue {}
extension Int64: WireValue {}
extension UInt8: WireValue {}
extension UInt16: WireValue {}
extension UInt32: WireValue {}
extension UInt64: WireValue {}

public struct HirselUnit: WireValue {
    public init() {}
    static func read(_ reader: inout WireReader) -> Self { Self() }
    func write(_ writer: inout WireWriter) {}
}

extension Never: WireValue {
    static func read(_ reader: inout WireReader) throws -> Self {
        throw HirselCodecError.invalidTag
    }

    func write(_ writer: inout WireWriter) {}
}

extension Bool: WireValue {
    static func read(_ reader: inout WireReader) throws -> Self {
        switch try reader.word(UInt8.self) {
        case 0: return false
        case 1: return true
        default: throw HirselCodecError.invalidTag
        }
    }

    func write(_ writer: inout WireWriter) { writer.word(self ? UInt8(1) : UInt8(0)) }
}

extension Unicode.Scalar: WireValue {
    static func read(_ reader: inout WireReader) throws -> Self {
        guard let scalar = Self(try reader.word(UInt32.self)) else { throw HirselCodecError.invalidScalar }
        return scalar
    }

    func write(_ writer: inout WireWriter) { writer.word(value) }
}

extension Float: WireValue {
    static func read(_ reader: inout WireReader) throws -> Self {
        let bits = try reader.word(UInt32.self)
        let value = Self(bitPattern: bits)
        guard !value.isNaN || bits == 0x7fc00000 else { throw HirselCodecError.noncanonicalNaN }
        return value
    }

    func write(_ writer: inout WireWriter) { writer.word(isNaN ? 0x7fc00000 : bitPattern) }
}

extension Double: WireValue {
    static func read(_ reader: inout WireReader) throws -> Self {
        let bits = try reader.word(UInt64.self)
        let value = Self(bitPattern: bits)
        guard !value.isNaN || bits == 0x7ff8000000000000 else { throw HirselCodecError.noncanonicalNaN }
        return value
    }

    func write(_ writer: inout WireWriter) { writer.word(isNaN ? 0x7ff8000000000000 : bitPattern) }
}

extension Data: WireValue {
    static func read(_ reader: inout WireReader) throws -> Self {
        if !reader.key {
            let count = try reader.count()
            return Data(try reader.take(count))
        }
        var bytes = Data()
        while true {
            let byte = try reader.word(UInt8.self)
            if byte != 0 { bytes.append(byte); continue }
            switch try reader.word(UInt8.self) {
            case 0: return bytes
            case 255: bytes.append(0)
            default: throw HirselCodecError.invalidEscape
            }
        }
    }

    func write(_ writer: inout WireWriter) throws {
        if !writer.key {
            try writer.count(count)
            writer.data.append(self)
            return
        }
        for byte in self {
            writer.word(byte)
            if byte == 0 { writer.word(UInt8(255)) }
        }
        writer.word(UInt16(0))
    }
}

extension String: WireValue {
    static func read(_ reader: inout WireReader) throws -> Self {
        guard let string = Self(validating: try Data.read(&reader), as: UTF8.self) else {
            throw HirselCodecError.invalidUTF8
        }
        return string
    }

    func write(_ writer: inout WireWriter) throws { try Data(utf8).write(&writer) }
}

extension Array: WireValue where Element: WireValue {
    static func read(_ reader: inout WireReader) throws -> Self {
        let count = try reader.count()
        var values = Self()
        for _ in 0..<count { values.append(try Element.read(&reader)) }
        return values
    }

    func write(_ writer: inout WireWriter) throws {
        try writer.count(count)
        for value in self { try value.write(&writer) }
    }
}

extension Optional: WireValue where Wrapped: WireValue {
    static func read(_ reader: inout WireReader) throws -> Self {
        switch try reader.count() {
        case 0: return .none
        case 1: return .some(try Wrapped.read(&reader))
        default: throw HirselCodecError.invalidTag
        }
    }

    func write(_ writer: inout WireWriter) throws {
        switch self {
        case .none: writer.word(UInt32(0))
        case .some(let value):
            writer.word(UInt32(1))
            try value.write(&writer)
        }
    }
}

public enum HirselNestedOption<Value: Equatable & Sendable>: Equatable, Sendable {
    case none
    case some(Value)
}

extension HirselNestedOption: WireValue where Value: WireValue {
    static func read(_ reader: inout WireReader) throws -> Self {
        switch try Optional<Value>.read(&reader) {
        case .none: return .none
        case .some(let value): return .some(value)
        }
    }

    func write(_ writer: inout WireWriter) throws {
        switch self {
        case .none: try Optional<Value>.none.write(&writer)
        case .some(let value): try Optional<Value>.some(value).write(&writer)
        }
    }
}

public enum HirselResult<Value: Equatable & Sendable, Failure: Equatable & Sendable>: Equatable, Sendable {
    case ok(Value)
    case error(Failure)
}

extension HirselResult: WireValue where Value: WireValue, Failure: WireValue {
    static func read(_ reader: inout WireReader) throws -> Self {
        switch try reader.count() {
        case 0: return .ok(try Value.read(&reader))
        case 1: return .error(try Failure.read(&reader))
        default: throw HirselCodecError.invalidTag
        }
    }

    func write(_ writer: inout WireWriter) throws {
        switch self {
        case .ok(let value):
            writer.word(UInt32(0))
            try value.write(&writer)
        case .error(let error):
            writer.word(UInt32(1))
            try error.write(&writer)
        }
    }
}

public struct HirselMap<Key: Equatable & Sendable, Value: Equatable & Sendable>: Equatable, Sendable {
    public struct Entry: Equatable, Sendable {
        public let key: Key
        public let value: Value
        public init(key: Key, value: Value) { self.key = key; self.value = value }
    }

    public let entries: [Entry]
    public init(_ entries: [Entry]) { self.entries = entries }
}

extension HirselMap: WireValue where Key: WireValue, Value: WireValue {
    static func read(_ reader: inout WireReader) throws -> Self {
        let count = try reader.count()
        var entries: [Entry] = []
        var seen: Set<Data> = []
        for _ in 0..<count {
            let start = reader.data.startIndex + reader.at
            let key = try Key.read(&reader)
            let end = reader.data.startIndex + reader.at
            guard seen.insert(Data(reader.data[start..<end])).inserted else {
                throw HirselCodecError.duplicateMapKey
            }
            entries.append(Entry(key: key, value: try Value.read(&reader)))
        }
        return Self(entries)
    }

    func write(_ writer: inout WireWriter) throws {
        try writer.count(entries.count)
        var seen: Set<Data> = []
        for entry in entries {
            var key = WireWriter(key: writer.key, inverted: writer.inverted)
            try entry.key.write(&key)
            guard seen.insert(key.data).inserted else { throw HirselCodecError.duplicateMapKey }
            writer.data.append(key.data)
            try entry.value.write(&writer)
        }
    }
}

public struct HirselDescending<Value: Equatable & Sendable>: Equatable, Sendable {
    public let value: Value
    public init(_ value: Value) { self.value = value }
}

extension HirselDescending: WireValue where Value: WireValue {
    static func read(_ reader: inout WireReader) throws -> Self {
        reader.inverted.toggle()
        defer { reader.inverted.toggle() }
        return Self(try Value.read(&reader))
    }

    func write(_ writer: inout WireWriter) throws {
        writer.inverted.toggle()
        defer { writer.inverted.toggle() }
        try value.write(&writer)
    }
}

// The generic calls also cover zero-byte types with non-throwing implementations.
func readWire<T: WireValue>(_ type: T.Type, _ reader: inout WireReader) throws -> T {
    return try T.read(&reader)
}

func writeWire<T: WireValue>(_ value: T, _ writer: inout WireWriter) throws {
    try value.write(&writer)
}
