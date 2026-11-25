// swift-tools-version:5.9
import PackageDescription

let package = Package(
    name: "Snippet",
    targets: [
        .target(name: "Lib"),
        .executableTarget(name: "App", dependencies: ["Lib"]),
    ]
)
