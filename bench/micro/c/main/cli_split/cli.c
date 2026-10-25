#include "cli.h"

static int usage(void)
{
    return 2;
}

int parse_args(int argc, char **argv)
{
    (void)argv;
    return argc > 1 ? 1 : 0;
}

int run(int mode)
{
    if (mode == 0)
        return usage();
    return 0;
}
