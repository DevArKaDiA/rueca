static int apply(int (*f)(int), int x)
{
    return f(x);
}

static int inc(int x)
{
    return x + 1;
}

int use_apply(void)
{
    return apply(inc, 2);
}
