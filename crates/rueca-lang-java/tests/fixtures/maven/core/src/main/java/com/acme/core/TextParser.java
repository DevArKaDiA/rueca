package com.acme.core;

import java.util.List;

public class TextParser extends Parser {
    @Override
    public List<Shape> parse(String text) {
        return super.parse(text.trim());
    }
}
