package app;

interface Closeable2 {
    void close();
}

interface Channel extends Closeable2 {
    void write(String data);
}

class Session {
    void finish(Channel ch) {
        ch.write("bye");
        ch.close();
    }
}
