import ApplicationServices
import CoreGraphics

public enum MacInputError: Error, Equatable {
    /// Accessibility is not granted, so events would be dropped silently.
    case accessibilityNotGranted
    /// The OS refused to create an event.
    case eventFailed
}

/// Posts keyboard and pointer events from the viewer.
///
/// Every event carries `injectionTag` in its `eventSourceUserData`, so the
/// local shortcut tap can recognise and reject our own output. Keys and buttons
/// that are held are remembered so `releaseAll` can lift them.
public final class MacInput {
    /// Marks events we post ("RBMT"). The local shortcut tap rejects events with this tag.
    public static let injectionTag: Int64 = 0x5242_4D54

    private let source: CGEventSource?
    private let display: CGRect
    private var heldKeys: Set<UInt16> = []
    private var heldButtons: Set<CGMouseButton> = []

    /// - Parameter display: the captured display's bounds in global points (`CGDisplayBounds`).
    public init(display: CGRect) {
        self.source = CGEventSource(stateID: .hidSystemState)
        self.display = display
    }

    /// `domCode` is the DOM `KeyboardEvent.code`. Unknown codes are ignored.
    public func key(_ domCode: String, down: Bool) throws {
        guard let vk = MacKeyMap.virtualKey(for: domCode) else { return }
        try post(makeKey(vk, down: down))
        if down { heldKeys.insert(vk) } else { heldKeys.remove(vk) }
    }

    public func button(_ button: CGMouseButton, down: Bool) throws {
        let location = currentLocation()
        let type = mouseType(button, down: down)
        try post(makeMouse(type: type, at: location, button: button))
        if down { heldButtons.insert(button) } else { heldButtons.remove(button) }
    }

    /// `dx` and `dy` in the viewer's units (DOM pixels). Positive dy scrolls down.
    public func wheel(dx: Double, dy: Double) throws {
        let vertical = Int32((-dy).rounded())
        let horizontal = Int32(dx.rounded())
        if vertical == 0 && horizontal == 0 { return }
        guard let event = CGEvent(
            scrollWheelEvent2Source: source,
            units: .pixel,
            wheelCount: 2,
            wheel1: vertical,
            wheel2: horizontal,
            wheel3: 0
        ) else { throw MacInputError.eventFailed }
        tag(event)
        try post(event)
    }

    /// `x` and `y` are 0...1 of the captured display.
    public func movePointer(x: Double, y: Double) throws {
        let point = PointerMapping.point(in: display, x: x, y: y)
        let type: CGEventType = heldButtons.contains(.left) ? .leftMouseDragged : .mouseMoved
        try post(makeMouse(type: type, at: point, button: .left))
    }

    /// Release every key and button still held by the viewer.
    public func releaseAll() throws {
        for vk in heldKeys {
            try post(makeKey(vk, down: false))
        }
        heldKeys.removeAll()
        let location = currentLocation()
        for button in heldButtons {
            try post(makeMouse(type: mouseType(button, down: false), at: location, button: button))
        }
        heldButtons.removeAll()
    }

    // MARK: - Helpers

    private func makeKey(_ vk: UInt16, down: Bool) throws -> CGEvent {
        guard let event = CGEvent(keyboardEventSource: source, virtualKey: vk, keyDown: down) else {
            throw MacInputError.eventFailed
        }
        tag(event)
        return event
    }

    private func makeMouse(type: CGEventType, at point: CGPoint, button: CGMouseButton) throws -> CGEvent {
        guard let event = CGEvent(mouseEventSource: source, mouseType: type, mouseCursorPosition: point, mouseButton: button) else {
            throw MacInputError.eventFailed
        }
        tag(event)
        return event
    }

    private func tag(_ event: CGEvent) {
        event.setIntegerValueField(.eventSourceUserData, value: MacInput.injectionTag)
    }

    private func post(_ event: CGEvent) throws {
        guard AXIsProcessTrusted() else { throw MacInputError.accessibilityNotGranted }
        event.post(tap: .cghidEventTap)
    }

    private func currentLocation() -> CGPoint {
        CGEvent(source: nil)?.location ?? CGPoint(x: display.midX, y: display.midY)
    }

    private func mouseType(_ button: CGMouseButton, down: Bool) -> CGEventType {
        switch (button, down) {
        case (.left, true): return .leftMouseDown
        case (.left, false): return .leftMouseUp
        case (.right, true): return .rightMouseDown
        case (.right, false): return .rightMouseUp
        case (_, true): return .otherMouseDown
        case (_, false): return .otherMouseUp
        }
    }
}
