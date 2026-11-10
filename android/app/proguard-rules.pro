# JNA, as documented in its FAQ.
-dontwarn java.awt.*
-keep class com.sun.jna.* { *; }
-keep class * extends com.sun.jna.* { *; }
-keepclassmembers class * extends com.sun.jna.* { public *; }
# UniFFI's generated bindings bind native symbols by method name.
-keep class audio.litz.native.** { *; }
