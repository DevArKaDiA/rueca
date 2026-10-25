package app;

class Box<T> {
    private T value;

    T get() {
        return value;
    }

    void put(T v) {
        value = v;
    }
}

class Fruit {
    String name() {
        return "apple";
    }
}

class Pantry {
    String take(Box<Fruit> box) {
        box.put(new Fruit());
        return box.get().name();
    }
}
