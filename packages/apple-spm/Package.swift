// swift-tools-version: 5.9
import PackageDescription

let package = Package(
    name: "Lilia",
    platforms: [
        .iOS(.v15),
        .macOS(.v12),
        .watchOS(.v8),
        .tvOS(.v15),
    ],
    products: [
        .library(
            name: "Lilia",
            targets: ["Lilia", "LiliaFramework"]
        ),
    ],
    targets: [
        .binaryTarget(
            name: "LiliaFramework",
            path: "../../release/apple/Lilia.xcframework"
        ),
        .target(
            name: "Lilia",
            dependencies: ["LiliaFramework"],
            path: "Sources/Lilia"
        ),
    ]
)
