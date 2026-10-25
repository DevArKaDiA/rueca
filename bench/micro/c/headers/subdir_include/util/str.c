#include "str.h"

int str_len(const char *s)
{
    int n = 0;
    while (s[n])
        n++;
    return n;
}
