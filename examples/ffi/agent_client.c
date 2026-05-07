// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

// agent_client.c — exercises the ddog_agent_client_* FFI surface
// end-to-end.
//
// The example is self-contained: it forks a child that runs a tiny
// HTTP/1.1 server on 127.0.0.1, then in the parent it builds an
// AgentClient, runs three subtests against the child, and tears down.
//
// Subtests:
//   1. agent_info — GET /info on a server that returns a canned JSON
//      body and a `Datadog-Agent-State` header. Asserts that
//      endpoints / version / state_hash / config_json are all parsed
//      out correctly.
//   2. send_traces — PUT /v0.4/traces with a trivial msgpack body;
//      asserts status == 200 and the rate_by_service JSON contains
//      the canned key.
//   3. send_telemetry — POST telemetry to the proxy endpoint; just
//      asserts the call returns success (200).
//
// On success exits 0; on any failure exits non-zero with a message on
// stderr. Suitable for `cargo ffi-test` which inspects exit codes.

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
#include <datadog/agent_client.h>
#include <datadog/shared-runtime.h>

#define INFO_BODY \
    "{\"version\":\"7.50.0\","\
    "\"endpoints\":[\"/v0.4/traces\",\"/v0.5/traces\"],"\
    "\"client_drop_p0s\":true,"\
    "\"config\":{\"hostname\":\"agent-host\",\"default_env\":\"none\"}}"
#define INFO_STATE_HASH "state-001"

#define TRACES_RESPONSE_BODY \
    "{\"rate_by_service\":{\"service:env\":0.5}}"

// --- Tiny HTTP server (forked child) ---------------------------------------

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
      buf[headers_end - 1] = '\0';
      const char *cl = strcasestr(buf, "Content-Length:");
      size_t expected_body = 0;
      if (cl != NULL) {
        cl += strlen("Content-Length:");
        while (*cl == ' ' || *cl == '\t') cl++;
        expected_body = (size_t)strtoul(cl, NULL, 10);
      }
      buf[headers_end - 1] = '\n';
      size_t want = headers_end + expected_body;
      if (total >= want) break;
    }
  }
  buf[total < cap ? total : cap - 1] = '\0';
  return (ssize_t)total;
}

// Returns 1 if `request` is a `method` to a path starting with `path`.
static int is_request_to(const char *request, const char *method,
                          const char *path) {
  size_t mlen = strlen(method);
  if (strncmp(request, method, mlen) != 0) return 0;
  if (request[mlen] != ' ') return 0;
  const char *p = request + mlen + 1;
  size_t plen = strlen(path);
  if (strncmp(p, path, plen) != 0) return 0;
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
    if (n <= 0) { close(conn); continue; }

    if (is_request_to(buf, "GET", "/info")) {
      char ok[1024];
      int len = snprintf(ok, sizeof(ok),
                         "HTTP/1.1 200 OK\r\n"
                         "Content-Type: application/json\r\n"
                         "Datadog-Agent-State: " INFO_STATE_HASH "\r\n"
                         "Datadog-Container-Tags-Hash: tagshash-xyz\r\n"
                         "Content-Length: %zu\r\n"
                         "Connection: close\r\n\r\n%s",
                         strlen(INFO_BODY), INFO_BODY);
      write_all(conn, ok, (size_t)len);
    } else if (is_request_to(buf, "PUT", "/v0.4/traces") ||
               is_request_to(buf, "PUT", "/v0.5/traces")) {
      char ok[256];
      int len = snprintf(ok, sizeof(ok),
                         "HTTP/1.1 200 OK\r\n"
                         "Content-Type: application/json\r\n"
                         "Content-Length: %zu\r\n"
                         "Connection: close\r\n\r\n%s",
                         strlen(TRACES_RESPONSE_BODY),
                         TRACES_RESPONSE_BODY);
      write_all(conn, ok, (size_t)len);
    } else if (is_request_to(buf, "POST", "/telemetry/proxy/api/v2/apmtelemetry")) {
      static const char ok[] =
          "HTTP/1.1 200 OK\r\n"
          "Content-Type: text/plain\r\n"
          "Content-Length: 2\r\n"
          "Connection: close\r\n\r\nok";
      write_all(conn, ok, sizeof(ok) - 1);
    } else {
      static const char nf[] =
          "HTTP/1.1 404 Not Found\r\n"
          "Content-Length: 0\r\n"
          "Connection: close\r\n\r\n";
      write_all(conn, nf, sizeof(nf) - 1);
    }
    shutdown(conn, SHUT_RDWR);
    close(conn);
  }
}

static int spawn_listener(uint16_t *out_port) {
  int fd = socket(AF_INET, SOCK_STREAM, 0);
  if (fd < 0) { perror("socket"); return -1; }
  int yes = 1;
  setsockopt(fd, SOL_SOCKET, SO_REUSEADDR, &yes, sizeof(yes));

  struct sockaddr_in addr = {0};
  addr.sin_family = AF_INET;
  addr.sin_addr.s_addr = htonl(INADDR_LOOPBACK);
  addr.sin_port = 0;
  if (bind(fd, (struct sockaddr *)&addr, sizeof(addr)) < 0) {
    perror("bind"); close(fd); return -1;
  }
  if (listen(fd, 8) < 0) {
    perror("listen"); close(fd); return -1;
  }
  struct sockaddr_in bound;
  socklen_t bound_len = sizeof(bound);
  if (getsockname(fd, (struct sockaddr *)&bound, &bound_len) < 0) {
    perror("getsockname"); close(fd); return -1;
  }
  *out_port = ntohs(bound.sin_port);
  return fd;
}

// --- Helpers ---------------------------------------------------------------

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

// Helper: build a charslice from a C string.
static ddog_CharSlice cs(const char *s) {
  ddog_CharSlice r = {.ptr = s, .len = strlen(s)};
  return r;
}

// Helper: construct an AgentClient pointed at 127.0.0.1:port. Returns NULL
// on failure with stderr message.
static ddog_AgentClient *make_client(uint16_t port) {
  ddog_AgentClientBuilder *builder = NULL;
  ddog_agent_client_builder_new(&builder);

  struct ddog_AgentClientError *err =
      ddog_agent_client_builder_set_http_endpoint(builder, cs("127.0.0.1"), port);
  if (err) goto fail;
  err = ddog_agent_client_builder_set_timeout_millis(builder, 5000);
  if (err) goto fail;
  err = ddog_agent_client_builder_set_allow_connection_pooling(builder, false);
  if (err) goto fail;

  ddog_LanguageMetadata *metadata = NULL;
  err = ddog_language_metadata_new(
      cs("ruby"), cs("3.2.0"), cs("MRI"), cs("2.0.0"), &metadata);
  if (err) goto fail;
  err = ddog_agent_client_builder_set_language_metadata(builder, metadata);
  if (err) goto fail;

  ddog_AgentClient *client = NULL;
  err = ddog_agent_client_builder_build(builder, &client);
  if (err) {
    fprintf(stderr, "agent_client builder_build failed: %s\n", err->msg);
    ddog_agent_client_error_free(err);
    return NULL;
  }
  return client;

fail:
  fprintf(stderr, "agent_client builder configuration failed: %s\n",
          err ? err->msg : "?");
  if (err) ddog_agent_client_error_free(err);
  ddog_agent_client_builder_drop(builder);
  return NULL;
}

// --- Subtest 1: agent_info -------------------------------------------------

static int subtest_agent_info(uint16_t port) {
  const struct ddog_SharedRuntime *runtime = make_runtime();
  if (!runtime) return 1;

  ddog_AgentClient *client = make_client(port);
  if (!client) {
    ddog_shared_runtime_free(runtime);
    return 1;
  }

  ddog_AgentInfo *info = NULL;
  struct ddog_AgentClientError *err = ddog_agent_client_agent_info_blocking(
      client, (struct ddog_SharedRuntime *)runtime, &info);
  if (err) {
    fprintf(stderr, "agent_info_blocking failed: %s\n", err->msg);
    ddog_agent_client_error_free(err);
    ddog_agent_client_drop(client);
    ddog_shared_runtime_free(runtime);
    return 1;
  }
  if (!info) {
    fprintf(stderr, "agent_info: server returned 404 unexpectedly\n");
    ddog_agent_client_drop(client);
    ddog_shared_runtime_free(runtime);
    return 1;
  }

  int rc = 0;

  // endpoints
  struct ddog_StringSlice endpoints = ddog_agent_info_endpoints(info);
  if (endpoints.len != 2) {
    fprintf(stderr, "agent_info: expected 2 endpoints got %zu\n",
            (size_t)endpoints.len);
    rc = 1;
  } else {
    if (endpoints.ptr[0].len != strlen("/v0.4/traces") ||
        memcmp(endpoints.ptr[0].ptr, "/v0.4/traces",
               strlen("/v0.4/traces")) != 0) {
      fprintf(stderr, "agent_info: endpoint[0] mismatch\n");
      rc = 1;
    }
  }
  ddog_string_slice_drop(endpoints);

  // client_drop_p0s
  if (!ddog_agent_info_client_drop_p0s(info)) {
    fprintf(stderr, "agent_info: client_drop_p0s expected true\n");
    rc = 1;
  }

  // version
  struct ddog_StringWrapper *version = ddog_agent_info_version(info);
  if (!version) {
    fprintf(stderr, "agent_info: version expected non-null\n");
    rc = 1;
  } else {
    ddog_CharSlice v = ddog_StringWrapper_message(version);
    if (v.len != strlen("7.50.0") ||
        memcmp(v.ptr, "7.50.0", strlen("7.50.0")) != 0) {
      fprintf(stderr, "agent_info: version mismatch\n");
      rc = 1;
    }
    ddog_StringWrapper_drop(version);
  }

  // state_hash
  struct ddog_StringWrapper *state = ddog_agent_info_state_hash(info);
  if (!state) {
    fprintf(stderr, "agent_info: state_hash expected non-null\n");
    rc = 1;
  } else {
    ddog_CharSlice s = ddog_StringWrapper_message(state);
    if (s.len != strlen(INFO_STATE_HASH) ||
        memcmp(s.ptr, INFO_STATE_HASH, strlen(INFO_STATE_HASH)) != 0) {
      fprintf(stderr, "agent_info: state_hash mismatch\n");
      rc = 1;
    }
    ddog_StringWrapper_drop(state);
  }

  // config_json — just confirm it's non-null and contains a known key.
  struct ddog_StringWrapper *config = ddog_agent_info_config_json(info);
  if (!config) {
    fprintf(stderr, "agent_info: config_json expected non-null\n");
    rc = 1;
  } else {
    ddog_CharSlice cj = ddog_StringWrapper_message(config);
    // Look for "hostname" substring in the JSON.
    char *cj_owned = malloc(cj.len + 1);
    memcpy(cj_owned, cj.ptr, cj.len);
    cj_owned[cj.len] = '\0';
    if (strstr(cj_owned, "\"hostname\"") == NULL ||
        strstr(cj_owned, "agent-host") == NULL) {
      fprintf(stderr, "agent_info: config_json missing keys (got %s)\n",
              cj_owned);
      rc = 1;
    }
    free(cj_owned);
    ddog_StringWrapper_drop(config);
  }

  // container_tags_hash
  struct ddog_StringWrapper *tags = ddog_agent_info_container_tags_hash(info);
  if (!tags) {
    fprintf(stderr, "agent_info: container_tags_hash expected non-null\n");
    rc = 1;
  } else {
    ddog_StringWrapper_drop(tags);
  }

  ddog_agent_info_drop(info);
  ddog_agent_client_drop(client);
  ddog_shared_runtime_free(runtime);
  return rc;
}

// --- Subtest 2: send_traces ------------------------------------------------

static int subtest_send_traces(uint16_t port) {
  const struct ddog_SharedRuntime *runtime = make_runtime();
  if (!runtime) return 1;

  ddog_AgentClient *client = make_client(port);
  if (!client) {
    ddog_shared_runtime_free(runtime);
    return 1;
  }

  // Trivial msgpack-shaped payload — the server doesn't actually
  // decode it, just echoes a canned response.
  static const uint8_t payload[] = {0x90 /* empty array */};
  ddog_ByteSlice payload_slice = {.ptr = payload, .len = sizeof(payload)};

  struct ddog_TraceSendOptions opts = {.computed_top_level = false};

  ddog_AgentResponse *resp = NULL;
  struct ddog_AgentClientError *err =
      ddog_agent_client_send_traces_blocking(
          client, payload_slice, 0,
          DDOG_TRACE_FORMAT_MSGPACK_V4, opts,
          (struct ddog_SharedRuntime *)runtime, &resp);
  if (err) {
    fprintf(stderr, "send_traces_blocking failed: code=%d msg=%s\n",
            (int)err->code, err->msg);
    ddog_agent_client_error_free(err);
    ddog_agent_client_drop(client);
    ddog_shared_runtime_free(runtime);
    return 1;
  }

  int rc = 0;
  uint16_t status = ddog_agent_response_status(resp);
  if (status != 200) {
    fprintf(stderr, "send_traces: expected 200 got %u\n", (unsigned)status);
    rc = 1;
  }

  // Verify we get a non-null rate_by_service JSON.
  struct ddog_StringWrapper *rates =
      ddog_agent_response_rate_by_service_json(resp);
  if (!rates) {
    fprintf(stderr, "send_traces: rate_by_service_json was null\n");
    rc = 1;
  } else {
    ddog_CharSlice s = ddog_StringWrapper_message(rates);
    char *owned = malloc(s.len + 1);
    memcpy(owned, s.ptr, s.len);
    owned[s.len] = '\0';
    if (strstr(owned, "service:env") == NULL) {
      fprintf(stderr, "send_traces: rate_by_service_json missing "
                      "service:env (got %s)\n", owned);
      rc = 1;
    }
    free(owned);
    ddog_StringWrapper_drop(rates);
  }

  ddog_agent_response_drop(resp);
  ddog_agent_client_drop(client);
  ddog_shared_runtime_free(runtime);
  return rc;
}

// --- Subtest 3: send_telemetry ---------------------------------------------

static int subtest_send_telemetry(uint16_t port) {
  const struct ddog_SharedRuntime *runtime = make_runtime();
  if (!runtime) return 1;

  ddog_AgentClient *client = make_client(port);
  if (!client) {
    ddog_shared_runtime_free(runtime);
    return 1;
  }

  static const uint8_t body[] = "{\"event\":\"app-started\"}";
  ddog_ByteSlice body_slice = {.ptr = body, .len = sizeof(body) - 1};

  ddog_TelemetryRequest *req = NULL;
  struct ddog_AgentClientError *err = ddog_telemetry_request_new(
      cs("app-started"), cs("v2"), body_slice, false, &req);
  if (err) {
    fprintf(stderr, "telemetry_request_new failed: %s\n", err->msg);
    ddog_agent_client_error_free(err);
    ddog_agent_client_drop(client);
    ddog_shared_runtime_free(runtime);
    return 1;
  }

  err = ddog_agent_client_send_telemetry_blocking(
      client, req, (struct ddog_SharedRuntime *)runtime);
  // `req` is consumed by the call regardless of success/failure.
  if (err) {
    fprintf(stderr, "send_telemetry_blocking failed: code=%d msg=%s\n",
            (int)err->code, err->msg);
    ddog_agent_client_error_free(err);
    ddog_agent_client_drop(client);
    ddog_shared_runtime_free(runtime);
    return 1;
  }

  ddog_agent_client_drop(client);
  ddog_shared_runtime_free(runtime);
  return 0;
}

// --- main ------------------------------------------------------------------

int main(int argc, char **argv) {
  (void)argc;
  (void)argv;

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
    int rc = run_server_child(listen_fd);
    close(listen_fd);
    _exit(rc);
  }

  close(listen_fd);

  fprintf(stdout, "exercising agent_client against 127.0.0.1:%u\n",
          (unsigned)port);

  int rc = 0;
  int part = subtest_agent_info(port);
  if (part != 0) {
    fprintf(stderr, "FAIL: agent_info subtest (rc=%d)\n", part);
    rc = 1;
  } else {
    fprintf(stdout, "PASS: agent_client agent_info\n");
  }

  part = subtest_send_traces(port);
  if (part != 0) {
    fprintf(stderr, "FAIL: send_traces subtest (rc=%d)\n", part);
    rc = 1;
  } else {
    fprintf(stdout, "PASS: agent_client send_traces 200\n");
  }

  part = subtest_send_telemetry(port);
  if (part != 0) {
    fprintf(stderr, "FAIL: send_telemetry subtest (rc=%d)\n", part);
    rc = 1;
  } else {
    fprintf(stdout, "PASS: agent_client send_telemetry 200\n");
  }

  kill(pid, SIGTERM);
  int status = 0;
  waitpid(pid, &status, 0);

  return rc;
}
