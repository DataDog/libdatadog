// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0


#ifndef DDOG_FFE_H
#define DDOG_FFE_H

#pragma once

#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>
#include "common.h"

#ifdef __cplusplus
extern "C" {
#endif // __cplusplus

/**
 * Evaluates a feature flag.
 *
 * # Ownership
 *
 * The caller must call `ddog_ffe_assignment_drop` on the returned value to free resources.
 *
 * # Safety
 *
 * - `config` must be a valid `Configuration` handle
 * - `flag_key` must be a valid C string
 * - `context` must be a valid `EvaluationContext` handle
 */
DDOG_CHECK_RETURN
ddog_ffe_Handle_ResolutionDetails ddog_ffe_get_assignment(ddog_ffe_Handle_Configuration config,
                                                          const char *flag_key,
                                                          enum ddog_ffe_ExpectedFlagType expected_type,
                                                          ddog_ffe_Handle_EvaluationContext context);

/**
 * Get value produced by evaluation.
 *
 * # Ownership
 *
 * The returned `VariantValue` borrows from `assignment`. It must not be used after `assignment` is
 * freed.
 *
 * # Safety
 * `assignment` must be a valid handle.
 */
struct ddog_ffe_VariantValue ddog_ffe_assignment_get_value(ddog_ffe_Handle_ResolutionDetails assignment);

/**
 * Get variant key produced by evaluation. Returns `NULL` if evaluation did not produce any value.
 *
 * # Ownership
 *
 * The returned string borrows from `assignment`. It must not be used after `assignment` is
 * freed.
 *
 * # Safety
 * `assignment` must be a valid handle.
 */
struct ddog_ffe_BorrowedStr ddog_ffe_assignment_get_variant(ddog_ffe_Handle_ResolutionDetails assignment);

/**
 * Get allocation key produced by evaluation. Returns `NULL` if evaluation did not produce any
 * value.
 *
 * # Ownership
 *
 * The returned string borrows from `assignment`. It must not be used after `assignment` is
 * freed.
 *
 * # Safety
 * `assignment` must be a valid handle.
 */
struct ddog_ffe_BorrowedStr ddog_ffe_assignment_get_allocation_key(ddog_ffe_Handle_ResolutionDetails assignment);

/**
 * # Safety
 * `assignment` must be a valid handle.
 */
enum ddog_ffe_Reason ddog_ffe_assignment_get_reason(ddog_ffe_Handle_ResolutionDetails assignment);

/**
 * # Safety
 * `assignment` must be a valid handle.
 */
enum ddog_ffe_ErrorCode ddog_ffe_assignment_get_error_code(ddog_ffe_Handle_ResolutionDetails assignment);

/**
 * # Safety
 * `assignment` must be a valid handle.
 */
struct ddog_ffe_BorrowedStr ddog_ffe_assignment_get_error_message(ddog_ffe_Handle_ResolutionDetails assignment);

/**
 * # Safety
 * `assignment` must be a valid handle.
 */
bool ddog_ffe_assignment_get_do_log(ddog_ffe_Handle_ResolutionDetails assignment);

/**
 * # Safety
 * `assignment` must be a valid handle.
 */
struct ddog_ffe_ArrayMap_BorrowedStr ddog_ffe_assignnment_get_flag_metadata(ddog_ffe_Handle_ResolutionDetails assignment);

/**
 * # Safety
 * `assignment` must be a valid handle.
 */
struct ddog_ffe_ArrayMap_BorrowedStr ddog_ffe_assignnment_get_extra_logging(ddog_ffe_Handle_ResolutionDetails assignment);

/**
 * Frees an Assignment handle.
 *
 * # Safety
 * - `assignment` must be a valid Assignment handle
 */
void ddog_ffe_assignment_drop(ddog_ffe_Handle_ResolutionDetails *assignment);

/**
 * Creates a new Configuration from JSON bytes.
 *
 * # Ownership
 *
 * The caller must call `ddog_ffe_configuration_drop` to release resources allocated for
 * configuration.
 *
 * # Safety
 *
 * - `json_bytes` must point to valid memory.
 */
DDOG_CHECK_RETURN
struct ddog_ffe_Result_HandleConfiguration ddog_ffe_configuration_new(struct ddog_ffe_BorrowedStr json_bytes);

/**
 * Frees a Configuration.
 *
 * # Safety
 *
 * `config` must be a valid Configuration handle created by `ddog_ffe_configuration_new`.
 */
void ddog_ffe_configuration_drop(ddog_ffe_Handle_Configuration *config);

/**
 * Creates a new EvaluationContext with the given targeting key and attributes.
 *
 * # Ownership
 *
 * `ddog_ffe_evaluation_context_drop` must be called on the result value to free resources.
 *
 * # Safety
 * - `targeting_key` must be a valid C string or NULL.
 * - `attributes` must point to a valid array of valid `AttributePair` structs (can be null if
 *   `attributes_count` is 0)
 */
DDOG_CHECK_RETURN
ddog_ffe_Handle_EvaluationContext ddog_ffe_evaluation_context_new(const char *targeting_key,
                                                                  const struct ddog_ffe_AttributePair *attributes,
                                                                  uintptr_t attributes_count);

/**
 * Frees an EvaluationContext
 *
 * # Safety
 * `context` must be a valid EvaluationContext handle created by `ddog_ffe_evaluation_context_new`
 */
void ddog_ffe_evaluation_context_drop(ddog_ffe_Handle_EvaluationContext *context);

#ifdef __cplusplus
}  // extern "C"
#endif  // __cplusplus

#endif  /* DDOG_FFE_H */
