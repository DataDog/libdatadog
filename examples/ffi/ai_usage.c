// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

// LLM usage and cost metrics through the C interface: one observation
// projected, a request batched and taken as DogStatsD datagrams and intake
// series, a provider stream read as it passes, and the error a rejected
// input returns.

#include <datadog/common.h>
#include <datadog/ai-usage.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

static const char *ATTEMPT =
    "{\"profile\": \"gen_ai.client.provider_attempt@0.1.0\","
    " \"observation\": {\"operation_name\": \"chat\", \"provider_name\": \"openai\","
    " \"duration_seconds\": 0.4, \"streaming\": false, \"input_tokens\": 10,"
    " \"input_basis\": \"includes_cache\", \"output_tokens\": 2,"
    " \"output_basis\": \"includes_reasoning\", \"observation_point\": \"gateway\"}}";

static ddog_CharSlice text(const char *value) {
  ddog_CharSlice slice = {.ptr = value, .len = strlen(value)};
  return slice;
}

static ddog_ByteSlice bytes(const char *value) {
  ddog_ByteSlice slice = {.ptr = (const uint8_t *)value, .len = strlen(value)};
  return slice;
}

static ddog_StringWrapper empty_output(void) {
  ddog_StringWrapper out;
  memset(&out, 0, sizeof out);
  return out;
}

// Print and release an error; 1 when there was one.
static int failed(const char *what, ddog_ai_usage_Error *error) {
  if (error == NULL) {
    return 0;
  }
  ddog_CharSlice message = ddog_Error_message(&error->message);
  fprintf(stderr, "%s failed: %s: %.*s\n", what, ddog_ai_usage_error_code_name(error),
          (int)message.len, message.ptr);
  ddog_ai_usage_error_free(error);
  return 1;
}

#define TRY(what, expr)                                                                            \
  do {                                                                                             \
    if (failed(what, expr)) {                                                                      \
      return 1;                                                                                    \
    }                                                                                              \
  } while (0)

// Print an output, then release it.
static void print_output(const char *label, ddog_StringWrapper *out) {
  ddog_CharSlice message = ddog_StringWrapper_message(out);
  int shown = message.len > 120 ? 120 : (int)message.len;
  printf("%s (%zu bytes): %.*s%s\n", label, (size_t)message.len, shown, message.ptr,
         message.len > 120 ? "..." : "");
  ddog_StringWrapper_drop(out);
}

int main(void) {
  if (ddog_ai_usage_abi_version() != ddog_ai_usage_ABI_VERSION) {
    fprintf(stderr, "ABI version %u, expected %u\n", ddog_ai_usage_abi_version(),
            (unsigned)ddog_ai_usage_ABI_VERSION);
    return 1;
  }

  // One observation to its Datadog API series.
  ddog_StringWrapper out = empty_output();
  TRY("project", ddog_ai_usage_project(text(ATTEMPT), &out));
  print_output("project", &out);

  // A request's documents batched, then taken at a flush.
  ddog_ai_usage_Handle_Batch batch;
  TRY("batch_new", ddog_ai_usage_batch_new(&batch));
  ddog_ai_usage_batch_set_writer(&batch, DDOG_CHARSLICE_C("gateway-1"), 42);
  char request[1024];
  snprintf(request, sizeof request, "[%s]", ATTEMPT);
  for (int i = 0; i < 3; i++) {
    TRY("batch_add_request", ddog_ai_usage_batch_add_request(&batch, text(request), NULL));
  }
  printf("batch holds %zu points\n", (size_t)ddog_ai_usage_batch_len(&batch));

  uintptr_t dropped_lines = 0;
  out = empty_output();
  TRY("batch_take_dogstatsd_text",
      ddog_ai_usage_batch_take_dogstatsd_text(&batch, DDOG_CHARSLICE_C("env:prod"),
                                              ddog_ai_usage_UDP_MAX_PAYLOAD, &dropped_lines,
                                              &out));
  ddog_CharSlice datagrams = ddog_StringWrapper_message(&out);
  size_t count = datagrams.len > 0 ? 1 : 0;
  for (size_t i = 0; i < datagrams.len; i++) {
    count += datagrams.ptr[i] == '\0';
  }
  printf("%zu DogStatsD datagrams, %zu lines dropped\n", count, (size_t)dropped_lines);
  if (count == 0 || !ddog_ai_usage_batch_is_empty(&batch)) {
    fprintf(stderr, "the batch was not taken\n");
    return 1;
  }
  ddog_StringWrapper_drop(&out);

  TRY("batch_add_request", ddog_ai_usage_batch_add_request(&batch, text(request), NULL));
  out = empty_output();
  TRY("batch_take_intake", ddog_ai_usage_batch_take_intake(&batch, 1700000000, &out));
  print_output("intake", &out);
  ddog_ai_usage_batch_drop(&batch);
  // Released handles are null, so a second release is a no-op.
  ddog_ai_usage_batch_drop(&batch);

  // A provider stream, read as it passes through in chunks that end
  // anywhere.
  const char *start = "event: message_start\ndata: {\"type\": \"message_start\", \"message\": "
                      "{\"model\": \"claude-sonnet-4-5\", \"usage\": {\"input_tokens\": 10, "
                      "\"output_tokens\": 1}}}\n\n";
  const char *delta = "event: message_delta\ndata: {\"type\": \"message_delta\", \"usage\": "
                      "{\"output_";
  const char *stop = "tokens\": 7}}\n\nevent: message_stop\ndata: {\"type\": \"message_stop\"}\n\n";
  const char *chunks[] = {start, delta, stop};
  ddog_ai_usage_Handle_Stream stream;
  TRY("stream_new", ddog_ai_usage_stream_new(DDOG_CHARSLICE_C("anthropic"), false, &stream));
  for (size_t i = 0; i < sizeof chunks / sizeof chunks[0]; i++) {
    TRY("stream_feed", ddog_ai_usage_stream_feed(&stream, bytes(chunks[i])));
  }
  if (ddog_ai_usage_stream_outcome(&stream) != DDOG_AI_USAGE_OUTCOME_COMPLETED) {
    fprintf(stderr, "the stream did not complete\n");
    return 1;
  }
  out = empty_output();
  TRY("stream_take", ddog_ai_usage_stream_take(&stream, &out));
  print_output("stream", &out);

  // A rejected input returns its stable code, and leaves the output empty.
  out = empty_output();
  ddog_ai_usage_Error *error =
      ddog_ai_usage_project(DDOG_CHARSLICE_C("{\"profile\": \"nope@1\"}"), &out);
  if (error == NULL || error->code != DDOG_AI_USAGE_ERROR_CODE_PROFILE_INVALID ||
      strcmp(ddog_ai_usage_error_code_name(error), "profile_invalid") != 0 ||
      ddog_StringWrapper_message(&out).len != 0) {
    fprintf(stderr, "the rejection was not reported\n");
    return 1;
  }
  ddog_CharSlice message = ddog_Error_message(&error->message);
  printf("rejected as expected: %s: %.*s\n", ddog_ai_usage_error_code_name(error),
         (int)message.len, message.ptr);
  ddog_ai_usage_error_free(error);
  ddog_StringWrapper_drop(&out);

  // A name the vocabulary does not know.
  out = empty_output();
  error = ddog_ai_usage_provider_name(DDOG_CHARSLICE_C("1"), &out);
  if (error == NULL || error->code != DDOG_AI_USAGE_ERROR_CODE_NO_MATCH) {
    fprintf(stderr, "an unknown provider name was not reported\n");
    return 1;
  }
  ddog_ai_usage_error_free(error);
  ddog_StringWrapper_drop(&out);

  printf("ai_usage example done\n");
  return 0;
}
