package app;

import static lib.Checks.*;

class Main {
    void validate(String name, int age) {
        notEmpty(name);
        positive(age);
    }
}
