package app;

class Outer {
    static class Config {
        static Config defaults() {
            return new Config();
        }

        int timeout() {
            return 5;
        }
    }

    int wait2() {
        return Outer.Config.defaults().timeout();
    }
}
