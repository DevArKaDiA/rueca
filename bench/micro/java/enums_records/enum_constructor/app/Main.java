package app;

enum Planet {
    EARTH(1.0);

    private final double mass;

    Planet(double mass) {
        this.mass = check(mass);
    }

    static double check(double m) {
        return m;
    }
}
