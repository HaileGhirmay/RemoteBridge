import CoreGraphics
import XCTest
@testable import RemoteBridgeMac

/// Pure logic only: nothing here needs a permission or a real display.
final class MacAdapterPolicyTests: XCTestCase {

    // MARK: - Key map

    func testLettersAndDigitsUseTheHardwareKeyCodes() {
        XCTAssertEqual(MacKeyMap.virtualKey(for: "KeyA"), 0x00)
        XCTAssertEqual(MacKeyMap.virtualKey(for: "KeyZ"), 0x06)
        XCTAssertEqual(MacKeyMap.virtualKey(for: "Digit1"), 0x12)
        XCTAssertEqual(MacKeyMap.virtualKey(for: "Digit0"), 0x1D)
    }

    func testEveryLetterIsMapped() {
        for letter in "ABCDEFGHIJKLMNOPQRSTUVWXYZ" {
            XCTAssertNotNil(MacKeyMap.virtualKey(for: "Key\(letter)"), "Key\(letter)")
        }
    }

    func testLeftAndRightModifiersAreDistinct() {
        XCTAssertEqual(MacKeyMap.virtualKey(for: "ShiftLeft"), 0x38)
        XCTAssertEqual(MacKeyMap.virtualKey(for: "ShiftRight"), 0x3C)
        XCTAssertEqual(MacKeyMap.virtualKey(for: "MetaLeft"), 0x37)
        XCTAssertEqual(MacKeyMap.virtualKey(for: "MetaRight"), 0x36)
        XCTAssertEqual(MacKeyMap.virtualKey(for: "AltLeft"), 0x3A)
        XCTAssertEqual(MacKeyMap.virtualKey(for: "AltRight"), 0x3D)
    }

    func testUnknownCodesAreRefusedNotGuessed() {
        XCTAssertNil(MacKeyMap.virtualKey(for: "Unidentified"))
        XCTAssertNil(MacKeyMap.virtualKey(for: ""))
        XCTAssertNil(MacKeyMap.virtualKey(for: "keya"), "codes are case sensitive")
        XCTAssertNil(MacKeyMap.virtualKey(for: "BrowserBack"))
    }

    func testNoTwoPhysicalKeysShareACode() {
        // Each key in the table must have its own virtual key code.
        let codes = ["KeyA", "KeyS", "KeyD", "KeyF", "Enter", "Tab", "Space", "Backspace", "Escape",
                     "ArrowLeft", "ArrowRight", "ArrowDown", "ArrowUp", "Home", "End", "PageUp", "PageDown"]
        let mapped = codes.compactMap { MacKeyMap.virtualKey(for: $0) }
        XCTAssertEqual(mapped.count, codes.count)
        XCTAssertEqual(Set(mapped).count, codes.count)
    }

    // MARK: - Pointer mapping

    func testCornersMapToTheCornersOfTheDisplay() {
        let display = CGRect(x: 0, y: 0, width: 1920, height: 1080)
        XCTAssertEqual(PointerMapping.point(in: display, x: 0, y: 0), CGPoint(x: 0, y: 0))
        XCTAssertEqual(PointerMapping.point(in: display, x: 1, y: 1), CGPoint(x: 1919, y: 1079))
    }

    func testASecondDisplayIsOffsetIntoTheDesktop() {
        // A display to the left of the main one has negative x in global points.
        let display = CGRect(x: -1280, y: 100, width: 1280, height: 800)
        XCTAssertEqual(PointerMapping.point(in: display, x: 0, y: 0), CGPoint(x: -1280, y: 100))
        XCTAssertEqual(PointerMapping.point(in: display, x: 0.5, y: 0.5).x, -1280 + 0.5 * 1279, accuracy: 0.0001)
    }

    func testOutOfRangeAndNonFiniteFractionsStayOnTheDisplay() {
        let display = CGRect(x: 0, y: 0, width: 100, height: 100)
        XCTAssertEqual(PointerMapping.point(in: display, x: -5, y: 9), CGPoint(x: 0, y: 99))
        // Non-finite input is treated as 0, not as the far edge.
        XCTAssertEqual(PointerMapping.point(in: display, x: .nan, y: .infinity), CGPoint(x: 0, y: 0))
    }

    // MARK: - Permission policy

    func testViewOnlyNeedsNoPermission() {
        XCTAssertEqual(MacPermissionNeeds(screen: false, input: false, microphone: false).required, [])
    }

    func testScreenNeedsScreenRecordingAndInputNeedsAccessibility() {
        XCTAssertEqual(MacPermissionNeeds(screen: true, input: false, microphone: false).required,
                       [.screenRecording])
        XCTAssertEqual(MacPermissionNeeds(screen: true, input: true, microphone: true).required,
                       [.screenRecording, .accessibility, .microphone])
    }

    func testMissingPermissionsAreListedInOrder() {
        let needs = MacPermissionNeeds(screen: true, input: true, microphone: true)
        XCTAssertEqual(needs.missing(granted: [.accessibility]), [.screenRecording, .microphone])
        XCTAssertEqual(needs.missing(granted: Set(MacPermission.allCases)), [])
    }

    func testEveryPermissionHasASettingsPathAndAName() {
        for permission in MacPermission.allCases {
            XCTAssertTrue(permission.settingsPath.hasPrefix("System Settings"), permission.name)
            XCTAssertFalse(permission.name.isEmpty)
        }
    }

    // MARK: - Injection tag

    func testInjectionTagSpellsRBMT() {
        // Same value as the Windows adapter's INJECT_TAG: the two hosts agree on it.
        XCTAssertEqual(MacInput.injectionTag, 0x5242_4D54)
    }
}
