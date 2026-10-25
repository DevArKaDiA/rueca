package app;

class Base {
    Base() {
        setup();
    }

    void setup() {
    }
}

class Child extends Base {
    void work() {
    }
}

class Factory {
    Child make() {
        Child c = new Child();
        c.work();
        return c;
    }
}
