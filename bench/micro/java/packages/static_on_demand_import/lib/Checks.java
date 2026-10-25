package lib;

public class Checks {
    public static void notEmpty(String s) {
        if (s.isEmpty()) {
            throw new IllegalArgumentException("empty");
        }
    }

    public static void positive(int n) {
        if (n <= 0) {
            throw new IllegalArgumentException("negative");
        }
    }
}
