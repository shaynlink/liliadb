Pod::Spec.new do |s|
  s.name             = 'liliadb_flutter'
  s.version          = '0.1.0-alpha.6'
  s.summary          = 'LiliaDB embedded local-first multimodel database for Flutter'
  s.description      = <<-DESC
LiliaDB provides durable Key/Value, JSON documents, Graph, FTS5, and SQL queries with transparent AES-256-GCM encryption.
                       DESC
  s.homepage         = 'https://github.com/shaynlink/lilia'
  s.license          = { :file => '../LICENSE' }
  s.author           = { 'LiliaDB Team' => 'dev@liliadb.io' }

  s.source           = { :path => '.' }
  s.source_files     = 'Classes/**/*'
  s.vendored_frameworks = '../../../release/apple/Lilia.xcframework'
  s.dependency 'Flutter'
  s.platform = :ios, '14.0'

  # Flutter.framework does not contain a i386 slice.
  s.pod_target_xcconfig = { 'DEFINES_MODULE' => 'YES', 'EXCLUDED_ARCHS[sdk=iphonesimulator*]' => 'i386' }
  s.swift_version = '5.9'
end
