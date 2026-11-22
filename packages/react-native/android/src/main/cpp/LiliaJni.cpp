// The JNI side of LiliaReactNativeModule: the installer of the JSI binding (cpp/LiliaJsi.h),
// which React Native runs with the app's JavaScript runtime when it creates the module.

#include <ReactCommon/BindingsInstallerHolder.h>
#include <fbjni/fbjni.h>
#include <jsi/jsi.h>

#include "LiliaJsi.h"

namespace {

using facebook::react::BindingsInstallerHolder;

struct LiliaReactNativeModule : facebook::jni::JavaClass<LiliaReactNativeModule> {
  static constexpr auto kJavaDescriptor = "Lio/liliadb/reactnative/LiliaReactNativeModule;";

  static facebook::jni::local_ref<BindingsInstallerHolder::javaobject>
  getBindingsInstaller(facebook::jni::alias_ref<LiliaReactNativeModule>) {
    return BindingsInstallerHolder::newObjectCxxArgs(
        [](facebook::jsi::Runtime &runtime,
           const std::shared_ptr<facebook::react::CallInvoker> &invoker) {
          liliadb::install(runtime, invoker);
        });
  }

  static void registerNatives() {
    javaClassLocal()->registerNatives({
        makeNativeMethod("getBindingsInstaller", LiliaReactNativeModule::getBindingsInstaller),
    });
  }
};

} // namespace

JNIEXPORT jint JNICALL JNI_OnLoad(JavaVM *vm, void *) {
  return facebook::jni::initialize(vm, [] { LiliaReactNativeModule::registerNatives(); });
}
