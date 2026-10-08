import AVFoundation
import Foundation

/// The default microphone, as interleaved signed 16-bit PCM.
///
/// Off unless the session's grant includes the microphone (AGENTS.md: never
/// available unattended). Permission is requested only when first started,
/// and the request is answered by the system's own prompt.
public final class MicrophoneCapturer {
    private let engine = AVAudioEngine()
    private var running = false

    public init() {}

    public static var authorization: AVAuthorizationStatus {
        AVCaptureDevice.authorizationStatus(for: .audio)
    }

    /// Start capture. Throws if the microphone permission is not granted.
    public func start(onChunk: @escaping (_ sampleRate: Int, _ channels: Int, _ samples: [Int16]) -> Void) throws {
        guard !running else { return }
        guard MicrophoneCapturer.authorization == .authorized else {
            throw MicrophoneError.notGranted
        }
        let input = engine.inputNode
        let format = input.outputFormat(forBus: 0)
        guard format.sampleRate > 0, format.channelCount > 0 else {
            throw MicrophoneError.noDevice
        }
        input.installTap(onBus: 0, bufferSize: 1024, format: format) { buffer, _ in
            guard let planes = buffer.floatChannelData else { return }
            let frames = Int(buffer.frameLength)
            let channels = Int(format.channelCount)
            var samples = [Int16](repeating: 0, count: frames * channels)
            for c in 0..<channels {
                for f in 0..<frames {
                    samples[f * channels + c] = ScreenCapturer.floatToInt16(planes[c][f])
                }
            }
            onChunk(Int(format.sampleRate), channels, samples)
        }
        try engine.start()
        running = true
    }

    public func stop() {
        guard running else { return }
        engine.inputNode.removeTap(onBus: 0)
        engine.stop()
        running = false
    }

    public var isRunning: Bool { running }
}

public enum MicrophoneError: Error, Equatable {
    case notGranted
    case noDevice
}
