// swift-tools-version:5.9
import PackageDescription

let package = Package(
    name: "Geometry",
    products: [.library(name: "Core", targets: ["Core"])],
    dependencies: [.package(url: "https://github.com/apple/swift-log.git", from: "1.5.0")],
    targets: [
        .target(name: "Core"),
        .executableTarget(name: "App", dependencies: ["Core"]),
        .testTarget(name: "CoreTests", dependencies: ["Core"]),
    ]
)
