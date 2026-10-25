package app;

import lib.*;

class Main {
    void go(Store store) {
        Item item = store.fetch("a");
        item.describe();
    }
}
