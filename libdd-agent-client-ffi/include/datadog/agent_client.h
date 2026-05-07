// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0


#ifndef DDOG_AGENT_CLIENT_H
#define DDOG_AGENT_CLIENT_H

#include <stdarg.h>
#include <stdbool.h>
#include <stdint.h>
#include <stdlib.h>
#include "common.h"
#include "http_client.h"
typedef struct ddog_AgentClient ddog_AgentClient;
typedef struct ddog_AgentClientBuilder ddog_AgentClientBuilder;
typedef struct ddog_AgentInfo ddog_AgentInfo;
typedef struct ddog_AgentResponse ddog_AgentResponse;
typedef struct ddog_LanguageMetadata ddog_LanguageMetadata;
typedef struct ddog_TelemetryRequest ddog_TelemetryRequest;


/**
 * Discriminant for [`DdogAgentClientError`].
 *
 * Mirrors `libdd_agent_client::BuildError` and
 * `libdd_agent_client::SendError` one-for-one, plus an
 * `InvalidArgument` variant for FFI boundary violations.
 */
typedef enum ddog_AgentClientErrorCode {
  /**
   * `BuildError::MissingTransport` — no transport was configured on
   * the builder.
   */
  DDOG_AGENT_CLIENT_ERROR_CODE_MISSING_TRANSPORT,
  /**
   * `BuildError::MissingLanguageMetadata` — no language metadata was
   * configured on the builder.
   */
  DDOG_AGENT_CLIENT_ERROR_CODE_MISSING_LANGUAGE_METADATA,
  /**
   * `BuildError::HttpClient` — the underlying HTTP client could not
   * be constructed.
   */
  DDOG_AGENT_CLIENT_ERROR_CODE_HTTP_CLIENT,
  /**
   * `SendError::Transport` — connection refused, timeout, or I/O
   * error.
   */
  DDOG_AGENT_CLIENT_ERROR_CODE_TRANSPORT,
  /**
   * `SendError::HttpError` — the server returned an HTTP error
   * status. `status` and `body` on the carrier struct are populated.
   */
  DDOG_AGENT_CLIENT_ERROR_CODE_HTTP_ERROR,
  /**
   * `SendError::RetriesExhausted` — all retry attempts exhausted
   * without a successful response.
   */
  DDOG_AGENT_CLIENT_ERROR_CODE_RETRIES_EXHAUSTED,
  /**
   * `SendError::Encoding` — payload serialisation or compression
   * failure.
   */
  DDOG_AGENT_CLIENT_ERROR_CODE_ENCODING,
  /**
   * A null pointer or otherwise invalid argument was passed across
   * the FFI.
   */
  DDOG_AGENT_CLIENT_ERROR_CODE_INVALID_ARGUMENT,
} ddog_AgentClientErrorCode;

/**
 * Wire format for a serialised trace payload.
 *
 * Mirrors [`libdd_agent_client::TraceFormat`]. Determines both the
 * `Content-Type` header and the target endpoint:
 * `MsgpackV5` -> `/v0.5/traces`, `MsgpackV4` -> `/v0.4/traces`.
 */
typedef enum ddog_TraceFormat {
  /**
   * `application/msgpack` to `/v0.5/traces`. Preferred.
   */
  DDOG_TRACE_FORMAT_MSGPACK_V5,
  /**
   * `application/msgpack` to `/v0.4/traces`. Fallback for Windows /
   * AppSec.
   */
  DDOG_TRACE_FORMAT_MSGPACK_V4,
} ddog_TraceFormat;

/**
 * A list of UTF-8 strings exposed across the FFI boundary.
 *
 * The `ptr` array is `len` `CharSlice` entries long. Each entry borrows
 * its bytes from the producing handle (e.g. an `AgentInfo`) and is
 * only valid while that handle is live. The slice itself (the `ptr`
 * allocation) is owned by the caller and must be released via the
 * matching `*_drop` function.
 */
typedef struct ddog_StringSlice {
  /**
   * Pointer to an array of `len` borrowed [`CharSlice`] values.
   * May be null if `len == 0`.
   */
  const ddog_CharSlice *ptr;
  /**
   * Number of `CharSlice` elements at `.ptr`.
   */
  uintptr_t len;
} ddog_StringSlice;

/**
 * FFI-safe error type for the agent client.
 *
 * `msg` is always non-null and owned by the struct; it must be
 * released via [`ddog_agent_client_error_free`]. For `HttpError`,
 * `status` is the HTTP status code and `body` (if non-null) is a
 * `\0`-terminated byte string with the response body. For other
 * variants `status` is 0 and `body` is null.
 */
typedef struct ddog_AgentClientError {
  /**
   * The error code discriminant.
   */
  enum ddog_AgentClientErrorCode code;
  /**
   * `\0`-terminated UTF-8 message describing the error.
   */
  char *msg;
  /**
   * HTTP status (only populated when `code == HttpError`; 0
   * otherwise).
   */
  uint16_t status;
  /**
   * Response body (only populated when `code == HttpError`; null
   * otherwise).
   */
  char *body;
} ddog_AgentClientError;

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
 * Per-request options for trace sends.
 *
 * FFI mirror of [`libdd_agent_client::TraceSendOptions`].
 */
typedef struct ddog_TraceSendOptions {
  /**
   * When `true`, appends `Datadog-Client-Computed-Top-Level: yes`.
   */
  bool computed_top_level;
} ddog_TraceSendOptions;

#ifdef __cplusplus
extern "C" {
#endif // __cplusplus

/**
 * Drop an [`AgentInfo`] handle.
 *
 * # Safety
 * `info` must be `None` or an info handle produced by
 * [`crate::ddog_agent_client_agent_info_blocking`] and not yet
 * dropped.
 */
void ddog_agent_info_drop(ddog_AgentInfo *info);

/**
 * Borrow the agent's reported endpoints (e.g. `["/v0.4/traces",
 * "/v0.5/traces"]`).
 *
 * The returned `DdogStringSlice` borrows from `info`; the entries
 * remain valid while `info` is alive. The outer array is owned by
 * the caller and must be released via
 * [`crate::ddog_string_slice_drop`].
 *
 * If `info` is `None`, returns an empty slice.
 *
 * # Safety
 * `info` must be `None` or a valid reference to an `AgentInfo`
 * produced by this crate.
 */
struct ddog_StringSlice ddog_agent_info_endpoints(const ddog_AgentInfo *info);

/**
 * Whether the agent supports client-side P0 dropping.
 *
 * Returns `false` if `info` is null.
 *
 * # Safety
 * `info` must be `None` or a valid reference produced by this crate.
 */
bool ddog_agent_info_client_drop_p0s(const ddog_AgentInfo *info);

/**
 * Borrow the agent's version string, if reported.
 *
 * Returns `None` (null pointer in C) when `info` is null or the
 * agent did not report a version. On `Some`, the caller owns the
 * returned `Box<StringWrapper>` and must release it via
 * `ddog_StringWrapper_drop`.
 *
 * # Safety
 * `info` must be `None` or a valid reference produced by this crate.
 */
ddog_StringWrapper *ddog_agent_info_version(const ddog_AgentInfo *info);

/**
 * Value of the `Datadog-Container-Tags-Hash` response header from the
 * last `/info` fetch, if any.
 *
 * # Safety
 * `info` must be `None` or a valid reference produced by this crate.
 */
ddog_StringWrapper *ddog_agent_info_container_tags_hash(const ddog_AgentInfo *info);

/**
 * Value of the `Datadog-Agent-State` response header from the last
 * `/info` fetch.
 *
 * The agent updates this opaque token whenever its internal state
 * changes (e.g. a configuration reload). Clients that poll `/info`
 * periodically can skip re-parsing the response body by comparing
 * this value across calls.
 *
 * # Safety
 * `info` must be `None` or a valid reference produced by this crate.
 */
ddog_StringWrapper *ddog_agent_info_state_hash(const ddog_AgentInfo *info);

/**
 * Serialise the agent-reported `config` JSON block to a JSON string.
 *
 * Returns the canonical JSON (no whitespace) representation of the
 * `config` value. The Rust API exposes a `serde_json::Value`; we
 * serialise it at the FFI boundary so callers can parse it with
 * whatever JSON library they prefer.
 *
 * Returns `null` if `info` is null or if serialisation fails (which
 * is essentially impossible for a `Value` produced by a successful
 * deserialisation, but we return null defensively rather than
 * panicking). The caller owns the returned `Box<StringWrapper>` and
 * must release it via `ddog_StringWrapper_drop`.
 *
 * # Safety
 * `info` must be `None` or a valid reference produced by this crate.
 */
ddog_StringWrapper *ddog_agent_info_config_json(const ddog_AgentInfo *info);

/**
 * Same as [`ddog_agent_info_config_json`] but writes the JSON into a
 * caller-managed `*mut Box<StringWrapper>` and returns a flat error
 * for failure. Provided for callers that prefer the explicit-error
 * style used by the rest of the API.
 *
 * # Safety
 * `info` must be `None` or a valid reference; `out` must be a valid
 * writable pointer to an uninitialised `*mut ddog_StringWrapper`.
 */
struct ddog_AgentClientError *ddog_agent_info_config_json_or_error(const ddog_AgentInfo *info,
                                                                   ddog_StringWrapper **out);

/**
 * Allocate a new [`AgentClientBuilder`] with default settings.
 *
 * Writes a `Box<AgentClientBuilder>` into `*out_handle`. The caller
 * owns the handle and must eventually pass it to either
 * [`ddog_agent_client_builder_build`] (which consumes it) or
 * [`ddog_agent_client_builder_drop`].
 *
 * Before calling this for the first time in a process, the caller
 * must install a rustls crypto provider via
 * `ddog_http_client_install_default_crypto_provider` (non-FIPS) or
 * `ddog_http_client_init_fips` (FIPS) from `libdd-http-client-ffi`.
 *
 * # Safety
 * `out_handle` must be a valid, writable pointer to an
 * uninitialised `*mut ddog_AgentClientBuilder`.
 */
void ddog_agent_client_builder_new(ddog_AgentClientBuilder **out_handle);

/**
 * Configure the agent client to connect over HTTP to the given host
 * and port (e.g. `("localhost", 8126)`).
 *
 * # Safety
 * `builder` must be valid; `host` must point to valid UTF-8 memory.
 */
struct ddog_AgentClientError *ddog_agent_client_builder_set_http_endpoint(ddog_AgentClientBuilder *builder,
                                                                          ddog_CharSlice host,
                                                                          uint16_t port);

/**
 * Configure the agent client to connect over HTTPS to the given host
 * and port.
 *
 * # Safety
 * `builder` must be valid; `host` must point to valid UTF-8 memory.
 */
struct ddog_AgentClientError *ddog_agent_client_builder_set_https_endpoint(ddog_AgentClientBuilder *builder,
                                                                           ddog_CharSlice host,
                                                                           uint16_t port);

/**
 * Route all connections through the given Unix Domain Socket path.
 *
 * # Safety
 * `builder` must be valid; `path` must point to valid UTF-8 memory.
 */
struct ddog_AgentClientError *ddog_agent_client_builder_set_unix_socket(ddog_AgentClientBuilder *builder,
                                                                        ddog_CharSlice path);

/**
 * Route all connections through the given Windows Named Pipe.
 *
 * # Safety
 * `builder` must be valid; `path` must point to valid UTF-8 memory.
 */
struct ddog_AgentClientError *ddog_agent_client_builder_set_named_pipe(ddog_AgentClientBuilder *builder,
                                                                       ddog_CharSlice path);

/**
 * Set the test session token (`x-datadog-test-session-token` header).
 *
 * # Safety
 * `builder` must be valid; `token` must point to valid UTF-8 memory.
 */
struct ddog_AgentClientError *ddog_agent_client_builder_set_test_session_token(ddog_AgentClientBuilder *builder,
                                                                               ddog_CharSlice token);

/**
 * Set the request timeout in milliseconds. Defaults to 2 000 ms when
 * not set.
 *
 * # Safety
 * `builder` must be valid.
 */
struct ddog_AgentClientError *ddog_agent_client_builder_set_timeout_millis(ddog_AgentClientBuilder *builder,
                                                                           uint64_t timeout_ms);

/**
 * Override the default retry configuration. Takes ownership of the
 * `RetryConfig`: the caller must not reuse or free it.
 *
 * The `RetryConfig` is the same handle used by `libdd-http-client-ffi`
 * — build it via `ddog_retry_config_new` and the
 * `ddog_retry_config_set_*` family from that crate.
 *
 * # Safety
 * `builder` must be valid; `cfg` must be `None` or a config produced
 * by `ddog_retry_config_new` (in `libdd-http-client-ffi`) and not yet
 * consumed.
 */
struct ddog_AgentClientError *ddog_agent_client_builder_set_retry(ddog_AgentClientBuilder *builder,
                                                                  ddog_RetryConfig *cfg);

/**
 * Set the language/runtime metadata. Required before
 * [`ddog_agent_client_builder_build`]. Takes ownership of the metadata
 * handle: the caller must not reuse or free it.
 *
 * # Safety
 * `builder` must be valid; `metadata` must be `None` or a handle
 * produced by [`crate::ddog_language_metadata_new`] and not yet
 * consumed.
 */
struct ddog_AgentClientError *ddog_agent_client_builder_set_language_metadata(ddog_AgentClientBuilder *builder,
                                                                              ddog_LanguageMetadata *metadata);

/**
 * Allow connection pooling. Defaults to `false`.
 *
 * The Datadog agent has a low keep-alive timeout that causes "pipe
 * closed" errors on every second connection — `false` is correct for
 * all periodic-flush writers (traces, stats, data streams). Set to
 * `true` only for high-frequency continuous senders.
 *
 * # Safety
 * `builder` must be valid.
 */
struct ddog_AgentClientError *ddog_agent_client_builder_set_allow_connection_pooling(ddog_AgentClientBuilder *builder,
                                                                                     bool allow);

/**
 * Append an extra custom header that will be injected on every
 * outgoing request.
 *
 * **Single-set semantics:** the underlying Rust builder's
 * `extra_headers` setter replaces the entire vector and exposes no
 * getter, so each call to this function REPLACES the previously-set
 * extra headers. Callers that need multiple headers should set them
 * in a single call by repeating this function once per header — but
 * only the last call's header survives until the Rust crate exposes
 * an additive setter. (Tracked separately; out of scope for the FFI
 * task. The five `Datadog-Meta-*`, `User-Agent`,
 * container/entity-ID, and test-token headers are injected
 * automatically and don't go through this path.)
 *
 * # Safety
 * `builder` must be valid; `name` and `value` must point to valid
 * UTF-8 memory.
 */
struct ddog_AgentClientError *ddog_agent_client_builder_add_extra_header(ddog_AgentClientBuilder *builder,
                                                                         ddog_CharSlice name,
                                                                         ddog_CharSlice value);

/**
 * Consume the builder and produce an [`AgentClient`].
 *
 * On success writes a `Box<AgentClient>` into `*out_handle` and
 * returns `None`. On failure, leaves `*out_handle` unchanged and
 * returns an error. The builder is consumed in either case.
 *
 * # Safety
 * `builder` must have been produced by
 * [`ddog_agent_client_builder_new`]. `out_handle` must be a valid,
 * writable pointer to an uninitialised `*mut ddog_AgentClient`.
 */
struct ddog_AgentClientError *ddog_agent_client_builder_build(ddog_AgentClientBuilder *builder,
                                                              ddog_AgentClient **out_handle);

/**
 * Drop a builder without building. Use when an error occurs partway
 * through configuration and you wish to abandon the builder.
 *
 * # Safety
 * `builder` must be `None` or a builder produced by
 * [`ddog_agent_client_builder_new`] and not yet consumed.
 */
void ddog_agent_client_builder_drop(ddog_AgentClientBuilder *builder);

/**
 * Drop an [`AgentClient`].
 *
 * # Safety
 * `client` must be `None` or a client produced by
 * [`ddog_agent_client_builder_build`] and not yet dropped.
 */
void ddog_agent_client_drop(ddog_AgentClient *client);

/**
 * Free a [`DdogAgentClientError`].
 *
 * After this call the pointer is invalid and must not be used again.
 *
 * # Safety
 * `error` must be `None` or have been produced by this crate's API and
 * not yet freed.
 */
void ddog_agent_client_error_free(struct ddog_AgentClientError *error);

/**
 * Allocate a new [`LanguageMetadata`].
 *
 * All four arguments must be valid UTF-8.
 *
 * - `language`: e.g. `"python"`, `"ruby"`.
 * - `language_version`: e.g. `"3.12.1"`.
 * - `language_interpreter`: e.g. `"CPython"`, `"MRI"`.
 * - `tracer_version`: e.g. `"2.18.0"`.
 *
 * On success writes a `Box<LanguageMetadata>` into `*out_handle` and
 * returns `None`. On failure returns the error and leaves
 * `*out_handle` unchanged.
 *
 * # Safety
 * All `CharSlice` arguments must point to valid memory for their
 * declared lengths. `out_handle` must be a valid, writable pointer to
 * an uninitialised `*mut ddog_LanguageMetadata`.
 */
struct ddog_AgentClientError *ddog_language_metadata_new(ddog_CharSlice language,
                                                         ddog_CharSlice language_version,
                                                         ddog_CharSlice language_interpreter,
                                                         ddog_CharSlice tracer_version,
                                                         ddog_LanguageMetadata **out_handle);

/**
 * Drop a [`LanguageMetadata`] that was not attached to a builder.
 *
 * # Safety
 * `metadata` must be `None` or a metadata produced by
 * [`ddog_language_metadata_new`] and not yet consumed by
 * [`crate::ddog_agent_client_builder_set_language_metadata`].
 */
void ddog_language_metadata_drop(ddog_LanguageMetadata *metadata);

/**
 * Read the HTTP status code from an [`AgentResponse`].
 *
 * Returns 0 if `response` is null.
 *
 * # Safety
 * `response` must be `None` or a valid reference produced by this
 * crate.
 */
uint16_t ddog_agent_response_status(const ddog_AgentResponse *response);

/**
 * Serialise the parsed `rate_by_service` sampling-rate map to a JSON
 * string.
 *
 * Returns `None` (null pointer in C) if `response` is null, the
 * agent did not return a `rate_by_service` field, or serialisation
 * fails. The caller owns the returned `Box<StringWrapper>` and must
 * release it via `ddog_StringWrapper_drop`.
 *
 * # Safety
 * `response` must be `None` or a valid reference produced by this
 * crate.
 */
ddog_StringWrapper *ddog_agent_response_rate_by_service_json(const ddog_AgentResponse *response);

/**
 * Drop an [`AgentResponse`].
 *
 * # Safety
 * `response` must be `None` or a response produced by this crate and
 * not yet dropped.
 */
void ddog_agent_response_drop(ddog_AgentResponse *response);

/**
 * Send a serialised trace payload synchronously.
 *
 * On success writes a `Box<AgentResponse>` into `*out_response` and
 * returns `None`. On failure returns the error and leaves
 * `*out_response` unchanged.
 *
 * # Parameters
 * - `client`: a client produced by
 *   [`crate::ddog_agent_client_builder_build`].
 * - `payload`: a borrowed byte buffer with the serialised traces. The
 *   bytes are copied into an owned `Bytes` for the duration of the
 *   request — the caller may free `payload` after the call returns.
 * - `trace_count`: number of traces in the payload (sets
 *   `X-Datadog-Trace-Count`).
 * - `format`: msgpack v5 or v4.
 * - `options`: per-request options (e.g. computed-top-level).
 * - `shared_runtime`: a runtime handle from `ddog_shared_runtime_new`.
 *   This call does not take ownership of the handle.
 * - `out_response`: where to write the resulting
 *   `Box<AgentResponse>`.
 *
 * # Safety
 * - `client` must be valid (non-null) and not dropped.
 * - `payload` must point to valid memory for its declared length.
 * - `shared_runtime` must be non-null and produced by
 *   `ddog_shared_runtime_new`.
 * - `out_response` must be a valid, writable pointer to an
 *   uninitialised `*mut ddog_AgentResponse`.
 */
struct ddog_AgentClientError *ddog_agent_client_send_traces_blocking(const ddog_AgentClient *client,
                                                                     ddog_ByteSlice payload,
                                                                     uintptr_t trace_count,
                                                                     enum ddog_TraceFormat format,
                                                                     struct ddog_TraceSendOptions options,
                                                                     ddog_SharedRuntime *shared_runtime,
                                                                     ddog_AgentResponse **out_response);

/**
 * Send span stats (APM concentrator buckets) synchronously to
 * `/v0.6/stats`.
 *
 * # Safety
 * See [`ddog_agent_client_send_traces_blocking`] — same contract.
 */
struct ddog_AgentClientError *ddog_agent_client_send_stats_blocking(const ddog_AgentClient *client,
                                                                    ddog_ByteSlice payload,
                                                                    ddog_SharedRuntime *shared_runtime);

/**
 * Send data-streams pipeline stats synchronously to
 * `/v0.1/pipeline_stats`. The payload is gzip-compressed by the
 * underlying client regardless of any client-level compression
 * setting.
 *
 * # Safety
 * See [`ddog_agent_client_send_traces_blocking`] — same contract.
 */
struct ddog_AgentClientError *ddog_agent_client_send_pipeline_stats_blocking(const ddog_AgentClient *client,
                                                                             ddog_ByteSlice payload,
                                                                             ddog_SharedRuntime *shared_runtime);

/**
 * Send a telemetry event synchronously to the agent's telemetry
 * proxy (`telemetry/proxy/api/v2/apmtelemetry`). Consumes the
 * telemetry request handle.
 *
 * # Safety
 * `client` and `shared_runtime` follow the standard contract;
 * `request` must be `None` or a request produced by
 * [`crate::ddog_telemetry_request_new`] and not yet consumed.
 */
struct ddog_AgentClientError *ddog_agent_client_send_telemetry_blocking(const ddog_AgentClient *client,
                                                                        ddog_TelemetryRequest *request,
                                                                        ddog_SharedRuntime *shared_runtime);

/**
 * Send an event via the agent's EVP (Event Platform) proxy.
 *
 * `subdomain` is the target intake (injected as
 * `X-Datadog-EVP-Subdomain`) and `path` is the endpoint on that
 * intake. Both must be valid UTF-8.
 *
 * # Safety
 * `subdomain`, `path`, `content_type` must point to valid UTF-8
 * memory; `payload` must point to valid memory for its declared
 * length. Other arguments follow the standard contract.
 */
struct ddog_AgentClientError *ddog_agent_client_send_evp_event_blocking(const ddog_AgentClient *client,
                                                                        ddog_CharSlice subdomain,
                                                                        ddog_CharSlice path,
                                                                        ddog_ByteSlice payload,
                                                                        ddog_CharSlice content_type,
                                                                        ddog_SharedRuntime *shared_runtime);

/**
 * Probe `GET /info` synchronously and surface the parsed agent
 * capabilities.
 *
 * On success writes either a `Box<AgentInfo>` into `*out_info`, or
 * `null` when the agent returned 404 (the `Ok(None)` path on the
 * Rust side — meaning the agent does not expose `/info`). Returns
 * `None` for both successful cases. On failure leaves `*out_info`
 * unchanged and returns the error.
 *
 * # Safety
 * Standard contract for `client` and `shared_runtime`. `out_info`
 * must be a valid, writable pointer to a `*mut ddog_AgentInfo` —
 * after a successful call, dereference to test for null before
 * dropping.
 */
struct ddog_AgentClientError *ddog_agent_client_agent_info_blocking(const ddog_AgentClient *client,
                                                                    ddog_SharedRuntime *shared_runtime,
                                                                    ddog_AgentInfo **out_info);

/**
 * Free a [`DdogStringSlice`] previously produced by this crate's API
 * (for example by [`crate::ddog_agent_info_endpoints`]).
 *
 * Frees only the outer array of `CharSlice` entries — the bytes the
 * entries point to are owned by the producing handle (e.g. an
 * `AgentInfo`) and remain alive until that handle is dropped.
 *
 * # Safety
 * `slice` must have been produced by this crate, paired with its
 * original `len`. It is safe to call this with a default-zeroed
 * `DdogStringSlice` (null `ptr`, `len = 0`).
 */
void ddog_string_slice_drop(struct ddog_StringSlice slice);

/**
 * Allocate a new [`TelemetryRequest`].
 *
 * `request_type` and `api_version` must be valid UTF-8.
 * `body` is an arbitrary byte payload (the agent expects
 * `application/json`; the caller is responsible for serialising the
 * telemetry event before constructing this).
 *
 * # Safety
 * All slice arguments must point to valid memory for their declared
 * lengths. `out_handle` must be a valid, writable pointer to an
 * uninitialised `*mut ddog_TelemetryRequest`.
 */
struct ddog_AgentClientError *ddog_telemetry_request_new(ddog_CharSlice request_type,
                                                         ddog_CharSlice api_version,
                                                         ddog_ByteSlice body,
                                                         bool debug,
                                                         ddog_TelemetryRequest **out_handle);

/**
 * Drop a [`TelemetryRequest`] that was not consumed by
 * [`crate::ddog_agent_client_send_telemetry_blocking`].
 *
 * # Safety
 * `request` must be `None` or a request produced by
 * [`ddog_telemetry_request_new`] and not yet consumed.
 */
void ddog_telemetry_request_drop(ddog_TelemetryRequest *request);

#ifdef __cplusplus
}  // extern "C"
#endif  // __cplusplus

#endif  /* DDOG_AGENT_CLIENT_H */
