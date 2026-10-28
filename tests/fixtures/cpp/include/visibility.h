// Macro-only header: the probe gives it to C, but the tree has no C source, so the C++ adapter owns it.
#pragma once

#ifdef FIXTURE_NO_VISIBILITY
#define FIXTURE_EXPORT
#else
#define FIXTURE_EXPORT __attribute__((visibility("default")))
#endif
