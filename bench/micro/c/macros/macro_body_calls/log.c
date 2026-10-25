#include <stdio.h>

static void log_msg(const char *m)
{
    puts(m);
}

#define LOG(m) log_msg(m)

void quiet(void)
{
    LOG("hidden in the macro body");
}

void loud(void)
{
    log_msg("direct call");
}
