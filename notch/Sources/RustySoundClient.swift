import Foundation

struct NowState {
    let playing: Bool
    let paused: Bool
    let now: String?
    let queued: Int
    let error: String?
    let artwork: String?
    let canPrev: Bool
    let radio: Bool
}

enum CLIResult<T> {
    case success(T)
    case serviceOff
    case failure(String)
}

/// Thin wrapper around the rusty-sound CLI. All methods block until the
/// process exits (or times out), so call them from a background queue only.
enum RustySoundClient {
    static let binary: URL = {
        if let override = ProcessInfo.processInfo.environment["RUSTY_SOUND_BIN"], !override.isEmpty {
            return URL(fileURLWithPath: override)
        }
        return FileManager.default.homeDirectoryForCurrentUser
            .appendingPathComponent(".local/bin/rusty-sound")
    }()

    private static let timeout: TimeInterval = 5

    static func now() -> CLIResult<NowState> {
        switch invoke(["now"]) {
        case .failure(let message):
            return .failure(message)
        case .serviceOff:
            return .serviceOff
        case .ok(let data):
            guard let d = data as? [String: Any] else {
                return .failure("Unexpected status data")
            }
            return .success(NowState(
                playing: d["playing"] as? Bool ?? false,
                paused: d["paused"] as? Bool ?? false,
                now: d["now"] as? String,
                queued: d["queued"] as? Int ?? 0,
                error: d["error"] as? String,
                artwork: d["artwork"] as? String,
                canPrev: d["can_prev"] as? Bool ?? false,
                radio: d["radio"] as? Bool ?? false
            ))
        }
    }

    static func command(_ name: String, _ extra: [String] = []) -> CLIResult<Void> {
        switch invoke([name] + extra) {
        case .ok:
            return .success(())
        case .serviceOff:
            return .serviceOff
        case .failure(let message):
            return .failure(message)
        }
    }

    private enum Outcome {
        case ok(Any?)
        case serviceOff
        case failure(String)
    }

    private static func invoke(_ args: [String]) -> Outcome {
        // Output goes to a temp file rather than a pipe, so a daemon child that
        // inherits the descriptor cannot keep us blocked after the CLI exits.
        let outURL = FileManager.default.temporaryDirectory
            .appendingPathComponent("rusty-notch-\(UUID().uuidString).json")
        FileManager.default.createFile(atPath: outURL.path, contents: nil)
        defer { try? FileManager.default.removeItem(at: outURL) }
        guard let outHandle = try? FileHandle(forWritingTo: outURL) else {
            return .failure("Cannot create temp file")
        }
        defer { try? outHandle.close() }

        let process = Process()
        process.executableURL = binary
        process.arguments = ["--json"] + args
        process.standardOutput = outHandle
        process.standardError = FileHandle.nullDevice
        process.standardInput = FileHandle.nullDevice

        let exited = DispatchSemaphore(value: 0)
        process.terminationHandler = { _ in exited.signal() }

        do {
            try process.run()
        } catch {
            return .failure("rusty-sound not runnable at \(binary.path)")
        }
        if exited.wait(timeout: DispatchTime.now() + timeout) == .timedOut {
            process.terminate()
            return .failure("rusty-sound timed out")
        }

        guard let data = try? Data(contentsOf: outURL),
              let obj = (try? JSONSerialization.jsonObject(with: data)) as? [String: Any] else {
            return .failure("No valid response from rusty-sound")
        }
        if (obj["ok"] as? Bool) == true {
            return .ok(obj["data"])
        }
        let err = obj["error"] as? [String: Any]
        if (err?["code"] as? String) == "service_off" {
            return .serviceOff
        }
        return .failure((err?["message"] as? String) ?? (err?["code"] as? String) ?? "Command failed")
    }
}
