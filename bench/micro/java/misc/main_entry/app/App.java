package app;

public class App {
    private final Server server;

    App(Server server) {
        this.server = server;
    }

    void run() {
        server.listen(8080);
    }

    public static void main(String[] args) {
        new App(new Server()).run();
    }
}
