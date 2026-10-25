#define MAX(a, b) ((a) > (b) ? (a) : (b))

static int left(void)
{
    return 1;
}

static int right(void)
{
    return 2;
}

int biggest(void)
{
    return MAX(left(), right());
}
