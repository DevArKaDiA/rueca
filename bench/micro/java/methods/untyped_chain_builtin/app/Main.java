package app;

import java.util.ArrayList;
import java.util.List;

class Names {
    List<String> names = new ArrayList<>();

    int count() {
        names.add("a");
        return names.size();
    }

    String first() {
        return names.get(0).trim();
    }
}
