plugins {
    id("com.android.application") version "9.4.1" apply false
    id("org.jetbrains.kotlin.android") version "2.3.21" apply false
    id("org.jetbrains.kotlin.plugin.compose") version "2.3.21" apply false
}

val kotlinFormatter = configurations.create("kotlinFormatter")
kotlinFormatter.attributes.attribute(
    org.gradle.api.attributes.Bundling.BUNDLING_ATTRIBUTE,
    objects.named(org.gradle.api.attributes.Bundling.SHADOWED),
)
kotlinFormatter.attributes.attribute(
    org.gradle.api.attributes.Usage.USAGE_ATTRIBUTE,
    objects.named(org.gradle.api.attributes.Usage.JAVA_RUNTIME),
)
kotlinFormatter.attributes.attribute(
    org.gradle.api.attributes.java.TargetJvmEnvironment.TARGET_JVM_ENVIRONMENT_ATTRIBUTE,
    objects.named(org.gradle.api.attributes.java.TargetJvmEnvironment.STANDARD_JVM),
)
dependencies { add(kotlinFormatter.name, "com.facebook:ktfmt:0.59") }
fun registerFormat(name: String, check: Boolean) = tasks.register<JavaExec>(name) {
    group = "verification"
    classpath = kotlinFormatter
    mainClass.set("com.facebook.ktfmt.cli.Main")
    args("--kotlinlang-style")
    if (check) args("--dry-run", "--set-exit-if-changed")
    args(fileTree("app/src") { include("**/*.kt") }.files.sorted().map { it.absolutePath })
}
registerFormat("formatKotlin", false)
registerFormat("checkKotlinFormat", true)
