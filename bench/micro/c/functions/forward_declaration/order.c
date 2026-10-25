static int late(int x);

int early(int x)
{
    return late(x) + 1;
}

static int late(int x)
{
    return x * x;
}
