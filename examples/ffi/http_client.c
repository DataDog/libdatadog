// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

// http_client.c — exercises the ddog_http_client_* FFI surface end-to-end.
//
// The example is self-contained: it forks a child that runs a tiny
// HTTP/1.1 server on 127.0.0.1, then in the parent it builds a client,
// runs three subtests against the child, and tears down.
//
// Subtests:
//   1. Plain GET — confirms 200/pong on the canned `/` path.
//   2. Multipart POST — POSTs a single MultipartPart to `/upload` and
//      asserts the request body is well-formed multipart/form-data.
//   3. Retry — POSTs to `/flaky`; the server fails with 503 twice then
//      returns 200, exercising RetryConfig.max_retries=2.
//
// On success exits 0; on any failure exits non-zero with a message on
// stderr. Suitable for `cargo ffi-test` which only inspects exit codes.

#define _GNU_SOURCE  // for strcasestr
#include <arpa/inet.h>
#include <errno.h>
#include <netinet/in.h>
#include <signal.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/socket.h>
#include <sys/types.h>
#include <sys/wait.h>
#include <unistd.h>

#include <datadog/common.h>
#include <datadog/http_client.h>
#include <datadog/shared-runtime.h>

#define BODY_PONG "pong"
#define BODY_UPLOAD_OK "uploaded"
#define BODY_FLAKY_OK "flaky-ok"

// Read until "\r\n\r\n" (request headers complete), then if the
// Content-Length is set, read that many additional body bytes. Stores
// the raw bytes (including headers and body) in `buf` of size `cap`,
// terminating with a NUL. Returns the number of bytes actually read
// (excluding the terminating NUL), or -1 on error.
//
// Caller must size `buf` generously; we cap at `cap - 1`.
static ssize_t read_full_request(int conn, char *buf, size_t cap) {
  size_t total = 0;
  size_t headers_end = 0;
  while (total + 1 < cap) {
    ssize_t n = read(conn, buf + total, cap - 1 - total);
    if (n <= 0) break;
    total += (size_t)n;
    if (headers_end == 0 && total >= 4) {
      for (size_t i = 0; i + 3 < total; i++) {
        if (buf[i] == '\r' && buf[i + 1] == '\n' &&
            buf[i + 2] == '\r' && buf[i + 3] == '\n') {
          headers_end = i + 4;
          break;
        }
      }
    }
    if (headers_end != 0) {
      // Look for Content-Length: <n> in headers.
      buf[headers_end - 1] = '\0';  // temp NUL for header parse
      const char *cl = strcasestr(buf, "Content-Length:");
      size_t expected_body = 0;
      if (cl != NULL) {
        cl += strlen("Content-Length:");
        while (*cl == ' ' || *cl == '\t') cl++;
        expected_body = (size_t)strtoul(cl, NULL, 10);
      }
      buf[headers_end - 1] = '\n';  // restore
      size_t want = headers_end + expected_body;
      if (total >= want) break;
    }
  }
  buf[total < cap ? total : cap - 1] = '\0';
  return (ssize_t)total;
}

// Returns 1 if `request` is a POST to `path` (matches request-line).
static int is_post_to(const char *request, const char *path) {
  // Request-line looks like: "POST /upload HTTP/1.1\r\n..."
  if (strncmp(request, "POST ", 5) != 0) return 0;
  const char *p = request + 5;
  size_t plen = strlen(path);
  if (strncmp(p, path, plen) != 0) return 0;
  // Next char must be space (URL terminator) or '?' (query).
  return (p[plen] == ' ' || p[plen] == '?');
}

static void write_all(int conn, const char *data, size_t len) {
  while (len > 0) {
    ssize_t n = write(conn, data, len);
    if (n <= 0) break;
    data += n;
    len -= (size_t)n;
  }
}

static int run_server_child(int listen_fd) {
  // The /flaky path fails twice then succeeds. Using a static counter
  // is fine because the child is a single-process, single-threaded
  // accept loop.
  int flaky_attempts = 0;

  for (;;) {
    struct sockaddr_in peer;
    socklen_t peer_len = sizeof(peer);
    int conn = accept(listen_fd, (struct sockaddr *)&peer, &peer_len);
    if (conn < 0) {
      if (errno == EINTR) continue;
      perror("accept");
      return 1;
    }

    char buf[8192];
    ssize_t n = read_full_request(conn, buf, sizeof(buf));
    if (n <= 0) {
      close(conn);
      continue;
    }

    if (is_post_to(buf, "/upload")) {
      // Sanity-check the request really is multipart/form-data.
      const char *ct = strcasestr(buf, "Content-Type:");
      int looks_multipart = (ct != NULL) &&
          (strstr(ct, "multipart/form-data") != NULL);
      if (!looks_multipart) {
        static const char bad[] =
            "HTTP/1.1 400 Bad Request\r\n"
            "Content-Length: 0\r\n"
            "Connection: close\r\n\r\n";
        write_all(conn, bad, sizeof(bad) - 1);
      } else {
        char ok[256];
        int len = snprintf(ok, sizeof(ok),
                           "HTTP/1.1 200 OK\r\n"
                           "Content-Type: text/plain\r\n"
                           "Content-Length: %zu\r\n"
                           "Connection: close\r\n\r\n%s",
                           strlen(BODY_UPLOAD_OK), BODY_UPLOAD_OK);
        write_all(conn, ok, (size_t)len);
      }
    } else if (is_post_to(buf, "/flaky")) {
      flaky_attempts++;
      if (flaky_attempts <= 2) {
        // Fail twice with 503.
        static const char fail[] =
            "HTTP/1.1 503 Service Unavailable\r\n"
            "Content-Type: text/plain\r\n"
            "Content-Length: 9\r\n"
            "Connection: close\r\n\r\nflaky-503";
        write_all(conn, fail, sizeof(fail) - 1);
      } else {
        char ok[256];
        int len = snprintf(ok, sizeof(ok),
                           "HTTP/1.1 200 OK\r\n"
                           "Content-Type: text/plain\r\n"
                           "Content-Length: %zu\r\n"
                           "Connection: close\r\n\r\n%s",
                           strlen(BODY_FLAKY_OK), BODY_FLAKY_OK);
        write_all(conn, ok, (size_t)len);
      }
    } else {
      // Default: serve canned pong on any other path (including the
      // plain GET subtest's `/`).
      static const char pong[] =
          "HTTP/1.1 200 OK\r\n"
          "Content-Type: text/plain\r\n"
          "Content-Length: 4\r\n"
          "Connection: close\r\n"
          "\r\n" BODY_PONG;
      write_all(conn, pong, sizeof(pong) - 1);
    }
    shutdown(conn, SHUT_RDWR);
    close(conn);
  }
}

// Bind a TCP listener on 127.0.0.1:0 and write the chosen port into
// *out_port. Returns the bound socket (already in LISTEN state) or -1.
static int spawn_listener(uint16_t *out_port) {
  int fd = socket(AF_INET, SOCK_STREAM, 0);
  if (fd < 0) {
    perror("socket");
    return -1;
  }
  int yes = 1;
  setsockopt(fd, SOL_SOCKET, SO_REUSEADDR, &yes, sizeof(yes));

  struct sockaddr_in addr = {0};
  addr.sin_family = AF_INET;
  addr.sin_addr.s_addr = htonl(INADDR_LOOPBACK);
  addr.sin_port = 0;
  if (bind(fd, (struct sockaddr *)&addr, sizeof(addr)) < 0) {
    perror("bind");
    close(fd);
    return -1;
  }
  if (listen(fd, 8) < 0) {
    perror("listen");
    close(fd);
    return -1;
  }

  struct sockaddr_in bound;
  socklen_t bound_len = sizeof(bound);
  if (getsockname(fd, (struct sockaddr *)&bound, &bound_len) < 0) {
    perror("getsockname");
    close(fd);
    return -1;
  }
  *out_port = ntohs(bound.sin_port);
  return fd;
}

// -----------------------------------------------------------------------------
// Helpers shared by the subtests
// -----------------------------------------------------------------------------

// Build a SharedRuntime; on failure print and return NULL.
static const struct ddog_SharedRuntime *make_runtime(void) {
  const struct ddog_SharedRuntime *runtime = NULL;
  struct ddog_SharedRuntimeFFIError *rt_err = ddog_shared_runtime_new(&runtime);
  if (rt_err) {
    fprintf(stderr, "ddog_shared_runtime_new failed\n");
    ddog_shared_runtime_error_free(rt_err);
    return NULL;
  }
  return runtime;
}

// Build a basic client with 5s timeout, pooling off, and an optional
// retry config attached. On error returns NULL after consuming `retry`
// regardless. `retry` is consumed by the call (success or failure).
static ddog_HttpClient *make_client(const char *base_url,
                                    ddog_RetryConfig *retry) {
  ddog_HttpClientBuilder *builder = NULL;
  ddog_http_client_builder_new(&builder);

  ddog_CharSlice base_slice = {.ptr = base_url, .len = strlen(base_url)};
  struct ddog_HttpClientError *err =
      ddog_http_client_builder_set_base_url(builder, base_slice);
  if (err) goto fail;
  err = ddog_http_client_builder_set_timeout(builder, 5000);
  if (err) goto fail;
  err = ddog_http_client_builder_set_allow_connection_pooling(builder, false);
  if (err) goto fail;
  if (retry != NULL) {
    err = ddog_http_client_builder_set_retry(builder, retry);
    retry = NULL;  // consumed regardless
    if (err) goto fail;
  }

  ddog_HttpClient *client = NULL;
  err = ddog_http_client_builder_build(builder, &client);
  if (err) {
    fprintf(stderr, "builder_build failed: %s\n", err->msg);
    ddog_http_client_error_free(err);
    return NULL;
  }
  return client;

fail:
  fprintf(stderr, "builder configuration failed: %s\n", err ? err->msg : "?");
  if (err) ddog_http_client_error_free(err);
  ddog_http_client_builder_drop(builder);
  if (retry) ddog_retry_config_drop(retry);
  return NULL;
}

// -----------------------------------------------------------------------------
// Subtest 1: plain GET
// -----------------------------------------------------------------------------

static int subtest_get(const char *base_url) {
  const struct ddog_SharedRuntime *runtime = make_runtime();
  if (!runtime) return 1;

  ddog_HttpClient *client = make_client(base_url, NULL);
  if (!client) {
    ddog_shared_runtime_free(runtime);
    return 1;
  }

  ddog_HttpRequest *req = NULL;
  ddog_CharSlice url_slice = {.ptr = base_url, .len = strlen(base_url)};
  struct ddog_HttpClientError *err =
      ddog_http_request_new(DDOG_HTTP_METHOD_GET, url_slice, &req);
  if (err) {
    fprintf(stderr, "request_new failed: %s\n", err->msg);
    ddog_http_client_error_free(err);
    ddog_http_client_drop(client);
    ddog_shared_runtime_free(runtime);
    return 1;
  }

  ddog_HttpResponse *resp = NULL;
  err = ddog_http_client_send_blocking(
      client, req, (struct ddog_SharedRuntime *)runtime, &resp);
  if (err) {
    fprintf(stderr, "send_blocking failed: %s\n", err->msg);
    ddog_http_client_error_free(err);
    ddog_http_client_drop(client);
    ddog_shared_runtime_free(runtime);
    return 1;
  }

  int rc = 0;
  uint16_t status = ddog_http_response_status(resp);
  if (status != 200) {
    fprintf(stderr, "GET: expected 200 got %u\n", (unsigned)status);
    rc = 1;
  } else {
    uintptr_t body_len = 0;
    const uint8_t *body = ddog_http_response_body(resp, &body_len);
    if (body_len != strlen(BODY_PONG) ||
        memcmp(body, BODY_PONG, strlen(BODY_PONG)) != 0) {
      fprintf(stderr, "GET: body mismatch len=%zu\n", (size_t)body_len);
      rc = 1;
    }
  }

  // Exercise the headers accessor too.
  struct ddog_HttpHeader *headers = NULL;
  uintptr_t headers_len = 0;
  err = ddog_http_response_headers(resp, &headers, &headers_len);
  if (err) {
    fprintf(stderr, "response_headers failed: %s\n", err->msg);
    ddog_http_client_error_free(err);
    rc = 1;
  } else {
    ddog_http_response_headers_free(headers, headers_len);
  }

  ddog_http_response_drop(resp);
  ddog_http_client_drop(client);
  ddog_shared_runtime_free(runtime);
  return rc;
}

// -----------------------------------------------------------------------------
// Subtest 2: multipart upload
// -----------------------------------------------------------------------------

static int subtest_multipart(const char *base_url) {
  const struct ddog_SharedRuntime *runtime = make_runtime();
  if (!runtime) return 1;

  ddog_HttpClient *client = make_client(base_url, NULL);
  if (!client) {
    ddog_shared_runtime_free(runtime);
    return 1;
  }

  // Build a request to /upload.
  char url[128];
  snprintf(url, sizeof(url), "%supload", base_url);
  ddog_HttpRequest *req = NULL;
  ddog_CharSlice url_slice = {.ptr = url, .len = strlen(url)};
  struct ddog_HttpClientError *err =
      ddog_http_request_new(DDOG_HTTP_METHOD_POST, url_slice, &req);
  if (err) {
    fprintf(stderr, "multipart: request_new failed: %s\n", err->msg);
    ddog_http_client_error_free(err);
    ddog_http_client_drop(client);
    ddog_shared_runtime_free(runtime);
    return 1;
  }

  // Build a multipart part: field name = "file", data = "hello", with
  // filename + content type set.
  ddog_MultipartPart *part = NULL;
  ddog_CharSlice name_slice = {.ptr = "file", .len = 4};
  static const uint8_t data[] = "hello";
  ddog_ByteSlice data_slice = {.ptr = data, .len = sizeof(data) - 1};
  err = ddog_multipart_part_new(name_slice, data_slice, &part);
  if (err) {
    fprintf(stderr, "multipart_part_new failed: %s\n", err->msg);
    ddog_http_client_error_free(err);
    ddog_http_request_drop(req);
    ddog_http_client_drop(client);
    ddog_shared_runtime_free(runtime);
    return 1;
  }
  ddog_CharSlice fn_slice = {.ptr = "a.txt", .len = 5};
  err = ddog_multipart_part_with_filename(part, fn_slice);
  if (err) {
    fprintf(stderr, "multipart_part_with_filename failed: %s\n", err->msg);
    ddog_http_client_error_free(err);
    ddog_multipart_part_drop(part);
    ddog_http_request_drop(req);
    ddog_http_client_drop(client);
    ddog_shared_runtime_free(runtime);
    return 1;
  }
  ddog_CharSlice ct_slice = {.ptr = "text/plain", .len = 10};
  err = ddog_multipart_part_with_content_type(part, ct_slice);
  if (err) {
    fprintf(stderr, "multipart_part_with_content_type failed: %s\n", err->msg);
    ddog_http_client_error_free(err);
    ddog_multipart_part_drop(part);
    ddog_http_request_drop(req);
    ddog_http_client_drop(client);
    ddog_shared_runtime_free(runtime);
    return 1;
  }

  // Attach the part — consumes it.
  err = ddog_http_request_with_multipart_part(req, part);
  if (err) {
    fprintf(stderr, "request_with_multipart_part failed: %s\n", err->msg);
    ddog_http_client_error_free(err);
    ddog_http_request_drop(req);
    ddog_http_client_drop(client);
    ddog_shared_runtime_free(runtime);
    return 1;
  }

  // Send it.
  ddog_HttpResponse *resp = NULL;
  err = ddog_http_client_send_blocking(
      client, req, (struct ddog_SharedRuntime *)runtime, &resp);
  if (err) {
    fprintf(stderr, "multipart send_blocking failed: %s\n", err->msg);
    ddog_http_client_error_free(err);
    ddog_http_client_drop(client);
    ddog_shared_runtime_free(runtime);
    return 1;
  }

  int rc = 0;
  uint16_t status = ddog_http_response_status(resp);
  if (status != 200) {
    fprintf(stderr, "multipart: expected 200 got %u\n", (unsigned)status);
    rc = 1;
  } else {
    uintptr_t body_len = 0;
    const uint8_t *body = ddog_http_response_body(resp, &body_len);
    if (body_len != strlen(BODY_UPLOAD_OK) ||
        memcmp(body, BODY_UPLOAD_OK, strlen(BODY_UPLOAD_OK)) != 0) {
      fprintf(stderr, "multipart: body mismatch len=%zu\n", (size_t)body_len);
      rc = 1;
    }
  }

  ddog_http_response_drop(resp);
  ddog_http_client_drop(client);
  ddog_shared_runtime_free(runtime);
  return rc;
}

// -----------------------------------------------------------------------------
// Subtest 3: retry on flaky endpoint
// -----------------------------------------------------------------------------

static int subtest_retry(const char *base_url) {
  const struct ddog_SharedRuntime *runtime = make_runtime();
  if (!runtime) return 1;

  // Build a retry config: max_retries=3 (server fails twice, third
  // succeeds), 1ms initial delay, no jitter for determinism.
  ddog_RetryConfig *retry = NULL;
  ddog_retry_config_new(&retry);
  struct ddog_HttpClientError *err =
      ddog_retry_config_set_max_retries(retry, 3);
  if (err) goto retry_cfg_fail;
  err = ddog_retry_config_set_initial_delay_millis(retry, 1);
  if (err) goto retry_cfg_fail;
  err = ddog_retry_config_set_jitter(retry, false);
  if (err) goto retry_cfg_fail;

  // make_client takes ownership of the retry config.
  ddog_HttpClient *client = make_client(base_url, retry);
  retry = NULL;
  if (!client) {
    ddog_shared_runtime_free(runtime);
    return 1;
  }

  // Build a POST request to /flaky.
  char url[128];
  snprintf(url, sizeof(url), "%sflaky", base_url);
  ddog_HttpRequest *req = NULL;
  ddog_CharSlice url_slice = {.ptr = url, .len = strlen(url)};
  err = ddog_http_request_new(DDOG_HTTP_METHOD_POST, url_slice, &req);
  if (err) {
    fprintf(stderr, "retry: request_new failed: %s\n", err->msg);
    ddog_http_client_error_free(err);
    ddog_http_client_drop(client);
    ddog_shared_runtime_free(runtime);
    return 1;
  }
  ddog_ByteSlice body = {.ptr = (const uint8_t *)"ping", .len = 4};
  err = ddog_http_request_set_body(req, body);
  if (err) {
    fprintf(stderr, "retry: set_body failed: %s\n", err->msg);
    ddog_http_client_error_free(err);
    ddog_http_request_drop(req);
    ddog_http_client_drop(client);
    ddog_shared_runtime_free(runtime);
    return 1;
  }

  ddog_HttpResponse *resp = NULL;
  err = ddog_http_client_send_blocking(
      client, req, (struct ddog_SharedRuntime *)runtime, &resp);
  if (err) {
    fprintf(stderr, "retry: send_blocking failed: code=%d msg=%s\n",
            (int)err->code, err->msg);
    ddog_http_client_error_free(err);
    ddog_http_client_drop(client);
    ddog_shared_runtime_free(runtime);
    return 1;
  }

  int rc = 0;
  uint16_t status = ddog_http_response_status(resp);
  if (status != 200) {
    fprintf(stderr, "retry: expected 200 got %u\n", (unsigned)status);
    rc = 1;
  } else {
    uintptr_t body_len = 0;
    const uint8_t *body_bytes = ddog_http_response_body(resp, &body_len);
    if (body_len != strlen(BODY_FLAKY_OK) ||
        memcmp(body_bytes, BODY_FLAKY_OK, strlen(BODY_FLAKY_OK)) != 0) {
      fprintf(stderr, "retry: body mismatch len=%zu\n", (size_t)body_len);
      rc = 1;
    }
  }

  ddog_http_response_drop(resp);
  ddog_http_client_drop(client);
  ddog_shared_runtime_free(runtime);
  return rc;

retry_cfg_fail:
  fprintf(stderr, "retry config setup failed: %s\n", err ? err->msg : "?");
  if (err) ddog_http_client_error_free(err);
  if (retry) ddog_retry_config_drop(retry);
  ddog_shared_runtime_free(runtime);
  return 1;
}

// -----------------------------------------------------------------------------
// Optional: FIPS init smoke (only built when DD_HTTP_CLIENT_FIPS_TEST is
// defined). The default ffi-test build doesn't enable the `fips` cargo
// feature, so we keep this gated to avoid a guaranteed-failing call.
// -----------------------------------------------------------------------------

#ifdef DD_HTTP_CLIENT_FIPS_TEST
static int subtest_fips_init(void) {
  struct ddog_HttpClientError *err = ddog_http_client_init_fips();
  if (err) {
    fprintf(stderr, "fips init failed: code=%d msg=%s\n",
            (int)err->code, err->msg);
    ddog_http_client_error_free(err);
    return 1;
  }
  // Idempotent: second call must be a no-op.
  err = ddog_http_client_init_fips();
  if (err) {
    fprintf(stderr, "fips init second call returned error: code=%d msg=%s\n",
            (int)err->code, err->msg);
    ddog_http_client_error_free(err);
    return 1;
  }
  return 0;
}
#endif

// -----------------------------------------------------------------------------
// main
// -----------------------------------------------------------------------------

int main(int argc, char **argv) {
  (void)argc;
  (void)argv;

#ifdef DD_HTTP_CLIENT_FIPS_TEST
  // FIPS mode: install the FIPS provider before any builder. Mutually
  // exclusive with the default rustls-ring path.
  if (subtest_fips_init() != 0) {
    fprintf(stderr, "FAIL: fips init smoke failed\n");
    return 1;
  }
#else
  // Default: install the rustls-ring crypto provider exactly once
  // before any builder is allocated. Mandatory since builder_new no
  // longer auto-installs.
  ddog_http_client_install_default_crypto_provider();
#endif

  uint16_t port = 0;
  int listen_fd = spawn_listener(&port);
  if (listen_fd < 0) return 1;

  pid_t pid = fork();
  if (pid < 0) {
    perror("fork");
    close(listen_fd);
    return 1;
  }

  if (pid == 0) {
    // Child: run the server. Exits via signal from parent.
    int rc = run_server_child(listen_fd);
    close(listen_fd);
    _exit(rc);
  }

  // Parent: don't need the listener.
  close(listen_fd);

  char base_url[64];
  snprintf(base_url, sizeof(base_url), "http://127.0.0.1:%u/", (unsigned)port);
  fprintf(stdout, "exercising http_client against %s\n", base_url);

  int rc = 0;
  int part = subtest_get(base_url);
  if (part != 0) {
    fprintf(stderr, "FAIL: GET subtest (rc=%d)\n", part);
    rc = 1;
  } else {
    fprintf(stdout, "PASS: http_client GET 200 pong\n");
  }

  part = subtest_multipart(base_url);
  if (part != 0) {
    fprintf(stderr, "FAIL: multipart subtest (rc=%d)\n", part);
    rc = 1;
  } else {
    fprintf(stdout, "PASS: http_client multipart upload 200 %s\n",
            BODY_UPLOAD_OK);
  }

  part = subtest_retry(base_url);
  if (part != 0) {
    fprintf(stderr, "FAIL: retry subtest (rc=%d)\n", part);
    rc = 1;
  } else {
    fprintf(stdout, "PASS: http_client retry 200 %s after 2x 503\n",
            BODY_FLAKY_OK);
  }

  // Reap child.
  kill(pid, SIGTERM);
  int status = 0;
  waitpid(pid, &status, 0);

  return rc;
}
