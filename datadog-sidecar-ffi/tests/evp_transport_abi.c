// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

#include "sidecar.h"

bool ddog_test_evp_transport_abi(void) {
  const enum ddog_EvpTransportMode modes[] = {
      DDOG_EVP_TRANSPORT_MODE_AGENT_ONLY,
      DDOG_EVP_TRANSPORT_MODE_PREFER_LOCAL_THEN_DIRECT,
  };
  const struct ddog_EvpProducerIdentity producer = {0};

  // Windows sidecar declarations need this type. Check the shared-header
  // dependency on every platform, including Unix.
  (void)sizeof(ddog_crasht_Metadata);

  const ddog_FfeExposure exposure = {.features_json = DDOG_CHARSLICE_C_BARE("{}")};
  const ddog_FfeFlagEvaluation evaluation = {.features_json = DDOG_CHARSLICE_C_BARE("{}")};
  const ddog_FfeFlagEvaluationV2 evaluation_v2 = {
      .event = evaluation,
      .environment_id = DDOG_CHARSLICE_C_BARE("11111111-1111-4111-8111-111111111111"),
      .environment_name = DDOG_CHARSLICE_C_BARE("Production"),
  };
  ddog_FfeSubmissionStatus (*submit_v2)(ddog_SidecarTransport *,
      const ddog_FfeTelemetryContext *, const ddog_FfeFlagEvaluationV2 *,
      ddog_Slice_FfeScalarAttribute, const ddog_FfeSnapshotState *) =
      ddog_sidecar_try_submit_ffe_flag_evaluation_v2;
  ddog_MaybeError (*submit_batch_v2)(ddog_SidecarTransport **,
      const ddog_FfeTelemetryContext *, ddog_Slice_FfeFlagEvaluationV2) =
      ddog_sidecar_send_ffe_flag_evaluation_batch_v2;

  // Compile a native call site against the generated declaration. The object
  // is not executed because null transport and endpoint pointers deliberately
  // violate the runtime contract.
  (void)ddog_sidecar_session_set_evp_transport(NULL, modes[0], NULL, NULL,
                                               (ddog_CharSlice){0}, &producer);

  return modes[0] != modes[1] && exposure.features_json.len == evaluation.features_json.len &&
         evaluation_v2.environment_id.len != 0 && submit_v2 != NULL && submit_batch_v2 != NULL;
}
