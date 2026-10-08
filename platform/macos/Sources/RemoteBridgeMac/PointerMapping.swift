import CoreGraphics

/// Maps viewer pointer fractions to global screen points.
///
/// The viewer sends `x` and `y` as 0...1 of the captured display. macOS takes
/// global coordinates in points with the origin at the top-left of the main
/// display, which is what `CGDisplayBounds` reports.
public enum PointerMapping {
    /// The point on the desktop for a fraction of `display`.
    public static func point(in display: CGRect, x: Double, y: Double) -> CGPoint {
        let fx = min(max(x.isFinite ? x : 0, 0), 1)
        let fy = min(max(y.isFinite ? y : 0, 0), 1)
        // Stay inside the display: the far edge is the last pixel, not one past it.
        let px = display.minX + fx * max(display.width - 1, 0)
        let py = display.minY + fy * max(display.height - 1, 0)
        return CGPoint(x: px, y: py)
    }
}
