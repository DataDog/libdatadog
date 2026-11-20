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
  ddog_FfeTelemetryContext context = { 0 };
  ddog_FfeFlagEvaluation event = { 0 };
  ddog_FfeSubmissionStatus status = ddog_sidecar_try_submit_ffe_flag_evaluation(
      NULL, &context, &event, attributes, &snapshot);
  // Compile the complete submission signature against generated declarations.
  ddog_FfeSubmissionStatus (*submit)(ddog_SidecarTransport *,
      const ddog_FfeTelemetryContext *, const ddog_FfeFlagEvaluation *,
      ddog_Slice_FfeScalarAttribute, const ddog_FfeSnapshotState *) =
      ddog_sidecar_try_submit_ffe_flag_evaluation;
  _Static_assert(sizeof(ddog_FfeAdmission) == sizeof(uint32_t), "admission status ABI");
  _Static_assert(ddog_FFE_ADMISSION_UNAVAILABLE == 0 && ddog_FFE_ADMISSION_ADMITTED == 1 &&
      ddog_FFE_ADMISSION_REPEAT == 2 && ddog_FFE_ADMISSION_UNKNOWN == 3 &&
      ddog_FFE_ADMISSION_QUEUE_FULL == 4, "admission status values");
  ddog_MaybeError (*admit)(ddog_SidecarTransport **, const ddog_FfeTelemetryContext *,
      const ddog_FfeExposure *, bool, uint32_t, ddog_FfeAdmission *) = ddog_sidecar_ffe_admit_v1;
  ddog_FfeExposure exposure = { 0 };
  ddog_FfeAdmission admission = UINT32_MAX;
  ddog_SidecarTransport *transport = NULL;
  ddog_MaybeError result = admit(&transport, &context, &exposure, true, 100, &admission);
  ddog_MaybeError_drop(result);
  return admission != ddog_FFE_ADMISSION_UNAVAILABLE || status != DDOG_FFE_SUBMISSION_STATUS_UNAVAILABLE || attributes.len != 1 ||
      snapshot.snapshot_error || submit == NULL;
}
