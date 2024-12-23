#include <datadog/common.h>
#include <datadog/library-config.h>
#include <stdio.h>

#define DDOG_VAL_STR_PTR(val) (val.tag == DDOG_VALUE_STR_VAL ? val.str_val.message.ptr : NULL)

#define DDOG_VAL_STR_LEN(val) (val.tag == DDOG_VALUE_STR_VAL ? val.str_val.message.len : 0)

#define DDOG_SLICE_CHARSLICE(arr)                                                                  \
  ((ddog_Slice_CharSlice){.ptr = arr, .len = sizeof(arr) / sizeof(arr[0])})

int main(int argc, const char *const *argv) {
  ddog_Configurator *configurator = ddog_library_config_new(true);
  ddog_CharSlice args[] = {
      DDOG_CHARSLICE_C("/bin/true"),
  };
  ddog_CharSlice envp[] = {
      DDOG_CHARSLICE_C("FOO=BAR"),
  };
  ddog_Vec_Config configs = ddog_library_config_get(
      configurator, (ddog_ProcessInfo){.args = DDOG_SLICE_CHARSLICE(args),
                                       .envp = DDOG_SLICE_CHARSLICE(envp),
                                       .language = DDOG_CHARSLICE_C("java")});
  for (int i = 0; i < configs.len; i++) {
    printf("%d %*.s\n", configs.ptr[i].name, (int)DDOG_VAL_STR_LEN(configs.ptr[i].value),
           DDOG_VAL_STR_PTR(configs.ptr[i].value));
  }
}