package app;

class Fmt {
    static String wrap(String s) {
        return "[" + s + "]";
    }

    static String pad(String s) {
        return " " + s;
    }

    static String both(String s) {
        return wrap(pad(s));
    }
}
