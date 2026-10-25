package app;

interface Notifier {
    void send(String msg);
}

class EmailNotifier implements Notifier {
    @Override
    public void send(String msg) {
    }
}

class Alerts {
    void fire(Notifier n) {
        n.send("down");
    }
}
