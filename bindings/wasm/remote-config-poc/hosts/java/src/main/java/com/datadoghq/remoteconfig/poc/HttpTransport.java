// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

package com.datadoghq.remoteconfig.poc;

import java.util.List;
import java.util.concurrent.CompletableFuture;

/**
 * Host-side HTTP transport for the shared remote-config reactor.
 *
 * <p>The guest emits HTTP requests as events; the transport executes them and completes the future
 * with a reply. Implementations run the request asynchronously, must not touch the guest from the
 * request thread, and must not throw synchronously — failures are reported through {@link
 * HttpReply#failure(String)}. The future is joined at the poll loop head, where the reply body is
 * attached to a guest buffer; no guest call ever runs on a transport thread.
 */
public interface HttpTransport {

  /**
   * Sends one request.
   *
   * @param method HTTP method name
   * @param url absolute request URL
   * @param headers header pairs, each a two-element array
   * @param body raw request body, never null
   * @return a future completing with the reply
   */
  CompletableFuture<HttpReply> send(String method, String url, List<String[]> headers, byte[] body);
}
