package com.acme.core;

import java.io.IOException;
import java.util.List;

/**
 * Parses text into shapes.
 */
public class Parser extends Base {
    public static final String DEFAULT = "default";
    private final Options options;

    public Parser() {
        this(new Options());
    }

    public Parser(Options options) {
        this.options = options;
    }

    /** Parses with the default limit. */
    public List<Shape> parse(String text) {
        return parse(text, options.limit);
    }

    public List<Shape> parse(String text, int limit) throws IOException {
        Cursor cursor = new Cursor(text);
        cursor.advance();
        close();
        return List.of();
    }

    @Override
    public void run() {
        super.run();
        parse(DEFAULT);
    }

    /** Static nested class. */
    public static class Options {
        int limit = 10;

        public Options withLimit(int limit) {
            this.limit = limit;
            return this;
        }
    }

    /** Inner (non-static) class. */
    class Cursor {
        private final String text;

        Cursor(String text) {
            this.text = text;
        }

        void advance() {
            log(text);
        }
    }
}

class ParserHelper {
    static Parser create() {
        return new Parser();
    }
}
