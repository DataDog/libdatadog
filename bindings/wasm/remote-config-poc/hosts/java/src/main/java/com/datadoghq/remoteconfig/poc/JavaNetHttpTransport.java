// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

package com.datadoghq.remoteconfig.poc;

import java.io.IOException;
import java.net.URI;
import java.net.http.HttpClient;
import java.net.http.HttpRequest;
import java.net.http.HttpResponse;
import java.nio.ByteBuffer;
import java.util.ArrayList;
import java.util.List;
import java.util.concurrent.CompletableFuture;
import java.util.concurrent.CompletionStage;
import java.util.concurrent.Flow;

/**
 * {@link HttpTransport} backed by {@link java.net.http.HttpClient}, the default transport of the
 * PoC host. Tracer integrations can substitute their own transport (for example OkHttp with proxy
 * and Unix-socket support) without touching the poll loop.
 */
public final class JavaNetHttpTransport implements HttpTransport {

  private static final int MAX_BODY = 16 * 1024 * 1024;

  private final HttpClient http;

  /** Creates a transport with its own {@link HttpClient}. */
  public JavaNetHttpTransport() {
    this(HttpClient.newHttpClient());
  }

  /** Creates a transport over the given {@link HttpClient}. */
  public JavaNetHttpTransport(HttpClient http) {
    this.http = http;
  }

  @Override
  public CompletableFuture<HttpReply> send(
      String method, String url, List<String[]> headers, byte[] body) {
    // HttpRequest.newBuilder/Builder.header throw synchronously on malformed URLs and on
    // JDK-restricted header names (content-length, connection, host, ...); the transport
    // contract turns those into per-request failures instead of killing the whole poll.
    CompletableFuture<HttpResponse<byte[]>> sent;
    try {
      HttpRequest.Builder request = HttpRequest.newBuilder(URI.create(url));
      request.method(method, HttpRequest.BodyPublishers.ofByteArray(body));
      for (String[] header : headers) {
        request.header(header[0], header[1]);
      }
      sent = http.sendAsync(request.build(), responseInfo -> new BoundedBodySubscriber());
    } catch (RuntimeException error) {
      return CompletableFuture.completedFuture(HttpReply.failure(String.valueOf(error)));
    }
    return sent.handle((response, error) -> {
      if (error != null) {
        Throwable cause = error.getCause() != null ? error.getCause() : error;
        // getMessage() is null for many throwables (e.g. bare NPEs); toString() always
        // names the failure so the guest never sees a dropped "error" field.
        return HttpReply.failure(String.valueOf(cause));
      }
      List<String[]> replyHeaders = new ArrayList<>();
      for (var entry : response.headers().map().entrySet()) {
        for (String value : entry.getValue()) {
          replyHeaders.add(new String[] {entry.getKey(), value});
        }
      }
      return HttpReply.of(response.statusCode(), replyHeaders, response.body());
    });
  }

  /**
   * Body subscriber that fails the reply as soon as the stream passes {@code MAX_BODY} bytes, so an
   * oversized response is dropped in flight instead of being buffered in host memory first.
   */
  private static final class BoundedBodySubscriber implements HttpResponse.BodySubscriber<byte[]> {

    private final HttpResponse.BodySubscriber<byte[]> delegate =
        HttpResponse.BodySubscribers.ofByteArray();
    private final CompletableFuture<Void> limit = new CompletableFuture<>();
    private Flow.Subscription subscription;
    private long received;

    @Override
    public CompletionStage<byte[]> getBody() {
      return delegate.getBody().thenCombine(limit, (bytes, ignored) -> bytes);
    }

    @Override
    public void onSubscribe(Flow.Subscription subscription) {
      this.subscription = subscription;
      delegate.onSubscribe(subscription);
    }

    @Override
    public void onNext(List<ByteBuffer> item) {
      for (ByteBuffer buffer : item) {
        received += buffer.remaining();
      }
      if (received > MAX_BODY) {
        limit.completeExceptionally(new IOException("Response exceeds 16 MiB"));
        subscription.cancel();
      } else {
        delegate.onNext(item);
      }
    }

    @Override
    public void onError(Throwable throwable) {
      limit.completeExceptionally(throwable);
      delegate.onError(throwable);
    }

    @Override
    public void onComplete() {
      limit.complete(null);
      delegate.onComplete();
    }
  }
}
