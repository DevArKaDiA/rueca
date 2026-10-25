package app;

class Primary {
    String get() {
        return "p";
    }
}

class Router {
    private Primary primary = new Primary();

    Primary pick() {
        return primary;
    }

    String route(boolean fast) {
        Primary target = fast ? primary : pick();
        return target.get();
    }
}
