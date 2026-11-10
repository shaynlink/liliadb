cmake_minimum_required(VERSION 3.13)
project(LiliaReactNativeJsi)

set(CMAKE_VERBOSE_MAKEFILE ON)
set(CMAKE_CXX_STANDARD 20)

include_directories(
    ../cpp
    ${REACT_NATIVE_DIR}/ReactCommon
    ${REACT_NATIVE_DIR}/ReactCommon/jsi
)

add_library(
    lilia_react_native_jsi
    SHARED
    ../cpp/LiliaJsiHostObject.cpp
    ../cpp/LiliaJsiInstaller.cpp
)

find_library(LOG_LIB log)

target_link_libraries(
    lilia_react_native_jsi
    ${LOG_LIB}
)
