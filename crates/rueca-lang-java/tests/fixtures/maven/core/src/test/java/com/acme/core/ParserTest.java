package com.acme.core;

import static org.junit.jupiter.api.Assertions.assertEquals;

import org.junit.jupiter.api.BeforeEach;
import org.junit.jupiter.api.Nested;
import org.junit.jupiter.api.Test;
import org.junit.jupiter.params.ParameterizedTest;
import org.junit.jupiter.params.provider.ValueSource;

class ParserTest {
    private Parser parser;

    @BeforeEach
    void setUp() {
        parser = new Parser();
    }

    @Test
    void parsesDefault() {
        assertEquals(0, parser.parse("x").size());
    }

    @ParameterizedTest
    @ValueSource(strings = {"a", "b"})
    void parsesMany(String text) {
        parser.parse(text, 3);
    }

    @Nested
    class Options {
        @Test
        void limit() {
            new Parser.Options().withLimit(2);
        }
    }

    private Parser helper() {
        return parser;
    }
}
