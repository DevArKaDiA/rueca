package app;

class Repository {
    void save(String row) {
    }
}

class Service {
    private final Repository repo = new Repository();

    void store(String row) {
        repo.save(row);
        this.repo.save(row);
    }
}
