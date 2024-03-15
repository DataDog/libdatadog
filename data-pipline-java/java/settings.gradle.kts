plugins {
  // Apply the foojay-resolver plugin to allow automatic download of JDKs
  id("org.gradle.toolchains.foojay-resolver-convention") version "0.4.0"
  id("com.palantir.git-version") version "3.0.0" apply false
  id("com.diffplug.spotless") version "6.25.0" apply false
}

rootProject.name = "dd-data-pipline"
