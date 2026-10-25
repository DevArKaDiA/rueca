#include <stdlib.h>
#include "counter.h"

struct counter {
    int value;
};

static void bump(counter *c, int by)
{
    c->value += by;
}

counter *counter_new(void)
{
    return calloc(1, sizeof(counter));
}

void counter_inc(counter *c)
{
    bump(c, 1);
}

int counter_get(const counter *c)
{
    return c->value;
}
