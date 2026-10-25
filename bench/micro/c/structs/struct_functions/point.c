struct point {
    int x;
    int y;
};

void point_init(struct point *p, int x, int y)
{
    p->x = x;
    p->y = y;
}

int point_len2(const struct point *p)
{
    return p->x * p->x + p->y * p->y;
}

int origin_distance(int x, int y)
{
    struct point p;
    point_init(&p, x, y);
    return point_len2(&p);
}
