package app;

class Engine {
    void start() {
    }
}

class Car {
    void drive() {
        var engine = new Engine();
        engine.start();
    }
}
