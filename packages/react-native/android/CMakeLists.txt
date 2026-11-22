cmake_minimum_required(VERSION 3.13)
project(lilia_react_native)

set(CMAKE_CXX_STANDARD 20)
set(CMAKE_CXX_STANDARD_REQUIRED ON)

find_package(ReactAndroid REQUIRED CONFIG)
find_package(fbjni REQUIRED CONFIG)

# The engine's C ABI (liblilia_ffi), built by tools/mobile/build-android.sh. It has no soname:
# it is linked by name, so the app finds it among its own libraries.
add_library(lilia_ffi SHARED IMPORTED)
set_target_properties(lilia_ffi PROPERTIES
    IMPORTED_LOCATION "${CMAKE_SOURCE_DIR}/../../../release/android/jniLibs/${ANDROID_ABI}/liblilia_ffi.so"
    IMPORTED_NO_SONAME TRUE)

add_library(lilia_react_native SHARED
    ../cpp/LiliaJsi.cpp
    src/main/cpp/LiliaJni.cpp)

target_include_directories(lilia_react_native PRIVATE ../cpp)

target_link_libraries(lilia_react_native
    lilia_ffi
    ReactAndroid::jsi
    ReactAndroid::reactnative
    fbjni::fbjni)
