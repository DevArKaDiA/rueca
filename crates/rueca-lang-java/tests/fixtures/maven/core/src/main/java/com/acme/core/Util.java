package com.acme.core;

public final class Util {
    private Util() {}

    public static int max(int a, int b) {
        return a > b ? a : b;
    }

    public static long max(long a, long b) {
        return a > b ? a : b;
    }

    public static String join(String sep, String... parts) {
        return String.join(sep, parts);
    }
}
