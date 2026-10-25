package app;

class Registry {
    static int loaded = load();

    static {
        register("core");
    }

    static int load() {
        return 1;
    }

    static void register(String name) {
    }

    void refresh() {
        register("extra");
    }
}
