#!/usr/bin/env bash
set -euo pipefail

# ==============================================================================
# LiliaDB - Apple Multiplatform Packaging Script (iOS, watchOS, tvOS, macOS)
# Generates Lilia.xcframework with Swift bindings via UniFFI
# ==============================================================================

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
BUILD_DIR="${ROOT_DIR}/target/apple-build"
OUT_DIR="${ROOT_DIR}/release/apple"
SPM_DIR="${ROOT_DIR}/packages/apple-spm"

echo "==> [LiliaDB] Preparing Apple build environment..."
mkdir -p "${BUILD_DIR}"
mkdir -p "${OUT_DIR}"
mkdir -p "${SPM_DIR}/Sources/Lilia"

# 1. Build host uniffi-bindgen CLI if needed
BINDGEN="${ROOT_DIR}/target/debug/uniffi-bindgen"
HOST_DYLIB="${ROOT_DIR}/target/debug/liblilia_ffi.dylib"

if [ ! -f "${BINDGEN}" ]; then
    echo "==> [LiliaDB] Building uniffi-bindgen tool..."
    cargo build -p lilia-ffi --features cli --bin uniffi-bindgen
fi

# 2. Build host dylib to extract UniFFI metadata & Swift bindings
if [ ! -f "${HOST_DYLIB}" ]; then
    echo "==> [LiliaDB] Building host dylib for metadata..."
    cargo build -p lilia-ffi --lib
fi

echo "==> [LiliaDB] Generating Swift bindings & C headers..."
${BINDGEN} generate "${HOST_DYLIB}" \
    --library \
    --language swift \
    --out-dir "${BUILD_DIR}/bindings"

# Copy generated Swift code and C headers to SPM package
cp "${BUILD_DIR}/bindings/lilia_ffi.swift" "${SPM_DIR}/Sources/Lilia/Lilia.swift"
mkdir -p "${BUILD_DIR}/headers"
cp "${BUILD_DIR}/bindings/lilia_ffiFFI.h" "${BUILD_DIR}/headers/lilia_ffiFFI.h"
cp "${BUILD_DIR}/bindings/lilia_ffiFFI.modulemap" "${BUILD_DIR}/headers/module.modulemap"

# 3. Compile available targets
TARGETS=(
    "aarch64-apple-darwin"       # macOS Apple Silicon
    "x86_64-apple-darwin"        # macOS Intel
    "aarch64-apple-ios"          # iOS Device
    "aarch64-apple-ios-sim"      # iOS Simulator (Apple Silicon)
    "x86_64-apple-ios"           # iOS Simulator (Intel)
    "aarch64-apple-watchos"      # Apple Watch Ultra / Series 9+
    "aarch64-apple-watchos-sim"  # watchOS Simulator
    "aarch64-apple-tvos"         # Apple TV Device
    "aarch64-apple-tvos-sim"     # tvOS Simulator
)

echo "==> [LiliaDB] Checking installed rustup Apple targets..."
for target in "${TARGETS[@]}"; do
    if rustup target list | grep -q "${target} (installed)"; then
        echo "==> Compiling for target: ${target}..."
        cargo build -p lilia-ffi --release --target "${target}"
    fi
done

XCFRAMEWORK_ARGS=()

# Group macOS slices
mkdir -p "${BUILD_DIR}/macos"
MACOS_LIBS=()
[ -f "${ROOT_DIR}/target/aarch64-apple-darwin/release/liblilia_ffi.a" ] && MACOS_LIBS+=("${ROOT_DIR}/target/aarch64-apple-darwin/release/liblilia_ffi.a")
[ -f "${ROOT_DIR}/target/x86_64-apple-darwin/release/liblilia_ffi.a" ] && MACOS_LIBS+=("${ROOT_DIR}/target/x86_64-apple-darwin/release/liblilia_ffi.a")

if [ ${#MACOS_LIBS[@]} -gt 1 ]; then
    lipo -create "${MACOS_LIBS[@]}" -output "${BUILD_DIR}/macos/liblilia_ffi.a"
    XCFRAMEWORK_ARGS+=("-library" "${BUILD_DIR}/macos/liblilia_ffi.a" "-headers" "${BUILD_DIR}/headers")
elif [ ${#MACOS_LIBS[@]} -eq 1 ]; then
    XCFRAMEWORK_ARGS+=("-library" "${MACOS_LIBS[0]}" "-headers" "${BUILD_DIR}/headers")
fi

# Group iOS Device slices
if [ -f "${ROOT_DIR}/target/aarch64-apple-ios/release/liblilia_ffi.a" ]; then
    XCFRAMEWORK_ARGS+=("-library" "${ROOT_DIR}/target/aarch64-apple-ios/release/liblilia_ffi.a" "-headers" "${BUILD_DIR}/headers")
fi

# Group iOS Simulator slices
IOS_SIM_LIBS=()
[ -f "${ROOT_DIR}/target/aarch64-apple-ios-sim/release/liblilia_ffi.a" ] && IOS_SIM_LIBS+=("${ROOT_DIR}/target/aarch64-apple-ios-sim/release/liblilia_ffi.a")
[ -f "${ROOT_DIR}/target/x86_64-apple-ios/release/liblilia_ffi.a" ] && IOS_SIM_LIBS+=("${ROOT_DIR}/target/x86_64-apple-ios/release/liblilia_ffi.a")

if [ ${#IOS_SIM_LIBS[@]} -gt 1 ]; then
    mkdir -p "${BUILD_DIR}/ios-sim"
    lipo -create "${IOS_SIM_LIBS[@]}" -output "${BUILD_DIR}/ios-sim/liblilia_ffi.a"
    XCFRAMEWORK_ARGS+=("-library" "${BUILD_DIR}/ios-sim/liblilia_ffi.a" "-headers" "${BUILD_DIR}/headers")
elif [ ${#IOS_SIM_LIBS[@]} -eq 1 ]; then
    XCFRAMEWORK_ARGS+=("-library" "${IOS_SIM_LIBS[0]}" "-headers" "${BUILD_DIR}/headers")
fi

# Group watchOS Device slices
if [ -f "${ROOT_DIR}/target/aarch64-apple-watchos/release/liblilia_ffi.a" ]; then
    XCFRAMEWORK_ARGS+=("-library" "${ROOT_DIR}/target/aarch64-apple-watchos/release/liblilia_ffi.a" "-headers" "${BUILD_DIR}/headers")
fi

# Group watchOS Simulator slices
if [ -f "${ROOT_DIR}/target/aarch64-apple-watchos-sim/release/liblilia_ffi.a" ]; then
    XCFRAMEWORK_ARGS+=("-library" "${ROOT_DIR}/target/aarch64-apple-watchos-sim/release/liblilia_ffi.a" "-headers" "${BUILD_DIR}/headers")
fi

# Group tvOS Device slices
if [ -f "${ROOT_DIR}/target/aarch64-apple-tvos/release/liblilia_ffi.a" ]; then
    XCFRAMEWORK_ARGS+=("-library" "${ROOT_DIR}/target/aarch64-apple-tvos/release/liblilia_ffi.a" "-headers" "${BUILD_DIR}/headers")
fi

# Group tvOS Simulator slices
if [ -f "${ROOT_DIR}/target/aarch64-apple-tvos-sim/release/liblilia_ffi.a" ]; then
    XCFRAMEWORK_ARGS+=("-library" "${ROOT_DIR}/target/aarch64-apple-tvos-sim/release/liblilia_ffi.a" "-headers" "${BUILD_DIR}/headers")
fi

# 4. Create XCFramework
XCFRAMEWORK_OUT="${OUT_DIR}/Lilia.xcframework"
rm -rf "${XCFRAMEWORK_OUT}"

if [ ${#XCFRAMEWORK_ARGS[@]} -gt 0 ] && command -v xcodebuild >/dev/null 2>&1; then
    echo "==> Creating XCFramework with xcodebuild..."
    xcodebuild -create-xcframework \
        "${XCFRAMEWORK_ARGS[@]}" \
        -output "${XCFRAMEWORK_OUT}"
    echo "==> [Success] XCFramework created at: ${XCFRAMEWORK_OUT}"
else
    echo "==> No Apple target libraries or xcodebuild not found. Static libs generated."
fi

# 5. Generate Package.swift for SPM
cat << 'EOF' > "${SPM_DIR}/Package.swift"
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
EOF

echo "==> [LiliaDB] Apple multiplatform build workflow complete!"

# 6. Create release zip archive and SHA256 checksum for SPM binaryTarget distribution
if [ -d "${XCFRAMEWORK_OUT}" ]; then
    echo "==> [LiliaDB] Compressing Lilia.xcframework for release..."
    (cd "${OUT_DIR}" && zip -q -r -y "Lilia.xcframework.zip" "Lilia.xcframework")
    CHECKSUM=$(shasum -a 256 "${OUT_DIR}/Lilia.xcframework.zip" | awk '{print $1}')
    echo "${CHECKSUM}" > "${OUT_DIR}/Lilia.xcframework.zip.sha256"
    echo "==> [Success] Created: ${OUT_DIR}/Lilia.xcframework.zip"
    echo "==> [Checksum] SHA-256: ${CHECKSUM}"
fi

