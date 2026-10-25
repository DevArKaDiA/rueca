package com.acme.core;

/** Colors with a code. */
public enum Color {
    RED("r"),
    GREEN("g"),
    BLUE("b") {
        @Override
        public String code() {
            return "blue";
        }
    };

    private final String code;

    Color(String code) {
        this.code = code;
    }

    public String code() {
        return code;
    }

    public static Color parse(String code) {
        for (Color c : values()) {
            if (c.code().equals(code)) return c;
        }
        return RED;
    }
}
