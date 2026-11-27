import Dispatch
import Foundation

public enum EmbedError: Error, Equatable, Sendable {
    case status(UInt32, reason: String? = nil)
    case sessionEnded(SessionEnd)
    case subscriptionEnded(SubEnd)
    case clientCapacity
    case invalidBuffer
    case idsExhausted
}

public enum Outcome<Value: Equatable & Sendable, Failure: Equatable & Sendable>: Equatable, Sendable {
    case returned(Value)
    case returnedError(Failure)
    case failed(RunnerFault)
    case refused(RunnerRefusal)
    case abandoned
    case inDoubt
}

func decodeOutcome<Value>(_ bytes: Data, _ codec: HirselCodec<Value>) throws -> Outcome<Value, Never> {
    return try readOutcome(bytes) { .returned(try codec.decode($0)) }
}

func decodeResultOutcome<Value, Failure>(
    _ bytes: Data, _ codec: HirselCodec<HirselResult<Value, Failure>>
) throws -> Outcome<Value, Failure> {
    return try readOutcome(bytes) {
        switch try codec.decode($0) {
        case .ok(let value): return .returned(value)
        case .error(let error): return .returnedError(error)
        }
    }
}

private func readOutcome<Value, Failure>(
    _ bytes: Data, returned: (Data) throws -> Outcome<Value, Failure>
) throws -> Outcome<Value, Failure> {
    switch try WireOutcomeCodec.decode(bytes) {
    case .returned(let data): return try returned(data)
    case .failed(let fault): return .failed(fault)
    case .refused(let refusal): return .refused(refusal)
    case .abandoned: return .abandoned
    case .inDoubt: return .inDoubt
    }
}

private func withBytes<Value>(_ data: Data, _ body: (hirsel_bytes) throws -> Value) rethrows -> Value {
    return try data.withUnsafeBytes { bytes in
        try body(hirsel_bytes(ptr: bytes.bindMemory(to: UInt8.self).baseAddress, len: bytes.count))
    }
}

private func copyBytes(_ bytes: hirsel_bytes) -> Data {
    guard let ptr = bytes.ptr else { return Data() }
    return Data(bytes: ptr, count: bytes.len)
}

private func checkStatus(_ status: hirsel_status) throws {
    guard status == HIRSEL_OK else { throw EmbedError.status(status.rawValue) }
}

private final class RuntimeEnd: @unchecked Sendable {
    private let lock = NSLock()
    private var reason: String?

    func set(_ bytes: hirsel_bytes) {
        let reason = String(decoding: copyBytes(bytes), as: UTF8.self)
        lock.withLock { self.reason = reason }
    }

    func get() -> String? { lock.withLock { reason } }
}

public struct Runtime: @unchecked Sendable {
    let handle: hirsel_runtime

    public static func start(
        grants: String,
        nPlace: UInt64 = 16 * 1024 * 1024,
        clientBytes: UInt64 = 64 * 1024 * 1024,
        startTimeoutMs: UInt32 = 10_000
    ) throws -> Runtime {
        let digest = requireEmbedAnchor()
        let files = FileManager.default
        let support = try files.url(for: .applicationSupportDirectory, in: .userDomainMask,
                                    appropriateFor: nil, create: true)
            .appendingPathComponent("Hirsel", isDirectory: true)
            .appendingPathComponent(embedPackage, isDirectory: true)
        let caches = try files.url(for: .cachesDirectory, in: .userDomainMask,
                                   appropriateFor: nil, create: true)
            .appendingPathComponent("Hirsel", isDirectory: true)
            .appendingPathComponent(embedPackage, isDirectory: true)
        let store = support.appendingPathComponent("store", isDirectory: true)
        let dirs = [
            ("config", support.appendingPathComponent("config", isDirectory: true)),
            ("cache", caches),
            ("data", support.appendingPathComponent("data", isDirectory: true)),
            ("state", support.appendingPathComponent("state", isDirectory: true)),
        ]
        for (_, dir) in dirs {
            try files.createDirectory(at: dir, withIntermediateDirectories: true)
        }
        try files.createDirectory(at: store, withIntermediateDirectories: true)
        let bases = Data(dirs.map { "\($0.0)=\($0.1.path)\n" }.joined().utf8)

        let ended = RuntimeEnd()
        let context = Unmanaged.passRetained(ended)
        var handle = hirsel_runtime()
        let status = withBytes(manifest_fp) { fingerprint in
            withBytes(digest) { digest in
                withBytes(Data(grants.utf8)) { grants in
                    withBytes(bases) { bases in
                        withBytes(Data(store.path.utf8)) { store in
                            var config = hirsel_embed_config(
                                size: UInt32(MemoryLayout<hirsel_embed_config>.size),
                                abi: embedABI, manifest_fp: fingerprint, digest: digest,
                                n_place: nPlace, client_bytes: clientBytes,
                                start_timeout_ms: startTimeoutMs, grants: grants,
                                dir_bases: bases, store_dir: store,
                                context: context.toOpaque(),
                                ended: { context, _, reason in
                                    if let context {
                                        Unmanaged<RuntimeEnd>.fromOpaque(context).takeRetainedValue().set(reason)
                                    }
                                }
                            )
                            return hirsel_embed_v1_start(&config, &handle)
                        }
                    }
                }
            }
        }
        if status == HIRSEL_REFUSED {
            throw EmbedError.status(status.rawValue, reason: ended.get())
        }
        if status != HIRSEL_OK {
            context.release()
            try checkStatus(status)
        }
        return Runtime(handle: handle)
    }

    public func open(credential: Data = Data()) throws -> Session {
        let state = SessionState()
        let context = Unmanaged.passRetained(state)
        var callbacks = hirsel_embed_callbacks(
            size: UInt32(MemoryLayout<hirsel_embed_callbacks>.size),
            context: context.toOpaque(),
            outcome: { context, id, bytes in sessionState(context)?.outcome(id, bytes) },
            change: { context, id, bytes in sessionState(context)?.change(id, bytes) },
            sub_ended: { context, id, bytes in sessionState(context)?.subEnded(id, bytes) },
            room_event: { context, id, bytes in sessionState(context)?.roomEvent(id, bytes) },
            session_ended: { context, bytes in
                if let context {
                    Unmanaged<SessionState>.fromOpaque(context).takeRetainedValue().sessionEnded(bytes)
                }
            }
        )
        var session = hirsel_session()
        let status = withBytes(credential) { hirsel_embed_v1_open(handle, &callbacks, $0, &session) }
        if status != HIRSEL_OK {
            context.release()
            try checkStatus(status)
        }
        return Session(handle: session, state: state)
    }

    public func stop() async throws {
        try await withCheckedThrowingContinuation { (resumed: CheckedContinuation<Void, Error>) in
            DispatchQueue.global().async {
                resumed.resume(with: Result { try checkStatus(hirsel_embed_v1_stop(handle)) })
            }
        }
    }
}

private func sessionState(_ context: UnsafeMutableRawPointer?) -> SessionState? {
    guard let context else { return nil }
    return Unmanaged<SessionState>.fromOpaque(context).takeUnretainedValue()
}

public struct Session: @unchecked Sendable {
    let handle: hirsel_session
    fileprivate let state: SessionState

    public var api: API { API(session: self) }

    public func close() async throws {
        try await withCheckedThrowingContinuation { (resumed: CheckedContinuation<Void, Error>) in
            DispatchQueue.global().async {
                resumed.resume(with: Result { try checkStatus(hirsel_embed_v1_close(handle)) })
            }
        }
    }

    func call(_ want: UInt32, _ args: Data, deadline: UInt32) async throws -> Data {
        let id = try state.nextID()
        let call = CallBox { hirsel_embed_v1_abandon(handle, id) }
        try state.insert(.call(call), at: id)
        defer { _ = state.remove(id) }
        return try await call.run {
            withBytes(args) { hirsel_embed_v1_call(handle, id, want, $0, deadline) }
        }
    }

    func subscribe(_ want: UInt32, _ args: Data, buffer: Int) throws -> AsyncThrowingStream<Change, Error> {
        let id = try state.nextID()
        let (stream, sink) = try stream(Change.self, buffer: buffer, id: id, end: { hirsel_embed_v1_unsubscribe(handle, id) })
        try state.insert(.subscription(sink), at: id)
        let status = withBytes(args) { hirsel_embed_v1_subscribe(handle, id, want, $0) }
        if status != HIRSEL_OK {
            _ = state.remove(id)
            try checkStatus(status)
        }
        return stream
    }

    func join<State, Message>(
        _ want: UInt32, _ args: Data, buffer: Int,
        state stateCodec: HirselCodec<State>, message messageCodec: HirselCodec<Message>
    ) throws -> Room<State, Message> {
        let id = try state.nextID()
        let (events, sink) = try stream(RoomEvent.self, buffer: buffer, id: id, end: { hirsel_embed_v1_room_leave(handle, id) })
        try state.insert(.room(sink), at: id)
        let status = withBytes(args) { hirsel_embed_v1_room_join(handle, id, want, $0) }
        if status != HIRSEL_OK {
            _ = state.remove(id)
            try checkStatus(status)
        }
        return Room(events: events, session: self, id: id, state: stateCodec, message: messageCodec)
    }

    private func stream<Element: Sendable>(
        _ element: Element.Type, buffer: Int, id: UInt64, end: @escaping @Sendable () -> hirsel_status
    ) throws -> (AsyncThrowingStream<Element, Error>, StreamSink<Element>) {
        return try StreamSink<Element>.make(buffer: buffer) { [weak state] in
            if state?.remove(id) != nil { _ = end() }
        }
    }
}

public struct Room<State: Equatable & Sendable, Message: Equatable & Sendable>: Sendable {
    public let events: AsyncThrowingStream<RoomEvent, Error>
    fileprivate let session: Session
    fileprivate let id: UInt64
    fileprivate let state: HirselCodec<State>
    fileprivate let message: HirselCodec<Message>

    public func set(_ value: State?) throws {
        let bytes = try value.map(state.encode) ?? Data()
        try checkStatus(withBytes(bytes) {
            hirsel_embed_v1_room_set(session.handle, id, value == nil ? 0 : 1, $0)
        })
    }

    public func send(_ value: Message, to peer: UInt64? = nil) throws {
        try checkStatus(withBytes(message.encode(value)) {
            hirsel_embed_v1_room_send(session.handle, id, peer ?? 0, $0)
        })
    }

    public func leave() throws {
        let status = hirsel_embed_v1_room_leave(session.handle, id)
        if status != HIRSEL_NOT_LIVE { try checkStatus(status) }
        if case .room(let sink) = session.state.remove(id) { sink.finish() }
    }
}

struct StreamSink<Element: Sendable>: Sendable {
    let continuation: AsyncThrowingStream<Element, Error>.Continuation

    static func make(
        buffer: Int, onTermination: @escaping @Sendable () -> Void
    ) throws -> (AsyncThrowingStream<Element, Error>, Self) {
        guard buffer >= 0 else { throw EmbedError.invalidBuffer }
        let (stream, continuation) = AsyncThrowingStream<Element, Error>.makeStream(
            bufferingPolicy: .bufferingOldest(buffer)
        )
        continuation.onTermination = { _ in onTermination() }
        return (stream, Self(continuation: continuation))
    }

    func yield(_ value: Element) {
        if case .dropped = continuation.yield(value) {
            finish(throwing: EmbedError.clientCapacity)
        }
    }

    func finish(throwing error: Error? = nil) {
        continuation.finish(throwing: error)
    }

    func receive(_ bytes: hirsel_bytes, _ decode: (Data) throws -> Element) {
        do { yield(try decode(copyBytes(bytes))) }
        catch { finish(throwing: error) }
    }
}

private enum Observer: Sendable {
    case call(CallBox)
    case subscription(StreamSink<Change>)
    case room(StreamSink<RoomEvent>)

    func finish(throwing error: Error) {
        switch self {
        case .call(let call): call.resolve(.failure(error))
        case .subscription(let sink): sink.finish(throwing: error)
        case .room(let sink): sink.finish(throwing: error)
        }
    }
}

private final class SessionState: @unchecked Sendable {
    private let lock = NSLock()
    private var observers: [UInt64: Observer] = [:]
    private var lastID: UInt64 = 0
    private var end: Error?

    func nextID() throws -> UInt64 {
        return try lock.withLock {
            if let end { throw end }
            guard lastID < UInt64.max else { throw EmbedError.idsExhausted }
            lastID += 1
            return lastID
        }
    }

    func insert(_ observer: Observer, at id: UInt64) throws {
        try lock.withLock {
            if let end { throw end }
            observers[id] = observer
        }
    }

    func remove(_ id: UInt64) -> Observer? {
        return lock.withLock { observers.removeValue(forKey: id) }
    }

    private func get(_ id: UInt64) -> Observer? { lock.withLock { observers[id] } }

    func outcome(_ id: UInt64, _ bytes: hirsel_bytes) {
        guard case .call(let call) = get(id) else { return }
        call.resolve(.success(copyBytes(bytes)))
    }

    func change(_ id: UInt64, _ bytes: hirsel_bytes) {
        guard case .subscription(let sink) = get(id) else { return }
        sink.receive(bytes, ChangeCodec.decode)
    }

    func subEnded(_ id: UInt64, _ bytes: hirsel_bytes) {
        guard case .subscription(let sink) = remove(id) else { return }
        do {
            switch try SubEndCodec.decode(copyBytes(bytes)) {
            case .clientCapacity: sink.finish(throwing: EmbedError.clientCapacity)
            case let end: sink.finish(throwing: EmbedError.subscriptionEnded(end))
            }
        } catch { sink.finish(throwing: error) }
    }

    func roomEvent(_ id: UInt64, _ bytes: hirsel_bytes) {
        guard case .room(let sink) = get(id) else { return }
        do {
            let event = try RoomEventCodec.decode(copyBytes(bytes))
            if case .ended(let end) = event {
                _ = remove(id)
                sink.yield(event)
                sink.finish(throwing: end == .clientCapacity ? EmbedError.clientCapacity : nil)
            } else {
                sink.yield(event)
            }
        } catch { sink.finish(throwing: error) }
    }

    func sessionEnded(_ bytes: hirsel_bytes) {
        let reason: Error
        do { reason = EmbedError.sessionEnded(try SessionEndCodec.decode(copyBytes(bytes))) }
        catch { reason = error }
        let pending = lock.withLock {
            end = reason
            let pending = observers
            observers = [:]
            return pending
        }
        for observer in pending.values { observer.finish(throwing: reason) }
    }
}

final class CallBox: @unchecked Sendable {
    private enum State {
        case pendingIssue
        case issuing(cancelRequested: Bool)
        case issued
        case cancelledBeforeIssue
        case abandoning
        case resolved
    }

    private let lock = NSLock()
    private let abandon: @Sendable () -> hirsel_status
    private var state = State.pendingIssue
    private var continuation: CheckedContinuation<Data, Error>?
    private var result: Result<Data, Error>?

    init(abandon: @escaping @Sendable () -> hirsel_status) { self.abandon = abandon }

    var issued: Bool {
        lock.withLock {
            if case .issued = state { return true }
            return false
        }
    }

    func run(register: @Sendable () -> hirsel_status) async throws -> Data {
        return try await withTaskCancellationHandler {
            try await withCheckedThrowingContinuation { next in
                let early: Result<Data, Error>? = lock.withLock {
                    if let result { return result }
                    continuation = next
                    state = .issuing(cancelRequested: false)
                    return nil
                }
                if let early {
                    next.resume(with: early)
                } else {
                    registered(register())
                }
            }
        } onCancel: {
            self.cancel()
        }
    }

    private func registered(_ status: hirsel_status) {
        if status != HIRSEL_OK {
            resolve(.failure(EmbedError.status(status.rawValue)))
            return
        }
        let detach = lock.withLock {
            guard case .issuing(let cancelRequested) = state else { return false }
            state = cancelRequested ? .abandoning : .issued
            return cancelRequested
        }
        if detach { abandonCall() }
    }

    func cancel() {
        let detach = lock.withLock {
            switch state {
            case .pendingIssue:
                state = .cancelledBeforeIssue
                result = .failure(CancellationError())
                return false
            case .issuing:
                state = .issuing(cancelRequested: true)
                return false
            case .issued:
                state = .abandoning
                return true
            case .cancelledBeforeIssue, .abandoning, .resolved:
                return false
            }
        }
        if detach { abandonCall() }
    }

    private func abandonCall() {
        let status = abandon()
        switch status {
        case HIRSEL_OK: resolve(.failure(CancellationError()))
        case HIRSEL_NOT_LIVE, HIRSEL_STOPPED: break
        default: resolve(.failure(EmbedError.status(status.rawValue)))
        }
    }

    func resolve(_ result: Result<Data, Error>) {
        let next = lock.withLock {
            guard self.result == nil else { return nil as CheckedContinuation<Data, Error>? }
            self.result = result
            state = .resolved
            let next = continuation
            continuation = nil
            return next
        }
        next?.resume(with: result)
    }
}
