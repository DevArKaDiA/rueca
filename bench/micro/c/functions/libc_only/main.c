#include <stdio.h>
#include <string.h>

static size_t width(const char *s)
{
    return strlen(s);
}

int report(const char *s)
{
    printf("%s\n", s);
    return (int)width(s);
}
