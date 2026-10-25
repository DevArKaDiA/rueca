package app;

class QueryBuilder {
    QueryBuilder select(String col) {
        return this;
    }

    QueryBuilder where(String cond) {
        return this;
    }

    String build() {
        return "q";
    }
}

class Query {
    String make() {
        return new QueryBuilder().select("a").where("b").build();
    }
}
