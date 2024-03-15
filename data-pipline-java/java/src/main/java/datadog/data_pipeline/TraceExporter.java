// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

package datadog.data_pipeline;

import java.io.IOException;
import java.nio.ByteBuffer;

public class TraceExporter implements AutoCloseable {

  private static boolean initialized = false;

  public static void initialize() throws IOException {
    if (!initialized) {
      NativeLibLoader.load();
      initialized = true;
    }
  }

  private final long handle;
  private volatile boolean closed = false;

  private TraceExporter(
      String host,
      int port,
      String tracerVersion,
      String language,
      String languageVersion,
      String interpreter,
      boolean proxy) {
    this.handle = create(host, port, tracerVersion, language, languageVersion, interpreter, proxy);
  }

  @Override
  public void close() throws Exception {
    synchronized (this) {
      if (!closed) {
        destroy(handle);
        closed = true;
      }
    }
  }

  public String sendTraces(ByteBuffer traces, int len, int count) {
    synchronized (this) {
      if (closed) {
        throw new IllegalStateException("Exporter is closed");
      }
      return sendTraces(handle, traces, len, count);
    }
  }

  // Only accessible for testing
  long getHandle() {
    return handle;
  }

  private static native long create(
      String host,
      int port,
      String tracerVersion,
      String language,
      String languageVersion,
      String interpreter,
      boolean proxy);

  private static native void destroy(long handle);

  private static native String sendTraces(long handle, ByteBuffer traces, int len, int count);

  public static Builder builder() {
    return new Builder();
  }

  public static class Builder {
    private String host;
    private int port = 8126;
    private String tracerVersion;
    private String language;
    private String languageVersion;
    private String interpreter = "";
    private boolean proxy = true;

    public Builder withHost(String host) {
      this.host = host;
      return this;
    }

    public Builder withPort(int port) {
      this.port = port;
      return this;
    }

    public Builder withTracerVersion(String tracerVersion) {
      this.tracerVersion = tracerVersion;
      return this;
    }

    public Builder withLanguage(String language) {
      this.language = language;
      return this;
    }

    public Builder withLanguageVersion(String languageVersion) {
      this.languageVersion = languageVersion;
      return this;
    }

    public Builder withInterpreter(String interpreter) {
      this.interpreter = interpreter;
      return this;
    }

    public Builder withProxy(boolean proxy) {
      this.proxy = proxy;
      return this;
    }

    public TraceExporter build() {
      return new TraceExporter(
          host, port, tracerVersion, language, languageVersion, interpreter, proxy);
    }
  }
}
