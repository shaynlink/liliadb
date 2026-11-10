module.exports = {
  dependency: {
    platforms: {
      ios: {
        podspecPath: "./LiliaReactNative.podspec",
      },
      android: {
        packageInstance: "new LiliaReactNativePackage()",
        packageImportPath: "import io.liliadb.reactnative.LiliaReactNativePackage;",
      },
    },
  },
};
