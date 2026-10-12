// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

#include "sidecar.h"

int main(void) {
  ddog_FfeScalarAttribute attribute = {
    .key = DDOG_CHARSLICE_C_BARE("plan"),
    .kind = 0,
    .string_value = DDOG_CHARSLICE_C_BARE("pro")
  };
  ddog_Slice_FfeScalarAttribute attributes = { .ptr = &attribute, .len = 1 };
  ddog_FfeSnapshotState snapshot = { 0 };
  ddog_FfeSubmissionStatus status = ddog_sidecar_check_ffe_submission(NULL);
  // Compile the complete submission signature against generated declarations.
  ddog_FfeSubmissionStatus (*submit)(const ddog_SidecarTransport *, const ddog_InstanceId *,
      const ddog_QueueId *, const ddog_FfeTelemetryContext *, const ddog_FfeFlagEvaluation *,
      ddog_Slice_FfeScalarAttribute, const ddog_FfeSnapshotState *) =
      ddog_sidecar_try_submit_ffe_flag_evaluation;
  return status != DDOG_FFE_SUBMISSION_STATUS_UNAVAILABLE || attributes.len != 1 ||
      snapshot.snapshot_error || submit == NULL;
}
