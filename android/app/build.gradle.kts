import org.gradle.api.tasks.Optional
import org.gradle.process.ExecOperations
import java.io.File
import javax.inject.Inject

plugins {
    id("com.android.application")
    id("org.jetbrains.kotlin.plugin.compose")
}

val cmakeVersionName = "3.22.1"
val minimumAndroidApi = providers.environmentVariable("LITZ_ANDROID_MIN_API").orElse("23").get().toInt()
val nativeAbis = (groovy.json.JsonSlurper().parseText(providers.environmentVariable("LITZ_ANDROID_TARGETS").orElse("""{"arm64-v8a":"aarch64-linux-android","x86_64":"x86_64-linux-android"}""").get()) as Map<*, *>)
    .entries.associate { it.key.toString() to it.value.toString() }
val rustDebugDirectory = layout.buildDirectory.dir("generated/rust/debug")
val rustReleaseDirectory = layout.buildDirectory.dir("generated/rust/release")
val cmakeMakeProgram = providers.environmentVariable("LITZ_CMAKE_MAKE_PROGRAM").orNull
val admobTestAppId = "ca-app-pub-3940256099942544~3347511713"
val admobTestBannerId = "ca-app-pub-3940256099942544/9214589741"
val admobAppId = providers.environmentVariable("LITZ_ADMOB_APP_ID").orElse("")
val admobBannerId = providers.environmentVariable("LITZ_ADMOB_BANNER_ID").orElse("")

android {
    namespace = "audio.litz"
    compileSdk = 37
    buildToolsVersion = "36.0.0"
    ndkVersion = providers.environmentVariable("LITZ_ANDROID_NDK_VERSION").orElse("28.2.13676358").get()

    defaultConfig {
        applicationId = "audio.litz"
        minSdk = minimumAndroidApi
        targetSdk = 37
        versionCode = 1
        versionName = "0.1.0"
        ndk { abiFilters += nativeAbis.keys }
        externalNativeBuild {
            cmake {
                arguments += "-DANDROID_STL=c++_shared"
                if (!cmakeMakeProgram.isNullOrBlank()) {
                    arguments += "-DCMAKE_MAKE_PROGRAM=$cmakeMakeProgram"
                }
            }
        }
    }

    buildFeatures {
        buildConfig = true
        compose = true
        prefab = true
    }
    androidResources {
        generateLocaleConfig = true
        localeFilters += listOf("en", "es")
    }
    flavorDimensions += "store"
    productFlavors {
        create("foss") { dimension = "store" }
        // The Mobile Ads SDK requires API 24.
        create("play") {
            dimension = "store"
            minSdk = maxOf(minimumAndroidApi, 24)
            manifestPlaceholders["admobAppId"] = admobTestAppId
            buildConfigField("String", "ADMOB_BANNER_ID", "\"$admobTestBannerId\"")
        }
    }
    testOptions { unitTests.isIncludeAndroidResources = true }
    buildTypes {
        debug {
            externalNativeBuild {
                cmake {
                    arguments += "-DLITZ_RUST_LIB_DIR=${rustDebugDirectory.get().asFile.absolutePath}"
                }
            }
        }
        release {
            isMinifyEnabled = true
            isShrinkResources = true
            proguardFiles(getDefaultProguardFile("proguard-android-optimize.txt"), "proguard-rules.pro")
            ndk { debugSymbolLevel = "SYMBOL_TABLE" }
            externalNativeBuild {
                cmake {
                    arguments += "-DLITZ_RUST_LIB_DIR=${rustReleaseDirectory.get().asFile.absolutePath}"
                }
            }
        }
    }
    externalNativeBuild {
        cmake {
            path = file("../../crates/phone/src/android/CMakeLists.txt")
            version = cmakeVersionName
        }
    }
    compileOptions {
        sourceCompatibility = JavaVersion.VERSION_17
        targetCompatibility = JavaVersion.VERSION_17
    }
    packaging { jniLibs { useLegacyPackaging = false } }
}

dependencies {
    implementation(platform("androidx.compose:compose-bom:2026.09.00"))
    implementation("androidx.activity:activity-compose:1.12.4")
    implementation("androidx.compose.material3:material3")
    implementation("androidx.lifecycle:lifecycle-viewmodel-compose:2.10.0")
    implementation("androidx.lifecycle:lifecycle-runtime-compose:2.10.0")
    implementation("androidx.datastore:datastore-preferences:1.2.1")
    implementation("com.google.oboe:oboe:1.11.0")
    implementation("net.java.dev.jna:jna:5.19.1@aar")
    "playImplementation"("com.google.android.gms:play-services-ads:25.5.0")
    "playImplementation"("com.google.android.ump:user-messaging-platform:4.0.0")
    testImplementation("junit:junit:4.13.2")
    testImplementation("net.java.dev.jna:jna:5.19.1")
    testImplementation("org.robolectric:robolectric:4.16.1")
    testImplementation("androidx.compose.ui:ui-test-junit4")
    debugImplementation("androidx.compose.ui:ui-test-manifest")
}

dependencyLocking { lockAllConfigurations() }

abstract class BuildRust : DefaultTask() {
    @get:OutputDirectory abstract val outputDirectory: DirectoryProperty
    @get:InputFiles @get:PathSensitive(PathSensitivity.RELATIVE)
    abstract val rustInputs: ConfigurableFileCollection
    @get:Internal abstract val repository: DirectoryProperty
    @get:Internal abstract val sdkDirectory: DirectoryProperty
    @get:Internal abstract val ndkDirectory: DirectoryProperty
    @get:LocalState abstract val targetDirectory: DirectoryProperty
    @get:Input abstract val minimumApi: Property<Int>
    @get:Input abstract val abis: MapProperty<String, String>
    @get:Input abstract val toolEnvironment: MapProperty<String, String>
    @get:InputFiles @get:PathSensitive(PathSensitivity.NONE)
    abstract val toolchainMetadata: ConfigurableFileCollection
    @get:Input abstract val cmakeVersion: Property<String>
    @get:Input @get:Optional abstract val cmakeMakeProgram: Property<String>
    @get:Input abstract val releaseBuild: Property<Boolean>
    @get:Inject abstract val exec: ExecOperations

    @TaskAction fun build() {
        val target = targetDirectory.get().asFile
        val windows = System.getProperty("os.name").lowercase().contains("windows")
        val exe = if (windows) ".exe" else ""
        // opusic-sys builds Opus with the cmake crate. Prefer the SDK's CMake
        // and Ninja so a separate host installation is not required. The Nix
        // shell overrides Ninja because the SDK's bundled binary cannot run
        // there.
        val sdkCmakeBin = sdkDirectory.get().asFile.resolve("cmake/${cmakeVersion.get()}/bin")
        val ninjaBin = cmakeMakeProgram.orNull?.let { File(it).parentFile }
        val sdkCmake = sdkCmakeBin.resolve("cmake$exe")
        abis.get().keys.forEach { abi ->
        val arguments = mutableListOf("cargo", "ndk", "-t", abi,
            "-P", minimumApi.get().toString(), "build", "--locked", "-p", "litz-phone",
            "--target-dir", target.absolutePath)
        if (releaseBuild.get()) arguments += "--release"
        exec.exec {
            workingDir(repository.get().asFile)
            commandLine(arguments)
            environment("ANDROID_NDK_HOME", ndkDirectory.get().asFile.absolutePath)
            environment(toolEnvironment.get())
            environment("PKG_CONFIG_ALLOW_CROSS", "0")
            if (sdkCmake.isFile) {
                environment("CMAKE", sdkCmake.absolutePath)
            }
            val toolBin = ninjaBin ?: sdkCmakeBin
            if (toolBin.isDirectory) {
                environment("PATH", "${toolBin.absolutePath}${File.pathSeparator}${toolEnvironment.get().getValue("PATH")}")
            }
            environment("CMAKE_GENERATOR", "Ninja")
        }
        }

        val output = outputDirectory.get().asFile
        val profile = if (releaseBuild.get()) "release" else "debug"
        abis.get()
            .forEach { (abi, triple) ->
                val source = target.resolve(triple).resolve(profile).resolve("liblitz_phone.a")
                check(source.isFile) { "cargo did not produce $source" }
                val destination = output.resolve(abi).resolve("liblitz_phone.a")
                destination.parentFile.mkdirs()
                source.copyTo(destination, overwrite = true)
            }
    }
}

fun registerRustTask(name: String, release: Boolean) = tasks.register<BuildRust>(name) {
    outputDirectory.set(layout.buildDirectory.dir("generated/rust/${if (release) "release" else "debug"}"))
    repository.set(rootProject.layout.projectDirectory.dir(".."))
    sdkDirectory.set(androidComponents.sdkComponents.sdkDirectory)
    ndkDirectory.set(androidComponents.sdkComponents.ndkDirectory)
    targetDirectory.set(layout.buildDirectory.dir("rust-target"))
    cmakeVersion.set(cmakeVersionName)
    minimumApi.set(minimumAndroidApi)
    abis.set(nativeAbis)
    toolEnvironment.set(providers.provider {
        val names = listOf("RUSTUP_TOOLCHAIN", "RUSTFLAGS", "CARGO_ENCODED_RUSTFLAGS", "CC", "CXX", "CFLAGS", "CXXFLAGS", "LDFLAGS", "NIX_CFLAGS_COMPILE", "NIX_LDFLAGS")
        buildMap {
            providers.environmentVariable("PATH").orElse(providers.environmentVariable("Path")).orNull
                ?.let { put("PATH", it) }
            names.forEach { name ->
                providers.environmentVariable(name).orNull?.let { put(name, it) }
            }
        }
    })
    toolchainMetadata.from(androidComponents.sdkComponents.ndkDirectory.map { it.file("source.properties") },
        androidComponents.sdkComponents.sdkDirectory.map { it.file("cmake/$cmakeVersionName/source.properties") })
    cmakeMakeProgram.set(providers.environmentVariable("LITZ_CMAKE_MAKE_PROGRAM"))
    releaseBuild.set(release)
    rustInputs.from(fileTree("../../crates/phone"), fileTree("../../crates/core"),
        file("../../Cargo.toml"), file("../../Cargo.lock"), file("../../rust-toolchain.toml"), fileTree("../../.cargo"))
}

val rustDebug = registerRustTask("buildRustDebug", false)
val rustRelease = registerRustTask("buildRustRelease", true)

fun hostEnvironment() = providers.provider {
    buildMap {
        providers.environmentVariable("PATH").orElse(providers.environmentVariable("Path")).orNull
            ?.let { put("PATH", it) }
        providers.environmentVariable("RUSTUP_TOOLCHAIN").orNull?.let { put("RUSTUP_TOOLCHAIN", it) }
    }
}

abstract class GenerateBindings : DefaultTask() {
    @get:InputFiles @get:PathSensitive(PathSensitivity.NONE)
    abstract val library: ConfigurableFileCollection
    @get:InputFiles @get:PathSensitive(PathSensitivity.RELATIVE)
    abstract val generatorInputs: ConfigurableFileCollection
    @get:OutputDirectory abstract val outputDirectory: DirectoryProperty
    @get:Internal abstract val repository: DirectoryProperty
    @get:LocalState abstract val targetDirectory: DirectoryProperty
    @get:Input abstract val toolEnvironment: MapProperty<String, String>
    @get:Inject abstract val exec: ExecOperations
    @get:Inject abstract val files: FileSystemOperations

    @TaskAction fun generate() {
        val output = outputDirectory.get().asFile
        files.delete { delete(output) }
        exec.exec {
            workingDir(repository.get().asFile)
            commandLine("cargo", "run", "--locked", "-p", "uniffi-bindgen",
                "--target-dir", targetDirectory.get().asFile.absolutePath, "--",
                "generate", "--library", library.singleFile.absolutePath,
                "--language", "kotlin", "--no-format", "--out-dir", output.absolutePath)
            environment(toolEnvironment.get())
        }
    }
}

// Bindings come from the same archive the variant links, so the Kotlin side
// always matches the native library it loads.
fun registerBindingsTask(name: String, rustTask: TaskProvider<BuildRust>) =
    tasks.register<GenerateBindings>(name) {
        library.from(rustTask.flatMap { it.outputDirectory.file("${nativeAbis.keys.first()}/liblitz_phone.a") })
        generatorInputs.from(file("../../crates/phone/uniffi.toml"), fileTree("../../crates/uniffi-bindgen"))
        outputDirectory.set(layout.buildDirectory.dir("generated/uniffi/${rustTask.name}"))
        repository.set(rootProject.layout.projectDirectory.dir(".."))
        targetDirectory.set(layout.buildDirectory.dir("rust-host-target"))
        toolEnvironment.set(hostEnvironment())
    }

val bindingsDebug = registerBindingsTask("generateBindingsDebug", rustDebug)
val bindingsRelease = registerBindingsTask("generateBindingsRelease", rustRelease)

abstract class BuildHostLibrary : DefaultTask() {
    @get:InputFiles @get:PathSensitive(PathSensitivity.RELATIVE)
    abstract val rustInputs: ConfigurableFileCollection
    @get:Internal abstract val repository: DirectoryProperty
    @get:OutputDirectory abstract val targetDirectory: DirectoryProperty
    @get:Input abstract val toolEnvironment: MapProperty<String, String>
    @get:Inject abstract val exec: ExecOperations

    @TaskAction fun build() {
        exec.exec {
            workingDir(repository.get().asFile)
            commandLine("cargo", "rustc", "--locked", "-p", "litz-phone", "--lib",
                "--crate-type", "cdylib", "--target-dir", targetDirectory.get().asFile.absolutePath)
            environment(toolEnvironment.get())
        }
    }
}

// JVM tests call the real bridge through JNA, against a host build of the library.
val hostLibrary = tasks.register<BuildHostLibrary>("buildHostLibrary") {
    rustInputs.from(fileTree("../../crates/phone"), fileTree("../../crates/core"),
        file("../../Cargo.toml"), file("../../Cargo.lock"), file("../../rust-toolchain.toml"))
    repository.set(rootProject.layout.projectDirectory.dir(".."))
    targetDirectory.set(layout.buildDirectory.dir("rust-host-library"))
    toolEnvironment.set(providers.provider {
        buildMap {
            putAll(hostEnvironment().get())
            listOf("CC", "CXX", "CFLAGS", "CXXFLAGS", "LDFLAGS", "NIX_CFLAGS_COMPILE", "NIX_LDFLAGS")
                .forEach { name -> providers.environmentVariable(name).orNull?.let { put(name, it) } }
        }
    })
}
tasks.withType<Test>().configureEach {
    dependsOn(hostLibrary)
    systemProperty("jna.library.path",
        layout.buildDirectory.dir("rust-host-library/debug").get().asFile.absolutePath)
}

abstract class CopyNativeNotices : DefaultTask() {
    @get:InputFile abstract val projectLicense: RegularFileProperty
    @get:InputFile abstract val licenseException: RegularFileProperty
    @get:InputFile abstract val thirdPartyNotices: RegularFileProperty
    @get:InputFile abstract val thirdPartyLicenses: RegularFileProperty
    @get:OutputDirectory abstract val outputDirectory: DirectoryProperty
    @get:Inject abstract val files: FileSystemOperations
    @TaskAction fun copy() {
        files.sync {
            from(projectLicense) { rename { "LITZ-LICENSE" } }
            from(licenseException) { rename { "LITZ-LICENSE-EXCEPTION" } }
            from(thirdPartyNotices)
            from(thirdPartyLicenses) { rename { "THIRD_PARTY_LICENSES" } }
            into(outputDirectory.dir("licenses"))
        }
    }
}
val nativeNotices = tasks.register<CopyNativeNotices>("copyNativeNotices") {
    projectLicense.set(rootProject.file("../LICENSE"))
    licenseException.set(rootProject.file("../LICENSE-EXCEPTION"))
    thirdPartyNotices.set(rootProject.file("../THIRD_PARTY_NOTICES"))
    thirdPartyLicenses.set(rootProject.file("../licenses/android.txt"))
    outputDirectory.set(layout.buildDirectory.dir("generated/native-notices"))
}

abstract class CheckAdMobIds : DefaultTask() {
    @get:Input abstract val appId: Property<String>
    @get:Input abstract val bannerId: Property<String>
    @TaskAction fun check() {
        check(Regex("ca-app-pub-\\d+~\\d+").matches(appId.get())) {
            "LITZ_ADMOB_APP_ID must be the AdMob app ID (ca-app-pub-…~…) for Play release builds"
        }
        check(Regex("ca-app-pub-\\d+/\\d+").matches(bannerId.get())) {
            "LITZ_ADMOB_BANNER_ID must be the AdMob banner unit ID (ca-app-pub-…/…) for Play release builds"
        }
    }
}
val checkAdMobIds = tasks.register<CheckAdMobIds>("checkAdMobIds") {
    appId.set(admobAppId)
    bannerId.set(admobBannerId)
}
tasks.matching { it.name == "prePlayReleaseBuild" }.configureEach { dependsOn(checkAdMobIds) }

androidComponents.onVariants { variant ->
    variant.sources.assets?.addGeneratedSourceDirectory(nativeNotices, CopyNativeNotices::outputDirectory)
    variant.sources.kotlin?.addGeneratedSourceDirectory(
        if (variant.buildType == "release") bindingsRelease else bindingsDebug,
        GenerateBindings::outputDirectory)
    if (variant.flavorName == "play" && variant.buildType == "release") {
        variant.manifestPlaceholders.put("admobAppId", admobAppId.get())
        variant.buildConfigFields?.put("ADMOB_BANNER_ID",
            com.android.build.api.variant.BuildConfigField("String", "\"${admobBannerId.get()}\"", null))
    }
}

// AGP names the CMake profiles Debug and RelWithDebInfo (Release is accepted as
// a fallback for configurations that do not use debug symbols). The Rust
// archive must exist before CMake configures, and changes must relink.
listOf("Debug" to rustDebug, "RelWithDebInfo" to rustRelease, "Release" to rustRelease)
    .forEach { (profile, rustTask) ->
        tasks.matching {
            it.name.startsWith("configureCMake$profile") || it.name.startsWith("buildCMake$profile")
        }.configureEach {
            dependsOn(rustTask)
            inputs.dir(rustTask.flatMap { it.outputDirectory })
        }
    }
