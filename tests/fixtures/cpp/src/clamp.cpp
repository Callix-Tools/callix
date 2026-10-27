#include "clamp.h"

namespace fixture {
int clamp_twice(int v) { return clamp(clamp(v)); }
}
