package app;

class Scheduler {
    void tick() {
    }

    Runnable task() {
        return new Runnable() {
            @Override
            public void run() {
                tick();
            }
        };
    }

    void start() {
        task().run();
    }
}
