typedef void (*handler_fn)(int);

static handler_fn current;

static void register_handler(handler_fn h)
{
    current = h;
}

static void on_event(int code)
{
    (void)code;
}

void setup(void)
{
    register_handler(on_event);
}

void fire(int code)
{
    current(code);
}
