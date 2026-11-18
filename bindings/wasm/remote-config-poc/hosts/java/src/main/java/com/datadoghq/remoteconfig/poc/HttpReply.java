// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

package com.datadoghq.remoteconfig.poc;

import java.util.List;

/**
 * One HTTP reply crossing the host bridge, with the body as raw bytes.
 *
 * <p>Exactly one of the fields carries the outcome: {@code error} for a transport failure, {@code
 * status == 0} for an empty reply (used to resolve timer completions), otherwise a real response.
 * Host code never touches the guest from completion threads; the reply is consumed at the poll loop
 * head only.
 */
public final class HttpReply {

  /** HTTP status code, 0 when this reply carries no response. */
  public final int status;

  /** Header pairs, never null when status is set; entries are two-element arrays. */
  public final List<String[]> headers;

  /** Raw response body, never null when status is set. */
  public final byte[] body;

  /** Transport failure message, null unless the request failed. */
  public final String error;

  private HttpReply(int status, List<String[]> headers, byte[] body, String error) {
    this.status = status;
    this.headers = headers;
    this.body = body;
    this.error = error;
  }

  /** Creates a successful reply. */
  public static HttpReply of(int status, List<String[]> headers, byte[] body) {
    return new HttpReply(
        status,
        java.util.Objects.requireNonNull(headers, "headers"),
        java.util.Objects.requireNonNull(body, "body"),
        null);
  }

  /** Creates the empty reply used to resolve timer completions. */
  public static HttpReply empty() {
    return new HttpReply(0, null, null, null);
  }

  /** Creates a failure reply carrying the given message. */
  public static HttpReply failure(String message) {
    return new HttpReply(0, null, null, message);
  }
}
