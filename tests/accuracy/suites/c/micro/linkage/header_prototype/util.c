#include "util.h"

static int inner(void) { return 1; }
int util_go(void) { return inner(); }
