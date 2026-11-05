// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

import java.security.MessageDigest

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

// The AOT input is the Java-specific artifact from scripts/build.mjs (wasm-opt -O1: every
// function stays small enough for one JVM method, so any interpreter fallback fails the
// build). -Pwasm=/path/to.wasm overrides it.
val aotWasm = providers.gradleProperty("wasm")
    .map { rootDir.resolve(it) }
    .orElse(rootDir.resolve("../../artifacts/remote-config-java.wasm"))

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
            aotWasm.get().toString(),
            "--prefix", "com.datadoghq.remoteconfig.poc.RemoteConfig",
            "--source-dir", aotSources.get().toString(),
            "--class-dir", aotClasses.get().toString(),
            "--wasm-dir", aotWasmDir.get().toString()
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
    doFirst {
        // Stamp the executed jar with the identity of the wasm it embeds.
        val digest = MessageDigest.getInstance("SHA-256")
        aotWasm.get().inputStream().use { input ->
            val buffer = ByteArray(64 * 1024)
            while (true) {
                val read = input.read(buffer)
                if (read < 0) break
                digest.update(buffer, 0, read)
            }
        }
        manifest.attributes(
            "Wasm-Sha256" to digest.digest().joinToString("") { byte -> "%02x".format(byte) })
    }
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
