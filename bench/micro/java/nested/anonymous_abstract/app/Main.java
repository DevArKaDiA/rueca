package app;

abstract class Step {
    abstract void exec();

    void log() {
    }
}

class Pipeline {
    void build() {
        Step s = new Step() {
            @Override
            void exec() {
                log();
            }
        };
        s.exec();
    }
}
