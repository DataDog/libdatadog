// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

package com.datadoghq.remoteconfig.poc;

import java.io.BufferedReader;
import java.io.InputStreamReader;
import java.net.URI;
import java.nio.charset.StandardCharsets;
import java.nio.file.Files;
import java.nio.file.Path;
import java.util.Base64;
import java.util.concurrent.CompletableFuture;
import java.util.concurrent.CompletionException;
import java.util.concurrent.LinkedBlockingQueue;
import org.json.JSONArray;
import org.json.JSONObject;

/**
 * CLI demo host for the shared remote-config WASM reactor, kept for the cross-host conformance
 * suite. The embeddable pieces live in the host library: {@link RemoteConfigHost} loads the
 * reactor, {@link RemoteConfigClient} drives the poll loop over an {@link HttpTransport}, and
 * {@link JavaNetHttpTransport} provides the default java.net.http transport.
 */
public final class Main {

  private Main() {}

  /** A guest call taking one input buffer. */
  @FunctionalInterface
  private interface GuestCall {
    int apply(int buffer);
  }

  public static void main(String[] args) throws Exception {
    if (args.length < 3) {
      throw new IllegalArgumentException(
          "Usage: Main <wasm-path> <url> <scenario.json> [--cancel-stdin]");
    }
    // The wasm path is still accepted as the first argument to keep the host CLI uniform with
    // the Node and .NET hosts, but the guest executed here is the one AOT-compiled at build
    // time and loaded by RemoteConfigHost.
    try (RemoteConfigHost host = RemoteConfigHost.load()) {
      RcAbi abi = host.abi();
      HttpTransport transport = new JavaNetHttpTransport();
      JSONObject scenario =
          new JSONObject(Files.readString(Path.of(args[2]), StandardCharsets.UTF_8));
      JSONObject options = scenario.getJSONObject("options");
      options.put("url", args[1]);
      options.put("language", "java");
      RemoteConfigClient client = RemoteConfigClient.create(abi, options);
      try {
        if (args.length > 3 && args[3].equals("--cancel-stdin")) {
          LinkedBlockingQueue<Object> wakeups = new LinkedBlockingQueue<>();
          CompletableFuture<JSONObject> pending =
              CompletableFuture.supplyAsync(() -> client.poll(transport, wakeups));
          BufferedReader stdin =
              new BufferedReader(new InputStreamReader(System.in, StandardCharsets.UTF_8));
          if (!"cancel".equals(stdin.readLine())) {
            // Drain the abandoned poll first: the close() in the finally below must
            // not touch the store while the poll thread is still inside the guest.
            wakeups.add(RemoteConfigClient.CANCEL_TOKEN);
            try {
              pending.join();
            } catch (CompletionException error) {
              // The cancelled poll failed on its own; the driver error below is
              // the one to report.
            }
            throw new IllegalStateException("Expected cancel command");
          }
          wakeups.add(RemoteConfigClient.CANCEL_TOKEN);
          System.out.println(pending.join());
          // After a cancelled poll the client can poll again.
          abi.check(abi.poll(client.handle()));
          abi.check(abi.cancel(client.handle()));
          RemoteConfigClient.resolveOutcome(abi, abi.json(abi.result(client.handle())));
          System.out.println("{\"restarted\":true}");
        } else {
          for (Object entry : scenario.getJSONArray("actions")) {
            JSONObject action = (JSONObject) entry;
            String kind = action.getString("kind");
            JSONObject outcome;
            if (kind.equals("ack")) {
              client.ack(action.getJSONObject("value"));
              outcome = new JSONObject().put("acknowledged", true);
            } else if (kind.equals("cancel") || kind.equals("timeout")) {
              outcome = cancelOrTimeout(abi, client, action, kind);
            } else if (kind.equals("fixture")) {
              outcome = fixture(abi, client, action);
            } else if (kind.equals("poll")) {
              outcome = client.poll(transport);
            } else {
              throw new IllegalStateException("Unknown scenario action: " + kind);
            }
            System.out.println(outcome);
          }
        }
      } finally {
        client.close();
      }
    }
    System.exit(0);
  }

  /** Drives the {@code cancel} and {@code timeout} scenario actions without a real network. */
  private static JSONObject cancelOrTimeout(
      RcAbi abi, RemoteConfigClient client, JSONObject action, String kind) {
    int handle = client.handle();
    abi.check(abi.poll(handle));
    String wanted = kind.equals("timeout") ? "sleep" : "http";
    JSONObject operation = find(abi.jsonArray(abi.check(abi.events(handle))), wanted);
    if (operation == null) throw new IllegalStateException("Expected pending operation");
    int operationId = operation.getInt("id");
    if (kind.equals("cancel")) {
      abi.check(abi.cancel(handle));
    } else {
      input(abi, handle, new JSONObject(), buffer -> abi.complete(handle, operationId, buffer));
    }
    JSONObject outcome = RemoteConfigClient.resolveOutcome(abi, abi.json(abi.result(handle)));
    int reply =
        abi.write(
            new JSONObject()
                .put("status", 200)
                .put("body", abi.writeBytes(Base64.getDecoder().decode("e30="))));
    try {
      if (abi.complete(handle, operationId, reply) != -1) {
        throw new IllegalStateException("Late completion accepted");
      }
    } finally {
      abi.check(abi.free(reply));
    }
    if (find(abi.jsonArray(abi.check(abi.events(handle))), "cancel") == null) {
      throw new IllegalStateException("No host cancellation");
    }
    return outcome;
  }

  /** Serves the agentless fixture requests directly through the ABI, never through HTTP. */
  private static JSONObject fixture(RcAbi abi, RemoteConfigClient client, JSONObject action) {
    int handle = client.handle();
    abi.check(abi.poll(handle));
    JSONArray requests = new JSONArray();
    JSONObject outcome = null;
    for (int step = 0; step < 32 && outcome == null; step++) {
      JSONArray events = abi.jsonArray(abi.check(abi.events(handle)));
      boolean hasHttp = find(events, "http") != null;
      for (Object entry : events) {
        JSONObject operation = (JSONObject) entry;
        String kind = operation.getString("kind");
        if (kind.equals("sleep") && !hasHttp) {
          int id = operation.getInt("id");
          input(abi, handle, new JSONObject(), buffer -> abi.complete(handle, id, buffer));
        }
        if (!kind.equals("http")) continue;
        JSONObject recorded = new JSONObject(operation, JSONObject.getNames(operation));
        recorded.put(
            "body", Base64.getEncoder().encodeToString(abi.read(operation.getInt("body"))));
        requests.put(recorded);
        int id = operation.getInt("id");
        String path = URI.create(operation.getString("url")).getPath();
        String body = path.equals("/api/v0.1/org") ? "" : action.getString("body");
        input(
            abi,
            handle,
            new JSONObject()
                .put("status", 200)
                .put("body", abi.writeBytes(Base64.getDecoder().decode(body))),
            buffer -> abi.complete(handle, id, buffer));
      }
      int result = abi.check(abi.result(handle));
      if (result > 0) {
        outcome =
            new JSONObject()
                .put("outcome", RemoteConfigClient.resolveOutcome(abi, abi.json(result)))
                .put("requests", requests);
      }
    }
    if (outcome == null) throw new IllegalStateException("Fixture did not complete");
    return outcome;
  }

  private static JSONObject find(JSONArray events, String kind) {
    for (Object entry : events) {
      JSONObject operation = (JSONObject) entry;
      if (operation.getString("kind").equals(kind)) return operation;
    }
    return null;
  }

  private static int input(RcAbi abi, int handle, Object json, GuestCall operation) {
    int buffer = abi.write(json);
    try {
      return abi.check(operation.apply(buffer));
    } finally {
      abi.check(abi.free(buffer));
    }
  }
}
