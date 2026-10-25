package app;

import java.util.List;

class Lists {
    static <T> T first(List<T> xs) {
        return xs.get(0);
    }
}

class Word {
    int length() {
        return 0;
    }
}

class Dictionary {
    int firstLength(List<Word> words) {
        return Lists.<Word>first(words).length();
    }
}
