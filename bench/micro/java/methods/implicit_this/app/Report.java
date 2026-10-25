package app;

class Report {
    String render() {
        return header() + body();
    }

    String header() {
        return "H";
    }

    String body() {
        return "B";
    }
}
