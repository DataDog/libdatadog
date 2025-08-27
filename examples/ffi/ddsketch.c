// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

#include <datadog/common.h>
#include <datadog/ddsketch.h>
#include <stdio.h>
#include <stdlib.h>

#define TRY(expr)                                                                                  \
  {                                                                                                \
    struct ddog_VoidResult result = expr;                                                         \
    if (result.tag == DDOG_VOID_RESULT_ERR) {                                                    \
      ddog_CharSlice message = ddog_Error_message((struct ddog_Error*)&result.err);             \
      fprintf(stderr, "ERROR: %.*s\n", (int)message.len, message.ptr);                          \
      ddog_Error_drop((struct ddog_Error*)&result.err);                                         \
      return 1;                                                                                   \
    }                                                                                             \
  }

int main(void) {
  // Create a new DDSketch
  struct ddsketch_Handle_DDSketch sketch = ddog_ddsketch_new();

  printf("Created DDSketch successfully\n");

  // Add some sample data points
  printf("Adding sample data points...\n");
  TRY(ddog_ddsketch_add(&sketch, 1.0));
  TRY(ddog_ddsketch_add(&sketch, 2.5));
  TRY(ddog_ddsketch_add(&sketch, 5.0));
  TRY(ddog_ddsketch_add(&sketch, 10.0));
  TRY(ddog_ddsketch_add(&sketch, 15.0));

  // Add points with specific counts
  printf("Adding points with specific counts...\n");
  TRY(ddog_ddsketch_add_with_count(&sketch, 3.0, 5.0));  // Add 3.0 with count 5
  TRY(ddog_ddsketch_add_with_count(&sketch, 7.0, 3.0));  // Add 7.0 with count 3

  // Get the total count
  double count = 0.0;
  TRY(ddog_ddsketch_count(&sketch, &count));
  printf("Total count in sketch: %.0f\n", count);


  // Encode the sketch to protobuf format
  printf("Encoding sketch to protobuf...\n");
  struct ddog_Vec_U8 encoded = ddog_ddsketch_encode(&sketch);
  
  printf("Encoded sketch size: %zu bytes\n", encoded.len);
  
  // Print first few bytes of encoded data (for demonstration)
  printf("First 10 bytes of encoded data: ");
  for (size_t i = 0; i < (encoded.len < 10 ? encoded.len : 10); i++) {
    printf("%02x ", encoded.ptr[i]);
  }
  printf("\n");

  // Clean up the encoded vector
  ddog_Vec_U8_drop(encoded);

  // Clean up the sketch (note: sketch is consumed by ddog_ddsketch_encode)
  // ddog_ddsketch_drop is not called here because the sketch was consumed

  printf("DDSketch example completed successfully!\n");
  return 0;
}
