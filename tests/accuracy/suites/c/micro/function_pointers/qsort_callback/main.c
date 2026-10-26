#include <stdlib.h>

int cmp(const void *a, const void *b) { return 0; }

int main(void) { int x[2] = {2, 1}; qsort(x, 2, sizeof(int), cmp); return 0; }
