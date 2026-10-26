#include <stdlib.h>
void cleanup(void) {}

int main(void) {
    atexit(cleanup);
    return 0;
}
