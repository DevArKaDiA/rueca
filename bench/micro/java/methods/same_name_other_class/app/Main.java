package app;

class FileStore {
    void save(String s) {
    }
}

class DbStore {
    void save(String s) {
    }
}

class Saver {
    void persist(FileStore files, String s) {
        files.save(s);
    }
}
