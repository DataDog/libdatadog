// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

#include <datadog/crashtracker.h>
#include <fcntl.h>
#include <signal.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>

#define MAX_FILE_PATH 512

int main(void) {
  const char *output_dir = getenv("DDOG_CRASHT_TEST_OUTPUT_DIR");
  if (!output_dir || output_dir[0] == '\0') {
    output_dir = "/tmp/crashreports";
  }

  char report_path[MAX_FILE_PATH];
  snprintf(report_path, sizeof(report_path), "%s/signal_safe_crashreport.txt", output_dir);

  int report_fd = open(report_path, O_CREAT | O_TRUNC | O_WRONLY, 0600);
  if (report_fd < 0) {
    perror("open signal-safe report");
    return EXIT_FAILURE;
  }

  struct ddog_crasht_SignalSafeConfig config = {
      .receiver_path = "/definitely/missing-signal-safe-receiver",
      .service = "signal-safe-ffi-test",
      .env = "test",
      .app_version = "1",
      .runtime_id = "00000000-0000-0000-0000-000000000001",
      .platform = "host",
      .library_name = "signal-safe-ffi-test",
      .library_version = "1.0.0",
      .family = "native",
      .default_service = "signal-safe-ffi-test",
      .force_on_top = false,
      .only_bootstrap = false,
      .debug_logging = false,
      .create_alt_stack = false,
      .use_alt_stack = false,
      .block_signals = true,
      .disarm_on_entry = false,
      .report_fd = report_fd,
      .collector_reap_ms = 500,
      .receiver_timeout_secs = 5,
      .max_frames = 32,
      .close_fds_on_receiver = true,
      .probe_seccomp = false,
  };

  if (ddog_crasht_signal_safe_init(&config) != DDOG_CRASHT_SIGNAL_SAFE_INIT_RESULT_ENABLED) {
    fprintf(stderr, "signal-safe crashtracker init failed\n");
    close(report_fd);
    return EXIT_FAILURE;
  }

  ddog_crasht_signal_safe_bootstrap_complete();
  raise(SIGABRT);
  return EXIT_FAILURE;
}
