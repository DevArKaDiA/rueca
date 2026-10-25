package app;

class A {
    void ping() {
    }
}

class B extends A {
}

class C extends B {
    void call() {
        ping();
    }
}

class Runner {
    void go(C c) {
        c.ping();
        c.call();
    }
}
