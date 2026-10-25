package app;

import java.util.List;

class Item {
    boolean active() {
        return true;
    }

    int price() {
        return 1;
    }
}

class Cart {
    int total(List<Item> items) {
        return items.stream().filter(i -> i.active()).mapToInt(i -> i.price()).sum();
    }
}
