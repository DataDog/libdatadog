// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0


#ifndef DDOG_HTTP_CLIENT_H
#define DDOG_HTTP_CLIENT_H

#include <stdarg.h>
#include <stdbool.h>
#include <stdint.h>
#include <stdlib.h>
#include "common.h"
typedef struct ddog_HttpClient ddog_HttpClient;
typedef struct ddog_HttpClientBuilder ddog_HttpClientBuilder;
typedef struct ddog_HttpRequest ddog_HttpRequest;
typedef struct ddog_HttpResponse ddog_HttpResponse;
typedef struct ddog_SharedRuntime ddog_SharedRuntime;


/**
 * Discriminant for [`DdogHttpClientError`].
 *
 * Mirrors [`libdd_http_client::HttpClientError`] one-for-one.
 */
typedef enum ddog_HttpClientErrorCode {
  /**
   * The TCP/socket connection to the server could not be established.
   */
  DDOG_HTTP_CLIENT_ERROR_CODE_CONNECTION_FAILED,
  /**
   * The request exceeded the configured timeout duration.
   */
  DDOG_HTTP_CLIENT_ERROR_CODE_TIMED_OUT,
  /**
   * The server returned an HTTP error status code (4xx / 5xx).
   *
   * `status` and `body` on the carrier struct are populated.
   */
  DDOG_HTTP_CLIENT_ERROR_CODE_REQUEST_FAILED,
  /**
   * The client / request configuration was invalid.
   */
  DDOG_HTTP_CLIENT_ERROR_CODE_INVALID_CONFIG,
  /**
   * An I/O error occurred during the request.
   */
  DDOG_HTTP_CLIENT_ERROR_CODE_IO_ERROR,
  /**
   * A null pointer or other invalid argument was passed across the FFI.
   */
  DDOG_HTTP_CLIENT_ERROR_CODE_INVALID_ARGUMENT,
} ddog_HttpClientErrorCode;

/**
 * FFI mirror of [`libdd_http_client::HttpMethod`].
 */
typedef enum ddog_HttpMethod {
  /**
   * `GET`
   */
  DDOG_HTTP_METHOD_GET,
  /**
   * `POST`
   */
  DDOG_HTTP_METHOD_POST,
  /**
   * `PUT`
   */
  DDOG_HTTP_METHOD_PUT,
  /**
   * `DELETE`
   */
  DDOG_HTTP_METHOD_DELETE,
  /**
   * `HEAD`
   */
  DDOG_HTTP_METHOD_HEAD,
  /**
   * `PATCH`
   */
  DDOG_HTTP_METHOD_PATCH,
  /**
   * `OPTIONS`
   */
  DDOG_HTTP_METHOD_OPTIONS,
} ddog_HttpMethod;

/**
 * FFI-safe error type for the HTTP client.
 *
 * `msg` is always non-null and owned by the struct; it must be released
 * via [`ddog_http_client_error_free`]. For `RequestFailed`, `status` is
 * the HTTP status code and `body` (if non-null) is a `\0`-terminated
 * UTF-8 string with the response body. For other variants `status` is 0
 * and `body` is null.
 */
typedef struct ddog_HttpClientError {
  /**
   * The error code discriminant.
   */
  enum ddog_HttpClientErrorCode code;
  /**
   * `\0`-terminated UTF-8 message describing the error.
   */
  char *msg;
  /**
   * HTTP status (only populated when `code == RequestFailed`; 0 otherwise).
   */
  uint16_t status;
  /**
   * Response body (only populated when `code == RequestFailed`; null otherwise).
   */
  char *body;
} ddog_HttpClientError;

typedef struct ddog_Slice_U8 {
  /**
   * Should be non-null and suitably aligned for the underlying type. It is
   * allowed but not recommended for the pointer to be null when the len is
   * zero.
   */
  const uint8_t *ptr;
  /**
   * The number of elements (not bytes) that `.ptr` points to. Must be less
   * than or equal to [isize::MAX].
   */
  uintptr_t len;
} ddog_Slice_U8;

/**
 * Use to represent bytes -- does not need to be valid UTF-8.
 */
typedef struct ddog_Slice_U8 ddog_ByteSlice;

/**
 * A single HTTP header (name + value).
 *
 * Both fields must contain valid UTF-8. The slices are borrowed for the
 * duration of the call that consumes them.
 */
typedef struct ddog_HttpHeader {
  /**
   * Header name.
   */
  ddog_CharSlice name;
  /**
   * Header value.
   */
  ddog_CharSlice value;
} ddog_HttpHeader;

typedef struct ddog_Slice_DdogHttpHeader {
  /**
   * Should be non-null and suitably aligned for the underlying type. It is
   * allowed but not recommended for the pointer to be null when the len is
   * zero.
   */
  const struct ddog_HttpHeader *ptr;
  /**
   * The number of elements (not bytes) that `.ptr` points to. Must be less
   * than or equal to [isize::MAX].
   */
  uintptr_t len;
} ddog_Slice_DdogHttpHeader;

/**
 * A slice of [`DdogHttpHeader`] values.
 */
typedef struct ddog_Slice_DdogHttpHeader ddog_HttpHeaderSlice;

#ifdef __cplusplus
extern "C" {
#endif // __cplusplus

/**
 * Install the rustls `ring` crypto provider as the process-wide default.
 *
 * This MUST be called exactly once per process, before
 * [`ddog_http_client_builder_new`], unless the caller installs a
 * different provider themselves (for example the FIPS provider that
 * Task 9b's `ddog_http_client_init_fips` will install).
 *
 * Idempotent: if a provider is already installed, the second call is a
 * no-op and the first install wins. This means it's safe to call this
 * after a FIPS init has already run; the FIPS provider is preserved.
 *
 * # Safety
 * Safe to call from any thread, but ordering relative to first
 * `ddog_http_client_builder_new` matters: install the provider first.
 */
void ddog_http_client_install_default_crypto_provider(void);

/**
 * Allocate a new [`HttpClientBuilder`] with default settings.
 *
 * Writes a `Box<HttpClientBuilder>` into `*out_handle`. The caller owns
 * the handle and must eventually pass it to either
 * [`ddog_http_client_builder_build`] (which consumes it) or
 * [`ddog_http_client_builder_drop`].
 *
 * Before calling this for the first time in a process, the caller must
 * install a rustls crypto provider via either
 * [`ddog_http_client_install_default_crypto_provider`] (non-FIPS) or
 * `ddog_http_client_init_fips` (FIPS, Task 9b).
 *
 * # Safety
 * `out_handle` must be a valid, writable pointer to an
 * uninitialised `*mut ddog_HttpClientBuilder`.
 */
void ddog_http_client_builder_new(ddog_HttpClientBuilder **out_handle);

/**
 * Set the default request timeout in milliseconds.
 *
 * Required before [`ddog_http_client_builder_build`].
 *
 * # Safety
 * `builder` must be `None` or a valid mutable reference to a builder
 * previously produced by [`ddog_http_client_builder_new`].
 */
struct ddog_HttpClientError *ddog_http_client_builder_set_timeout(ddog_HttpClientBuilder *builder,
                                                                  uint64_t timeout_ms);

/**
 * Set the base URL for the client.
 *
 * `url` must be valid UTF-8. Required before
 * [`ddog_http_client_builder_build`].
 *
 * # Safety
 * `builder` must be valid; `url` must point to valid memory for its
 * declared length.
 */
struct ddog_HttpClientError *ddog_http_client_builder_set_base_url(ddog_HttpClientBuilder *builder,
                                                                   ddog_CharSlice url);

/**
 * Route all connections through the given Unix Domain Socket path.
 *
 * The host portion of any URL is ignored once a socket is set.
 *
 * # Safety
 * `builder` must be valid; `path` must point to valid memory for its
 * declared length and contain valid UTF-8.
 */
struct ddog_HttpClientError *ddog_http_client_builder_set_unix_socket(ddog_HttpClientBuilder *builder,
                                                                      ddog_CharSlice path);

/**
 * Route all connections through the given Windows Named Pipe.
 *
 * # Safety
 * `builder` must be valid; `pipe` must point to valid memory for its
 * declared length and contain valid UTF-8.
 */
struct ddog_HttpClientError *ddog_http_client_builder_set_named_pipe(ddog_HttpClientBuilder *builder,
                                                                     ddog_CharSlice pipe);

/**
 * Configure connection pooling. Defaults to `true`.
 *
 * # Safety
 * `builder` must be valid.
 */
struct ddog_HttpClientError *ddog_http_client_builder_set_allow_connection_pooling(ddog_HttpClientBuilder *builder,
                                                                                   bool allow);

/**
 * Consume the builder and produce an [`HttpClient`].
 *
 * On success writes a `Box<HttpClient>` into `*out_handle` and returns
 * `None`. On failure, leaves `*out_handle` unchanged and returns an
 * error. The builder is consumed in either case.
 *
 * # Safety
 * `builder` must have been produced by
 * [`ddog_http_client_builder_new`]. `out_handle` must be a valid,
 * writable pointer to an uninitialised `*mut ddog_HttpClient`.
 */
struct ddog_HttpClientError *ddog_http_client_builder_build(ddog_HttpClientBuilder *builder,
                                                            ddog_HttpClient **out_handle);

/**
 * Drop a builder without building. Use when an error occurs partway
 * through configuration and you wish to abandon the builder.
 *
 * # Safety
 * `builder` must be `None` or a builder produced by
 * [`ddog_http_client_builder_new`] and not yet consumed.
 */
void ddog_http_client_builder_drop(ddog_HttpClientBuilder *builder);

/**
 * Drop an [`HttpClient`].
 *
 * # Safety
 * `client` must be `None` or a client produced by
 * [`ddog_http_client_builder_build`] and not yet dropped.
 */
void ddog_http_client_drop(ddog_HttpClient *client);

/**
 * Free a [`DdogHttpClientError`].
 *
 * After this call the pointer is invalid and must not be used again.
 *
 * # Safety
 * `error` must be `None` or have been produced by this crate's API and
 * not yet freed.
 */
void ddog_http_client_error_free(struct ddog_HttpClientError *error);

/**
 * Construct a new HTTP request.
 *
 * `url` must be valid UTF-8. The new request is written into
 * `*out_handle` and owned by the caller.
 *
 * # Safety
 * `url` must point to valid memory for its declared length.
 * `out_handle` must be a valid, writable pointer to an
 * uninitialised `*mut ddog_HttpRequest`.
 */
struct ddog_HttpClientError *ddog_http_request_new(enum ddog_HttpMethod method,
                                                   ddog_CharSlice url,
                                                   ddog_HttpRequest **out_handle);

/**
 * Update the HTTP method on an existing request, preserving headers,
 * body, and timeout.
 *
 * # Safety
 * `request` must be `None` or a valid mutable reference to a request
 * produced by [`ddog_http_request_new`].
 */
struct ddog_HttpClientError *ddog_http_request_set_method(ddog_HttpRequest *request,
                                                          enum ddog_HttpMethod method);

/**
 * Set the request body to the given bytes (any byte sequence; not
 * required to be UTF-8). Replaces any previously set body. Empty body is
 * represented by a `body` slice of length zero.
 *
 * # Safety
 * `request` must be valid; `body` must point to valid memory for its
 * declared length.
 */
struct ddog_HttpClientError *ddog_http_request_set_body(ddog_HttpRequest *request,
                                                        ddog_ByteSlice body);

/**
 * Append a single header to the request.
 *
 * `name` and `value` must be valid UTF-8. Header names are not
 * case-folded; HTTP semantics are preserved by the underlying backend.
 *
 * # Safety
 * `request` must be valid; `name` and `value` must point to valid memory.
 */
struct ddog_HttpClientError *ddog_http_request_with_header(ddog_HttpRequest *request,
                                                           ddog_CharSlice name,
                                                           ddog_CharSlice value);

/**
 * Append multiple headers in one call.
 *
 * `headers` is a slice of (name, value) pairs. Duplicate header names
 * are preserved in insertion order.
 *
 * # Safety
 * `request` must be valid; `headers` must point to a valid array of
 * `DdogHttpHeader` for its declared length, and each header's `name` /
 * `value` must point to valid UTF-8 memory.
 */
struct ddog_HttpClientError *ddog_http_request_with_headers(ddog_HttpRequest *request,
                                                            ddog_HttpHeaderSlice headers);

/**
 * Set a per-request timeout (overriding the client-level default).
 *
 * # Safety
 * `request` must be valid.
 */
struct ddog_HttpClientError *ddog_http_request_set_timeout(ddog_HttpRequest *request,
                                                           uint64_t timeout_ms);

/**
 * Drop a request that was not consumed by `send_blocking`.
 *
 * # Safety
 * `request` must be `None` or a request produced by
 * [`ddog_http_request_new`] and not yet consumed.
 */
void ddog_http_request_drop(ddog_HttpRequest *request);

/**
 * Read the HTTP status code (e.g. 200, 404, 503).
 *
 * Returns 0 if `response` is null.
 *
 * # Safety
 * `response` must be `None` or a valid reference produced by
 * [`crate::ddog_http_client_send_blocking`] that has not yet been
 * dropped.
 */
uint16_t ddog_http_response_status(const ddog_HttpResponse *response);

/**
 * Borrow the response body as a byte slice.
 *
 * The returned pointer is valid until the response is dropped via
 * [`ddog_http_response_drop`]. If `response` is null the returned
 * pointer is null and `*out_len` is set to 0.
 *
 * # Safety
 * `response` must be valid; `out_len` must be a valid mutable pointer
 * or null.
 */
const uint8_t *ddog_http_response_body(const ddog_HttpResponse *response, uintptr_t *out_len);

/**
 * Borrow the response headers.
 *
 * `out_headers` is allocated by this call and written into via
 * `*out_ptr` / `*out_len`. The header memory (the array of
 * [`DdogHttpHeader`] entries) is owned by the caller and must be
 * released via [`ddog_http_response_headers_free`]. The `name` and
 * `value` slices inside each header point into the response and remain
 * valid for the response's lifetime.
 *
 * # Safety
 * `response` must be valid; `out_ptr` and `out_len` must be valid
 * writable pointers.
 */
struct ddog_HttpClientError *ddog_http_response_headers(const ddog_HttpResponse *response,
                                                        struct ddog_HttpHeader **out_ptr,
                                                        uintptr_t *out_len);

/**
 * Free a header array previously produced by
 * [`ddog_http_response_headers`].
 *
 * # Safety
 * `ptr` must have come from `ddog_http_response_headers` paired with
 * the original `len`. The associated response must still be alive (the
 * `name` / `value` slices borrow from it) — but freeing only frees the
 * outer array, so freeing after dropping the response is also safe.
 */
void ddog_http_response_headers_free(struct ddog_HttpHeader *ptr, uintptr_t len);

/**
 * Drop an HTTP response.
 *
 * # Safety
 * `response` must be `None` or a response produced by
 * [`crate::ddog_http_client_send_blocking`] and not yet dropped.
 */
void ddog_http_response_drop(ddog_HttpResponse *response);

/**
 * Send a request synchronously, blocking until the response is received.
 *
 * On success writes a `Box<HttpResponse>` into `*out_response` and returns
 * `None`. On failure returns the error and leaves `*out_response`
 * unchanged. The `request` is consumed by the call (regardless of success
 * or failure) and must not be reused or freed by the caller.
 *
 * # Parameters
 * - `client`: a client produced by [`crate::ddog_http_client_builder_build`].
 * - `request`: a request produced by [`crate::ddog_http_request_new`].
 * - `shared_runtime`: a shared-runtime handle obtained via
 *   `ddog_shared_runtime_new` (in `libdd-shared-runtime-ffi`). This call
 *   does *not* take ownership of the handle: the caller must still
 *   eventually free it via `ddog_shared_runtime_free`.
 * - `out_response`: where to write the resulting `Box<HttpResponse>`.
 *
 * # Safety
 * - `client` must be a valid reference (non-null) to a client that has
 *   not been dropped.
 * - `request` must be a valid `Box<HttpRequest>` produced by this crate.
 * - `shared_runtime` must be a non-null pointer produced by
 *   `ddog_shared_runtime_new` whose underlying `Arc` is still alive.
 * - `out_response` must be a valid, writable pointer to an
 *   uninitialised `*mut ddog_HttpResponse`.
 */
struct ddog_HttpClientError *ddog_http_client_send_blocking(const ddog_HttpClient *client,
                                                            ddog_HttpRequest *request,
                                                            ddog_SharedRuntime *shared_runtime,
                                                            ddog_HttpResponse **out_response);

#ifdef __cplusplus
}  // extern "C"
#endif  // __cplusplus

#endif  /* DDOG_HTTP_CLIENT_H */
