struct ops {
    int (*run)(int);
};

static int square(int x)
{
    return x * x;
}

static const struct ops table = { square };

int run_ops(const struct ops *o, int v)
{
    return o->run(v) + table.run(v);
}
