// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

// This example demonstrates how to use the ExporterManager API with fork support.
// The ExporterManager runs a background thread that sends profiling data asynchronously.

#include <datadog/common.h>
#include <datadog/profiling.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/wait.h>
#include <unistd.h>

static ddog_CharSlice to_slice_c_char(const char *s) { return (ddog_CharSlice){.ptr = s, .len = strlen(s)}; }

void print_error(const char *s, const ddog_Error *err) {
    ddog_CharSlice charslice = ddog_Error_message(err);
    printf("%s (%.*s)\n", s, (int)charslice.len, charslice.ptr);
}

// Helper function to create a simple profile with one sample
ddog_prof_Profile *create_profile_with_sample(void) {
    // Use the SampleType enum instead of ValueType struct
    const ddog_prof_SampleType wall_time = DDOG_PROF_SAMPLE_TYPE_WALL_TIME;
    const ddog_prof_Slice_SampleType sample_types = {&wall_time, 1};
    const ddog_prof_Period period = {
        .sample_type = wall_time,
        .value = 60,
    };

    ddog_prof_Profile_NewResult profile_result = ddog_prof_Profile_new(sample_types, &period);
    if (profile_result.tag != DDOG_PROF_PROFILE_NEW_RESULT_OK) {
        print_error("Failed to create profile", &profile_result.err);
        ddog_Error_drop(&profile_result.err);
        return NULL;
    }

    ddog_prof_Profile *profile = malloc(sizeof(ddog_prof_Profile));
    *profile = profile_result.ok;

    ddog_prof_Location root_location = {
        .mapping = (ddog_prof_Mapping){0},
        .function = (struct ddog_prof_Function){
            .name = DDOG_CHARSLICE_C_BARE("{main}"),
            .filename = DDOG_CHARSLICE_C_BARE("/srv/example/index.c"),
        },
    };

    int64_t value = 100;
    const ddog_prof_Label label = {
        .key = DDOG_CHARSLICE_C_BARE("language"),
        .str = DDOG_CHARSLICE_C_BARE("c"),
    };

    ddog_prof_Sample sample = {
        .locations = {&root_location, 1},
        .values = {&value, 1},
        .labels = {&label, 1},
    };

    // Pass 0 as the timestamp parameter
    ddog_prof_Profile_Result add_result = ddog_prof_Profile_add(profile, sample, 0);
    if (add_result.tag != DDOG_PROF_PROFILE_RESULT_OK) {
        print_error("Failed to add sample to profile", &add_result.err);
        ddog_Error_drop(&add_result.err);
        ddog_prof_Profile_drop(profile);
        free(profile);
        return NULL;
    }

    return profile;
}

int main(int argc, char *argv[]) {
    // Default service name for automated testing
    const char *service = (argc >= 2) ? argv[1] : "libdatadog-test";

    // Create tags vector
    ddog_Vec_Tag tags = ddog_Vec_Tag_new();
    ddog_Vec_Tag_PushResult tag_result;
    
    // Note: ddog_Vec_Tag_push returns a result, but the push is best-effort
    tag_result = ddog_Vec_Tag_push(&tags, to_slice_c_char("service"), to_slice_c_char(service));
    if (tag_result.tag == DDOG_VEC_TAG_PUSH_RESULT_ERR) {
        print_error("Failed to push service tag", &tag_result.err);
        ddog_Error_drop(&tag_result.err);
        ddog_Vec_Tag_drop(tags);
        return 1;
    }

    tag_result = ddog_Vec_Tag_push(&tags, to_slice_c_char("env"), to_slice_c_char("dev"));
    if (tag_result.tag == DDOG_VEC_TAG_PUSH_RESULT_ERR) {
        print_error("Failed to push env tag", &tag_result.err);
        ddog_Error_drop(&tag_result.err);
        ddog_Vec_Tag_drop(tags);
        return 1;
    }

    // Create an endpoint (using file endpoint for this example)
    ddog_prof_Endpoint endpoint = ddog_Endpoint_file(to_slice_c_char("/tmp/exporter_manager_example.txt"));

    // Create the ProfileExporter
    ddog_prof_ProfileExporter_Result exporter_result = ddog_prof_Exporter_new(
        to_slice_c_char("libdatadog-example"),
        to_slice_c_char("1.0.0"),
        to_slice_c_char("native"),
        &tags,
        endpoint
    );

    ddog_Vec_Tag_drop(tags);

    if (exporter_result.tag != DDOG_PROF_PROFILE_EXPORTER_RESULT_OK_HANDLE_PROFILE_EXPORTER) {
        print_error("Failed to create exporter", &exporter_result.err);
        ddog_Error_drop(&exporter_result.err);
        return 1;
    }

    // We need to heap-allocate the exporter to pass it to ExporterManager_new
    // The function takes ownership via a mutable pointer
    ddog_prof_ProfileExporter exporter = exporter_result.ok;
    ddog_prof_ProfileExporter *exporter_ptr = &exporter;

    // Create the ExporterManager
    printf("Creating ExporterManager...\n");
    ddog_prof_Result_HandleExporterManager manager_result = ddog_prof_ExporterManager_new(exporter_ptr);
    if (manager_result.tag != DDOG_PROF_RESULT_HANDLE_EXPORTER_MANAGER_OK_HANDLE_EXPORTER_MANAGER) {
        print_error("Failed to create ExporterManager", &manager_result.err);
        ddog_Error_drop(&manager_result.err);
        return 1;
    }

    ddog_prof_Handle_ExporterManager manager = manager_result.ok;

    // Create a profile and add it to the queue
    printf("Queueing a profile...\n");
    ddog_prof_Profile *profile = create_profile_with_sample();
    if (profile == NULL) {
        ddog_prof_ExporterManager_drop(&manager);
        return 1;
    }

    // Serialize the profile
    ddog_prof_Profile_SerializeResult serialize_result =
        ddog_prof_Profile_serialize(profile, NULL, NULL);
    if (serialize_result.tag != DDOG_PROF_PROFILE_SERIALIZE_RESULT_OK) {
        print_error("Failed to serialize profile", &serialize_result.err);
        ddog_Error_drop(&serialize_result.err);
        ddog_prof_Profile_drop(profile);
        free(profile);
        ddog_prof_ExporterManager_drop(&manager);
        return 1;
    }

    // The queue function takes ownership of the encoded profile, so we need a mutable pointer
    ddog_prof_EncodedProfile encoded_profile = serialize_result.ok;
    ddog_prof_EncodedProfile *encoded_profile_ptr = &encoded_profile;

    // Queue the profile (no additional files or tags for this simple example)
    ddog_prof_Exporter_Slice_File empty_files = ddog_prof_Exporter_Slice_File_empty();
    ddog_Vec_Tag empty_tags = ddog_Vec_Tag_new();

    ddog_VoidResult queue_result = ddog_prof_ExporterManager_queue(
        &manager,
        encoded_profile_ptr,
        empty_files,
        &empty_tags,
        NULL,  // optional_process_tags
        NULL,  // optional_internal_metadata_json
        NULL   // optional_info_json
    );

    ddog_Vec_Tag_drop(empty_tags);

    if (queue_result.tag != DDOG_VOID_RESULT_OK) {
        print_error("Failed to queue profile", &queue_result.err);
        ddog_Error_drop(&queue_result.err);
        // encoded_profile was consumed by queue, so we don't drop it here
        ddog_prof_Profile_drop(profile);
        free(profile);
        ddog_prof_ExporterManager_drop(&manager);
        return 1;
    }

    printf("Profile queued successfully!\n");

    // Clean up profile after queueing
    // Note: encoded_profile was consumed by queue, so we don't drop it
    ddog_prof_Profile_drop(profile);
    free(profile);

    // Sleep briefly to let the background thread process
    sleep(1);

    // ========== FORK WORKFLOW EXAMPLE ==========
    printf("\n=== Fork Workflow Example ===\n");

    // Create another profile for the fork demonstration
    printf("Creating profile for fork example...\n");
    profile = create_profile_with_sample();
    if (profile == NULL) {
        ddog_prof_ExporterManager_drop(&manager);
        return 1;
    }

    serialize_result = ddog_prof_Profile_serialize(profile, NULL, NULL);
    if (serialize_result.tag != DDOG_PROF_PROFILE_SERIALIZE_RESULT_OK) {
        print_error("Failed to serialize profile for fork example", &serialize_result.err);
        ddog_Error_drop(&serialize_result.err);
        ddog_prof_Profile_drop(profile);
        free(profile);
        ddog_prof_ExporterManager_drop(&manager);
        return 1;
    }

    // The queue function takes ownership of the encoded profile, so we need a mutable pointer
    ddog_prof_EncodedProfile encoded_profile2 = serialize_result.ok;
    ddog_prof_EncodedProfile *encoded_profile_ptr2 = &encoded_profile2;

    // Queue another profile
    empty_tags = ddog_Vec_Tag_new();
    queue_result = ddog_prof_ExporterManager_queue(
        &manager,
        encoded_profile_ptr2,
        empty_files,
        &empty_tags,
        NULL,  // optional_process_tags
        NULL,  // optional_internal_metadata_json
        NULL   // optional_info_json
    );
    ddog_Vec_Tag_drop(empty_tags);

    if (queue_result.tag != DDOG_VOID_RESULT_OK) {
        print_error("Failed to queue profile for fork example", &queue_result.err);
        ddog_Error_drop(&queue_result.err);
        // encoded_profile was consumed by queue, so we don't drop it here
        ddog_prof_Profile_drop(profile);
        free(profile);
        ddog_prof_ExporterManager_drop(&manager);
        return 1;
    }

    // Note: encoded_profile was consumed by queue, so we don't drop it
    ddog_prof_Profile_drop(profile);
    free(profile);

    // Step 1: Call prefork before forking
    printf("Calling prefork...\n");
    ddog_VoidResult prefork_result = ddog_prof_ExporterManager_prefork(&manager);
    if (prefork_result.tag != DDOG_VOID_RESULT_OK) {
        print_error("Failed to call prefork", &prefork_result.err);
        ddog_Error_drop(&prefork_result.err);
        return 1;
    }

    printf("prefork successful! Background thread stopped.\n");

    // Step 2: Fork the process
    printf("Forking process...\n");
    pid_t pid = fork();

    if (pid < 0) {
        printf("Fork failed!\n");
        return 1;
    } else if (pid == 0) {
        // Child process
        printf("[CHILD] In child process (PID: %d)\n", getpid());
        
        // Step 3a: In child, call postfork_child
        printf("[CHILD] Calling postfork_child...\n");
        ddog_VoidResult child_result = ddog_prof_ExporterManager_postfork_child(&manager);
        if (child_result.tag != DDOG_VOID_RESULT_OK) {
            print_error("[CHILD] Failed to call postfork_child", &child_result.err);
            ddog_Error_drop(&child_result.err);
            exit(1);
        }

        printf("[CHILD] postfork_child successful! Manager restarted.\n");

        // Use the manager briefly
        sleep(1);

        // Clean up manager
        printf("[CHILD] Aborting manager...\n");
        ddog_VoidResult child_abort_result = ddog_prof_ExporterManager_abort(&manager);
        if (child_abort_result.tag != DDOG_VOID_RESULT_OK) {
            print_error("[CHILD] Failed to abort manager", &child_abort_result.err);
            ddog_Error_drop(&child_abort_result.err);
            exit(1);
        }

        ddog_prof_ExporterManager_drop(&manager);

        printf("[CHILD] Child process exiting.\n");
        exit(0);
    } else {
        // Parent process
        printf("[PARENT] In parent process (PID: %d), child PID: %d\n", getpid(), pid);
        
        // Step 3b: In parent, call postfork_parent
        printf("[PARENT] Calling postfork_parent...\n");
        ddog_VoidResult parent_result = ddog_prof_ExporterManager_postfork_parent(&manager);
        if (parent_result.tag != DDOG_VOID_RESULT_OK) {
            print_error("[PARENT] Failed to call postfork_parent", &parent_result.err);
            ddog_Error_drop(&parent_result.err);
            return 1;
        }

        printf("[PARENT] postfork_parent successful! Manager restarted with inflight requests.\n");

        // Wait for child to complete
        int status;
        waitpid(pid, &status, 0);
        printf("[PARENT] Child process finished.\n");

        // Continue using manager
        sleep(1);

        // Clean up manager
        printf("[PARENT] Aborting manager...\n");
        ddog_VoidResult parent_abort_result = ddog_prof_ExporterManager_abort(&manager);
        if (parent_abort_result.tag != DDOG_VOID_RESULT_OK) {
            print_error("[PARENT] Failed to abort manager", &parent_abort_result.err);
            ddog_Error_drop(&parent_abort_result.err);
            return 1;
        }

        ddog_prof_ExporterManager_drop(&manager);

        printf("[PARENT] Parent process exiting.\n");
    }

    printf("\nExample completed successfully!\n");
    return 0;
}
