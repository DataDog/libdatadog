/* Probe: does backtrace() exist and link successfully? */
#include <execinfo.h>
int main(void) {
    void *buf[1];
    return backtrace(buf, 1) >= 0 ? 0 : 1;
}
