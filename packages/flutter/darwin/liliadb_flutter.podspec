Pod::Spec.new do |s|
  s.name             = 'liliadb_flutter'
  s.version          = '0.2.0-alpha.1'
  s.summary          = 'LiliaDB embedded local-first multimodel database for Flutter'
  s.description      = <<-DESC
LiliaDB provides durable key-value records, JSON documents with secondary indexes, graphs, full-text search and read-only SQL, called from Dart through dart:ffi.
                       DESC
  s.homepage         = 'https://github.com/shaynlink/liliadb'
  s.license          = { :type => 'Apache-2.0', :file => '../../../LICENSE' }
  s.author           = { 'LiliaDB Team' => 'dev@liliadb.io' }
  s.source           = { :path => '.' }

  # The same sources as the Swift package (liliadb_flutter/Package.swift), for CocoaPods apps.
  s.source_files        = 'liliadb_flutter/Sources/liliadb_flutter/**/*.{c,h}'
  s.public_header_files = 'liliadb_flutter/Sources/liliadb_flutter/include/*.h'
  s.vendored_frameworks = 'liliadb_flutter/Lilia.xcframework'

  s.ios.deployment_target = '15.0'
  s.ios.dependency 'Flutter'
  s.osx.deployment_target = '12.0'
  s.osx.dependency 'FlutterMacOS'

  # Flutter.framework has no i386 slice.
  s.pod_target_xcconfig = {
    'DEFINES_MODULE' => 'YES',
    'EXCLUDED_ARCHS[sdk=iphonesimulator*]' => 'i386',
  }
end
