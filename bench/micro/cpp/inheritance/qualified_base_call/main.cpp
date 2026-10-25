class Logger {
public:
    virtual void log() {}
    virtual ~Logger() {}
};

class TimedLogger : public Logger {
public:
    void log() override {
        stamp();
        Logger::log();
    }
    void stamp() {}
};
