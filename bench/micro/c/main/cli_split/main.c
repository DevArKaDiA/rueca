#include "cli.h"

int main(int argc, char **argv)
{
    return run(parse_args(argc, argv));
}
