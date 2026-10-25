static int add(int a, int b)
{
    return a + b;
}

int compute(void)
{
    return add(1, 2);
}

int main(void)
{
    return compute();
}
