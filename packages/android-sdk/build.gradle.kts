// AGP 9 compiles Kotlin itself: no Kotlin Gradle plugin.
plugins {
    id("com.android.library")
}

android {
    namespace = "io.liliadb"
    compileSdk = 37

    defaultConfig {
        minSdk = 21 // Android 5.0+, Wear OS 1.0+, Android TV
        consumerProguardFiles("consumer-rules.pro")
    }

    sourceSets {
        getByName("main") {
            jniLibs.srcDirs("../../release/android/jniLibs")
        }
    }
}

dependencies {
    implementation("net.java.dev.jna:jna:5.19.1@aar")
    implementation("org.jetbrains.kotlinx:kotlinx-coroutines-core:1.11.0")
}
