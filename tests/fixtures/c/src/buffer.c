#include "buffer.h"

size_t buffer_length(const buffer_t *b)
{
    return b->len;
}

int number_as_int(number_t n)
{
    return n.i;
}
