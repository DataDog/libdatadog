// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

package datadog.data_pipeline;

import static org.assertj.core.api.Assertions.assertThat;

import java.io.IOException;
import java.math.BigInteger;
import java.nio.ByteBuffer;
import org.junit.jupiter.api.BeforeAll;
import org.junit.jupiter.api.Test;
import org.msgpack.core.MessagePack;
import org.msgpack.core.MessagePacker;
import org.msgpack.core.buffer.ArrayBufferOutput;

class TraceExporterTest {

  @BeforeAll
  static void setup() throws IOException {
    TraceExporter.initialize();
  }

  @Test
  void testExporter() throws Exception {
    TraceExporter exporter =
        TraceExporter.builder()
            .withHost("localhost")
            .withPort(8126)
            .withTracerVersion("0.4711.0")
            .withLanguage("java")
            .withLanguageVersion("1.8")
            .withInterpreter("java")
            .build();
    assertThat(exporter.getHandle()).isNotEqualTo(0);
    ByteBuffer traces = ByteBuffer.allocateDirect(1024);
    serializeTrace(
        traces, 1729, 4711, 0, 1234567890, 1000000, "service1", "operation1", "resource1", "web");
    traces.flip();
    String res1 = exporter.sendTraces(traces, traces.limit(), 1);
    assertThat(res1).isNotEmpty();
    traces.clear();
    serializeTrace(
        traces, 2917, 1147, 0, 1234567891, 1000000, "service2", "operation2", "resource2", "web");
    traces.flip();
    String res2 = exporter.sendTraces(traces, traces.limit(), 1);
    assertThat(res2).isNotEmpty();
    exporter.close();
  }

  private static void serializeTrace(
      ByteBuffer traces,
      long traceId,
      long spanId,
      long parentId,
      long startTime,
      long duration,
      String serviceName,
      String operationName,
      String resourceName,
      String type)
      throws IOException {
    ArrayBufferOutput output = new ArrayBufferOutput();
    MessagePacker packer = MessagePack.newDefaultPacker(output);
    // We send 1 trace
    packer.packArrayHeader(1);
    // That has 1 span
    packer.packArrayHeader(1);
    // And that span has 12 fields
    packer.packMapHeader(12);
    /* 1  */ writeKVString("service", serviceName, packer);
    /* 2  */ writeKVString("name", operationName, packer);
    /* 3  */ writeKVString("resource", resourceName, packer);
    /* 4  */ writeKVULong("trace_id", traceId, packer);
    /* 5  */ writeKVULong("span_id", spanId, packer);
    /* 6  */ writeKVULong("parent_id", parentId, packer);
    /* 7  */ writeKVLong("start", startTime, packer);
    /* 8  */ writeKVLong("duration", duration, packer);
    /* 9  */ writeKVString("type", type, packer);
    /* 10 */ writeKVLong("error", 0, packer);
    /* 11 */ writeKVEmptyMap("metrics", packer);
    /* 12 */ writeKVEmptyMap("meta", packer);
    packer.flush();
    traces.put(output.toByteArray());
  }

  private static void writeKVString(String key, String value, MessagePacker destination)
      throws IOException {
    destination.packString(key);
    if (value == null) {
      destination.packNil();
    } else {
      destination.packString(value);
    }
  }

  private static void writeKVLong(String key, long value, MessagePacker destination)
      throws IOException {
    destination.packString(key);
    destination.packLong(value);
  }

  private static void writeKVULong(String key, long value, MessagePacker destination)
      throws IOException {
    destination.packString(key);
    destination.packBigInteger(BigInteger.valueOf(value));
  }

  private static void writeKVEmptyMap(String key, MessagePacker destination) throws IOException {
    destination.packString(key);
    destination.packMapHeader(0);
  }
}
