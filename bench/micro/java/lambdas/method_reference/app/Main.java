package app;

import java.util.List;

class Text {
    static String upper(String s) {
        return s.toUpperCase();
    }

    static String lower(String s) {
        return s.toLowerCase();
    }
}

class Pipeline {
    List<String> shout(List<String> words) {
        return words.stream().map(Text::upper).toList();
    }

    String quiet(String w) {
        return Text.lower(w);
    }
}
