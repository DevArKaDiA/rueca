static int twice(int x)
{
    return 2 * x;
}

int indirect(int v)
{
    int (*op)(int) = twice;
    return op(v);
}

int direct(int v)
{
    return twice(v);
}
