package app;

interface Ids {
    static String next() {
        return "id";
    }
}

class Orders {
    String newOrder() {
        return Ids.next();
    }
}
