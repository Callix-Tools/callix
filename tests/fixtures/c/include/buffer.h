/* Types named only by a typedef: each is a class of that name. */
#ifndef FIXTURE_BUFFER_H
#define FIXTURE_BUFFER_H

#include <stddef.h>

/* A byte buffer. */
typedef struct {
    size_t len;
    char *data;
} buffer_t;

typedef union {
    int i;
    float f;
} number_t;

size_t buffer_length(const buffer_t *b);

#endif /* FIXTURE_BUFFER_H */
