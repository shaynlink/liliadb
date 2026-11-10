#import <React/RCTBridgeModule.h>

@interface RCT_EXTERN_MODULE(LiliaReactNativeModule, NSObject)

RCT_EXTERN_METHOD(openDatabase:(NSDictionary *)options
                  resolver:(RCTPromiseResolveBlock)resolve
                  rejecter:(RCTPromiseRejectBlock)reject)

RCT_EXTERN_METHOD(prepareForBackground:(NSString *)handle
                  resolver:(RCTPromiseResolveBlock)resolve
                  rejecter:(RCTPromiseRejectBlock)reject)

RCT_EXTERN_METHOD(kvSet:(NSString *)handle
                  namespace:(NSString *)namespace
                  keyHex:(NSString *)keyHex
                  valHex:(NSString *)valHex
                  expiresAtMs:(NSNumber *)expiresAtMs
                  resolver:(RCTPromiseResolveBlock)resolve
                  rejecter:(RCTPromiseRejectBlock)reject)

RCT_EXTERN_METHOD(kvGet:(NSString *)handle
                  namespace:(NSString *)namespace
                  keyHex:(NSString *)keyHex
                  resolver:(RCTPromiseResolveBlock)resolve
                  rejecter:(RCTPromiseRejectBlock)reject)

RCT_EXTERN_METHOD(jsonPut:(NSString *)handle
                  space:(NSString *)space
                  id:(NSString *)id
                  valueJson:(NSString *)valueJson
                  resolver:(RCTPromiseResolveBlock)resolve
                  rejecter:(RCTPromiseRejectBlock)reject)

RCT_EXTERN_METHOD(jsonGet:(NSString *)handle
                  space:(NSString *)space
                  id:(NSString *)id
                  resolver:(RCTPromiseResolveBlock)resolve
                  rejecter:(RCTPromiseRejectBlock)reject)

RCT_EXTERN_METHOD(sqlQuery:(NSString *)handle
                  sql:(NSString *)sql
                  paramsJson:(NSString *)paramsJson
                  resolver:(RCTPromiseResolveBlock)resolve
                  rejecter:(RCTPromiseRejectBlock)reject)

RCT_EXTERN_METHOD(close:(NSString *)handle
                  timeoutMs:(NSNumber *)timeoutMs
                  resolver:(RCTPromiseResolveBlock)resolve
                  rejecter:(RCTPromiseRejectBlock)reject)

+ (BOOL)requiresMainQueueSetup
{
    return NO;
}

@end
