package app;

class ValidationException extends RuntimeException {
    ValidationException(String field) {
        super("invalid " + field);
    }
}

class Validator {
    void require(String field, String value) {
        if (value == null) {
            throw new ValidationException(field);
        }
    }
}
