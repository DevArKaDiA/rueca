unsigned long fact(unsigned n)
{
    if (n < 2)
        return 1;
    return n * fact(n - 1);
}

int main(void)
{
    return (int)fact(5);
}
