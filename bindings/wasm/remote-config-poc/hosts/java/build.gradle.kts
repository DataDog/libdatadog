// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

import java.security.MessageDigest

plugins {
    `java-library`
    `maven-publish`
}

// The crate's build manifest (written by scripts/build.mjs): the libdatadog revision the wasm
// was built from, the wasm hashes, and the optimizer identity. The publish flow keys the
// artifact version and its checksums manifest on these values.
val crateManifest = groovy.json.JsonSlurper()
    .parseText(rootDir.resolve("../../artifacts/manifest.json").readText()) as Map<String, Any>

// Every Endive dependency is pinned to the same version: the runtime that executes the AOT
// machine classes, the WASI support, and the build-time compiler CLI that emits them.
val endiveVersion = "1.2.0"

// PoC coordinates under the personal GitHub namespace (the distribution-repository details
// live in scripts/publish-java-aot.mjs): publish to a static gh-pages Maven repository,
// upstream to com.datadoghq on Maven Central later.
group = "io.github.bantonsson"
version = "${providers.gradleProperty("baseVersion").getOrElse("0.1.0")}+${(crateManifest["sourceBase"] as String).take(7)}"

repositories {
    mavenCentral()
}

// Classpath for the Endive build-time compiler CLI (wasm → JVM classes).
val endiveCompilerCli by configurations.creating

// The AOT input is the Java-specific artifact from scripts/build.mjs (wasm-opt -O1 keeps every
// wasm function below the JVM's 64 KiB single-method code limit, so the Endive AOT compiler
// maps each one to its own JVM method). -Pwasm=/path/to.wasm overrides it.
val aotWasm = providers.gradleProperty("wasm")
    .map { rootDir.resolve(it) }
    .orElse(rootDir.resolve("../../artifacts/remote-config-java.wasm"))

val aotSources = layout.buildDirectory.dir("generated/aot/sources")
val aotClasses = layout.buildDirectory.dir("generated/aot/classes")
val aotWasmDir = layout.buildDirectory.dir("generated/aot/wasm")

// The provenance record embedded in every published jar: the exact wasm input (by hash and
// optimizer), the libdatadog revision it was built from, and the compiler versions. The wasm
// builds are bit-reproducible, so this is a reproducibility claim a verifier can check.
val aotRecordDir = layout.buildDirectory.dir("generated/aot/record")
val aotRecordFile = aotRecordDir.map { it.file("remote-config-aot.json") }
val aotRecord by tasks.registering {
    inputs.property("sourceBase", crateManifest["sourceBase"])
    inputs.property("javaSha256", crateManifest["javaSha256"])
    inputs.property("javaBytes", crateManifest["javaBytes"])
    inputs.property("optimizer", crateManifest["optimizer"])
    inputs.property("endiveVersion", endiveVersion)
    outputs.file(aotRecordFile)
    doLast {
        aotRecordFile.get().asFile.writeText("""
            {
              "libdatadogRevision": "${crateManifest["sourceBase"]}",
              "wasm": {
                "sha256": "${crateManifest["javaSha256"]}",
                "bytes": ${crateManifest["javaBytes"]},
                "optimizer": "${crateManifest["optimizer"]}",
                "optimization": "${crateManifest["javaOptimization"]}"
              },
              "endiveCli": "$endiveVersion",
              "endiveRuntime": "$endiveVersion",
              "abiVersion": ${crateManifest["abiVersion"]}
            }
        """.trimIndent() + "\n")
    }
}

dependencies {
    implementation("run.endive:runtime:$endiveVersion")
    implementation("run.endive:wasi:$endiveVersion")
    // RcAbi's public API crosses org.json types (write/json/jsonArray), so consumers
    // need it at compile scope, not just runtime.
    api("org.json:json:20250517")
    endiveCompilerCli("run.endive:build-time-compiler-cli-experimental:$endiveVersion")
}

sourceSets {
    main {
        // The generated RemoteConfig wrapper compiles with the main sources; the emitted machine
        // classes and the stripped .meta wasm load from the classpath.
        java.srcDir(aotSources)
        resources.srcDir(aotClasses)
        resources.srcDir(aotWasmDir)
        resources.srcDir(aotRecordDir)
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
    // The machine classes, .meta wasm, and provenance record come from aotCompile/aotRecord
    // via the source set resources.
    dependsOn(aotCompile)
    dependsOn(aotRecord)
}

/** The sha256 of the AOT wasm input, stamped into every jar manifest. */
fun wasmSha256(): String {
    val digest = MessageDigest.getInstance("SHA-256")
    aotWasm.get().inputStream().use { input ->
        val buffer = ByteArray(64 * 1024)
        while (true) {
            val read = input.read(buffer)
            if (read < 0) break
            digest.update(buffer, 0, read)
        }
    }
    return digest.digest().joinToString("") { byte -> "%02x".format(byte) }
}

// The published library jar: AOT machine classes, RemoteConfig.meta, the generated wrapper,
// the host library, and the provenance record — everything the Java build needs so it never
// runs the wasm toolchain. The Main CLI demo stays local (it exists for the conformance
// runner, which uses the fatJar below).
tasks.jar {
    dependsOn(aotCompile)
    dependsOn(aotRecord)
    // The Main CLI demo stays local (it exists for the conformance runner, which uses the
    // fatJar below); exclude it and its nested classes from the published library jar.
    exclude("com/datadoghq/remoteconfig/poc/Main.class")
    exclude("com/datadoghq/remoteconfig/poc/Main\$*.class")
    manifest {
        attributes("Implementation-Title" to "remote-config-wasm-aot")
    }
    doFirst {
        // Stamp the published jar with the identity of the wasm it embeds; with -Pwasm=…
        // this is the overriding wasm, matching the jar actually built.
        manifest.attributes("Wasm-Sha256" to wasmSha256())
    }
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
        manifest.attributes("Wasm-Sha256" to wasmSha256())
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

// Publishes the AOT jar into a Maven-layout directory under hosts/java/build/maven-repo;
// scripts/publish-java-aot.mjs pushes that layout to the gh-pages branch of the distribution
// scripts/publish-java-aot.mjs pushes that layout to the gh-pages branch of the distribution
// repository. Consumers add one repository line:
//   maven { url "https://bantonsson.github.io/dd-remote-config-wasm-aot/" }
publishing {
    publications {
        create<MavenPublication>("remoteConfigAot") {
            artifactId = "remote-config-wasm-aot"
            from(components["java"])
            pom {
                name.set("Datadog remote-config WASM reactor, AOT-compiled for the JVM")
                description.set(
                    "The libdatadog remote-config WASM PoC reactor, AOT-compiled to JVM " +
                        "machine classes with Endive and shipped with its Java host library. " +
                        "Prebuilt so consumers do not need the wasm toolchain (see the artifact's " +
                        "remote-config-aot.json provenance record).")
                licenses {
                    license {
                        name.set("Apache-2.0")
                        url.set("https://www.apache.org/licenses/LICENSE-2.0.txt")
                    }
                }
                properties.set(
                    mapOf(
                        "libdatadog.revision" to crateManifest["sourceBase"] as String,
                        "endive.cli.version" to endiveVersion,
                        "wasm.opt.version" to crateManifest["optimizer"] as String,
                        "wasm.java.sha256" to crateManifest["javaSha256"] as String
                    )
                )
            }
        }
    }
    repositories {
        maven {
            name = "staging"
            url = uri(layout.buildDirectory.dir("maven-repo"))
        }
    }
}
