class Engine {
public:
    void start() {}
};

class Car {
public:
    void drive() {
        engine.start();
        this->engine.start();
    }

private:
    Engine engine;
};
