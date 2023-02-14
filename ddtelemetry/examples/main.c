#include <stdio.h>
#include <stdlib.h>
#include <dlfcn.h>

int main(int argc, char *argv[]) {
    if (argc != 2) {
        printf("Too few arguments, exiting");
        exit(1);
    }
    char *path = argv[1];
    printf("Loading %s\n", path);

    void *handle = dlopen(path, RTLD_LAZY);

    if (handle == NULL) {
        printf("Error loading: %s\n", path);
        printf(dlerror());
        exit(1);
    }

    dlclose(handle);
}