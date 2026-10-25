package app;

class Pet {
}

class Cat extends Pet {
    void purr() {
    }
}

class Owner {
    void pet(Pet p) {
        ((Cat) p).purr();
    }

    void maybe(Pet p) {
        if (p instanceof Cat c) {
            c.purr();
        }
    }
}
