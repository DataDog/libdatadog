// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

plugins {
    java
}

group = "com.datadoghq.libdatadog"
version = "0.1.0"

repositories {
    mavenCentral()
}

// Classpath for the Endive build-time compiler CLI (wasm → JVM classes).
val endiveCompilerCli by configurations.creating

// Prefer the canonical artifact from scripts/build.mjs (wasm-opt optimized); fall back to the raw
// cargo output so a standalone Gradle build works after only `cargo build --release --target
// wasm32-wasip1`. -Pwasm=/path/to.wasm overrides both.
val canonicalWasm = rootDir.resolve("../../artifacts/remote-config.wasm")
val cargoWasm = rootDir.resolve("../../target/wasm32-wasip1/release/libdd_remote_config_wasm_poc.wasm")
val aotWasm: Any = providers.gradleProperty("wasm")
    .map { rootDir.resolve(it) }
    .getOrElse(if (canonicalWasm.exists()) canonicalWasm else cargoWasm)

val aotSources = layout.buildDirectory.dir("generated/aot/sources")
val aotClasses = layout.buildDirectory.dir("generated/aot/classes")
val aotWasmDir = layout.buildDirectory.dir("generated/aot/wasm")

dependencies {
    implementation("run.endive:runtime:1.0.1")
    implementation("run.endive:wasi:1.0.1")
    implementation("org.json:json:20250517")
    endiveCompilerCli("run.endive:build-time-compiler-cli-experimental:1.0.1")
}

sourceSets {
    main {
        // The generated RemoteConfig wrapper compiles with the main sources; the emitted machine
        // classes and the stripped .meta wasm load from the classpath.
        java.srcDir(aotSources)
        resources.srcDir(aotClasses)
        resources.srcDir(aotWasmDir)
    }
}

// Gradle itself runs on a modern JDK (17+); all Java compilation targets Java 11 bytecode.
tasks.withType<JavaCompile>().configureEach {
    options.release.set(11)
}

// Ahead-of-time compile the reactor with the Endive build-time compiler CLI: every wasm function
// body becomes JVM bytecode, the stripped module structure is kept in a RemoteConfig.meta wasm,
// and a CompiledModule wrapper source is generated for the main source set.
val aotCompile by tasks.registering(JavaExec::class) {
    group = "build"
    classpath = endiveCompilerCli
    mainClass.set("run.endive.experimental.compiler.cli.Cli")
    argumentProviders.add(CommandLineArgumentProvider {
        listOf(
            aotWasm.toString(),
            "--prefix", "com.datadoghq.remoteconfig.poc.RemoteConfig",
            "--source-dir", aotSources.get().toString(),
            "--class-dir", aotClasses.get().toString(),
            "--wasm-dir", aotWasmDir.get().toString(),
            "--interpreter-fallback", "WARN"
        )
    })
    inputs.file(aotWasm)
    outputs.dirs(aotSources, aotClasses, aotWasmDir)
}

tasks.named<JavaCompile>("compileJava") {
    dependsOn(aotCompile)
    // The generated wrapper references the machine class constructor.
    classpath += files(aotClasses)
}

tasks.named("processResources") {
    // The machine classes and .meta wasm come from aotCompile via the source set resources.
    dependsOn(aotCompile)
}

// A self-contained jar so the conformance runner can spawn
// `java -jar hosts/java/build/libs/remote-config-host.jar <wasm> <url> <scenario>`.
val fatJar = tasks.register<Jar>("fatJar") {
    archiveFileName.set("remote-config-host.jar")
    duplicatesStrategy = DuplicatesStrategy.EXCLUDE
    manifest {
        attributes("Main-Class" to "com.datadoghq.remoteconfig.poc.Main")
    }
    from(sourceSets.main.get().output)
    from(aotClasses)
    from(aotWasmDir)
    dependsOn(aotCompile)
    dependsOn(configurations.runtimeClasspath)
    from({
        configurations.runtimeClasspath.get()
            .filter { it.name.endsWith(".jar") }
            .map { zipTree(it) }
    }) {
        exclude("module-info.class", "META-INF/LICENSE*", "META-INF/NOTICE*", "META-INF/versions/**/module-info.class")
    }
}

tasks.build {
    dependsOn(fatJar)
}

tasks.test {
    useJUnitPlatform()
    testLogging {
        events("failed")
    }
}
