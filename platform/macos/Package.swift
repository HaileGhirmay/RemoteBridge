// swift-tools-version:5.9
import PackageDescription

// macOS 13+ app shell. The Rust core is linked through the C ABI in bridge/;
// the cbindgen header and static library are wired in with the first adapter.
let package = Package(
    name: "RemoteBridgeMac",
    platforms: [.macOS(.v13)],
    products: [
        .library(name: "RemoteBridgeMac", targets: ["RemoteBridgeMac"])
    ],
    targets: [
        .target(name: "RemoteBridgeMac"),
        .testTarget(name: "RemoteBridgeMacTests", dependencies: ["RemoteBridgeMac"]),
    ]
)
