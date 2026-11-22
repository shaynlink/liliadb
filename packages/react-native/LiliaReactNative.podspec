require "json"

package = JSON.parse(File.read(File.join(__dir__, "package.json")))

Pod::Spec.new do |s|
  s.name         = "LiliaReactNative"
  s.version      = package["version"]
  s.summary      = package["description"]
  s.homepage     = "https://github.com/shaynlink/lilia"
  s.license      = package["license"]
  s.authors      = package["author"]

  s.platforms    = { :ios => min_ios_version_supported }
  s.source       = { :git => "https://github.com/shaynlink/lilia.git", :tag => "v#{s.version}" }

  s.source_files = "ios/**/*.{h,mm}", "cpp/**/*.{h,cpp}"
  s.private_header_files = "cpp/**/*.h"
  # The engine's C ABI (liblilia_ffi), built by tools/mobile/build-apple.sh.
  s.vendored_frameworks = "../../release/apple/Lilia.xcframework"

  s.pod_target_xcconfig = {
    "CLANG_CXX_LANGUAGE_STANDARD" => "c++20",
    "HEADER_SEARCH_PATHS" => "\"$(PODS_TARGET_SRCROOT)/cpp\""
  }

  # React Native's dependencies of a TurboModule, codegen included.
  install_modules_dependencies(s)
end
