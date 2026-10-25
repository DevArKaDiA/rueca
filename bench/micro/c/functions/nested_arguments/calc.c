static int inc(int x)
{
    return x + 1;
}

static int dbl(int x)
{
    return x * 2;
}

static int sum(int a, int b)
{
    return a + b;
}

int calc(int v)
{
    return sum(dbl(inc(v)), inc(v));
}
