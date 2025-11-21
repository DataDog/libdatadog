// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <errno.h>

#include "datadog/ffe.h"

// Helper function to read file contents into a string
char* read_file(const char* filepath, size_t* out_size) {
    FILE* file = fopen(filepath, "rb");
    if (!file) {
        fprintf(stderr, "Failed to open file '%s': %s\n", filepath, strerror(errno));
        return NULL;
    }

    // Get file size
    fseek(file, 0, SEEK_END);
    long file_size = ftell(file);
    fseek(file, 0, SEEK_SET);

    if (file_size < 0) {
        fprintf(stderr, "Failed to get file size: %s\n", strerror(errno));
        fclose(file);
        return NULL;
    }

    // Allocate buffer and read file
    char* buffer = (char*)malloc(file_size + 1);
    if (!buffer) {
        fprintf(stderr, "Failed to allocate memory for file contents\n");
        fclose(file);
        return NULL;
    }

    size_t bytes_read = fread(buffer, 1, file_size, file);
    fclose(file);

    if (bytes_read != (size_t)file_size) {
        fprintf(stderr, "Failed to read entire file\n");
        free(buffer);
        return NULL;
    }

    buffer[file_size] = '\0';
    *out_size = file_size;
    return buffer;
}

// Helper function to print a BorrowedStr
void print_borrowed_str(const char* label, struct ddog_ffe_BorrowedStr str) {
    if (str.ptr != NULL && str.len > 0) {
        printf("%s: %.*s\n", label, (int)str.len, (const char*)str.ptr);
    } else {
        printf("%s: (empty)\n", label);
    }
}

// Helper function to print variant value
void print_variant_value(struct ddog_ffe_VariantValue value) {
    switch (value.tag) {
        case DDOG_FFE_VARIANT_VALUE_NONE:
            printf("  Value: (none)\n");
            break;
        case DDOG_FFE_VARIANT_VALUE_STRING:
            printf("  Value (string): %.*s\n", (int)value.string.len, (const char*)value.string.ptr);
            break;
        case DDOG_FFE_VARIANT_VALUE_INTEGER:
            printf("  Value (integer): %lld\n", (long long)value.integer);
            break;
        case DDOG_FFE_VARIANT_VALUE_FLOAT:
            printf("  Value (float): %f\n", value.float_);
            break;
        case DDOG_FFE_VARIANT_VALUE_BOOLEAN:
            printf("  Value (boolean): %s\n", value.boolean ? "true" : "false");
            break;
        case DDOG_FFE_VARIANT_VALUE_OBJECT:
            printf("  Value (object): %.*s\n", (int)value.object.len, (const char*)value.object.ptr);
            break;
    }
}

// Helper function to print evaluation reason
const char* reason_to_string(enum ddog_ffe_Reason reason) {
    switch (reason) {
        case DDOG_FFE_REASON_STATIC: return "STATIC";
        case DDOG_FFE_REASON_DEFAULT: return "DEFAULT";
        case DDOG_FFE_REASON_TARGETING_MATCH: return "TARGETING_MATCH";
        case DDOG_FFE_REASON_SPLIT: return "SPLIT";
        case DDOG_FFE_REASON_DISABLED: return "DISABLED";
        case DDOG_FFE_REASON_ERROR: return "ERROR";
        default: return "UNKNOWN";
    }
}

// Helper function to print error code
const char* error_code_to_string(enum ddog_ffe_ErrorCode code) {
    switch (code) {
        case DDOG_FFE_ERROR_CODE_OK: return "OK";
        case DDOG_FFE_ERROR_CODE_TYPE_MISMATCH: return "TYPE_MISMATCH";
        case DDOG_FFE_ERROR_CODE_PARSE_ERROR: return "PARSE_ERROR";
        case DDOG_FFE_ERROR_CODE_FLAG_NOT_FOUND: return "FLAG_NOT_FOUND";
        case DDOG_FFE_ERROR_CODE_TARGETING_KEY_MISSING: return "TARGETING_KEY_MISSING";
        case DDOG_FFE_ERROR_CODE_INVALID_CONTEXT: return "INVALID_CONTEXT";
        case DDOG_FFE_ERROR_CODE_PROVIDER_NOT_READY: return "PROVIDER_NOT_READY";
        case DDOG_FFE_ERROR_CODE_GENERAL: return "GENERAL";
        default: return "UNKNOWN";
    }
}

// Helper function to evaluate and print flag results
void evaluate_and_print_flag(
    ddog_ffe_Handle_Configuration config,
    ddog_ffe_Handle_EvaluationContext context,
    const char* flag_key,
    enum ddog_ffe_ExpectedFlagType expected_type
) {
    printf("\n=== Evaluating flag: %s ===\n", flag_key);

    // Evaluate the flag
    ddog_ffe_Handle_ResolutionDetails assignment = ddog_ffe_get_assignment(
        config,
        flag_key,
        expected_type,
        context
    );

    // Get and print the value
    struct ddog_ffe_VariantValue value = ddog_ffe_assignment_get_value(assignment);
    print_variant_value(value);

    // Get and print the variant (allocation key)
    struct ddog_ffe_BorrowedStr variant = ddog_ffe_assignment_get_variant(assignment);
    print_borrowed_str("  Variant", variant);

    // Get and print the allocation key
    struct ddog_ffe_BorrowedStr allocation_key = ddog_ffe_assignment_get_allocation_key(assignment);
    print_borrowed_str("  Allocation Key", allocation_key);

    // Get and print evaluation reason
    enum ddog_ffe_Reason reason = ddog_ffe_assignment_get_reason(assignment);
    printf("  Reason: %s\n", reason_to_string(reason));

    // Check for errors
    enum ddog_ffe_ErrorCode error_code = ddog_ffe_assignment_get_error_code(assignment);
    if (error_code != DDOG_FFE_ERROR_CODE_OK) {
        printf("  Error Code: %s\n", error_code_to_string(error_code));
        struct ddog_ffe_BorrowedStr error_msg = ddog_ffe_assignment_get_error_message(assignment);
        print_borrowed_str("  Error Message", error_msg);
    }

    // Check if logging is enabled
    bool do_log = ddog_ffe_assignment_get_do_log(assignment);
    printf("  Do Log: %s\n", do_log ? "true" : "false");

    // Get and print flag metadata
    struct ddog_ffe_ArrayMap_BorrowedStr metadata = ddog_ffe_assignnment_get_flag_metadata(assignment);
    if (metadata.count > 0) {
        printf("  Flag Metadata (%zu entries):\n", metadata.count);
        for (size_t i = 0; i < metadata.count; i++) {
            struct ddog_ffe_KeyValue_BorrowedStr kv = metadata.elements[i];
            printf("    - %.*s: %.*s\n",
                   (int)kv.key.len, (const char*)kv.key.ptr,
                   (int)kv.value.len, (const char*)kv.value.ptr);
        }
    } else {
        printf("  Flag Metadata: (empty)\n");
    }

    // Clean up
    ddog_ffe_assignment_drop(&assignment);
}

int main(int argc, char* argv[]) {
    printf("Datadog FFE FFI Example\n");
    printf("=======================\n\n");

    // Step 1: Load configuration from JSON file
    const char* config_path;
    if (argc > 1) {
        config_path = argv[1];
    } else {
        // Default to the test data file
        config_path = "./datadog-ffe/tests/data/flags-v1.json";
    }

    printf("Step 1: Loading configuration from file...\n");
    printf("  Config file: %s\n", config_path);

    size_t json_size = 0;
    char* json_config = read_file(config_path, &json_size);
    if (!json_config) {
        fprintf(stderr, "Failed to read configuration file\n");
        return 1;
    }

    struct ddog_ffe_BorrowedStr json_bytes = {
        .ptr = (const uint8_t*)json_config,
        .len = json_size
    };

    struct ddog_ffe_Result_HandleConfiguration config_result = ddog_ffe_configuration_new(json_bytes);

    // Free the JSON buffer as it has been copied by the FFI
    free(json_config);

    if (config_result.tag != DDOG_FFE_RESULT_HANDLE_CONFIGURATION_OK_HANDLE_CONFIGURATION) {
        fprintf(stderr, "Failed to create configuration: %.*s\n",
                (int)config_result.err.message.len,
                (const char*)config_result.err.message.ptr);
        return 1;
    }

    ddog_ffe_Handle_Configuration config = config_result.ok;
    printf("  Configuration loaded successfully\n");

    // Step 2: Create evaluation context with targeting key and attributes
    printf("\nStep 2: Creating evaluation context...\n");

    // Define some attributes for the evaluation context
    // These attributes match targeting rules in the flags-v1.json test data
    struct ddog_ffe_AttributePair attributes[] = {
        {
            .name = "country",
            .value = {
                .tag = DDOG_FFE_ATTRIBUTE_VALUE_STRING,
                .string = "US"
            }
        },
        {
            .name = "email",
            .value = {
                .tag = DDOG_FFE_ATTRIBUTE_VALUE_STRING,
                .string = "user@example.com"
            }
        },
        {
            .name = "age",
            .value = {
                .tag = DDOG_FFE_ATTRIBUTE_VALUE_NUMBER,
                .number = 55.0
            }
        }
    };

    const char* targeting_key = "user-12345";
    size_t attributes_count = sizeof(attributes) / sizeof(attributes[0]);
    ddog_ffe_Handle_EvaluationContext context = ddog_ffe_evaluation_context_new(
        targeting_key,
        attributes,
        attributes_count
    );

    printf("  Evaluation context created with targeting key: %s\n", targeting_key);
    printf("  Attributes:\n");
    printf("    - country: US\n");
    printf("    - email: user@example.com\n");
    printf("    - age: 55.0\n");

    // Step 3: Evaluate feature flags
    printf("\nStep 3: Evaluating feature flags...\n");

    // Evaluate flags from the test data
    // This flag should evaluate to "on" because country=US and age=55 (>= 50)
    evaluate_and_print_flag(config, context, "kill-switch", DDOG_FFE_EXPECTED_FLAG_TYPE_BOOLEAN);

    // This flag should evaluate to integer value 3 because country=US and email matches @example.com
    evaluate_and_print_flag(config, context, "integer-flag", DDOG_FFE_EXPECTED_FLAG_TYPE_INTEGER);

    // This flag evaluates to numeric value (pi = 3.1415926)
    evaluate_and_print_flag(config, context, "numeric_flag", DDOG_FFE_EXPECTED_FLAG_TYPE_NUMBER);

    // This flag evaluates to a JSON object
    evaluate_and_print_flag(config, context, "json-config-flag", DDOG_FFE_EXPECTED_FLAG_TYPE_OBJECT);

    // Try to evaluate a non-existent flag (demonstrates error handling)
    evaluate_and_print_flag(config, context, "non-existent-flag", DDOG_FFE_EXPECTED_FLAG_TYPE_BOOLEAN);

    // Step 4: Clean up resources
    printf("\nStep 4: Cleaning up resources...\n");
    ddog_ffe_evaluation_context_drop(&context);
    ddog_ffe_configuration_drop(&config);
    printf("  Resources cleaned up successfully\n");

    printf("\n=== Example completed successfully ===\n");
    printf("\nUsage: %s [config-file.json]\n", argv[0]);
    printf("  If no config file is specified, uses the default test data file.\n");
    return 0;
}
