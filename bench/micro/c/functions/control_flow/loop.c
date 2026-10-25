static int check(int x)
{
    return x > 3;
}

static int next(int *state)
{
    return --*state;
}

static int fallback(void)
{
    return -1;
}

int drive(int start)
{
    int s = start;
    int total = 0;
    while (next(&s) > 0) {
        if (check(s))
            total += s;
    }
    return total > 0 ? total : fallback();
}
