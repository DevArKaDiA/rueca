int lib_entry(int x);

static int format_local(int x)
{
    return x;
}

int main(void)
{
    return lib_entry(1) + format_local(2);
}
