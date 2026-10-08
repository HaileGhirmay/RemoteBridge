import XCTest
@testable import RemoteBridgeMac

final class RemoteBridgeMacTests: XCTestCase {
    func testMinimumMacOSVersion() {
        XCTAssertEqual(RemoteBridgeMac.minimumMacOSMajorVersion, 13)
    }
}
