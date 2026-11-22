// The iOS TurboModule of LiliaDB: installs the JSI binding (cpp/LiliaJsi.h) in the app's
// JavaScript runtime when React Native creates it, and names the directories a database may
// live in.

#import <Foundation/Foundation.h>
#import <LiliaReactNativeSpec/LiliaReactNativeSpec.h>
#import <ReactCommon/RCTTurboModuleWithJSIBindings.h>

#include "LiliaJsi.h"

@interface LiliaReactNative : NSObject <NativeLiliaReactNativeSpec, RCTTurboModuleWithJSIBindings>
@end

@implementation LiliaReactNative

RCT_EXPORT_MODULE(LiliaReactNative)

- (std::shared_ptr<facebook::react::TurboModule>)getTurboModule:
    (const facebook::react::ObjCTurboModule::InitParams &)params {
  return std::make_shared<facebook::react::NativeLiliaReactNativeSpecJSI>(params);
}

- (void)installJSIBindingsWithRuntime:(facebook::jsi::Runtime &)runtime
                          callInvoker:(const std::shared_ptr<facebook::react::CallInvoker> &)callInvoker {
  liliadb::install(runtime, callInvoker);
}

/// The path of `directory` in the user domain, created if missing.
static NSString *DirectoryPath(NSSearchPathDirectory directory) {
  NSURL *url = [[NSFileManager defaultManager] URLsForDirectory:directory inDomains:NSUserDomainMask].firstObject;
  [[NSFileManager defaultManager] createDirectoryAtURL:url withIntermediateDirectories:YES attributes:nil error:nil];
  return url.path;
}

- (NSDictionary *)directories {
  return @{
    @"applicationSupport" : DirectoryPath(NSApplicationSupportDirectory),
    @"documents" : DirectoryPath(NSDocumentDirectory),
    @"caches" : DirectoryPath(NSCachesDirectory),
  };
}

- (NSString *_Nullable)groupContainer:(NSString *)identifier {
  return [[NSFileManager defaultManager] containerURLForSecurityApplicationGroupIdentifier:identifier].path;
}

- (NSNumber *)excludeFromBackup:(NSString *)path {
  NSURL *url = [NSURL fileURLWithPath:path isDirectory:YES];
  return @([url setResourceValue:@YES forKey:NSURLIsExcludedFromBackupKey error:nil]);
}

@end
