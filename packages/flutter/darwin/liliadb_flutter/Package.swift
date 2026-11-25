// swift-tools-version: 5.9
// The plugin for Swift Package Manager, which Flutter uses for iOS and macOS apps.

import PackageDescription

let package = Package(
    name: "liliadb_flutter",
    platforms: [
        .iOS("15.0"),
        .macOS("12.0"),
    ],
    products: [
        .library(name: "liliadb-flutter", targets: ["liliadb_flutter"]),
    ],
    targets: [
        // The engine, which tools/mobile/build-apple.sh builds and copies here.
        .binaryTarget(name: "Lilia", path: "Lilia.xcframework"),
        .target(name: "liliadb_flutter", dependencies: ["Lilia"]),
    ]
)
