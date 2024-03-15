import kotlin.io.path.absolutePathString

plugins {
  `java-library`
  `maven-publish`
  id("com.palantir.git-version")
  id("com.diffplug.spotless")
}

val gitVersion: groovy.lang.Closure<String> by extra
project.group = "com.datadoghq"
project.version = "0.0.1" // gitVersion()

repositories {
  mavenLocal()
  mavenCentral()
  gradlePluginPortal()
}

java {
  toolchain {
    languageVersion.set(JavaLanguageVersion.of(8))
  }
}

dependencies {
  api("org.apache.commons:commons-math3:3.6.1")
  testImplementation("org.junit.jupiter:junit-jupiter:5.9.3")
  testImplementation("org.assertj:assertj-core:3.25.1")
  testImplementation("org.msgpack:msgpack-core:0.9.8")
  testRuntimeOnly("org.junit.platform:junit-platform-launcher")
}

spotless {
  java {
    googleJavaFormat()
    formatAnnotations()
    licenseHeader(
      """
      |// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
      |// SPDX-License-Identifier: Apache-2.0
      |
      |
      """.trimMargin(),
    )
  }
  kotlinGradle {
    ktlint().editorConfigOverride(
      mapOf(
        "indent_size" to 2,
      ),
    )
  }
}

publishing {
  publications {
    create<MavenPublication>("maven") {
      from(components["java"])
    }
  }
}

val compileNativeLibrary = tasks.register<Exec>("compileNativeLibrary") {
  outputs.upToDateWhen { false }
  description = "Compile the native library"
  group = "build"
  workingDir = projectDir.resolve("../")
  commandLine("cargo", "build", "--target-dir", "target")
}

val nativeSourceDir = projectDir.resolve("../target/debug").toPath()?.absolutePathString() ?: "."
val nativeTargetDir = sourceSets.main.get().output.resourcesDir?.toPath()?.resolve("native")?.absolutePathString() ?: "."

val copyNativeLibs = tasks.register<Copy>("copyNativeLibs") {
  from(file(nativeSourceDir).resolve("libdata_pipline_java.dylib"))
  into(file(nativeTargetDir))
  dependsOn(compileNativeLibrary)
}

tasks.withType<JavaCompile>().configureEach {
  mustRunAfter(copyNativeLibs)
}

tasks.withType<Jar>().named("jar").configure {
  dependsOn(copyNativeLibs)
}

tasks.withType<Test>().configureEach {
  useJUnitPlatform()
  systemProperty(
    "java.library.path",
    nativeTargetDir,
  )
  testLogging.showStandardStreams = true
  dependsOn(copyNativeLibs)
}
