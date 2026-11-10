#!/usr/bin/env bash
set -euo pipefail

# ==============================================================================
# LiliaDB - Android Multiplatform Packaging Script (Mobile, Wear OS, Android TV)
# Generates native .so libraries and Kotlin UniFFI bindings
# ==============================================================================

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
BUILD_DIR="${ROOT_DIR}/target/android-build"
OUT_DIR="${ROOT_DIR}/release/android"
KOTLIN_DIR="${ROOT_DIR}/packages/android-sdk"

echo "==> [LiliaDB] Preparing Android build environment..."
mkdir -p "${BUILD_DIR}"
mkdir -p "${OUT_DIR}/jniLibs"
mkdir -p "${KOTLIN_DIR}/src/main/java"

# 1. Build host uniffi-bindgen CLI if needed
BINDGEN="${ROOT_DIR}/target/debug/uniffi-bindgen"
HOST_DYLIB="${ROOT_DIR}/target/debug/liblilia_ffi.dylib"

if [ ! -f "${BINDGEN}" ]; then
    echo "==> [LiliaDB] Building uniffi-bindgen tool..."
    cargo build -p lilia-ffi --features cli --bin uniffi-bindgen
fi

# 2. Build host dylib to extract UniFFI metadata & Kotlin bindings if needed
if [ ! -f "${HOST_DYLIB}" ]; then
    echo "==> [LiliaDB] Building host dylib for metadata..."
    cargo build -p lilia-ffi --lib
fi

echo "==> [LiliaDB] Generating Kotlin bindings..."
${BINDGEN} generate "${HOST_DYLIB}" \
    --library \
    --language kotlin \
    --out-dir "${KOTLIN_DIR}/src/main/java"

# 3. Android Target Architectures Mapping
# - arm64-v8a   -> aarch64-linux-android  (Smartphones, Wear OS 64-bit, Android TV)
# - armeabi-v7a -> armv7-linux-androideabi (Wear OS 32-bit, budget TV dongles)
# - x86_64      -> x86_64-linux-android   (Emulators for Mobile, Wear OS, Android TV)
TARGETS=("aarch64-linux-android" "armv7-linux-androideabi" "x86_64-linux-android")
ABIS=("arm64-v8a" "armeabi-v7a" "x86_64")

echo "==> [LiliaDB] Checking Android NDK targets..."
if command -v cargo-ndk >/dev/null 2>&1 && [ -n "${ANDROID_NDK_HOME:-}" ]; then
    for i in "${!TARGETS[@]}"; do
        target="${TARGETS[$i]}"
        abi="${ABIS[$i]}"
        echo "==> Compiling for ABI: ${abi} (target: ${target})..."
        cargo ndk -t "${abi}" -o "${OUT_DIR}/jniLibs" build -p lilia-ffi --release
    done
    echo "==> [Success] Android JNI shared libraries generated at: ${OUT_DIR}/jniLibs"
else
    echo "==> cargo-ndk or ANDROID_NDK_HOME not detected in current environment."
    echo "==> To compile Android native shared libraries (.so):"
    echo "    1. Install cargo-ndk: cargo install cargo-ndk"
    echo "    2. Set ANDROID_NDK_HOME: export ANDROID_NDK_HOME=/path/to/ndk"
    echo "    3. Run this script again."
fi

# 4. Generate build.gradle.kts for Android Library
cat << 'EOF' > "${KOTLIN_DIR}/build.gradle.kts"
plugins {
    id("com.android.library")
    id("org.jetbrains.kotlin.android")
}

android {
    namespace = "io.liliadb"
    compileSdk = 34

    defaultConfig {
        minSdk = 21 // Android 5.0+, Wear OS 1.0+, Android TV
        targetSdk = 34
        consumerProguardFiles("consumer-rules.pro")
    }

    sourceSets {
        getByName("main") {
            jniLibs.srcDirs("../../release/android/jniLibs")
        }
    }
}

dependencies {
    implementation("net.java.dev.jna:jna:5.14.0@aar")
    implementation("org.jetbrains.kotlinx:kotlinx-coroutines-core:1.8.0")
}
EOF

echo "==> [LiliaDB] Android multiplatform build workflow complete!"
