// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

#include <datadog/common.h>
#include <datadog/http-client.h>
#include <stdio.h>

// Prints and frees `error`. Returns its code, or -1 if there was no error.
static int consume_error(ddog_HttpClientError *error, const char *what) {
  if (error == NULL) {
    fprintf(stderr, "%s: expected an error\n", what);
    return -1;
  }
  ddog_HttpClientErrorCode code = ddog_http_client_error_code(error);
  ddog_CharSlice message = ddog_http_client_error_message(error);
  printf("%s: %.*s\n", what, (int)message.len, message.ptr);
  ddog_http_client_error_free(error);
  return (int)code;
}

int main(void) {
  ddog_HttpClient *client = NULL;

  ddog_HttpClientConfig missing_url = {.timeout_ms = 1000};
  if (consume_error(ddog_http_client_new(&missing_url, NULL, &client),
                    "config without base_url") != DDOG_HTTP_CLIENT_ERROR_CODE_INVALID_CONFIG) {
    return 1;
  }

  // Nothing listens on port 1, so requests to it are refused.
  ddog_HttpClientConfig config = {
      .base_url = DDOG_CHARSLICE_C("http://127.0.0.1:1"),
      .timeout_ms = 2000,
  };
  ddog_HttpClientError *error = ddog_http_client_new(&config, NULL, &client);
  if (error != NULL) {
    consume_error(error, "client creation");
    return 1;
  }

  ddog_HttpResponse *response = NULL;
  ddog_HttpRequest request = {
      .method = DDOG_HTTP_METHOD_GET,
      .url = DDOG_CHARSLICE_C("/info"),
  };
  if (consume_error(ddog_http_client_send(client, &request, NULL, &response),
                    "refused connection") < 0) {
    return 1;
  }

  ddog_HttpCancellationToken *token = ddog_http_cancel_token_new();
  if (token == NULL) {
    return 1;
  }
  (void)ddog_http_cancel_token_cancel(token);
  int cancelled = consume_error(ddog_http_client_send(client, &request, token, &response),
                                "cancelled request");
  ddog_http_cancel_token_drop(token);
  if (cancelled != DDOG_HTTP_CLIENT_ERROR_CODE_CANCELLED) {
    return 1;
  }

  ddog_HttpRequest relative = {
      .method = DDOG_HTTP_METHOD_GET,
      .url = DDOG_CHARSLICE_C("info"),
  };
  if (consume_error(ddog_http_client_send(client, &relative, NULL, &response),
                    "url without a leading slash") != DDOG_HTTP_CLIENT_ERROR_CODE_INVALID_ARGUMENT) {
    return 1;
  }

  ddog_http_client_drop(client);
  return 0;
}
