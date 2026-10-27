// Macro-only header: the probe gives it to C, so the C++ graph reads it for erasure only.
#pragma once

#ifdef FIXTURE_NO_VISIBILITY
#define FIXTURE_EXPORT
#else
#define FIXTURE_EXPORT __attribute__((visibility("default")))
#endif
