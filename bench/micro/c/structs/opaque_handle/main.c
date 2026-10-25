#include "counter.h"

int main(void)
{
    counter *c = counter_new();
    counter_inc(c);
    return counter_get(c);
}
