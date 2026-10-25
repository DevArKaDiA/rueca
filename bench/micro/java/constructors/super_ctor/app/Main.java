package app;

class Animal {
    final String name;

    Animal(String name) {
        this.name = name;
    }
}

class Dog extends Animal {
    Dog() {
        super("dog");
    }
}

class Zoo {
    Animal adopt() {
        return new Dog();
    }
}
