import Darwin
import Foundation
import SwiftUI
#if !PR_COUNTER
import CounterGate
#endif

@main
struct CounterHost: App {
    var body: some Scene {
        WindowGroup {
            Text("Hirsel counter comparison").task {
                CounterHost.run()
            }
        }
    }

    @MainActor
    private static var started = false

    @MainActor
    private static func run() {
        guard !started else { return }
        started = true
        let read = ProcessInfo.processInfo.environment["HIRSEL_COMPARE_READ"] == "1"
        #if PR_COUNTER
        let documents = FileManager.default.urls(for: .documentDirectory, in: .userDomainMask)[0]
        setenv("XDG_DATA_HOME", documents.path, 1)
        hios_host_register_wake({ _ in
            DispatchQueue.main.async {
                var command: UnsafeMutablePointer<UInt8>?
                var length: UInt64 = 0
                while hios_host_pop_command(&command, &length) == 1, let command {
                    defer { hios_host_free_buffer(command) }
                    var reader = BinaryReader(data: UnsafeBufferPointer(start: command, count: Int(length)))
                    if case .renderTree(let tree)? = reader.readCommand(),
                       tree.id == "counter", case .text(let text, _, _, _, _) = tree.kind,
                       let value = UInt64(text) {
                        record(value)
                    }
                }
            }
        }, nil)
        var thread: pthread_t?
        var attributes = pthread_attr_t()
        pthread_attr_init(&attributes)
        pthread_attr_setstacksize(&attributes, 4 * 1024 * 1024)
        let status = pthread_create(&thread, &attributes, { _ in
            let arg0 = strdup("hirsel-ios-counter")
            var args: [UnsafeMutablePointer<CChar>?] = [arg0, nil]
            _ = args.withUnsafeMutableBufferPointer { hirsel_app_main(1, $0.baseAddress) }
            free(arg0)
            return nil
        }, nil)
        pthread_attr_destroy(&attributes)
        precondition(status == 0)
        if let thread { pthread_detach(thread) }
        HirselEvents.sendLifecycle(read ? 1 : 0)
        #else
        Task.detached {
            let runtime = try Runtime.start(grants: "Clock\nRandom\nTasks\nStderr\nNet=Connect 127.0.0.1/32 1-1")
            let session = try runtime.open()
            let result = try await session.api.call.main.counter_step(read ? 0 : 1, deadline: 0)
            if case .returned(let value) = result {
                await record(value)
            }
        }
        #endif
    }

    @MainActor
    private static func record(_ value: UInt64) {
        dispatchPrecondition(condition: .onQueue(.main))
        let tick = mach_absolute_time()
        let memory = hirsel_gate_memory_sample()
        precondition(memory.status == KERN_SUCCESS)
        let documents = FileManager.default.urls(for: .documentDirectory, in: .userDomainMask)[0]
        try! FileManager.default.createDirectory(at: documents, withIntermediateDirectories: true)
        let output = documents.appendingPathComponent("comparison-sample.txt")
        try! "\(value) \(tick) \(memory.resident)\n".write(to: output, atomically: true, encoding: .utf8)
    }
}
