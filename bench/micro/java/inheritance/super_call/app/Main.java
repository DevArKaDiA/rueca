package app;

class Widget {
    void draw() {
    }
}

class Button extends Widget {
    @Override
    void draw() {
        super.draw();
        label();
    }

    void label() {
    }
}
