/* Declarations shared by both translation units. */
#ifndef FIXTURE_ENGINE_H
#define FIXTURE_ENGINE_H

#include <stddef.h>

enum region {
    REGION_EUROPE,
    REGION_AMERICA
};

struct engine {
    const char *label;
    enum region region;
};

typedef struct engine engine_t;

/* Declared here, defined in engine.c: the two share a qualified name. */
engine_t engine_build(enum region region);
int engine_ping(const engine_t *engine);
const char *engine_describe(const engine_t *engine);

#endif /* FIXTURE_ENGINE_H */
