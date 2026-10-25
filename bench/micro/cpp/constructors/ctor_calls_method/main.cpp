class Service {
public:
    Service() { init(); }
    ~Service() { shutdown(); }
    void init() {}
    void shutdown() {}
};

Service *boot() { return new Service(); }
