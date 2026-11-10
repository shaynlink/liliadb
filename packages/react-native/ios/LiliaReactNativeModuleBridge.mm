#import <React/RCTBridgeModule.h>
#import <React/RCTBridge+Private.h>
#import <jsi/jsi.h>
#include "LiliaJsiInstaller.h"

@interface LiliaReactNativeJsiModule : NSObject <RCTBridgeModule>
@property (nonatomic, weak) RCTBridge *bridge;
@end

@implementation LiliaReactNativeJsiModule

RCT_EXPORT_MODULE(LiliaReactNativeJsi)

+ (BOOL)requiresMainQueueSetup {
    return YES;
}

- (void)setBridge:(RCTBridge *)bridge {
    _bridge = bridge;
    
    RCTCxxBridge *cxxBridge = (RCTCxxBridge *)bridge;
    if (cxxBridge.runtime) {
        facebook::jsi::Runtime *runtime = (facebook::jsi::Runtime *)cxxBridge.runtime;
        if (runtime) {
            liliadb::installLiliaJsi(*runtime);
        }
    }
}

@end
