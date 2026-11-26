import XCTest
@testable import Core

final class PointTests: XCTestCase {
    func testLength() {
        let p = Point(x: 3, y: 4)
        XCTAssertEqual(p.length(), 5)
    }
}
