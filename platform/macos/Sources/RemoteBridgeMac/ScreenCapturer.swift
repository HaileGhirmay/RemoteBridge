import AudioToolbox
import CoreGraphics
import CoreMedia
import CoreVideo
import Foundation
import ScreenCaptureKit

public enum ScreenCaptureError: Error, Equatable {
    /// Screen Recording is not granted. Reported; never worked around.
    case screenRecordingNotGranted
    case displayNotFound
    case stream(String)
}

/// A captured screen frame: packed BGRA, top row first.
public struct ScreenFrame {
    public let width: Int
    public let height: Int
    public let bgra: [UInt8]
}

/// System audio as interleaved signed 16-bit PCM.
public struct SystemAudioChunk {
    public let sampleRate: Int
    public let channels: Int
    public let samples: [Int16]
}

/// Screen and system audio through ScreenCaptureKit.
///
/// The cursor is drawn by the OS (`showsCursor`). Our own banner windows are
/// excluded from this capture only; the OS capture indicators are untouched.
/// If the user stops sharing or the desktop changes, `onStopped` fires and the
/// caller reports `capture_failed`.
public final class ScreenCapturer: NSObject, SCStreamOutput, SCStreamDelegate {
    private let sampleQueue = DispatchQueue(label: "rb.capture.samples")
    private var stream: SCStream?
    private var onFrame: ((ScreenFrame) -> Void)?
    private var onAudio: ((SystemAudioChunk) -> Void)?
    private var onStopped: ((Error?) -> Void)?

    public override init() {
        super.init()
    }

    /// Screen Recording permission, without prompting.
    public static var screenRecordingGranted: Bool {
        CGPreflightScreenCaptureAccess()
    }

    /// Ask the system for Screen Recording. Returns the current state; the
    /// user answers in System Settings, so this does not wait.
    @discardableResult
    public static func requestScreenRecording() -> Bool {
        CGRequestScreenCaptureAccess()
    }

    /// Start capturing `displayID` at its native size, with system audio.
    /// - Parameters:
    ///   - excludingWindowIDs: our own banner windows (`CGWindowID`s), kept out of the stream.
    public func start(
        displayID: CGDirectDisplayID,
        excludingWindowIDs: [CGWindowID],
        onFrame: @escaping (ScreenFrame) -> Void,
        onAudio: @escaping (SystemAudioChunk) -> Void,
        onStopped: @escaping (Error?) -> Void
    ) async throws {
        guard ScreenCapturer.screenRecordingGranted else {
            throw ScreenCaptureError.screenRecordingNotGranted
        }
        let content = try await SCShareableContent.excludingDesktopWindows(false, onScreenWindowsOnly: true)
        guard let display = content.displays.first(where: { $0.displayID == displayID }) else {
            throw ScreenCaptureError.displayNotFound
        }
        let excluded = content.windows.filter { excludingWindowIDs.contains($0.windowID) }
        let filter = SCContentFilter(display: display, excludingWindows: excluded)

        let config = SCStreamConfiguration()
        config.width = display.width
        config.height = display.height
        config.pixelFormat = kCVPixelFormatType_32BGRA
        config.showsCursor = true
        config.minimumFrameInterval = CMTime(value: 1, timescale: 30)
        config.capturesAudio = true
        config.excludesCurrentProcessAudio = true
        config.sampleRate = 48_000
        config.channelCount = 2

        self.onFrame = onFrame
        self.onAudio = onAudio
        self.onStopped = onStopped

        let stream = SCStream(filter: filter, configuration: config, delegate: self)
        try stream.addStreamOutput(self, type: .screen, sampleHandlerQueue: sampleQueue)
        try stream.addStreamOutput(self, type: .audio, sampleHandlerQueue: sampleQueue)
        try await stream.startCapture()
        self.stream = stream
    }

    public func stop() async {
        guard let stream else { return }
        self.stream = nil
        onFrame = nil
        onAudio = nil
        try? await stream.stopCapture()
    }

    // MARK: - SCStreamOutput

    public func stream(_ stream: SCStream, didOutputSampleBuffer sampleBuffer: CMSampleBuffer, of type: SCStreamOutputType) {
        guard CMSampleBufferIsValid(sampleBuffer) else { return }
        switch type {
        case .screen:
            guard let pixels = CMSampleBufferGetImageBuffer(sampleBuffer) else { return } // idle frame
            if let frame = ScreenCapturer.frame(from: pixels) {
                onFrame?(frame)
            }
        case .audio:
            if let chunk = ScreenCapturer.audio(from: sampleBuffer) {
                onAudio?(chunk)
            }
        @unknown default:
            break
        }
    }

    // MARK: - SCStreamDelegate

    public func stream(_ stream: SCStream, didStopWithError error: Error) {
        self.stream = nil
        onStopped?(error)
    }

    // MARK: - Conversions (pure enough to read; the framework types make them untestable here)

    static func frame(from pixels: CVImageBuffer) -> ScreenFrame? {
        CVPixelBufferLockBaseAddress(pixels, .readOnly)
        defer { CVPixelBufferUnlockBaseAddress(pixels, .readOnly) }
        guard let base = CVPixelBufferGetBaseAddress(pixels) else { return nil }
        let width = CVPixelBufferGetWidth(pixels)
        let height = CVPixelBufferGetHeight(pixels)
        let stride = CVPixelBufferGetBytesPerRow(pixels)
        let rowBytes = width * 4
        guard stride >= rowBytes else { return nil }
        var out = [UInt8](repeating: 0, count: rowBytes * height)
        let src = base.assumingMemoryBound(to: UInt8.self)
        out.withUnsafeMutableBufferPointer { dst in
            for y in 0..<height {
                let row = src.advanced(by: y * stride)
                (dst.baseAddress! + y * rowBytes).update(from: row, count: rowBytes)
            }
        }
        return ScreenFrame(width: width, height: height, bgra: out)
    }

    static func audio(from sampleBuffer: CMSampleBuffer) -> SystemAudioChunk? {
        guard let format = CMSampleBufferGetFormatDescription(sampleBuffer),
              let basic = CMAudioFormatDescriptionGetStreamBasicDescription(format)?.pointee,
              basic.mFormatFlags & kAudioFormatFlagIsFloat != 0 else {
            return nil
        }
        var sizeNeeded = 0
        CMSampleBufferGetAudioBufferListWithRetainedBlockBuffer(
            sampleBuffer, bufferListSizeNeededOut: &sizeNeeded, bufferListOut: nil,
            bufferListSize: 0, blockBufferAllocator: nil, blockBufferMemoryAllocator: nil,
            flags: 0, blockBufferOut: nil)
        guard sizeNeeded > 0 else { return nil }
        let raw = UnsafeMutableRawPointer.allocate(byteCount: sizeNeeded, alignment: 16)
        defer { raw.deallocate() }
        let listPointer = raw.bindMemory(to: AudioBufferList.self, capacity: 1)
        var block: CMBlockBuffer?
        let status = CMSampleBufferGetAudioBufferListWithRetainedBlockBuffer(
            sampleBuffer, bufferListSizeNeededOut: nil, bufferListOut: listPointer,
            bufferListSize: sizeNeeded, blockBufferAllocator: nil, blockBufferMemoryAllocator: nil,
            flags: 0, blockBufferOut: &block)
        guard status == noErr else { return nil }

        let buffers = UnsafeMutableAudioBufferListPointer(listPointer)
        let frames = CMSampleBufferGetNumSamples(sampleBuffer)
        let channels = Int(basic.mChannelsPerFrame)
        guard frames > 0, channels > 0 else { return nil }
        var samples = [Int16](repeating: 0, count: frames * channels)

        if buffers.count == 1 {
            // Interleaved: one buffer, channels are adjacent.
            guard let data = buffers[0].mData?.assumingMemoryBound(to: Float32.self) else { return nil }
            for i in 0..<(frames * channels) {
                samples[i] = floatToInt16(data[i])
            }
        } else {
            // Planar: one buffer per channel, interleave them.
            for c in 0..<min(buffers.count, channels) {
                guard let data = buffers[c].mData?.assumingMemoryBound(to: Float32.self) else { continue }
                for f in 0..<frames {
                    samples[f * channels + c] = floatToInt16(data[f])
                }
            }
        }
        return SystemAudioChunk(sampleRate: Int(basic.mSampleRate), channels: channels, samples: samples)
    }

    /// Same rule as the Rust side: clamp to -1...1, scale, round.
    static func floatToInt16(_ v: Float32) -> Int16 {
        Int16((min(max(v, -1), 1) * 32767).rounded())
    }
}
