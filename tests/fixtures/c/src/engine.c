#include "engine.h"

static const char *const DEFAULT_LABEL = "base";

/* File-local: main.c declares a function of the same name, and the two are
   different functions. */
static const char *label_for(enum region region)
{
    return region == REGION_EUROPE ? "eu" : "us";
}

engine_t engine_build(enum region region)
{
    engine_t engine;
    engine.label = label_for(region);
    engine.region = region;
    return engine;
}

int engine_ping(const engine_t *engine)
{
    return engine != NULL;
}

const char *engine_describe(const engine_t *engine)
{
    if (engine->label == NULL) {
        return DEFAULT_LABEL;
    }
    return engine->label;
}
