# Rules R8 applies to the apps that depend on this library.
#
# JNA finds the native functions and the structures it passes through reflection: its classes,
# and the generated binding's (its library interfaces, structures and callbacks), must keep
# their names and members in a minified app.
-dontwarn java.awt.**
-keep class com.sun.jna.** { *; }
-keep class * implements com.sun.jna.** { *; }
-keepclassmembers class * extends com.sun.jna.** { public *; }

# The UniFFI binding of lilia_ffi.
-keep class uniffi.lilia_ffi.** { *; }
