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

  // Compile a native call site against the generated declaration. The object
  // is not executed because null transport and endpoint pointers deliberately
  // violate the runtime contract.
  (void)ddog_sidecar_session_set_evp_transport(NULL, modes[0], NULL, NULL,
                                               (ddog_CharSlice){0}, &producer);

  return modes[0] != modes[1];
}
