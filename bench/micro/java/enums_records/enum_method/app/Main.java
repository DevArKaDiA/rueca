package app;

enum Color {
    RED, GREEN;

    String label() {
        return name().toLowerCase();
    }
}

class Palette {
    String first() {
        return Color.RED.label();
    }

    String any(Color c) {
        return c.label();
    }
}
