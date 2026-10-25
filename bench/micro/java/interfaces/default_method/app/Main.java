package app;

interface Greeter {
    String name();

    default String greet() {
        return "hi " + name();
    }
}

class English implements Greeter {
    @Override
    public String name() {
        return "Bob";
    }
}

class Hall {
    String welcome(English e) {
        return e.greet();
    }
}
