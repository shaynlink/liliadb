// swift-tools-version: 5.9
import PackageDescription

let package = Package(
    name: "Lilia",
    platforms: [
        .iOS("15.0"),
        .macOS("12.0"),
        .watchOS("8.0"),
        .tvOS("15.0"),
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
