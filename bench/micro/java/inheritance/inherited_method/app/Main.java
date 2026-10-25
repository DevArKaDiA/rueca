package app;

class Vehicle {
    void honk() {
    }
}

class Truck extends Vehicle {
    void load() {
    }
}

class Depot {
    void dispatch(Truck truck) {
        truck.load();
        truck.honk();
    }
}
