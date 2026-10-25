static int op_add(int a, int b)
{
    return a + b;
}

static int op_sub(int a, int b)
{
    return a - b;
}

static int (*const ops[])(int, int) = { op_add, op_sub };

int dispatch(int i, int a, int b)
{
    return ops[i](a, b);
}
