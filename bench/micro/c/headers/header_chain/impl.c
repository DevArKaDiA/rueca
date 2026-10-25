#include "api.h"

int base_value(void)
{
    return 41;
}

int api_value(void)
{
    return base_value() + 1;
}
