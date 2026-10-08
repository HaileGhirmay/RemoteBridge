/// The three macOS privacy permissions the host needs, and the policy for them.
///
/// Missing permissions are reported (`report_failure permissions_unavailable`)
/// and the user is shown the System Settings page. Nothing here tries to work
/// around a refusal.
public enum MacPermission: CaseIterable, Equatable {
    /// Screen Recording: needed for screen and system audio (ScreenCaptureKit).
    case screenRecording
    /// Accessibility: needed to post keyboard and mouse events (CGEvent).
    case accessibility
    /// Microphone: needed only for an attended session that granted it.
    case microphone

    /// The System Settings pane a user should open.
    public var settingsPath: String {
        switch self {
        case .screenRecording: return "System Settings > Privacy & Security > Screen & System Audio Recording"
        case .accessibility: return "System Settings > Privacy & Security > Accessibility"
        case .microphone: return "System Settings > Privacy & Security > Microphone"
        }
    }

    /// Name used in the platform error and the guide.
    public var name: String {
        switch self {
        case .screenRecording: return "screen recording"
        case .accessibility: return "accessibility"
        case .microphone: return "microphone"
        }
    }
}

/// What a session needs from the machine.
public struct MacPermissionNeeds: Equatable {
    public var screen: Bool
    public var input: Bool
    public var microphone: Bool

    public init(screen: Bool, input: Bool, microphone: Bool) {
        self.screen = screen
        self.input = input
        self.microphone = microphone
    }

    /// The permissions that must be granted for these needs. Screen covers
    /// system audio too; input needs Accessibility; the microphone is its own.
    public var required: [MacPermission] {
        var out: [MacPermission] = []
        if screen { out.append(.screenRecording) }
        if input { out.append(.accessibility) }
        if microphone { out.append(.microphone) }
        return out
    }

    /// The needed permissions that are not granted, in a stable order.
    public func missing(granted: Set<MacPermission>) -> [MacPermission] {
        required.filter { !granted.contains($0) }
    }
}
