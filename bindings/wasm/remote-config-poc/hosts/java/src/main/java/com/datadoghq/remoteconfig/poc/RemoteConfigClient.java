// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

package com.datadoghq.remoteconfig.poc;

import java.util.LinkedHashMap;
import java.util.List;
import java.util.Map;
import java.util.concurrent.BlockingQueue;
import java.util.concurrent.CompletableFuture;
import java.util.concurrent.LinkedBlockingQueue;
import java.util.concurrent.ScheduledFuture;
import java.util.concurrent.ScheduledThreadPoolExecutor;
import java.util.concurrent.TimeUnit;
import org.json.JSONArray;
import org.json.JSONObject;

/**
 * A managed client for the shared remote-config WASM reactor, extracted from the CLI demo so tracer
 * hosts can embed the poll loop as a library.
 *
 * <p>The client drives one guest client handle: {@link #poll} runs the reactor until an outcome is
 * ready, {@link #ack} reports acknowledgement states back, and {@link #close} disposes the guest
 * handle. Guest calls never overlap: transport completions are taken from the wakeups queue at the
 * loop head and completed one at a time, so no guest call is ever re-entered. The HTTP body bytes
 * cross the bridge through raw guest buffers, never base64.
 *
 * <p>Ownership mirrors the other hosts: operation ids are never reused, late or duplicate
 * completions are rejected by the guest, and a transport failure cancels the poll so no future
 * retains client state.
 *
 * <p>One client is driven by one thread at a time: {@link #poll}, {@link #ack}, and {@link #close}
 * must not run concurrently with each other, or the guest calls they issue would overlap.
 */
public final class RemoteConfigClient {

  /** Wakeup token callers place on the queue to cancel an in-flight poll. */
  public static final Object CANCEL_TOKEN = new Object();

  private final RcAbi abi;
  private final int client;

  /**
   * Daemon timer threads idle out after this long without a poll, keeping a quiescent client
   * thread-free.
   */
  private static final long TIMER_KEEPALIVE_SECONDS = 60;

  /**
   * Runs the guest's timer events for the life of this client, so polls do not create and tear down
   * an executor (and possibly a thread) each call. Timer callbacks only complete futures, so the
   * guest stays serialized at the loop head regardless of which thread fires them.
   */
  private final ScheduledThreadPoolExecutor scheduler = newTimerScheduler();

  private static ScheduledThreadPoolExecutor newTimerScheduler() {
    ScheduledThreadPoolExecutor executor =
        new ScheduledThreadPoolExecutor(
            1,
            runnable -> {
              Thread thread = new Thread(runnable, "remote-config-timers");
              thread.setDaemon(true);
              return thread;
            });
    executor.setKeepAliveTime(TIMER_KEEPALIVE_SECONDS, TimeUnit.SECONDS);
    executor.allowCoreThreadTimeOut(true);
    return executor;
  }

  private RemoteConfigClient(RcAbi abi, int client) {
    this.abi = abi;
    this.client = client;
  }

  /**
   * Creates a guest client from options JSON matching the crate's {@code ConfigOptions} shape
   * (clientId, service, env, url, products, tags, and optionally apiKey/hostname/configRoot/
   * directorRoot for agentless mode). The {@code language} field is set by the caller.
   */
  public static RemoteConfigClient create(RcAbi abi, JSONObject options) {
    int buffer = abi.write(options);
    try {
      return new RemoteConfigClient(abi, abi.check(abi.create(buffer)));
    } finally {
      abi.check(abi.free(buffer));
    }
  }

  /**
   * Runs one poll with a fresh wakeups queue, resolving content buffers into base64. See {@link
   * #poll(HttpTransport, BlockingQueue)}.
   */
  public JSONObject poll(HttpTransport transport) {
    return resolveOutcome(abi, pollRaw(transport, new LinkedBlockingQueue<>()));
  }

  /**
   * Runs one poll to completion: starts the reactor with {@code rc_poll}, then drains host events,
   * dispatches HTTP and timers through the transport, and completes one operation at a time from
   * the loop head. Returns the outcome JSON (changes or error) with content buffers already
   * resolved — see {@link #resolveOutcome}. Callers offering external cancellation pass their own
   * queue and push {@link #CANCEL_TOKEN} on it.
   */
  public JSONObject poll(HttpTransport transport, BlockingQueue<Object> wakeups) {
    return resolveOutcome(abi, pollRaw(transport, wakeups));
  }

  /**
   * Runs one poll with a fresh wakeups queue, leaving content buffers unresolved. See {@link
   * #pollRaw(HttpTransport, BlockingQueue)}.
   */
  public JSONObject pollRaw(HttpTransport transport) {
    return pollRaw(transport, new LinkedBlockingQueue<>());
  }

  /**
   * Runs one poll to completion, like {@link #poll(HttpTransport, BlockingQueue)}, but leaves the
   * outcome raw: each change's {@code contents} field holds a guest buffer id the host resolves
   * itself through {@code RcAbi.read}. Hosts delivering config bytes to listeners use this variant
   * to avoid a base64 round trip; every content buffer must be read exactly once. Callers offering
   * external cancellation pass their own queue and push {@link #CANCEL_TOKEN} on it.
   */
  public JSONObject pollRaw(HttpTransport transport, BlockingQueue<Object> wakeups) {
    return loop(transport, wakeups);
  }

  /** The poll loop shared by {@link #poll} and {@link #pollRaw}; returns the raw outcome JSON. */
  private JSONObject loop(HttpTransport transport, BlockingQueue<Object> wakeups) {
    Map<Integer, Pending> pending = new LinkedHashMap<>();
    try {
      abi.check(abi.poll(client));
      while (true) {
        for (Object entry : abi.jsonArray(abi.check(abi.events(client)))) {
          JSONObject operation = (JSONObject) entry;
          int id = operation.getInt("id");
          String kind = operation.getString("kind");
          if (kind.equals("cancel")) {
            Pending resource = pending.remove(id);
            if (resource != null) resource.canceller.run();
          } else {
            pending.put(id, dispatch(transport, operation, scheduler, wakeups));
          }
        }
        int result = abi.check(abi.result(client));
        if (result != 0) return abi.json(result);
        if (pending.isEmpty()) throw new IllegalStateException("Pending guest has no host work");
        Object wake;
        try {
          wake = wakeups.take();
        } catch (InterruptedException interrupted) {
          Thread.currentThread().interrupt();
          throw new IllegalStateException("Poll dispatcher interrupted", interrupted);
        }
        if (wake == CANCEL_TOKEN) {
          abi.check(abi.cancel(client));
          return abi.json(abi.result(client));
        }
        if (!(wake instanceof Integer)) {
          // The queue is caller-owned; reject anything that is neither an operation id nor the
          // cancel token instead of throwing a bare ClassCastException on the cast below.
          throw new IllegalStateException("Unknown wakeup object: " + wake);
        }
        Pending completed = pending.remove((Integer) wake);
        if (completed == null) continue; // Stale wake for an operation the guest cancelled.
        HttpReply replyPayload = completed.completion.join();
        int operationId = completed.id;
        input(reply(replyPayload), buffer -> abi.complete(client, operationId, buffer));
      }
    } catch (RuntimeException error) {
      // A transport failure must not leave a guest future retaining client state.
      abi.cancel(client);
      int result = abi.result(client);
      if (result > 0) resolveOutcome(abi, abi.json(result));
      throw error;
    } finally {
      // Cancel still-pending timers so no stale completion races the next poll's queue; the
      // shared scheduler itself stays usable for the client's lifetime.
      for (Pending resource : pending.values()) resource.canceller.run();
    }
  }

  /** Reports one acknowledgement state (path, state, optional error) back to the guest. */
  public void ack(JSONObject acknowledgement) {
    input(acknowledgement, buffer -> abi.ack(client, buffer));
  }

  /** Disposes the guest client; the handle, its buffers and its timers return to the host. */
  public void close() {
    try {
      abi.check(abi.close(client));
    } finally {
      scheduler.shutdownNow();
    }
  }

  /** The guest client handle, for hosts driving scenario-level ABI calls directly. */
  public int handle() {
    return client;
  }

  /**
   * Resolves raw content buffer ids in an outcome into base64 fields ({@code contentsBase64}); each
   * buffer is copied out and freed, so ownership returns to the guest. Hosts that want raw bytes
   * can read the {@code contents} ids themselves before calling this method.
   */
  public static JSONObject resolveOutcome(RcAbi abi, JSONObject outcome) {
    JSONArray changes = outcome.optJSONArray("changes");
    if (changes == null) return outcome;
    for (Object entry : changes) {
      JSONObject change = (JSONObject) entry;
      if (!change.has("contents")) continue;
      Object contents = change.remove("contents");
      if (contents.equals(JSONObject.NULL)) {
        change.put("contentsBase64", JSONObject.NULL);
      } else {
        byte[] bytes = abi.read((int) contents);
        change.put("contentsBase64", java.util.Base64.getEncoder().encodeToString(bytes));
      }
    }
    return outcome;
  }

  /** One pending host operation: either an in-flight HTTP request or a timer. */
  private static final class Pending {
    final int id;
    final CompletableFuture<HttpReply> completion;
    final Runnable canceller;

    Pending(int id, CompletableFuture<HttpReply> completion, Runnable canceller) {
      this.id = id;
      this.completion = completion;
      this.canceller = canceller;
    }
  }

  /** A guest call taking one input buffer. */
  @FunctionalInterface
  private interface GuestCall {
    int apply(int buffer);
  }

  /** Builds the wire reply for one completed host operation. */
  private JSONObject reply(HttpReply payload) {
    if (payload.error != null) {
      return new JSONObject().put("error", payload.error);
    }
    if (payload.status == 0) {
      return new JSONObject();
    }
    // Bodies attach to guest buffers at the loop head only; guest calls never run on
    // HTTP or timer completion threads.
    JSONArray headers = new JSONArray();
    for (String[] header : payload.headers) {
      headers.put(new JSONArray().put(header[0]).put(header[1]));
    }
    return new JSONObject()
        .put("status", payload.status)
        .put("headers", headers)
        .put("body", abi.writeBytes(payload.body));
  }

  /** Starts the host work described by one event and returns its handle. */
  private Pending dispatch(
      HttpTransport transport,
      JSONObject operation,
      ScheduledThreadPoolExecutor scheduler,
      BlockingQueue<Object> wakeups) {
    int id = operation.getInt("id");
    String kind = operation.getString("kind");
    CompletableFuture<HttpReply> completion = new CompletableFuture<>();
    if (kind.equals("sleep")) {
      long milliseconds = (long) operation.getDouble("milliseconds");
      // The timer must enqueue a wakeup itself: the loop blocks on the queue, so completing
      // the future alone would never unblock it (mirrors the Node reference host).
      ScheduledFuture<?> timer =
          scheduler.schedule(
              () -> {
                completion.complete(HttpReply.empty());
                wakeups.add(id);
              },
              milliseconds,
              TimeUnit.MILLISECONDS);
      return new Pending(id, completion, () -> timer.cancel(false));
    }
    if (!kind.equals("http")) throw new IllegalStateException("Unknown host operation: " + kind);

    List<String[]> headers = new java.util.ArrayList<>();
    for (Object entry : operation.getJSONArray("headers")) {
      JSONArray header = (JSONArray) entry;
      headers.add(new String[] {header.getString(0), header.getString(1)});
    }
    // Reading the request body frees the guest buffer it arrived in.
    byte[] body = abi.read(operation.getInt("body"));
    CompletableFuture<HttpReply> sent =
        transport.send(operation.getString("method"), operation.getString("url"), headers, body);
    sent.whenComplete((reply, error) -> wakeups.add(id));
    return new Pending(id, sent, () -> sent.cancel(true));
  }

  /** Runs one guest call that takes a JSON input buffer, freeing the buffer afterwards. */
  private int input(Object json, GuestCall operation) {
    int buffer = abi.write(json);
    try {
      return abi.check(operation.apply(buffer));
    } finally {
      abi.check(abi.free(buffer));
    }
  }
}
