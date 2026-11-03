// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0
#include <stdio.h>
#include <string.h>
#include "datadog/ffe.h"

#define CHECK(condition) do { if (!(condition)) { fprintf(stderr, "failed: %s\n", #condition); return 1; } } while (0)

int main(void) {
    const char *json = "{\"createdAt\":\"2026-01-01T00:00:00Z\",\"environment\":{\"name\":\"test\"},\"observeFullEvaluationData\":false,\"flags\":{\"root\":{\"key\":\"root\",\"enabled\":true,\"variationType\":\"BOOLEAN\",\"variations\":{\"on\":{\"key\":\"on\",\"value\":true},\"off\":{\"key\":\"off\",\"value\":false}},\"allocations\":[{\"key\":\"root-allocation\",\"doLog\":true,\"splits\":[{\"variationKey\":\"on\",\"shards\":[]}],\"rules\":[{\"conditions\":[{\"flagEvaluation\":{\"key\":\"parent\"},\"operator\":\"ONE_OF\",\"value\":[\"on\"]},{\"flagEvaluation\":{\"key\":\"parent\"},\"operator\":\"ONE_OF\",\"value\":[\"on\"]}]}]}]},\"parent\":{\"key\":\"parent\",\"enabled\":true,\"variationType\":\"BOOLEAN\",\"variations\":{\"on\":{\"key\":\"on\",\"value\":true},\"off\":{\"key\":\"off\",\"value\":false}},\"allocations\":[{\"key\":\"parent-allocation\",\"doLog\":true,\"splits\":[{\"variationKey\":\"on\",\"shards\":[]}]}]},\"missing\":{\"key\":\"missing\",\"enabled\":true,\"variationType\":\"BOOLEAN\",\"variations\":{\"on\":{\"key\":\"on\",\"value\":true},\"off\":{\"key\":\"off\",\"value\":false}},\"allocations\":[{\"key\":\"missing-allocation\",\"doLog\":true,\"splits\":[{\"variationKey\":\"on\",\"shards\":[]}],\"rules\":[{\"conditions\":[{\"flagEvaluation\":{\"key\":\"absent\"},\"operator\":\"ONE_OF\",\"value\":[\"on\"]}]}]}]}}}";
    struct ddog_ffe_Result_HandleConfiguration parsed = ddog_ffe_configuration_new(
        (struct ddog_ffe_BorrowedStr){.ptr = (const uint8_t *)json, .len = strlen(json)});
    CHECK(parsed.tag == DDOG_FFE_RESULT_HANDLE_CONFIGURATION_OK_HANDLE_CONFIGURATION);
    ddog_ffe_Handle_Configuration config = parsed.ok;
    ddog_ffe_Handle_EvaluationContext context = ddog_ffe_evaluation_context_new("subject", NULL, 0);
    ddog_ffe_Handle_ResolutionDetails root = ddog_ffe_get_assignment(
        config, "root", DDOG_FFE_EXPECTED_FLAG_TYPE_BOOLEAN, context);
    CHECK(ddog_ffe_assignment_dependency_count(root) == 1);
    struct ddog_ffe_BorrowedStr key = ddog_ffe_assignment_dependency_key(root, 0);
    CHECK(key.len == 6 && memcmp(key.ptr, "parent", 6) == 0);
    CHECK(ddog_ffe_assignment_dependency_exposure_eligible(root, 0));
    CHECK(ddog_ffe_assignment_dependency_key(root, 1).len == 0);
    CHECK(!ddog_ffe_assignment_dependency_exposure_eligible(root, 1));
    ddog_ffe_Handle_ResolutionDetails child = ddog_ffe_assignment_dependency_result(root, 0);
    ddog_ffe_assignment_drop(&root);
    // Each child owns its data independently of the root result and configuration.
    ddog_ffe_configuration_drop(&config);
    CHECK(ddog_ffe_assignment_get_error_code(child) == DDOG_FFE_ERROR_CODE_OK);
    struct ddog_ffe_VariantValue value = ddog_ffe_assignment_get_value(child);
    CHECK(value.tag == DDOG_FFE_VARIANT_VALUE_BOOLEAN && value.boolean);
    CHECK(ddog_ffe_assignment_dependency_count(child) == 0);
    ddog_ffe_assignment_drop(&child);
    ddog_ffe_evaluation_context_drop(&context);
    return 0;
}
