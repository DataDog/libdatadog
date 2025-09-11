/*
 * Example usage of the runtime callback registration system
 *
 * This demonstrates how a language runtime (e.g., Ruby, Python, PHP)
 * would register a callback to provide runtime-specific stack traces
 * during crash handling.
 */

#include <stdio.h>
#include <string.h>

// Forward declarations for the runtime callback API
// (These would normally be in a header file)
typedef struct {
  const char *function_name;
  const char *file_name;
  unsigned int line_number;
  unsigned int column_number;
  const char *class_name;
  const char *module_name;
} ddog_RuntimeStackFrame;

typedef enum { Ok, AlreadyRegistered, NullCallback, UnknownError } CallbackResult;

typedef void (*emit_frame_fn)(const ddog_RuntimeStackFrame *);
typedef void (*runtime_callback_fn)(emit_frame_fn, void *);

CallbackResult ddog_crasht_register_runtime_stack_callback(runtime_callback_fn callback,
                                                           void *context);

// Example runtime-specific stack collection
static void ruby_stack_callback(emit_frame_fn emit_frame, void *context) {
  // In a real implementation, this would:
  // 1. Access the Ruby VM's internal call stack
  // 2. Walk through the Ruby frames
  // 3. Extract method names, file names, line numbers
  // 4. Call emit_frame for each frame

  // For this example, we'll simulate a Ruby stack trace
  ddog_RuntimeStackFrame frames[] = {{.function_name = "ActiveRecord::Base.find",
                                      .file_name = "/app/models/user.rb",
                                      .line_number = 42,
                                      .column_number = 15,
                                      .class_name = "User",
                                      .module_name = "ActiveRecord"},
                                     {.function_name = "UserController#show",
                                      .file_name = "/app/controllers/user_controller.rb",
                                      .line_number = 18,
                                      .column_number = 5,
                                      .class_name = "UserController",
                                      .module_name = NULL},
                                     {.function_name = "ActionController::Base.dispatch",
                                      .file_name = "/gems/actionpack/lib/action_controller/base.rb",
                                      .line_number = 195,
                                      .column_number = 12,
                                      .class_name = "ActionController::Base",
                                      .module_name = "ActionController"}};

  size_t frame_count = sizeof(frames) / sizeof(frames[0]);
  for (size_t i = 0; i < frame_count; i++) {
    emit_frame(&frames[i]);
  }
}

// Example initialization function that a Ruby extension would call
int initialize_ruby_crashtracker() {
  printf("Registering Ruby crash callback...\n");

  CallbackResult result =
      ddog_crasht_register_runtime_stack_callback(ruby_stack_callback,
                                                  NULL // No context needed for this example
      );

  switch (result) {
  case Ok:
    printf("✓ Ruby crash callback registered successfully\n");
    return 0;
  case AlreadyRegistered:
    printf("⚠ A callback is already registered\n");
    return 1;
  case NullCallback:
    printf("✗ Null callback provided\n");
    return 1;
  default:
    printf("✗ Unknown error occurred\n");
    return 1;
  }
}

int main() {
  printf("Runtime Callback Registration Example\n");
  printf("=====================================\n\n");

  printf("This example demonstrates how language runtimes can register\n");
  printf("callbacks to provide meaningful stack traces during crashes.\n\n");

  printf("When a crash occurs:\n");
  printf("1. The crashtracker captures native stack trace\n");
  printf("2. It invokes the registered runtime callback\n");
  printf("3. The callback provides runtime-specific frames\n");
  printf("4. Both traces are included in the crash report\n\n");

  return initialize_ruby_crashtracker();
}

/*
 * Expected output when a crash occurs with this callback registered:
 *
 * CrashInfo {
 *   // ... standard fields ...
 *   "experimental": {
 *     "runtime_stack": {
 *       "format": "Datadog Runtime Callback 1.0",
 *       "runtime_type": "unknown",
 *       "frames": [
 *         {
 *           "function": "ActiveRecord::Base.find",
 *           "file": "/app/models/user.rb",
 *           "line": 42,
 *           "column": 15,
 *           "class_name": "User",
 *           "module_name": "ActiveRecord"
 *         },
 *         {
 *           "function": "UserController#show",
 *           "file": "/app/controllers/user_controller.rb",
 *           "line": 18,
 *           "column": 5,
 *           "class_name": "UserController"
 *         },
 *         {
 *           "function": "ActionController::Base.dispatch",
 *           "file": "/gems/actionpack/lib/action_controller/base.rb",
 *           "line": 195,
 *           "column": 12,
 *           "class_name": "ActionController::Base",
 *           "module_name": "ActionController"
 *         }
 *       ]
 *     }
 *   }
 * }
 */
