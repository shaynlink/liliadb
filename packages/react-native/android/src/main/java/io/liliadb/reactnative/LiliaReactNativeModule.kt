package io.liliadb.reactnative

import com.facebook.proguard.annotations.DoNotStrip
import com.facebook.react.bridge.Arguments
import com.facebook.react.bridge.ReactApplicationContext
import com.facebook.react.bridge.WritableMap
import com.facebook.react.module.annotations.ReactModule
import com.facebook.react.turbomodule.core.interfaces.BindingsInstallerHolder
import com.facebook.react.turbomodule.core.interfaces.TurboModuleWithJSIBindings
import java.io.File

/**
 * The Android TurboModule of LiliaDB: React Native installs its JSI binding (cpp/LiliaJsi.h)
 * through [getBindingsInstaller] when it creates the module; [directories] names the
 * directories a database may live in.
 */
@ReactModule(name = LiliaReactNativeModule.NAME)
class LiliaReactNativeModule(context: ReactApplicationContext) :
    NativeLiliaReactNativeSpec(context), TurboModuleWithJSIBindings {

    override fun directories(): WritableMap {
        val context = reactApplicationContext
        return Arguments.createMap().apply {
            putString("noBackup", existing(context.noBackupFilesDir))
            putString("files", existing(context.filesDir))
            putString("cache", existing(context.cacheDir))
        }
    }

    /** App Groups are an iOS notion. */
    override fun groupContainer(identifier: String): String? = null

    /** Android leaves `noBackup` out of Auto Backup; other directories follow the app's rules. */
    override fun excludeFromBackup(path: String): Boolean = false

    @DoNotStrip
    external override fun getBindingsInstaller(): BindingsInstallerHolder

    private fun existing(directory: File): String {
        directory.mkdirs()
        return directory.absolutePath
    }

    companion object {
        const val NAME = "LiliaReactNative"

        init {
            System.loadLibrary("lilia_react_native")
        }
    }
}
