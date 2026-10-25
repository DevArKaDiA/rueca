#define SQ(x) ((x) * (x))

static int offset(void)
{
    return 3;
}

int area(int s)
{
    return SQ(s) + offset();
}
