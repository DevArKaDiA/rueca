package app;

class Token {
    String value() {
        return "t";
    }
}

class Lexer {
    String next() {
        Token t = new Token();
        return t.value();
    }
}
