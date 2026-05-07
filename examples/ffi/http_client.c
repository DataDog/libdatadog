// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

// http_client.c — exercises the ddog_http_client_* FFI surface end-to-end.
//
// The example is self-contained: it forks a child that runs a tiny
// HTTP/1.1 server on 127.0.0.1, then in the parent it builds a client,
// issues a GET against the child, asserts the response, and tears down.
//
// On success exits 0; on any failure exits non-zero with a message on
// stderr. Suitable for `cargo ffi-test` which only inspects exit codes.

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

static int run_server_child(int listen_fd) {
  // Tight loop: accept one connection, read its request line, write a
  // canned 200/pong response, close, repeat. The parent will SIGTERM us
  // when it's done.
  for (;;) {
    struct sockaddr_in peer;
    socklen_t peer_len = sizeof(peer);
    int conn = accept(listen_fd, (struct sockaddr *)&peer, &peer_len);
    if (conn < 0) {
      if (errno == EINTR) continue;
      perror("accept");
      return 1;
    }

    // Drain request: read until "\r\n\r\n".
    char buf[4096];
    size_t total = 0;
    int saw_end = 0;
    while (total < sizeof(buf)) {
      ssize_t n = read(conn, buf + total, sizeof(buf) - total);
      if (n <= 0) break;
      total += (size_t)n;
      if (total >= 4) {
        for (size_t i = 0; i + 3 < total; i++) {
          if (buf[i] == '\r' && buf[i + 1] == '\n' &&
              buf[i + 2] == '\r' && buf[i + 3] == '\n') {
            saw_end = 1;
            break;
          }
        }
      }
      if (saw_end) break;
    }

    static const char response[] =
        "HTTP/1.1 200 OK\r\n"
        "Content-Type: text/plain\r\n"
        "Content-Length: 4\r\n"
        "Connection: close\r\n"
        "\r\n" BODY_PONG;
    ssize_t to_write = (ssize_t)(sizeof(response) - 1);
    const char *p = response;
    while (to_write > 0) {
      ssize_t n = write(conn, p, (size_t)to_write);
      if (n <= 0) break;
      to_write -= n;
      p += n;
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

static int issue_get(const char *base_url) {
  // Build a request.
  ddog_HttpRequest *req = NULL;
  ddog_CharSlice url_slice = {.ptr = base_url, .len = strlen(base_url)};
  struct ddog_HttpClientError *err =
      ddog_http_request_new(DDOG_HTTP_METHOD_GET, url_slice, &req);
  if (err) {
    fprintf(stderr, "ddog_http_request_new failed: %s\n", err->msg);
    ddog_http_client_error_free(err);
    return 1;
  }

  // Build a SharedRuntime to drive the async backend synchronously.
  const struct ddog_SharedRuntime *runtime = NULL;
  struct ddog_SharedRuntimeFFIError *rt_err = ddog_shared_runtime_new(&runtime);
  if (rt_err) {
    fprintf(stderr, "ddog_shared_runtime_new failed\n");
    ddog_shared_runtime_error_free(rt_err);
    ddog_http_request_drop(req);
    return 1;
  }

  // Build a client.
  ddog_HttpClientBuilder *builder = NULL;
  ddog_http_client_builder_new(&builder);
  ddog_CharSlice base_slice = {.ptr = base_url, .len = strlen(base_url)};
  err = ddog_http_client_builder_set_base_url(builder, base_slice);
  if (err) {
    fprintf(stderr, "set_base_url failed: %s\n", err->msg);
    ddog_http_client_error_free(err);
    ddog_http_client_builder_drop(builder);
    ddog_http_request_drop(req);
    ddog_shared_runtime_free(runtime);
    return 1;
  }
  err = ddog_http_client_builder_set_timeout(builder, 5000);
  if (err) {
    fprintf(stderr, "set_timeout failed: %s\n", err->msg);
    ddog_http_client_error_free(err);
    ddog_http_client_builder_drop(builder);
    ddog_http_request_drop(req);
    ddog_shared_runtime_free(runtime);
    return 1;
  }
  err = ddog_http_client_builder_set_allow_connection_pooling(builder, false);
  if (err) {
    fprintf(stderr, "set_allow_connection_pooling failed: %s\n", err->msg);
    ddog_http_client_error_free(err);
    ddog_http_client_builder_drop(builder);
    ddog_http_request_drop(req);
    ddog_shared_runtime_free(runtime);
    return 1;
  }

  ddog_HttpClient *client = NULL;
  err = ddog_http_client_builder_build(builder, &client);
  if (err) {
    fprintf(stderr, "builder_build failed: %s\n", err->msg);
    ddog_http_client_error_free(err);
    ddog_http_request_drop(req);
    ddog_shared_runtime_free(runtime);
    return 1;
  }

  // Send the request synchronously.
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

  // Assert.
  uint16_t status = ddog_http_response_status(resp);
  if (status != 200) {
    fprintf(stderr, "expected status 200 got %u\n", (unsigned)status);
    ddog_http_response_drop(resp);
    ddog_http_client_drop(client);
    ddog_shared_runtime_free(runtime);
    return 1;
  }

  uintptr_t body_len = 0;
  const uint8_t *body = ddog_http_response_body(resp, &body_len);
  if (body_len != strlen(BODY_PONG) ||
      memcmp(body, BODY_PONG, strlen(BODY_PONG)) != 0) {
    fprintf(stderr, "body mismatch: len=%zu\n", (size_t)body_len);
    ddog_http_response_drop(resp);
    ddog_http_client_drop(client);
    ddog_shared_runtime_free(runtime);
    return 1;
  }

  // Exercise the headers accessor too (no value-level assertions; just
  // verify it doesn't error and that `*_free` is callable).
  struct ddog_HttpHeader *headers = NULL;
  uintptr_t headers_len = 0;
  err = ddog_http_response_headers(resp, &headers, &headers_len);
  if (err) {
    fprintf(stderr, "response_headers failed: %s\n", err->msg);
    ddog_http_client_error_free(err);
    ddog_http_response_drop(resp);
    ddog_http_client_drop(client);
    ddog_shared_runtime_free(runtime);
    return 1;
  }
  ddog_http_response_headers_free(headers, headers_len);

  ddog_http_response_drop(resp);
  ddog_http_client_drop(client);
  ddog_shared_runtime_free(runtime);
  return 0;
}

int main(int argc, char **argv) {
  (void)argc;
  (void)argv;

  // Install the rustls crypto provider exactly once before any builder
  // is allocated. Mandatory after the explicit-init refactor (Task 9a
  // follow-up): builder_new no longer auto-installs.
  ddog_http_client_install_default_crypto_provider();

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

  int rc = issue_get(base_url);

  // Reap child.
  kill(pid, SIGTERM);
  int status = 0;
  waitpid(pid, &status, 0);

  if (rc != 0) {
    fprintf(stderr, "FAIL: http_client smoke failed (rc=%d)\n", rc);
    return rc;
  }
  fprintf(stdout, "PASS: http_client GET 200 pong\n");
  return 0;
}
