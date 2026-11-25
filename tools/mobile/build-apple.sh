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
# Cargo profile of the Apple slices: `release` for the shipped XCFramework; CI checks that every
# slice compiles and links with `apple-check` (see Cargo.toml).
PROFILE="${LILIA_APPLE_PROFILE:-release}"

# Minimum OS versions: the platforms of the generated Package.swift, and the deployment targets
# that rustc and the C compiler of every `cc` dependency (zstd, blake3, psm) build for. Without
# them rustc links for its own minimum (iOS 10) objects the C compiler built for the SDK's
# version, and the link fails on symbols those older versions lack (`___chkstk_darwin`).
IOS_MIN="15.0"
MACOS_MIN="12.0"
WATCHOS_MIN="8.0"
TVOS_MIN="15.0"
export IPHONEOS_DEPLOYMENT_TARGET="${IOS_MIN}"
export MACOSX_DEPLOYMENT_TARGET="${MACOS_MIN}"
export WATCHOS_DEPLOYMENT_TARGET="${WATCHOS_MIN}"
export TVOS_DEPLOYMENT_TARGET="${TVOS_MIN}"

# The iOS, watchOS and tvOS SDKs ship with Xcode, not with the Command Line Tools that
# `.cargo/config.toml` names: an exported DEVELOPER_DIR takes precedence over Cargo's `[env]`.
export DEVELOPER_DIR="${DEVELOPER_DIR:-$(xcode-select -p)}"
if ! xcrun --sdk iphoneos --show-sdk-path >/dev/null 2>&1; then
    echo "error: no iOS SDK in ${DEVELOPER_DIR}: install Xcode, then set DEVELOPER_DIR to its Contents/Developer or run xcode-select -s" >&2
    exit 1
fi

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
        cargo build -p lilia-ffi --profile "${PROFILE}" --target "${target}"
    fi
done

XCFRAMEWORK_ARGS=()

# Group macOS slices
mkdir -p "${BUILD_DIR}/macos"
MACOS_LIBS=()
[ -f "${ROOT_DIR}/target/aarch64-apple-darwin/${PROFILE}/liblilia_ffi.a" ] && MACOS_LIBS+=("${ROOT_DIR}/target/aarch64-apple-darwin/${PROFILE}/liblilia_ffi.a")
[ -f "${ROOT_DIR}/target/x86_64-apple-darwin/${PROFILE}/liblilia_ffi.a" ] && MACOS_LIBS+=("${ROOT_DIR}/target/x86_64-apple-darwin/${PROFILE}/liblilia_ffi.a")

if [ ${#MACOS_LIBS[@]} -gt 1 ]; then
    lipo -create "${MACOS_LIBS[@]}" -output "${BUILD_DIR}/macos/liblilia_ffi.a"
    XCFRAMEWORK_ARGS+=("-library" "${BUILD_DIR}/macos/liblilia_ffi.a" "-headers" "${BUILD_DIR}/headers")
elif [ ${#MACOS_LIBS[@]} -eq 1 ]; then
    XCFRAMEWORK_ARGS+=("-library" "${MACOS_LIBS[0]}" "-headers" "${BUILD_DIR}/headers")
fi

# Group iOS Device slices
if [ -f "${ROOT_DIR}/target/aarch64-apple-ios/${PROFILE}/liblilia_ffi.a" ]; then
    XCFRAMEWORK_ARGS+=("-library" "${ROOT_DIR}/target/aarch64-apple-ios/${PROFILE}/liblilia_ffi.a" "-headers" "${BUILD_DIR}/headers")
fi

# Group iOS Simulator slices
IOS_SIM_LIBS=()
[ -f "${ROOT_DIR}/target/aarch64-apple-ios-sim/${PROFILE}/liblilia_ffi.a" ] && IOS_SIM_LIBS+=("${ROOT_DIR}/target/aarch64-apple-ios-sim/${PROFILE}/liblilia_ffi.a")
[ -f "${ROOT_DIR}/target/x86_64-apple-ios/${PROFILE}/liblilia_ffi.a" ] && IOS_SIM_LIBS+=("${ROOT_DIR}/target/x86_64-apple-ios/${PROFILE}/liblilia_ffi.a")

if [ ${#IOS_SIM_LIBS[@]} -gt 1 ]; then
    mkdir -p "${BUILD_DIR}/ios-sim"
    lipo -create "${IOS_SIM_LIBS[@]}" -output "${BUILD_DIR}/ios-sim/liblilia_ffi.a"
    XCFRAMEWORK_ARGS+=("-library" "${BUILD_DIR}/ios-sim/liblilia_ffi.a" "-headers" "${BUILD_DIR}/headers")
elif [ ${#IOS_SIM_LIBS[@]} -eq 1 ]; then
    XCFRAMEWORK_ARGS+=("-library" "${IOS_SIM_LIBS[0]}" "-headers" "${BUILD_DIR}/headers")
fi

# Group watchOS Device slices
if [ -f "${ROOT_DIR}/target/aarch64-apple-watchos/${PROFILE}/liblilia_ffi.a" ]; then
    XCFRAMEWORK_ARGS+=("-library" "${ROOT_DIR}/target/aarch64-apple-watchos/${PROFILE}/liblilia_ffi.a" "-headers" "${BUILD_DIR}/headers")
fi

# Group watchOS Simulator slices
if [ -f "${ROOT_DIR}/target/aarch64-apple-watchos-sim/${PROFILE}/liblilia_ffi.a" ]; then
    XCFRAMEWORK_ARGS+=("-library" "${ROOT_DIR}/target/aarch64-apple-watchos-sim/${PROFILE}/liblilia_ffi.a" "-headers" "${BUILD_DIR}/headers")
fi

# Group tvOS Device slices
if [ -f "${ROOT_DIR}/target/aarch64-apple-tvos/${PROFILE}/liblilia_ffi.a" ]; then
    XCFRAMEWORK_ARGS+=("-library" "${ROOT_DIR}/target/aarch64-apple-tvos/${PROFILE}/liblilia_ffi.a" "-headers" "${BUILD_DIR}/headers")
fi

# Group tvOS Simulator slices
if [ -f "${ROOT_DIR}/target/aarch64-apple-tvos-sim/${PROFILE}/liblilia_ffi.a" ]; then
    XCFRAMEWORK_ARGS+=("-library" "${ROOT_DIR}/target/aarch64-apple-tvos-sim/${PROFILE}/liblilia_ffi.a" "-headers" "${BUILD_DIR}/headers")
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
    # The Flutter plugin's copy: a Swift package's binary target lies inside the package, and
    # Flutter copies the package into the app's build, so neither a path out of it nor a
    # symbolic link would survive.
    FLUTTER_XCFRAMEWORK="${ROOT_DIR}/packages/flutter/darwin/liliadb_flutter/Lilia.xcframework"
    rm -rf "${FLUTTER_XCFRAMEWORK}"
    cp -R "${XCFRAMEWORK_OUT}" "${FLUTTER_XCFRAMEWORK}"
else
    echo "==> No Apple target libraries or xcodebuild not found. Static libs generated."
fi

# 5. Generate Package.swift for SPM
cat << EOF > "${SPM_DIR}/Package.swift"
// swift-tools-version: 5.9
import PackageDescription

let package = Package(
    name: "Lilia",
    platforms: [
        .iOS("${IOS_MIN}"),
        .macOS("${MACOS_MIN}"),
        .watchOS("${WATCHOS_MIN}"),
        .tvOS("${TVOS_MIN}"),
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

