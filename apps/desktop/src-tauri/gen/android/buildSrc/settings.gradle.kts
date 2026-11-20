// The kotlin-dsl plugin pulls in the Kotlin Gradle plugin and standard
// library. Gradle's plugin portal sometimes fails to serve those, which
// breaks the Android build, so they come from Maven Central first.
pluginManagement {
    repositories {
        mavenCentral()
        google()
        gradlePluginPortal()
    }
}
