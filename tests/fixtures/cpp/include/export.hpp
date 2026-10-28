// Annotation macros of the project: erased before parsing.
#pragma once

#include "visibility.h"

#define FIXTURE_GUARDED_BY(x) __attribute__((guarded_by(x)))

namespace fixture {

class FIXTURE_EXPORT Exported {
public:
    int bump();

private:
    int count_ FIXTURE_GUARDED_BY(lock_);
    int lock_;
};

}  // namespace fixture
