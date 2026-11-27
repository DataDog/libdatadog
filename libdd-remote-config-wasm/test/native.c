/* Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
 * SPDX-License-Identifier: Apache-2.0
 */

#include "ddog_remote_config.h"

#include <assert.h>
#include <inttypes.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#ifdef _WIN32
#include <process.h>
#include <windows.h>
#else
#include <pthread.h>
#endif

struct pending_poll {
    ddog_rc_reactor *reactor;
    int32_t client;
    int32_t http;
    int32_t timer;
    const char *const *expected_request;
};

static int32_t input_bytes(ddog_rc_reactor *reactor, const void *value, size_t length) {
    assert(length <= INT32_MAX);
    int32_t handle = ddog_rc_alloc(reactor, (int32_t)length);
    assert(handle > 0);
    uint8_t *destination = ddog_rc_input_bytes(reactor, handle);
    assert(destination != NULL);
    memcpy(destination, value, length);
    return handle;
}

static int32_t input(ddog_rc_reactor *reactor, const char *value) {
    return input_bytes(reactor, value, strlen(value));
}

static char *take(ddog_rc_reactor *reactor, int32_t handle) {
    assert(handle > 0);
    int32_t length = ddog_rc_buffer_len(reactor, handle);
    assert(length >= 0);
    char *result = malloc((size_t)length + 1);
    assert(result != NULL);
    const uint8_t *source = ddog_rc_buffer_bytes(reactor, handle);
    assert(source != NULL);
    memcpy(result, source, (size_t)length);
    result[length] = 0;
    assert(ddog_rc_free(reactor, handle) == 0);
    return result;
}

static int32_t read_id(const char *json, const char *key) {
    const char *field = strstr(json, key);
    assert(field != NULL);
    long value = strtol(field + strlen(key), NULL, 10);
    assert(value > 0 && value <= INT32_MAX);
    return (int32_t)value;
}

static void start_poll(struct pending_poll *poll) {
    ddog_rc_reactor *reactor = poll->reactor;
    assert(ddog_rc_poll(reactor, poll->client) == 0);

    int32_t event = ddog_rc_event(reactor, poll->client);
    assert(event > 0);
    assert(ddog_rc_input_bytes(reactor, event) == NULL);
    char *error = take(reactor, ddog_rc_error(reactor));
    assert(strstr(error, "read-only buffer") != NULL);
    free(error);

    char *http = take(reactor, event);
    assert(strstr(http, "\"kind\":\"http\"") != NULL);
    assert(strstr(http, "\"url\":\"http://127.0.0.1:8126/prefix%2Ftenant/v0.7/config\"") != NULL);
    poll->http = read_id(http, "\"id\":");
    char *request = take(reactor, read_id(http, "\"body\":"));
    for (const char *const *field = poll->expected_request; *field != NULL; field++) {
        assert(strstr(request, *field) != NULL);
    }
    free(request);
    free(http);

    char *timer = take(reactor, ddog_rc_event(reactor, poll->client));
    assert(strstr(timer, "\"kind\":\"sleep\"") != NULL);
    poll->timer = read_id(timer, "\"id\":");
    free(timer);
    assert(ddog_rc_event(reactor, poll->client) == 0);
    assert(ddog_rc_result(reactor, poll->client) == 0);
}

#ifdef _WIN32
static unsigned __stdcall start_poll_thread(void *data) {
    start_poll(data);
    return 0;
}
#else
static void *start_poll_thread(void *data) {
    start_poll(data);
    return NULL;
}
#endif

static void finish_no_change(const struct pending_poll *poll) {
    ddog_rc_reactor *reactor = poll->reactor;
    int32_t metadata = input(reactor, "{\"status\":204}");
    assert(ddog_rc_http_succeed(reactor, poll->client, poll->http, metadata, 0) == 0);
    assert(ddog_rc_free(reactor, metadata) == 0);
    free(take(reactor, ddog_rc_event(reactor, poll->client)));
    char *result = take(reactor, ddog_rc_result(reactor, poll->client));
    assert(strstr(result, "\"batch\":0") != NULL);
    assert(strstr(result, "\"error\":") == NULL);
    free(result);
}

static void verify_apply_plan(struct pending_poll *poll) {
    ddog_rc_reactor *reactor = poll->reactor;
    start_poll(poll);
    int32_t metadata = input(reactor, "{\"status\":200}");
    int32_t body = input(reactor,
        "{\"targets\":\"eyJzaWduZWQiOnsiY3VzdG9tIjp7Im9wYXF1ZV9iYWNrZW5kX3N0YXRlIjoidGVzdCJ9LCJ0YXJn"
        "ZXRzIjp7ImRhdGFkb2cvMi9BUE1fVFJBQ0lORy9uYXRpdmUvY29uZmlnIjp7ImN1c3RvbSI6eyJ2"
        "IjoxfSwiaGFzaGVzIjp7InNoYTI1NiI6IjJjZjI0ZGJhNWZiMGEzMGUyNmU4M2IyYWM1YjllMjll"
        "MWIxNjFlNWMxZmE3NDI1ZTczMDQzMzYyOTM4Yjk4MjQifSwibGVuZ3RoIjo1fX0sInZlcnNpb24i"
        "OjF9fQ==\",\"target_files\":[{\"path\":\"datadog/2/APM_TRACING/native/config\",\"raw\":\"aGVsbG8=\"}],"
        "\"client_configs\":[\"datadog/2/APM_TRACING/native/config\"]}");
    assert(ddog_rc_http_succeed(reactor, poll->client, poll->http, metadata, body) == 0);
    assert(ddog_rc_free(reactor, metadata) == 0);
    free(take(reactor, ddog_rc_event(reactor, poll->client)));
    char *result = take(reactor, ddog_rc_result(reactor, poll->client));
    int32_t batch = read_id(result, "\"batch\":");
    free(result);

    const uint8_t decision[] = {1, 0, 0, 0, 0, 0};
    int32_t plan = input_bytes(reactor, decision, sizeof(decision));
    struct pending_poll pending = *poll;
    const char *const pending_request[] = {"\"apply_state\":1", NULL};
    pending.expected_request = pending_request;
    start_poll(&pending);
    assert(ddog_rc_apply_plan(reactor, poll->client, batch, plan) == -1);
    finish_no_change(&pending);

    assert(ddog_rc_apply_plan(reactor, poll->client, batch, plan) == 1);
    assert(ddog_rc_free(reactor, plan) == 0);
    assert(ddog_rc_view_release(reactor, batch) == 0);
    const char *const applied_request[] = {"\"apply_state\":2", NULL};
    pending.expected_request = applied_request;
    start_poll(&pending);
    finish_no_change(&pending);
}

static void verify_deferred_receipt(struct pending_poll *poll) {
    ddog_rc_reactor *reactor = poll->reactor;
    int32_t snapshot = ddog_rc_snapshot(reactor, poll->client);
    assert(snapshot > 0);
    char *description = take(reactor, ddog_rc_view_describe(reactor, snapshot));
    assert(read_id(description, "\"receiptOwner\":") == poll->client);
    assert(strstr(description, "\"generation\":\"1\"") != NULL);
    free(description);
    assert(ddog_rc_view_release(reactor, snapshot) == 0);

    char receipt[192];
    int length = snprintf(receipt, sizeof(receipt),
        "{\"receiptOwner\":%" PRId32 ",\"outcomes\":[{\"path\":\"datadog/2/APM_TRACING/native/config\","
        "\"generation\":\"1\",\"state\":3,\"message\":\"deferred failure\"}]}", poll->client);
    assert(length > 0 && (size_t)length < sizeof(receipt));
    int32_t plan = input_bytes(reactor, receipt, (size_t)length);
    start_poll(poll);
    assert(ddog_rc_apply_receipts(reactor, poll->client, plan) == -1);
    finish_no_change(poll);
    assert(ddog_rc_apply_receipts(reactor, poll->client, plan) == 1);
    assert(ddog_rc_free(reactor, plan) == 0);

    const char *const request[] = {"\"apply_state\":3", "\"apply_error\":\"deferred failure\"", NULL};
    struct pending_poll pending = *poll;
    pending.expected_request = request;
    start_poll(&pending);
    finish_no_change(&pending);
}

static void verify_watch(struct pending_poll *poll) {
    ddog_rc_reactor *reactor = poll->reactor;
    int32_t options = input(reactor, "{\"delivery\":\"latestState\",\"products\":[\"APM_TRACING\"]}");
    int32_t watch = ddog_rc_watch_open(reactor, poll->client, options);
    assert(watch > 0);
    assert(ddog_rc_free(reactor, options) == 0);
    int32_t view = ddog_rc_watch_next(reactor, poll->client, watch);
    assert(view > 0);
    char *description = take(reactor, ddog_rc_view_describe(reactor, view));
    assert(strstr(description, "\"reason\":\"replay\"") != NULL);
    free(description);
    const uint8_t decision[] = {0, 0, 0, 0, 0, 0};
    int32_t plan = input_bytes(reactor, decision, sizeof(decision));
    struct pending_poll pending = *poll;
    start_poll(&pending);
    assert(ddog_rc_watch_finish(reactor, poll->client, watch, view, plan) == -1);
    finish_no_change(&pending);
    assert(ddog_rc_watch_finish(reactor, poll->client, watch, view, plan) == 1);
    assert(ddog_rc_free(reactor, plan) == 0);
    assert(ddog_rc_view_describe(reactor, view) == -1);
    assert(ddog_rc_watch_close(reactor, poll->client, watch) == 1);
}

int main(void) {
    assert(ddog_rc_version() == 1);
    assert(ddog_rc_alloc(NULL, 1) == -1);
    ddog_rc_reactor_drop(NULL);

    ddog_rc_reactor *reactor = ddog_rc_reactor_new();
    ddog_rc_reactor *other = ddog_rc_reactor_new();
    assert(reactor != NULL && other != NULL);
    int32_t config = input(reactor,
        "{\"target\":{\"service\":\"test\",\"env\":\"test\",\"appVersion\":\"1\"},"
        "\"identity\":{\"clientId\":\"setup-client\",\"runtimeId\":\"setup-runtime\"},"
        "\"interest\":{\"products\":[],\"capabilityIndices\":[]},"
        "\"language\":\"go\",\"tracerVersion\":\"1\","
        "\"url\":\"http://127.0.0.1:8126/prefix%2Ftenant/?unused=1\",\"timeoutMs\":2000}");
    assert(ddog_rc_buffer_len(other, config) == -1);
    ddog_rc_reactor_drop(other);
    int32_t client = ddog_rc_open(reactor, config);
    assert(client > 0);
    assert(ddog_rc_free(reactor, config) == 0);

    int32_t initial_interest = input(reactor, "{\"products\":[\"APM_TRACING\"],\"capabilityIndices\":[12]}");
    int32_t initial_identity = input(reactor,
        "{\"clientId\":\"native-client\",\"runtimeId\":\"native-runtime\","
        "\"processTags\":[\"svc.auto:before\"]}");
    assert(ddog_rc_set_interest(reactor, client, initial_interest) == 0);
    assert(ddog_rc_set_identity(reactor, client, initial_identity) == 0);
    assert(ddog_rc_free(reactor, initial_interest) == 0);
    assert(ddog_rc_free(reactor, initial_identity) == 0);

    const char *const initial_request[] = {
        "\"id\":\"native-client\"",
        "\"runtime_id\":\"native-runtime\"",
        "\"process_tags\":[\"svc.auto:before\"]",
        "\"products\":[\"APM_TRACING\"]",
        "\"capabilities\":[16,0]",
        NULL,
    };
    struct pending_poll poll = {reactor, client, 0, 0, initial_request};
#ifdef _WIN32
    HANDLE worker = (HANDLE)_beginthreadex(NULL, 0, start_poll_thread, &poll, 0, NULL);
    assert(worker != NULL);
    assert(WaitForSingleObject(worker, INFINITE) == WAIT_OBJECT_0);
    assert(CloseHandle(worker));
#else
    pthread_t worker;
    assert(pthread_create(&worker, NULL, start_poll_thread, &poll) == 0);
    assert(pthread_join(worker, NULL) == 0);
#endif

    int32_t interest = input(reactor, "{\"products\":[\"AGENT_CONFIG\"],\"capabilityIndices\":[46]}");
    int32_t identity = input(reactor,
        "{\"clientId\":\"updated-client\",\"runtimeId\":\"updated-runtime\","
        "\"tags\":[\"region:west\"],\"extraServices\":[\"dependent-service\"],"
        "\"processTags\":[\"svc.user:true\"]}");
    assert(ddog_rc_set_interest(reactor, client, interest) == -1);
    assert(ddog_rc_set_identity(reactor, client, identity) == -1);

    int32_t metadata = input(reactor, "{\"status\":200}");
    int32_t body = input(reactor, "{}");
    assert(ddog_rc_http_succeed(reactor, client, poll.timer, metadata, body) == -1);
    assert(ddog_rc_buffer_len(reactor, body) == 2);
    assert(ddog_rc_http_succeed(reactor, client, poll.http, metadata, body) == 0);
    assert(ddog_rc_buffer_len(reactor, body) == -1);
    assert(ddog_rc_free(reactor, metadata) == 0);

    char *cancel = take(reactor, ddog_rc_event(reactor, client));
    assert(strstr(cancel, "\"kind\":\"cancel\"") != NULL);
    assert(read_id(cancel, "\"id\":") == poll.timer);
    free(cancel);
    char *result = take(reactor, ddog_rc_result(reactor, client));
    assert(strstr(result, "\"batch\":0") != NULL);
    assert(strstr(result, "\"error\":") == NULL);
    free(result);

    start_poll(&poll);
    finish_no_change(&poll);
    verify_apply_plan(&poll);
    verify_deferred_receipt(&poll);
    verify_watch(&poll);

    assert(ddog_rc_set_interest(reactor, client, interest) == 0);
    assert(ddog_rc_set_identity(reactor, client, identity) == 0);
    assert(ddog_rc_free(reactor, interest) == 0);
    assert(ddog_rc_free(reactor, identity) == 0);
    const char *const updated_request[] = {
        "\"id\":\"updated-client\"",
        "\"runtime_id\":\"updated-runtime\"",
        "\"process_tags\":[\"svc.user:true\"]",
        "\"products\":[\"AGENT_CONFIG\"]",
        "\"capabilities\":[64,0,0,0,0,0]",
        "\"tags\":[\"region:west\"]",
        "\"extra_services\":[\"dependent-service\"]",
        NULL,
    };
    poll.expected_request = updated_request;
    start_poll(&poll);
    finish_no_change(&poll);

    assert(ddog_rc_poll(reactor, client) == 0);
    assert(ddog_rc_cancel(reactor, client) == 0);
    assert(ddog_rc_event(reactor, client) == 0);
    result = take(reactor, ddog_rc_result(reactor, client));
    assert(strstr(result, "poll cancelled") != NULL);
    free(result);
    assert(ddog_rc_close(reactor, client) == 0);
    assert(ddog_rc_poll(reactor, client) == -1);
    ddog_rc_reactor_drop(reactor);
}
