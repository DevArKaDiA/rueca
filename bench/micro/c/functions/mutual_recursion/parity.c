static int is_odd(unsigned n);

static int is_even(unsigned n)
{
    return n == 0 ? 1 : is_odd(n - 1);
}

static int is_odd(unsigned n)
{
    return n == 0 ? 0 : is_even(n - 1);
}

int parity(unsigned n)
{
    return is_even(n);
}
