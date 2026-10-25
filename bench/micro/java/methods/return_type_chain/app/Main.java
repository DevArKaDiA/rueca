package app;

class Address {
    String city() {
        return "x";
    }
}

class User {
    Address address() {
        return new Address();
    }
}

class Directory {
    User find(String id) {
        return new User();
    }

    String cityOf(String id) {
        return find(id).address().city();
    }
}
