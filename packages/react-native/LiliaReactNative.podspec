require "json"

package = JSON.parse(File.read(File.join(__dir__, "package.json")))

Pod::Spec.new do |s|
  s.name         = "LiliaReactNative"
  s.version      = package["version"]
  s.summary      = package["description"]
  s.homepage     = "https://github.com/shaynlink/lilia"
  s.license      = package["license"]
  s.authors      = package["author"]

  s.platforms    = { :ios => "14.0", :tvos => "14.0", :osx => "11.0" }
  s.source       = { :git => "https://github.com/shaynlink/lilia.git", :tag => "v#{s.version}" }

  s.source_files = "ios/**/*.{h,m,mm,swift}", "cpp/**/*.{h,cpp}"
  s.vendored_frameworks = "../../release/apple/Lilia.xcframework"

  s.pod_target_xcconfig = {
    "CLANG_CXX_LANGUAGE_STANDARD" => "c++20",
    "HEADER_SEARCH_PATHS" => "\"$(PODS_TARGET_SRCROOT)/cpp\""
  }

  s.dependency "React-Core"
  s.dependency "React-callinvoker"
  s.swift_version = "5.9"
end
