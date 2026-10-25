package app;

class Engine {
    static Engine create() {
        return new Engine();
    }

    void start() {
    }
}

class Ignition {
    void turn() {
        Engine.create().start();
    }
}
