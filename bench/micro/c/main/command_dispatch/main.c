static int cmd_add(void)
{
    return 1;
}

static int cmd_remove(void)
{
    return 2;
}

static int usage(void)
{
    return 64;
}

int main(int argc, char **argv)
{
    (void)argv;
    switch (argc) {
    case 2:
        return cmd_add();
    case 3:
        return cmd_remove();
    default:
        return usage();
    }
}
