// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

package datadog.data_pipeline;

import java.io.File;
import java.io.FileOutputStream;
import java.io.IOException;
import java.io.InputStream;
import java.io.OutputStream;
import java.nio.file.Files;
import java.nio.file.Path;

public class NativeLibLoader {

  public static void load() throws IOException {
    File jniLibrary = extractLibrary();
    System.load(jniLibrary.getAbsolutePath());
  }

  private static File extractLibrary() throws IOException {
    ClassLoader cl = NativeLibLoader.class.getClassLoader();
    // TODO we should use the OS type to load the correct library
    String libName = "libdata_pipline_java.dylib";
    Path tempDir = Files.createTempDirectory("data_pipeline");

    String clPath = "native/" + libName;
    InputStream input = cl.getResourceAsStream(clPath);
    if (input == null) {
      throw new IOException("Not found: " + clPath);
    }
    File jniLibrary = new File(tempDir.toFile(), new File(clPath).getName());
    try {
      copyToFile(input, jniLibrary);
    } finally {
      input.close();
    }
    jniLibrary.deleteOnExit();
    return jniLibrary;
  }

  private static void copyToFile(InputStream input, File dest) throws IOException {
    OutputStream os = null;
    try {
      os = new FileOutputStream(dest);
      copy(input, os);
      os.flush();
    } finally {
      if (os != null) {
        os.close();
      }
    }
  }

  private static long copy(InputStream from, OutputStream to) throws IOException {
    byte[] buf = new byte[8192];
    long total = 0;
    while (true) {
      int r = from.read(buf);
      if (r == -1) {
        break;
      }
      to.write(buf, 0, r);
      total += r;
    }
    return total;
  }
}
