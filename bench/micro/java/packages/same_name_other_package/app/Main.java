package app;

import data.Parser;

class Main {
    void load(String text) {
        Parser p = new Parser();
        p.parse(text);
    }
}
