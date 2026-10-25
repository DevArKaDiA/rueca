class Job {
public:
    void run() {
        prepare();
        execute();
    }
    void prepare() {}
    void execute() {}
};
