package app;

class Connection {
    Connection(String url) {
        open(url);
    }

    void open(String url) {
    }
}

class Pool {
    Connection create() {
        return new Connection("db://local");
    }
}
