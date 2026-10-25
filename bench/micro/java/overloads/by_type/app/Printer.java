package app;

class Printer {
    void print(int n) {
    }

    void print(String s) {
    }

    void print(Object o) {
    }
}

class Client {
    void show(Printer p) {
        p.print("text");
    }

    void showNumber(Printer p) {
        p.print(42);
    }
}
