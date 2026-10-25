void emit() {}

class Logger {
public:
    void emit() {}
    void run() { emit(); }
    void run_global() { ::emit(); }
};

void free_run() { emit(); }
